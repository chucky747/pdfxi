//! End-to-end smoke tests for the `pdfxi` command-line binary.
//!
//! Every test writes a deterministic fixture produced by `pdfxi-testkit` to a
//! unique temporary file, runs the real binary through [`std::process::Command`],
//! and inspects the exit status, stdout and stderr. No external test harness is
//! required.

use std::fs;
use std::path::{Path, PathBuf};
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
    one_page_pdf_versioned("1.7", "", content_dict_extra, content)
}

/// Build a one-page PDF with a chosen header version and optional catalog
/// entries, computing all cross-reference offsets from real byte lengths.
fn one_page_pdf_versioned(
    version: &str,
    catalog_extra: &str,
    content_dict_extra: &str,
    content: &str,
) -> Vec<u8> {
    let dictionary = if content_dict_extra.is_empty() {
        format!("<< /Length {} >>", content.len())
    } else {
        format!("<< /Length {} {} >>", content.len(), content_dict_extra)
    };
    let catalog = if catalog_extra.is_empty() {
        "<< /Type /Catalog /Pages 2 0 R >>".to_owned()
    } else {
        format!("<< /Type /Catalog /Pages 2 0 R {catalog_extra} >>")
    };
    let objects = [
        catalog,
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R >>".to_owned(),
        format!("{dictionary}\nstream\n{content}\nendstream"),
    ];

    let mut out = format!("%PDF-{version}\n");
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

// ---------------------------------------------------------------------------
// create (CREATE-003 images, CREATE-004 text/Markdown)
// ---------------------------------------------------------------------------

/// A unique temporary path used as either a command input or output.
struct TempPath {
    path: PathBuf,
    display: String,
}

impl TempPath {
    /// Choose a unique, not-yet-created path with the given extension.
    fn new(extension: &str) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let name = format!(
            "pdfxi-cli-path-{}-{nanos}-{id}.{extension}",
            std::process::id()
        );
        let path = std::env::temp_dir().join(name);
        let display = path.to_string_lossy().into_owned();
        Self { path, display }
    }

    /// Write bytes to this path (for use as a command input).
    fn write(&self, bytes: &[u8]) {
        fs::write(&self.path, bytes).expect("write temporary input");
    }

    /// Return the path as a command-line argument.
    fn arg(&self) -> &str {
        &self.display
    }
}

impl Drop for TempPath {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

/// A compact JPEG-shaped byte buffer with a 300 dpi JFIF density marker.
fn sample_jpeg() -> Vec<u8> {
    let mut out = vec![0xFF, 0xD8];
    let mut app0 = Vec::from(&b"JFIF\0"[..]);
    app0.extend_from_slice(&[1, 1]);
    app0.extend_from_slice(&300u16.to_be_bytes());
    app0.extend_from_slice(&300u16.to_be_bytes());
    app0.extend_from_slice(&[0, 0]);
    out.push(0xFF);
    out.push(0xE0);
    out.extend_from_slice(&((app0.len() + 2) as u16).to_be_bytes());
    out.extend_from_slice(&app0);
    let mut sof = vec![8];
    sof.extend_from_slice(&100u16.to_be_bytes());
    sof.extend_from_slice(&200u16.to_be_bytes());
    sof.extend_from_slice(&[3, 1, 0x11, 0, 2, 0x11, 1, 0x11, 1]);
    out.push(0xFF);
    out.push(0xC0);
    out.extend_from_slice(&((sof.len() + 2) as u16).to_be_bytes());
    out.extend_from_slice(&sof);
    out.extend_from_slice(&[0xFF, 0xDA, 0x00, 0x02, 0xFF, 0xD9]);
    out
}

/// Append one PNG chunk. The embedder does not validate CRCs, so a zero CRC is
/// sufficient for this structural fixture.
fn push_png_chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    out.extend_from_slice(&[0u8; 4]);
}

/// A minimal non-interlaced 8-bit RGB PNG fixture.
fn sample_png() -> Vec<u8> {
    let mut out = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&2u32.to_be_bytes());
    ihdr.extend_from_slice(&2u32.to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
    push_png_chunk(&mut out, b"IHDR", &ihdr);
    push_png_chunk(
        &mut out,
        b"IDAT",
        &[0x78, 0x9C, 0x00, 0x00, 0x00, 0xFF, 0xFF],
    );
    push_png_chunk(&mut out, b"IEND", &[]);
    out
}

/// Run `create text` over `text` and return the generated PDF path.
fn create_text_pdf(text: &str) -> TempPath {
    let input = TempPath::new("txt");
    input.write(text.as_bytes());
    let pdf = TempPath::new("pdf");
    let output = run(&["create", "text", input.arg(), pdf.arg()]);
    assert!(
        output.status.success(),
        "create text failed: {}",
        stderr(&output)
    );
    pdf
}

#[test]
fn create_text_writes_a_parseable_pdf() {
    let pdf = create_text_pdf("Hello PDFXI.\nSecond line.\nThird line.");
    let bytes = fs::read(&pdf.path).expect("read generated PDF");
    assert!(bytes.starts_with(b"%PDF-"), "not a PDF: {:?}", &bytes[..8]);
    let document = pdfxi_core::Document::from_bytes(bytes).expect("core parses generated PDF");
    assert_eq!(document.page_count().expect("page count"), 1);
    let text = document.page_text(0).expect("page text");
    assert!(text.contains("Hello PDFXI."), "text: {text:?}");
    assert!(text.contains("Third line."), "text: {text:?}");
}

#[test]
fn create_text_output_is_deterministic() {
    let input = TempPath::new("txt");
    input.write(b"Deterministic document.\nLine two.");
    let a = TempPath::new("pdf");
    let b = TempPath::new("pdf");
    for out in [&a, &b] {
        let output = run(&["create", "text", input.arg(), out.arg()]);
        assert!(output.status.success(), "stderr: {}", stderr(&output));
    }
    assert_eq!(
        fs::read(&a.path).expect("first PDF"),
        fs::read(&b.path).expect("second PDF"),
        "identical inputs must produce byte-identical PDFs"
    );
}

#[test]
fn create_markdown_by_extension_and_by_flag() {
    let md = TempPath::new("md");
    md.write(b"# Title\n\nBody text.\n");
    let md_pdf = TempPath::new("pdf");
    let output = run(&["create", "text", md.arg(), md_pdf.arg()]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let document =
        pdfxi_core::Document::from_bytes(fs::read(&md_pdf.path).expect("read markdown PDF"))
            .expect("parse markdown PDF");
    let text = document.page_text(0).expect("page text");
    assert!(text.contains("Title"), "heading missing: {text:?}");
    assert!(text.contains("Body text."), "paragraph missing: {text:?}");

    // The --markdown flag forces Markdown parsing regardless of extension.
    let txt = TempPath::new("txt");
    txt.write(b"# Title\n\nBody text.\n");
    let flag_pdf = TempPath::new("pdf");
    let output = run(&["create", "text", txt.arg(), flag_pdf.arg(), "--markdown"]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let flagged =
        pdfxi_core::Document::from_bytes(fs::read(&flag_pdf.path).expect("read flagged PDF"))
            .expect("parse flagged PDF");
    let flagged_text = flagged.page_text(0).expect("page text");
    assert!(flagged_text.contains("Title"), "text: {flagged_text:?}");
    assert!(
        !flagged_text.contains("# Title"),
        "Markdown heading marker must be consumed, not emitted literally: {flagged_text:?}"
    );
}

#[test]
fn create_text_empty_input_is_a_typed_error() {
    let input = TempPath::new("txt");
    input.write(b"   \n");
    let out = TempPath::new("pdf");
    let output = run(&["create", "text", input.arg(), out.arg()]);
    assert!(!output.status.success(), "blank input must fail");
    assert!(
        stderr(&output).contains("cannot create a PDF from empty input"),
        "stderr: {}",
        stderr(&output)
    );
    assert!(!out.path.exists(), "no PDF should be written on failure");
}

#[test]
fn create_image_jpeg_writes_a_parseable_pdf() {
    let input = TempPath::new("jpg");
    input.write(&sample_jpeg());
    let out = TempPath::new("pdf");
    let output = run(&["create", "image", input.arg(), out.arg()]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let document = pdfxi_core::Document::from_bytes(fs::read(&out.path).expect("read image PDF"))
        .expect("core parses image PDF");
    assert_eq!(document.page_count().expect("page count"), 1);
    assert_eq!(document.version(), "1.4");
}

#[test]
fn create_image_png_writes_a_parseable_pdf() {
    let input = TempPath::new("png");
    input.write(&sample_png());
    let out = TempPath::new("pdf");
    let output = run(&["create", "image", input.arg(), out.arg()]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let document = pdfxi_core::Document::from_bytes(fs::read(&out.path).expect("read PNG PDF"))
        .expect("core parses PNG PDF");
    assert_eq!(document.page_count().expect("page count"), 1);
}

#[test]
fn create_image_output_is_deterministic() {
    let input = TempPath::new("jpg");
    input.write(&sample_jpeg());
    let a = TempPath::new("pdf");
    let b = TempPath::new("pdf");
    for out in [&a, &b] {
        let output = run(&["create", "image", input.arg(), out.arg()]);
        assert!(output.status.success(), "stderr: {}", stderr(&output));
    }
    assert_eq!(
        fs::read(&a.path).expect("first PDF"),
        fs::read(&b.path).expect("second PDF"),
        "identical image input must produce byte-identical PDFs"
    );
}

#[test]
fn create_image_rejects_unsupported_extension_and_malformed_bytes() {
    let gif = TempPath::new("gif");
    gif.write(b"GIF89a");
    let out = TempPath::new("pdf");
    let output = run(&["create", "image", gif.arg(), out.arg()]);
    assert!(!output.status.success(), "unsupported extension must fail");
    assert!(
        stderr(&output).contains("unsupported image extension"),
        "stderr: {}",
        stderr(&output)
    );

    let bad = TempPath::new("jpg");
    bad.write(b"not a jpeg");
    let output = run(&["create", "image", bad.arg(), out.arg()]);
    assert!(!output.status.success(), "malformed JPEG must fail");
    assert!(
        stderr(&output).contains("malformed JPEG"),
        "stderr: {}",
        stderr(&output)
    );
    assert!(!out.path.exists(), "no PDF should be written on failure");
}

// ---------------------------------------------------------------------------
// export tagged (EXPORT-007)
// ---------------------------------------------------------------------------

#[test]
fn export_tagged_writes_structured_roles_in_reading_order() {
    let pdf = create_text_pdf("Hello PDFXI.\nSecond line.\nThird line.");
    let out = TempPath::new("txt");
    let output = run(&["export", "tagged", pdf.arg(), out.arg()]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));

    let text = fs::read_to_string(&out.path).expect("read tagged output");
    assert!(text.contains("@document"), "output: {text:?}");
    assert!(text.contains("@page"), "output: {text:?}");
    assert!(text.contains("@p "), "output: {text:?}");
    let document_at = text.find("@document").expect("document role");
    let page_at = text.find("@page").expect("page role");
    let paragraph_at = text.find("@p ").expect("paragraph role");
    assert!(
        document_at < page_at && page_at < paragraph_at,
        "structure roles must appear in document/page/paragraph order: {text:?}"
    );
    // Text content round-trips through the derived accessibility tree.
    assert!(text.contains("Hello PDFXI."), "output: {text:?}");
    assert!(text.contains("Second line."), "output: {text:?}");
    assert!(text.contains("Third line."), "output: {text:?}");
}

#[test]
fn export_tagged_plain_has_role_markers_and_text() {
    let pdf = create_text_pdf("Alpha content.\nBeta content.");
    let out = TempPath::new("txt");
    let output = run(&[
        "export",
        "tagged",
        pdf.arg(),
        out.arg(),
        "--format",
        "plain",
    ]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));

    let text = fs::read_to_string(&out.path).expect("read plain tagged output");
    for role in ["[document]", "[page]", "[paragraph]"] {
        assert!(text.contains(role), "missing {role}: {text:?}");
    }
    assert!(text.contains("Alpha content."), "output: {text:?}");
    assert!(text.contains("Beta content."), "output: {text:?}");
}

#[test]
fn export_tagged_output_is_deterministic() {
    let pdf = create_text_pdf("Deterministic tagged export.");
    let a = TempPath::new("txt");
    let b = TempPath::new("txt");
    for out in [&a, &b] {
        let output = run(&["export", "tagged", pdf.arg(), out.arg()]);
        assert!(output.status.success(), "stderr: {}", stderr(&output));
    }
    assert_eq!(
        fs::read(&a.path).expect("first export"),
        fs::read(&b.path).expect("second export"),
        "identical PDFs must produce byte-identical tagged text"
    );
}

#[test]
fn export_tagged_rejects_a_non_pdf_input() {
    let input = TempPath::new("pdf");
    input.write(b"this is not a PDF");
    let out = TempPath::new("txt");
    let output = run(&["export", "tagged", input.arg(), out.arg()]);
    assert!(!output.status.success(), "non-PDF input must fail");
    assert!(
        stderr(&output).contains("not a parseable PDF"),
        "stderr: {}",
        stderr(&output)
    );
    assert!(!out.path.exists(), "no output should be written on failure");
}

// ---------------------------------------------------------------------------
// api (AUTO-006 local JSON API, AUTO-007 custom toolsets)
// ---------------------------------------------------------------------------

use std::io::Write;
use std::process::Stdio;

/// Runs the real binary with `args`, writing `input` to its stdin.
fn run_with_stdin(args: &[&str], input: &str) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_pdfxi"))
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn pdfxi binary");
    if let Some(stdin) = child.stdin.as_mut() {
        stdin
            .write_all(input.as_bytes())
            .expect("write request lines");
    }
    child.wait_with_output().expect("collect api output")
}

/// Writes a deterministic two-action file used by the API tests.
fn api_action_file() -> TempPath {
    let file = TempPath::new("txt");
    file.write(
        b"ACTION\tnightly\tNightly staging\n\
STEP\tload\timport\n\
STEP\tsave\texport\n\
PARAM\ttarget\tout.pdf\n\
\n\
ACTION\tdanger\tUnsafe\n\
STEP\tsec\tsecurity\n\
PARAM\tpassword\tsecret\n",
    );
    file
}

#[test]
fn api_once_answers_one_canonical_json_line() {
    let request =
        serde_json::json!({"version": 1, "id": "one", "command": "version", "params": {}})
            .to_string();
    let mut input = request.clone();
    input.push('\n');
    input.push_str("{\"version\":1,\"id\":\"two\",\"command\":\"version\",\"params\":{}}\n");
    let output = run_with_stdin(&["api", "--once"], &input);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let text = stdout(&output);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 1, "--once must answer exactly one request");
    let value: Value = serde_json::from_str(lines[0]).expect("canonical JSON response");
    assert_eq!(value["id"].as_str(), Some("one"));
    assert_eq!(value["ok"].as_bool(), Some(true));
    assert_eq!(value["version"].as_u64(), Some(1));
    assert!(value["result"]["engine"].as_str().is_some());
    assert!(value["error"].is_null());
}

#[test]
fn api_stream_answers_every_nonblank_line_in_order() {
    let first =
        serde_json::json!({"version": 1, "id": 1, "command": "commands", "params": {}}).to_string();
    let second =
        serde_json::json!({"version": 1, "id": 2, "command": "version", "params": {}}).to_string();
    let input = format!("\n{first}\n   \n{second}\n");
    let output = run_with_stdin(&["api"], &input);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let text = stdout(&output);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 2, "blank lines must not produce responses");
    let ids: Vec<String> = lines
        .iter()
        .map(|line| {
            let value: Value = serde_json::from_str(line).expect("JSON line");
            value["id"].as_str().unwrap_or("").to_owned()
        })
        .collect();
    assert_eq!(
        ids,
        ["1", "2"],
        "numeric ids echo as strings in request order"
    );
}

#[test]
fn api_reports_errors_without_exiting_the_process() {
    let valid = serde_json::json!({"version": 1, "id": "ok", "command": "version", "params": {}})
        .to_string();
    let bad = serde_json::json!({"version": 9, "id": "bad", "command": "version"}).to_string();
    let input = format!("not json\n{valid}\n{bad}\n");
    let output = run_with_stdin(&["api"], &input);
    assert!(
        output.status.success(),
        "protocol errors are responses, not crashes"
    );
    let text = stdout(&output);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 3);
    let values: Vec<Value> = lines
        .iter()
        .map(|line| serde_json::from_str(line).expect("JSON line"))
        .collect();
    assert_eq!(values[0]["error"]["code"].as_str(), Some("invalid_json"));
    assert_eq!(values[1]["ok"].as_bool(), Some(true));
    assert_eq!(
        values[2]["error"]["code"].as_str(),
        Some("version_mismatch")
    );
}

#[test]
fn api_dry_run_batch_and_toolset_use_local_files() {
    let actions = api_action_file();
    let input_pdf = TempPath::new("pdf");
    input_pdf.write(b"not a real PDF, only staged");
    let toolset = TempPath::new("json");
    toolset.write(
        serde_json::json!({
            "version": 1,
            "name": "maintenance",
            "tools": {
                "daily": {
                    "file": actions.arg(),
                    "action": "nightly",
                    "defaults": {"note": "daily"}
                }
            }
        })
        .to_string()
        .as_bytes(),
    );

    let dry = serde_json::json!({
        "version": 1, "id": "dry", "command": "dry-run",
        "params": {"file": actions.arg(), "action": "nightly"}
    })
    .to_string();
    let batch = serde_json::json!({
        "version": 1, "id": "batch", "command": "run-batch",
        "params": {
            "file": actions.arg(), "action": "nightly",
            "inputs": [input_pdf.arg()]
        }
    })
    .to_string();
    let run_tool = serde_json::json!({
        "version": 1, "id": "tool", "command": "run-toolset",
        "params": {
            "file": toolset.arg(), "tool": "daily",
            "inputs": [input_pdf.arg()]
        }
    })
    .to_string();
    let input = format!("{dry}\n{batch}\n{run_tool}\n");
    let output = run_with_stdin(&["api"], &input);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let text = stdout(&output);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 3);
    let values: Vec<Value> = lines
        .iter()
        .map(|line| serde_json::from_str(line).expect("JSON line"))
        .collect();
    assert_eq!(values[0]["ok"].as_bool(), Some(true));
    assert_eq!(values[0]["result"]["steps"].as_u64(), Some(2));
    assert_eq!(values[1]["ok"].as_bool(), Some(true));
    assert_eq!(
        values[1]["result"]["run"]["jobs"][0]["state"].as_str(),
        Some("succeeded")
    );
    assert_eq!(
        values[2]["ok"].as_bool(),
        Some(true),
        "response: {}",
        lines[2]
    );
    assert_eq!(values[2]["result"]["tool"].as_str(), Some("daily"));
}

#[test]
fn api_rejects_dangerous_aliased_operations() {
    let actions = api_action_file();
    let wrapped = TempPath::new("json");
    wrapped.write(
        serde_json::json!({
            "version": 1,
            "name": "maintenance",
            "tools": {
                "daily": {"file": actions.arg(), "action": "danger"}
            }
        })
        .to_string()
        .as_bytes(),
    );
    let input_pdf = TempPath::new("pdf");
    input_pdf.write(b"staged");
    let request = serde_json::json!({
        "version": 1, "id": "wrapped", "command": "run-toolset",
        "params": {
            "file": wrapped.arg(), "tool": "daily",
            "inputs": [input_pdf.arg()]
        }
    })
    .to_string();
    let output = run_with_stdin(&["api", "--once"], &(request + "\n"));
    assert!(output.status.success());
    let value: Value = serde_json::from_str(stdout(&output).trim()).expect("JSON response");
    assert_eq!(value["ok"].as_bool(), Some(false));
    assert_eq!(value["error"]["code"].as_str(), Some("dangerous_operation"));
}

// ---------------------------------------------------------------------------
// preflight and marks (T-0131)
// ---------------------------------------------------------------------------

/// Assemble a classic-xref PDF from numbered object bodies.
fn t0131_pdf(version: &str, bodies: &[String]) -> Vec<u8> {
    let mut out = format!("%PDF-{version}\n%PDFXI\n").into_bytes();
    let mut offsets = Vec::with_capacity(bodies.len());
    for (index, body) in bodies.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n{}\nendobj\n", index + 1, body).as_bytes());
    }
    let xref = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n", bodies.len() + 1).as_bytes());
    out.extend_from_slice(b"0000000000 65535 f \n");
    for offset in &offsets {
        out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            bodies.len() + 1
        )
        .as_bytes(),
    );
    out
}

/// A PDF/A-2b-shaped fixture with XMP, a GTS_PDFA1 intent and print boxes.
fn t0131_conforming_pdf() -> Vec<u8> {
    let content = "BT (Hi) Tj ET";
    let xmp = "<?xpacket begin=\"\" id=\"W5M0\"?><x:xmpmeta xmlns:x=\"adobe:ns:meta/\"/>\
               <?xpacket end=\"w\"?>";
    t0131_pdf(
        "1.7",
        &[
            "<< /Type /Catalog /Pages 2 0 R /Metadata 5 0 R /OutputIntents [6 0 R] >>".to_owned(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
             /CropBox [10 10 602 782] /BleedBox [20 20 592 772] /TrimBox [30 30 582 762] \
             /Contents 4 0 R >>"
                .to_owned(),
            format!(
                "<< /Length {} >>\nstream\n{}\nendstream",
                content.len(),
                content
            ),
            format!(
                "<< /Type /Metadata /Subtype /XML /Length {} >>\nstream\n{}\nendstream",
                xmp.len(),
                xmp
            ),
            "<< /Type /OutputIntent /S /GTS_PDFA1 /DestOutputProfile 7 0 R >>".to_owned(),
            "<< /Length 0 /N 3 >>\nstream\n\nendstream".to_owned(),
        ],
    )
}

/// A violating fixture: low version, no XMP/intent, prohibited /JavaScript.
fn t0131_violating_pdf() -> Vec<u8> {
    let content = "BT (Hi) Tj ET";
    t0131_pdf(
        "1.4",
        &[
            "<< /Type /Catalog /Pages 2 0 R /JavaScript 1 >>".to_owned(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R >>".to_owned(),
            format!(
                "<< /Length {} >>\nstream\n{}\nendstream",
                content.len(),
                content
            ),
        ],
    )
}

/// A one-page fixture with enough trim inset for all printer marks.
fn t0131_roomy_pdf() -> Vec<u8> {
    let content = "BT (Hi) Tj ET";
    t0131_pdf(
        "1.7",
        &[
            "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
             /CropBox [10 10 602 782] /BleedBox [20 20 592 772] /TrimBox [30 30 582 762] \
             /Contents 4 0 R >>"
                .to_owned(),
            format!(
                "<< /Length {} >>\nstream\n{}\nendstream",
                content.len(),
                content
            ),
        ],
    )
}

#[test]
fn preflight_pass_exits_zero_and_reports_no_errors() {
    let pdf = TempPdf::new(&t0131_conforming_pdf());
    let output = run(&["preflight", "PDF/A-2b", pdf.arg()]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    assert_eq!(output.status.code(), Some(0));
    let text = stdout(&output);
    assert!(text.contains("profile: PDF/A-2b"));
    assert!(text.contains("verdict: pass"));
    assert!(text.contains("certification: not claimed"));
    assert!(text.contains("reported-only: "));
    assert!(!text.contains("error "));
}

#[test]
fn preflight_fail_exits_two_with_actionable_codes() {
    let pdf = TempPdf::new(&t0131_violating_pdf());
    let output = run(&["preflight", "PDF/A-2b", pdf.arg()]);
    assert_eq!(output.status.code(), Some(2), "stderr: {}", stderr(&output));
    let text = stdout(&output);
    assert!(text.contains("verdict: fail"));
    for code in [
        "PDFA2B.VERSION_TOO_LOW",
        "PDFA2B.NO_OUTPUT_INTENT",
        "PDFA2B.NO_XMP_METADATA",
        "PDFA2B.FORBIDDEN_ENTRY",
    ] {
        assert!(text.contains(code), "missing {code} in {text}");
    }
}

#[test]
fn preflight_json_is_stable_machine_readable() {
    let pdf = TempPdf::new(&t0131_violating_pdf());
    let first = run(&["preflight", "pdf/a-2b", pdf.arg(), "--json"]);
    let second = run(&["preflight", "PDF/A-2b", pdf.arg(), "--json"]);
    assert_eq!(first.status.code(), Some(2));
    assert_eq!(second.status.code(), Some(2));
    assert_eq!(
        stdout(&first),
        stdout(&second),
        "JSON must be deterministic"
    );
    let value: Value = serde_json::from_str(stdout(&first).trim()).expect("preflight JSON");
    assert_eq!(value["profile"].as_str(), Some("PDF/A-2b"));
    assert_eq!(value["conforming"].as_bool(), Some(false));
    assert_eq!(value["exit_code"].as_i64(), Some(2));
    assert_eq!(value["certification_claimed"].as_bool(), Some(false));
    assert!(
        value["before"]["findings"]
            .as_array()
            .is_some_and(|a| !a.is_empty())
    );
    assert!(
        value["reported_only"]
            .as_array()
            .is_some_and(|a| !a.is_empty())
    );
}

#[test]
fn preflight_rejects_invalid_profile_and_missing_input() {
    let pdf = TempPdf::new(&t0131_conforming_pdf());
    let bad_profile = run(&["preflight", "PDF/A-9c", pdf.arg()]);
    assert_eq!(bad_profile.status.code(), Some(1));
    assert!(stderr(&bad_profile).contains("unsupported preflight profile"));

    let missing = run(&[
        "preflight",
        "PDF/A-2b",
        "C:/definitely/not/a/pdfxi-file.pdf",
    ]);
    assert_eq!(missing.status.code(), Some(1));
    assert!(stderr(&missing).contains("failed to read"));
}

#[test]
fn preflight_fix_writes_reparable_bytes_and_reports_enforced_checks() {
    let pdf = TempPdf::new(&t0131_violating_pdf());
    let out = TempOut::new("pdf");
    let output = run(&[
        "preflight",
        "PDF/A-2b",
        pdf.arg(),
        "--fix",
        "--output",
        out.arg(),
    ]);
    // The placeholder ICC and unfixed /JavaScript keep the result non-conforming.
    assert_eq!(output.status.code(), Some(2), "stderr: {}", stderr(&output));
    let bytes = fs::read(&out.path).expect("normalized output exists");
    assert_ne!(bytes, t0131_violating_pdf());
    let reparsed = pdfxi_core::Document::from_bytes(bytes).expect("normalized bytes reparse");
    assert_eq!(reparsed.page_count().expect("page count"), 1);

    let text = stdout(&output);
    assert!(text.contains("after: "));
    assert!(text.contains("fixups: "));
    assert!(text.contains("enforced: minimum_version"));
    assert!(text.contains("certification: not claimed"));
    assert!(text.contains(&format!("output: {}", out.arg())));

    let repeat = TempOut::new("pdf");
    let second = run(&[
        "preflight",
        "PDF/A-2b",
        pdf.arg(),
        "--fix",
        "--output",
        repeat.arg(),
    ]);
    assert_eq!(second.status.code(), Some(2));
    assert_eq!(
        fs::read(&out.path).expect("first bytes"),
        fs::read(&repeat.path).expect("second bytes"),
        "fix output must be deterministic"
    );
}

#[test]
fn marks_applies_all_families_and_reparses() {
    let pdf = TempPdf::new(&t0131_roomy_pdf());
    let out = TempOut::new("pdf");
    let output = run(&[
        "marks",
        pdf.arg(),
        out.arg(),
        "--crop-marks",
        "--registration",
        "--colour-bars",
        "--page-info",
    ]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let bytes = fs::read(&out.path).expect("marked output exists");
    let text = String::from_utf8_lossy(&bytes).into_owned();
    assert!(text.contains("PdfxiMarks"), "form invocation is present");
    let reparsed = pdfxi_core::Document::from_bytes(bytes).expect("marked bytes reparse");
    assert_eq!(reparsed.page_count().expect("page count"), 1);
    assert!(stdout(&output).contains("wrote "));
}

#[test]
fn marks_partial_selection_is_deterministic() {
    let pdf = TempPdf::new(&t0131_roomy_pdf());
    let first = TempOut::new("pdf");
    let second = TempOut::new("pdf");
    let a = run(&["marks", pdf.arg(), first.arg(), "--registration"]);
    let b = run(&["marks", pdf.arg(), second.arg(), "--registration"]);
    assert!(a.status.success(), "stderr: {}", stderr(&a));
    assert!(b.status.success(), "stderr: {}", stderr(&b));
    assert_eq!(
        fs::read(&first.path).expect("first marked bytes"),
        fs::read(&second.path).expect("second marked bytes")
    );
    let text = stdout(&a);
    assert!(text.contains("registration=true"));
    assert!(text.contains("crop=false"));
}

#[test]
fn marks_rejects_malformed_and_tight_box_inputs() {
    let bad = TempPdf::new(b"not a pdf");
    let out = TempOut::new("pdf");
    let output = run(&["marks", bad.arg(), out.arg()]);
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("failed to parse"));

    let tight = TempPdf::new(&pdfxi_testkit::minimal_pdf());
    let out2 = TempOut::new("pdf");
    let tight_output = run(&["marks", tight.arg(), out2.arg(), "--crop-marks"]);
    assert_eq!(tight_output.status.code(), Some(1));
    assert!(stderr(&tight_output).contains("trim"));
}

// ---------------------------------------------------------------------------
// repair (EDIT-015)
// ---------------------------------------------------------------------------

/// Build a one-page PDF in-test, then damage it by removing `startxref` so the
/// core repair path must rescan objects and rebuild the cross-reference table.
fn damaged_one_page_pdf() -> Vec<u8> {
    let source = one_page_pdf("", "BT (Hello, repair CLI!) Tj ET");
    let text = String::from_utf8(source).expect("ASCII fixture");
    let marker = "startxref\n";
    let start = text.find(marker).expect("startxref marker");
    let end = start + text[start..].find("%%EOF").expect("eof marker");
    let mut out = String::with_capacity(text.len());
    out.push_str(&text[..start]);
    out.push_str(&text[end..]);
    out.into_bytes()
}

#[test]
fn repair_repairs_a_damaged_fixture_without_touching_the_input() {
    let damaged = damaged_one_page_pdf();
    let pdf = TempPdf::new(&damaged);
    let out = TempOut::new("pdf");
    let output = run(&["repair", pdf.arg(), out.arg()]);
    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));

    let bytes = fs::read(&out.path).expect("repaired output exists");
    let document = pdfxi_core::Document::from_bytes(bytes).expect("repaired PDF reparses");
    assert_eq!(document.page_count().expect("page count"), 1);
    assert_eq!(
        document.page_text(0).expect("page text"),
        "Hello, repair CLI!"
    );
    assert_eq!(fs::read(&pdf.path).expect("input unchanged"), damaged);

    let text = stdout(&output);
    assert!(text.contains("warnings: 2"), "stdout: {text}");
    assert!(text.contains("startxref_missing"), "stdout: {text}");
    assert!(text.contains("xref_rebuilt_by_scan"), "stdout: {text}");
    assert!(text.contains("repaired: ok"), "stdout: {text}");
}

#[test]
fn repair_json_reports_typed_warnings_with_a_stable_shape() {
    let pdf = TempPdf::new(&damaged_one_page_pdf());
    let out = TempOut::new("pdf");
    let output = run(&["repair", pdf.arg(), out.arg(), "--json"]);
    assert_eq!(output.status.code(), Some(0), "stderr: {}", stderr(&output));

    let value: Value = serde_json::from_str(stdout(&output).trim()).expect("repair JSON");
    let map = value.as_object().expect("JSON object");
    for key in [
        "ok",
        "input",
        "output",
        "error",
        "version",
        "bytes",
        "pages",
        "warnings",
        "warning_count",
        "exit_code",
    ] {
        assert!(map.contains_key(key), "missing field {key}");
    }
    assert_eq!(map.len(), 10, "unexpected extra fields: {map:?}");
    assert_eq!(value["ok"].as_bool(), Some(true));
    assert!(value["error"].is_null());
    assert_eq!(value["pages"].as_u64(), Some(1));
    assert_eq!(value["exit_code"].as_i64(), Some(0));
    assert_eq!(value["input"].as_str(), Some(pdf.arg()));
    assert_eq!(value["output"].as_str(), Some(out.arg()));

    let warnings = value["warnings"].as_array().expect("warnings array");
    assert_eq!(warnings.len(), 2);
    let codes: Vec<&str> = warnings
        .iter()
        .map(|w| w["code"].as_str().unwrap_or(""))
        .collect();
    assert_eq!(codes, ["startxref_missing", "xref_rebuilt_by_scan"]);
    assert!(warnings[0]["message"].as_str().is_some());
    assert_eq!(value["warning_count"].as_u64(), Some(2));

    // The same input and output produce byte-identical JSON.
    let repeat = run(&["repair", pdf.arg(), out.arg(), "--json"]);
    assert_eq!(
        stdout(&repeat),
        stdout(&output),
        "JSON must be deterministic"
    );
}

#[test]
fn repair_refuses_to_overwrite_the_input() {
    let damaged = damaged_one_page_pdf();
    let pdf = TempPdf::new(&damaged);
    let output = run(&["repair", pdf.arg(), pdf.arg()]);
    assert_eq!(output.status.code(), Some(1), "same path must be refused");
    let message = stderr(&output);
    assert!(
        message.contains("refusing to overwrite") && message.contains("input"),
        "stderr: {message}"
    );
    assert_eq!(
        fs::read(&pdf.path).expect("input still readable"),
        damaged,
        "the input must never be modified"
    );
}

#[test]
fn repair_rejects_traversal_in_the_output_path() {
    let pdf = TempPdf::new(&damaged_one_page_pdf());
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let sentinel = format!("pdfxi-cli-repair-traversal-{nanos}.pdf");
    let outside = std::env::temp_dir().join(&sentinel);
    assert!(!outside.exists(), "traversal sentinel must start absent");
    let traversal = format!("../{sentinel}");
    let output = run(&["repair", pdf.arg(), &traversal]);
    assert_eq!(output.status.code(), Some(1), "traversal must be refused");
    assert!(
        stderr(&output).contains("traversal"),
        "stderr: {}",
        stderr(&output)
    );
    assert!(!outside.exists(), "no file may be written via traversal");
}

#[test]
fn repair_unrecoverable_input_exits_three_with_a_typed_report() {
    let pdf = TempPdf::new(b"not a PDF at all");
    let out = TempOut::new("pdf");
    let output = run(&["repair", pdf.arg(), out.arg()]);
    assert_eq!(output.status.code(), Some(3), "stderr: {}", stderr(&output));
    assert!(stderr(&output).contains("repair failed"));
    assert!(!out.path.exists(), "no output on unrecoverable input");

    let json_out = TempOut::new("pdf");
    let json = run(&["repair", pdf.arg(), json_out.arg(), "--json"]);
    assert_eq!(json.status.code(), Some(3));
    let value: Value = serde_json::from_str(stdout(&json).trim()).expect("failure JSON");
    assert_eq!(value["ok"].as_bool(), Some(false));
    assert_eq!(value["exit_code"].as_i64(), Some(3));
    assert!(
        value["error"]
            .as_str()
            .is_some_and(|message| !message.is_empty())
    );
    assert_eq!(value["warnings"].as_array().map_or(0, Vec::len), 0);
}

#[test]
fn repair_missing_input_exits_one() {
    let name = format!(
        "pdfxi-cli-repair-missing-{}-{}.pdf",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    );
    let path = std::env::temp_dir().join(name);
    assert!(!path.exists());
    let display = path.to_string_lossy().into_owned();
    let out = TempOut::new("pdf");
    let output = run(&["repair", &display, out.arg()]);
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("failed to read"));
    assert!(!out.path.exists(), "no output when the input is missing");
}

#[test]
fn compat_reports_a_14_document_and_every_gate() {
    let pdf = TempPdf::new(&one_page_pdf_versioned(
        "1.4",
        "",
        "",
        "BT /F1 12 Tf 72 720 Td (hi) Tj ET",
    ));
    let output = run(&["compat", pdf.arg()]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let text = stdout(&output);
    assert!(text.contains("effective_version: 1.4"), "{text}");
    assert!(text.contains("source: header"), "{text}");
    assert!(text.contains("supported: yes"), "{text}");
    assert!(text.contains("supported=2/12"), "{text}");
    assert!(
        text.contains("cross-reference streams: not supported (needs 1.5)"),
        "{text}"
    );
    assert!(
        text.contains("object streams: not supported (needs 1.5)"),
        "{text}"
    );
    assert!(text.contains("transparency: supported"), "{text}");
    assert!(text.contains("detected: none"), "{text}");
    assert!(text.contains("warnings: none"), "{text}");
}

#[test]
fn compat_reports_a_20_document_as_fully_supported() {
    let pdf = TempPdf::new(&one_page_pdf_versioned("2.0", "", "", "q Q"));
    let output = run(&["compat", pdf.arg()]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let text = stdout(&output);
    assert!(text.contains("effective_version: 2.0"), "{text}");
    assert!(text.contains("supported=12/12"), "{text}");
    assert!(
        text.contains("encrypted metadata: supported"),
        "every gate must be supported at 2.0: {text}"
    );
}

#[test]
fn compat_reports_a_catalog_version_override() {
    let pdf = TempPdf::new(&one_page_pdf_versioned("1.4", "/Version /1.7", "", "q Q"));
    let output = run(&["compat", pdf.arg()]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let text = stdout(&output);
    assert!(text.contains("header_version: 1.4"), "{text}");
    assert!(text.contains("effective_version: 1.7"), "{text}");
    assert!(text.contains("source: catalog"), "{text}");
    assert!(
        text.contains("catalog /Version 1.7 overrides header version 1.4"),
        "{text}"
    );
    assert!(text.contains("supported=9/12"), "{text}");
}

#[test]
fn compat_flags_a_version_outside_the_supported_range() {
    let pdf = TempPdf::new(&one_page_pdf_versioned("2.1", "", "", "q Q"));
    let output = run(&["compat", pdf.arg()]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let text = stdout(&output);
    assert!(text.contains("effective_version: 2.1"), "{text}");
    assert!(text.contains("supported: no"), "{text}");
    assert!(
        text.contains("PDF version 2.1 is outside the supported 1.0-2.0 range"),
        "{text}"
    );
}

#[test]
fn compat_strict_exits_three_for_a_construct_beyond_the_version() {
    let pdf = TempPdf::new(&one_page_pdf_versioned(
        "1.4",
        "",
        "",
        "BT /F1 12 Tf 72 720 Td (/ObjStm token) Tj ET",
    ));
    let output = run(&["compat", pdf.arg(), "--strict"]);
    assert_eq!(output.status.code(), Some(3), "stderr: {}", stderr(&output));
    assert!(
        stderr(&output).contains("does not allow: object streams"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn compat_json_has_stable_fields() {
    let pdf = TempPdf::new(&one_page_pdf_versioned("1.7", "", "", "q Q"));
    let output = run(&["compat", pdf.arg(), "--json"]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let value: Value = serde_json::from_str(&stdout(&output)).expect("valid JSON");
    assert_eq!(value["header_version"], "1.7");
    assert_eq!(value["effective_version"], "1.7");
    assert_eq!(value["source"], "header");
    assert_eq!(value["supported"], true);
    let features = value["features"].as_array().expect("features array");
    assert_eq!(features.len(), 12);
    assert!(features.iter().any(|entry| {
        entry["feature"] == "transparency"
            && entry["min_version"] == "1.4"
            && entry["supported"] == true
    }));
    assert!(
        value["detected"]
            .as_array()
            .expect("detected array")
            .is_empty()
    );
}

#[test]
fn compat_handles_a_real_world_document() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/realworld/powerpoint-slides.pdf");
    assert!(
        path.exists(),
        "real-world fixture missing: {}",
        path.display()
    );
    let output = run(&["compat", path.to_str().expect("utf-8 fixture path")]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let text = stdout(&output);
    assert!(text.contains("effective_version: "), "{text}");
    assert!(text.contains("gates: effective="), "{text}");
    assert!(text.contains("features:"), "{text}");
}

#[test]
fn printer_status_reports_an_absolute_port_for_a_missing_queue() {
    let output = run(&[
        "printer",
        "status",
        "--name",
        "PDFXI Nonexistent Queue",
        "--json",
    ]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let value: Value = serde_json::from_str(&stdout(&output)).expect("valid JSON");
    assert_eq!(value["installed"], false);
    assert_eq!(value["port_file_exists"], false);
    assert_eq!(value["capture_ready"], false);
    let port = value["port"].as_str().expect("port string");
    assert!(Path::new(port).is_absolute(), "{port}");
    assert!(port.ends_with("capture.pdf"), "{port}");
}

#[test]
fn printer_install_rejects_a_relative_port() {
    let output = run(&["printer", "install", "--port", "capture.pdf"]);
    assert!(!output.status.success());
    assert!(stderr(&output).contains("absolute"), "{}", stderr(&output));
}

#[test]
fn printer_watch_times_out_without_a_job() {
    let directory = std::env::temp_dir().join(format!(
        "pdfxi-watch-test-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or_default()
    ));
    fs::create_dir_all(&directory).expect("create watch directory");
    let out = directory.join("captured.pdf");
    let output = run(&[
        "printer",
        "watch",
        "--port",
        directory.join("capture.pdf").to_str().expect("utf-8 port"),
        "--out",
        out.to_str().expect("utf-8 out"),
        "--timeout",
        "1",
    ]);
    let _ = fs::remove_dir_all(&directory);
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr(&output).contains("no print job arrived"),
        "{}",
        stderr(&output)
    );
}

/// Absolute path of a checked-in real-world fixture.
fn real_fixture(name: &str) -> PathBuf {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/realworld")
        .join(name);
    assert!(path.exists(), "fixture missing: {}", path.display());
    path
}

/// Path to a hand-authored fixture under `fixtures/authoring`.
fn authoring_fixture(name: &str) -> PathBuf {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/authoring")
        .join(name);
    assert!(path.exists(), "fixture missing: {}", path.display());
    path
}

/// Path to a fixture directly under `fixtures/`.
fn root_fixture(name: &str) -> PathBuf {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures")
        .join(name);
    assert!(path.exists(), "fixture missing: {}", path.display());
    path
}

/// Every non-empty line the `text` command prints, one line per page here.
fn all_page_texts(path: &Path) -> Vec<String> {
    let output = run(&["text", path.to_str().expect("utf-8 path")]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    stdout(&output)
        .lines()
        .map(str::to_owned)
        .collect::<Vec<_>>()
}

/// Extract the page texts from a PDF through the CLI and return them in order.
fn page_texts(path: &Path) -> Vec<String> {
    let output = run(&["text", path.to_str().expect("utf-8 path")]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let text = stdout(&output);
    text.lines()
        .filter(|line| line.contains("slide marker"))
        .map(str::to_owned)
        .collect()
}

#[test]
fn pages_extract_keeps_real_world_pages_in_order() {
    let source = real_fixture("powerpoint-slides.pdf");
    let out = TempOut::new("pdf");
    let output = run(&[
        "pages",
        "extract",
        source.to_str().expect("utf-8 source"),
        "--pages",
        "3,1",
        "--out",
        out.arg(),
    ]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    assert!(stdout(&output).contains("pages: 2"), "{}", stdout(&output));
    let texts = page_texts(&out.path);
    assert_eq!(texts.len(), 2, "{texts:?}");
    assert!(texts[0].contains("Meadow"), "{texts:?}");
    assert!(texts[1].contains("Lantern"), "{texts:?}");
}

#[test]
fn pages_delete_removes_a_real_world_page() {
    let source = real_fixture("powerpoint-slides.pdf");
    let out = TempOut::new("pdf");
    let output = run(&[
        "pages",
        "delete",
        source.to_str().expect("utf-8 source"),
        "--pages",
        "2",
        "--out",
        out.arg(),
    ]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    assert!(stdout(&output).contains("pages: 2"), "{}", stdout(&output));
    let texts = page_texts(&out.path);
    assert_eq!(texts.len(), 2, "{texts:?}");
    assert!(texts[0].contains("Lantern"), "{texts:?}");
    assert!(texts[1].contains("Meadow"), "{texts:?}");
    assert!(
        !texts.iter().any(|line| line.contains("Harbor")),
        "{texts:?}"
    );
}

#[test]
fn pages_move_reorders_real_world_pages() {
    let source = real_fixture("powerpoint-slides.pdf");
    let out = TempOut::new("pdf");
    let output = run(&[
        "pages",
        "move",
        source.to_str().expect("utf-8 source"),
        "--from",
        "1",
        "--to",
        "3",
        "--out",
        out.arg(),
    ]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let texts = page_texts(&out.path);
    assert_eq!(texts.len(), 3, "{texts:?}");
    assert!(texts[0].contains("Harbor"), "{texts:?}");
    assert!(texts[1].contains("Meadow"), "{texts:?}");
    assert!(texts[2].contains("Lantern"), "{texts:?}");
}

#[test]
fn pages_rotate_writes_rotation_on_the_selected_page() {
    let source = real_fixture("powerpoint-slides.pdf");
    let out = TempOut::new("pdf");
    let output = run(&[
        "pages",
        "rotate",
        source.to_str().expect("utf-8 source"),
        "--pages",
        "1",
        "--angle",
        "90",
        "--out",
        out.arg(),
    ]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let bytes = fs::read(&out.path).expect("read rotation output");
    let text = String::from_utf8_lossy(&bytes);
    assert!(text.contains("/Rotate 90"), "no /Rotate 90 in output");
    let info = run(&["info", out.arg()]);
    assert!(stdout(&info).contains("pages: 3"), "{}", stdout(&info));
}

#[test]
fn pages_insert_places_every_page_of_the_other_document() {
    let one = TempPdf::new(&pdfxi_testkit::minimal_pdf());
    let three = TempPdf::new(&pdfxi_testkit::multipage_pdf(3));
    let out = TempOut::new("pdf");
    let output = run(&[
        "pages",
        "insert",
        three.arg(),
        "--after",
        "1",
        one.arg(),
        "--out",
        out.arg(),
    ]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    assert!(stdout(&output).contains("pages: 4"), "{}", stdout(&output));
}

#[test]
fn pages_replace_swaps_one_page() {
    let source = real_fixture("powerpoint-slides.pdf");
    let out = TempOut::new("pdf");
    let output = run(&[
        "pages",
        "replace",
        source.to_str().expect("utf-8 source"),
        "--page",
        "2",
        source.to_str().expect("utf-8 source"),
        "--source-page",
        "3",
        "--out",
        out.arg(),
    ]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let texts = page_texts(&out.path);
    assert_eq!(texts.len(), 3, "{texts:?}");
    assert!(texts[1].contains("Meadow"), "{texts:?}");
    assert!(texts[2].contains("Meadow"), "{texts:?}");
}

#[test]
fn pages_merge_concatenates_documents() {
    let first = TempPdf::new(&pdfxi_testkit::minimal_pdf());
    let second = TempPdf::new(&pdfxi_testkit::multipage_pdf(2));
    let out = TempOut::new("pdf");
    let output = run(&[
        "pages",
        "merge",
        first.arg(),
        second.arg(),
        "--out",
        out.arg(),
    ]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    assert!(stdout(&output).contains("pages: 3"), "{}", stdout(&output));
}

#[test]
fn pages_split_writes_numbered_parts() {
    let source = real_fixture("powerpoint-slides.pdf");
    let directory = std::env::temp_dir().join(format!(
        "pdfxi-split-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or_default()
    ));
    let output = run(&[
        "pages",
        "split",
        source.to_str().expect("utf-8 source"),
        "--every",
        "1",
        "--out-dir",
        directory.to_str().expect("utf-8 directory"),
    ]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    assert!(stdout(&output).contains("parts: 3"), "{}", stdout(&output));
    for index in 1..=3 {
        let part = directory.join(format!("powerpoint-slides-{index:02}.pdf"));
        assert!(part.exists(), "missing {}", part.display());
        let info = run(&["info", part.to_str().expect("utf-8 part")]);
        assert!(stdout(&info).contains("pages: 1"), "{}", stdout(&info));
    }
    let _ = fs::remove_dir_all(&directory);
}

#[test]
fn pages_crop_sets_the_crop_box_on_a_real_document() {
    let source = real_fixture("powerpoint-slides.pdf");
    let out = TempOut::new("pdf");
    let output = run(&[
        "pages",
        "crop",
        source.to_str().expect("utf-8 source"),
        "--bounds",
        "10,20,700,560",
        "--out",
        out.arg(),
    ]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    assert!(stdout(&output).contains("pages: 3"), "{}", stdout(&output));
    let bytes = fs::read(&out.path).expect("read crop output");
    let text = String::from_utf8_lossy(&bytes);
    assert!(
        text.contains("/CropBox [ 10 20 700 560 ]"),
        "missing crop box"
    );
    let texts = page_texts(&out.path);
    assert_eq!(
        texts.len(),
        3,
        "cropping must keep the page text: {texts:?}"
    );
}

#[test]
fn pages_resize_scales_the_page_and_keeps_its_content() {
    let source = real_fixture("powerpoint-slides.pdf");
    let out = TempOut::new("pdf");
    let output = run(&[
        "pages",
        "resize",
        source.to_str().expect("utf-8 source"),
        "--size",
        "400x300",
        "--out",
        out.arg(),
    ]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let bytes = fs::read(&out.path).expect("read resize output");
    let text = String::from_utf8_lossy(&bytes);
    assert!(
        text.contains("/MediaBox [ 0 0 400 300 ]"),
        "missing media box"
    );
    assert!(
        text.contains("/CropBox [ 0 0 400 300 ]"),
        "missing crop box"
    );
    assert!(text.contains(" cm"), "content transform must be written");
    let texts = page_texts(&out.path);
    assert_eq!(
        texts.len(),
        3,
        "resizing must keep the page text: {texts:?}"
    );
}

#[test]
fn pages_crop_rejects_a_degenerate_box() {
    let source = real_fixture("powerpoint-slides.pdf");
    let out = TempOut::new("pdf");
    let output = run(&[
        "pages",
        "crop",
        source.to_str().expect("utf-8 source"),
        "--bounds",
        "10,20,10,40",
        "--out",
        out.arg(),
    ]);
    assert!(!output.status.success());
    assert!(!out.path.exists(), "no output for a rejected box");
}

#[test]
fn links_report_a_real_annotation_fixture() {
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/authoring/authoring-annotations.pdf");
    assert!(source.exists(), "fixture missing: {}", source.display());
    let output = run(&["links", source.to_str().expect("utf-8 source")]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let text = stdout(&output);
    assert!(text.contains("links: 1"), "{text}");
    assert!(
        text.contains("target: uri https://example.invalid/doc"),
        "{text}"
    );
}

#[test]
fn links_report_explicit_uri_and_named_destinations() {
    let pdf = TempPdf::new(&pdfxi_testkit::annotated_pdf());
    let output = run(&["links", pdf.arg()]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let text = stdout(&output);
    assert!(text.contains("links: 3"), "{text}");
    assert!(text.contains("target: page 1 (named here)"), "{text}");
    assert!(text.contains("target: uri https://example.com"), "{text}");
    assert!(text.contains("target: page 1"), "{text}");
}

#[test]
fn attachments_report_an_embedded_file() {
    let pdf = TempPdf::new(&pdfxi_testkit::annotated_pdf());
    let output = run(&["attachments", pdf.arg()]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let text = stdout(&output);
    assert!(text.contains("attachments: 1"), "{text}");
    assert!(text.contains("name: notes.txt"), "{text}");
    assert!(text.contains("size: 5"), "{text}");
    assert!(text.contains("description: Sample notes"), "{text}");
}

#[test]
fn annotate_adds_pencil_and_stamp() {
    let source = authoring_fixture("authoring-page-boxes.pdf");
    let first = TempOut::new("pdf");
    let second = TempOut::new("pdf");
    let pencil = run(&[
        "annotate",
        "add",
        source.to_str().expect("utf-8 source"),
        "--out",
        first.arg(),
        "--kind",
        "pencil",
        "--ink",
        "72,600 120,620 160,600; 80,560 140,560",
        "--stroke-width",
        "3",
        "--text",
        "Pencil note",
        "--author",
        "Dana",
    ]);
    assert!(pencil.status.success(), "stderr: {}", stderr(&pencil));
    assert!(
        stdout(&pencil).contains("added: pdfxi-ink-"),
        "{}",
        stdout(&pencil)
    );

    let stamp = run(&[
        "annotate",
        "add",
        first.arg(),
        "--out",
        second.arg(),
        "--kind",
        "stamp",
        "--rect",
        "400,600,560,650",
        "--stamp-name",
        "Approved",
        "--text",
        "Approved 2026",
        "--author",
        "Eve",
    ]);
    assert!(stamp.status.success(), "stderr: {}", stderr(&stamp));

    let listed = run(&["annotate", "list", second.arg(), "--json"]);
    assert!(listed.status.success(), "stderr: {}", stderr(&listed));
    let value: Value = serde_json::from_str(&stdout(&listed)).expect("valid JSON");
    let rows = value["annotations"].as_array().expect("annotations array");
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["subtype"], "Ink");
    assert_eq!(rows[0]["author"], "Dana");
    assert_eq!(rows[1]["subtype"], "Stamp");
    assert_eq!(rows[1]["contents"], "Approved 2026");
}

#[test]
fn annotate_file_attachment_is_extractable() {
    let source = authoring_fixture("authoring-page-boxes.pdf");
    let payload = TempPath::new("bin");
    let bytes = b"attachment payload 123";
    payload.write(bytes);
    let out = TempOut::new("pdf");
    let added = run(&[
        "annotate",
        "add",
        source.to_str().expect("utf-8 source"),
        "--out",
        out.arg(),
        "--kind",
        "file-attachment",
        "--rect",
        "72,700,96,724",
        "--attachment",
        payload.arg(),
        "--name",
        "evidence.bin",
        "--author",
        "Alice",
    ]);
    assert!(added.status.success(), "stderr: {}", stderr(&added));

    let listed = run(&["attachments", out.arg()]);
    assert!(listed.status.success(), "stderr: {}", stderr(&listed));
    let text = stdout(&listed);
    assert!(text.contains("name: evidence.bin"), "{text}");
    assert!(text.contains("size: 22"), "{text}");

    let directory = std::env::temp_dir().join(format!(
        "pdfxi-attach-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    let _ = fs::remove_dir_all(&directory);
    let extraction = run(&[
        "attachments",
        out.arg(),
        "--extract",
        directory.to_str().expect("utf-8 dir"),
    ]);
    assert!(
        extraction.status.success(),
        "stderr: {}",
        stderr(&extraction)
    );
    let extracted = fs::read(directory.join("evidence.bin")).expect("extracted attachment");
    assert_eq!(extracted, bytes);
    let _ = fs::remove_dir_all(&directory);
}

#[test]
fn annotate_add_and_list_round_trip() {
    let source = authoring_fixture("authoring-page-boxes.pdf");
    let out = TempOut::new("pdf");
    let added = run(&[
        "annotate",
        "add",
        source.to_str().expect("utf-8 source"),
        "--out",
        out.arg(),
        "--kind",
        "free-text",
        "--rect",
        "72,600,320,660",
        "--text",
        "Free text note",
        "--author",
        "Alice",
    ]);
    assert!(added.status.success(), "stderr: {}", stderr(&added));
    assert!(
        stdout(&added).contains("added: pdfxi-freetext-"),
        "{}",
        stdout(&added)
    );

    let listed = run(&["annotate", "list", out.arg(), "--json"]);
    assert!(listed.status.success(), "stderr: {}", stderr(&listed));
    let value: Value = serde_json::from_str(&stdout(&listed)).expect("valid JSON");
    assert_eq!(value["page"], 1);
    let rows = value["annotations"].as_array().expect("annotations array");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["subtype"], "FreeText");
    assert_eq!(rows[0]["contents"], "Free text note");
    assert_eq!(rows[0]["author"], "Alice");
    assert_eq!(rows[0]["rect"][0], 72.0);
}

#[test]
fn annotate_adds_callout_and_cloud_and_keeps_earlier_ones() {
    let source = authoring_fixture("authoring-page-boxes.pdf");
    let first = TempOut::new("pdf");
    let second = TempOut::new("pdf");
    let third = TempOut::new("pdf");
    let add = |file: &std::path::Path, out: &TempOut, extra: &[&str]| {
        let mut args = vec![
            "annotate",
            "add",
            file.to_str().expect("utf-8"),
            "--out",
            out.arg(),
        ];
        args.extend_from_slice(extra);
        run(&args)
    };
    let one = add(
        &source,
        &first,
        &[
            "--kind",
            "free-text",
            "--rect",
            "72,600,320,660",
            "--text",
            "Text",
            "--author",
            "A",
        ],
    );
    assert!(one.status.success(), "stderr: {}", stderr(&one));
    let two = add(
        &first.path,
        &second,
        &[
            "--kind",
            "callout",
            "--rect",
            "72,500,320,560",
            "--anchor",
            "60,470",
            "--text",
            "Callout",
            "--author",
            "B",
        ],
    );
    assert!(two.status.success(), "stderr: {}", stderr(&two));
    let three = add(
        &second.path,
        &third,
        &[
            "--kind",
            "cloud",
            "--rect",
            "72,380,320,460",
            "--author",
            "C",
        ],
    );
    assert!(three.status.success(), "stderr: {}", stderr(&three));

    let listed = run(&["annotate", "list", third.arg(), "--json"]);
    assert!(listed.status.success(), "stderr: {}", stderr(&listed));
    let value: Value = serde_json::from_str(&stdout(&listed)).expect("valid JSON");
    let rows = value["annotations"].as_array().expect("annotations array");
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0]["subtype"], "FreeText");
    assert_eq!(rows[1]["subtype"], "FreeText");
    assert_eq!(rows[2]["subtype"], "Polygon");
    assert_eq!(rows[2]["author"], "C");
    // The cloud keeps a margin for its bumps.
    assert_eq!(rows[2]["rect"][0], 64.0);
}

#[test]
fn annotate_rejects_bad_requests() {
    let source = authoring_fixture("authoring-page-boxes.pdf");
    let out = TempOut::new("pdf");
    let missing_text = run(&[
        "annotate",
        "add",
        source.to_str().expect("utf-8 source"),
        "--out",
        out.arg(),
        "--kind",
        "free-text",
        "--rect",
        "72,600,320,660",
    ]);
    assert!(!missing_text.status.success());
    assert!(
        stderr(&missing_text).contains("--text"),
        "{}",
        stderr(&missing_text)
    );

    let missing_anchor = run(&[
        "annotate",
        "add",
        source.to_str().expect("utf-8 source"),
        "--out",
        out.arg(),
        "--kind",
        "callout",
        "--rect",
        "72,500,320,560",
        "--text",
        "x",
    ]);
    assert!(!missing_anchor.status.success());
    assert!(
        stderr(&missing_anchor).contains("--anchor"),
        "{}",
        stderr(&missing_anchor)
    );

    let bad_rect = run(&[
        "annotate",
        "add",
        source.to_str().expect("utf-8 source"),
        "--out",
        out.arg(),
        "--kind",
        "cloud",
        "--rect",
        "10,10,10,20",
    ]);
    assert!(!bad_rect.status.success());

    let blank = run(&[
        "annotate",
        "add",
        source.to_str().expect("utf-8 source"),
        "--out",
        out.arg(),
        "--kind",
        "free-text",
        "--rect",
        "72,600,320,660",
        "--text",
        "   ",
    ]);
    assert!(!blank.status.success());
}

#[test]
fn linearize_writes_a_valid_and_deterministic_file() {
    let source = root_fixture("minimal.pdf");
    let first = TempOut::new("pdf");
    let second = TempOut::new("pdf");
    let linearized = run(&[
        "linearize",
        source.to_str().expect("utf-8"),
        "--out",
        first.arg(),
    ]);
    assert!(
        linearized.status.success(),
        "stderr: {}",
        stderr(&linearized)
    );
    let text = stdout(&linearized);
    assert!(text.contains("declared_length:"), "{text}");
    assert!(text.contains("pages: 1 (text verified: 1)"), "{text}");
    assert!(text.contains("first_page_object:"), "{text}");
    assert!(text.contains("hint: offset"), "{text}");

    let repeated = run(&[
        "linearize",
        source.to_str().expect("utf-8"),
        "--out",
        second.arg(),
    ]);
    assert!(repeated.status.success(), "stderr: {}", stderr(&repeated));
    let left = fs::read(&first.path).expect("first linearized");
    let right = fs::read(&second.path).expect("second linearized");
    assert_eq!(left, right, "linearized output is deterministic");

    let source_text = run(&["text", source.to_str().expect("utf-8")]);
    let saved_text = run(&["text", first.arg()]);
    assert!(
        saved_text.status.success(),
        "stderr: {}",
        stderr(&saved_text)
    );
    assert_eq!(stdout(&source_text), stdout(&saved_text));

    let info = run(&["info", first.arg()]);
    assert!(info.status.success(), "stderr: {}", stderr(&info));
    assert!(stdout(&info).contains("pages: 1"), "{}", stdout(&info));
}

#[test]
fn linearize_and_pack_handle_a_hybrid_source() {
    let source = root_fixture("realworld/microsoft-excel-2019-sample.pdf");
    let linearized = TempOut::new("pdf");
    let packed = TempOut::new("pdf");
    let source_text = run(&["text", source.to_str().expect("utf-8")]);
    assert!(source_text.status.success());

    let linear = run(&[
        "linearize",
        source.to_str().expect("utf-8"),
        "--out",
        linearized.arg(),
        "--json",
    ]);
    assert!(linear.status.success(), "stderr: {}", stderr(&linear));
    let value: Value = serde_json::from_str(&stdout(&linear)).expect("valid JSON");
    assert_eq!(value["declared_length"], value["bytes"]);
    assert_eq!(value["page_count"], 2);
    assert_eq!(value["text_pages_verified"], 2);
    let linear_text = run(&["text", linearized.arg()]);
    assert!(
        linear_text.status.success(),
        "stderr: {}",
        stderr(&linear_text)
    );
    assert_eq!(stdout(&source_text), stdout(&linear_text));

    let pack = run(&[
        "pack",
        source.to_str().expect("utf-8"),
        "--out",
        packed.arg(),
        "--json",
    ]);
    assert!(pack.status.success(), "stderr: {}", stderr(&pack));
    let value: Value = serde_json::from_str(&stdout(&pack)).expect("valid JSON");
    assert_eq!(value["object_streams"], true);
    assert_eq!(value["xref_stream"], true);
    assert_eq!(value["text_pages_verified"], 2);
    let packed_text = run(&["text", packed.arg()]);
    assert!(
        packed_text.status.success(),
        "stderr: {}",
        stderr(&packed_text)
    );
    assert_eq!(stdout(&source_text), stdout(&packed_text));

    let invalid = run(&[
        "pack",
        source.to_str().expect("utf-8"),
        "--out",
        packed.arg(),
        "--max-objects",
        "0",
    ]);
    assert!(!invalid.status.success());
    assert!(
        stderr(&invalid).contains("--max-objects"),
        "{}",
        stderr(&invalid)
    );
}

#[test]
fn update_appends_a_validated_incremental_revision() {
    let source = root_fixture("minimal.pdf");
    let out = TempOut::new("pdf");
    let updated = run(&[
        "update",
        source.to_str().expect("utf-8"),
        "--out",
        out.arg(),
        "--title",
        "Incremental title",
        "--author",
        "PDFXI",
    ]);
    assert!(updated.status.success(), "stderr: {}", stderr(&updated));
    let text = stdout(&updated);
    assert!(text.contains("xref_chain_verified: true"), "{text}");
    assert!(text.contains("revision: Classic"), "{text}");
    assert!(text.contains("text_pages_verified: 1"), "{text}");
    assert!(text.contains("title: Incremental title"), "{text}");

    let original = fs::read(&source).expect("source bytes");
    let saved = fs::read(&out.path).expect("saved bytes");
    assert!(saved.starts_with(&original), "original bytes are a prefix");
    assert!(saved.len() > original.len(), "a revision was appended");

    let missing = run(&[
        "update",
        source.to_str().expect("utf-8"),
        "--out",
        out.arg(),
    ]);
    assert!(!missing.status.success());
    assert!(stderr(&missing).contains("--title"), "{}", stderr(&missing));
}

#[test]
fn update_handles_a_hybrid_xref_source_honestly() {
    let source = root_fixture("realworld/microsoft-excel-2019-sample.pdf");
    let first = TempOut::new("pdf");
    let second = TempOut::new("pdf");
    let updated = run(&[
        "update",
        source.to_str().expect("utf-8"),
        "--out",
        first.arg(),
        "--title",
        "Hybrid incremental",
        "--json",
    ]);
    assert!(updated.status.success(), "stderr: {}", stderr(&updated));
    let value: Value = serde_json::from_str(&stdout(&updated)).expect("valid JSON");
    let original = fs::read(&source).expect("source bytes");
    assert_eq!(
        value["preserved_prefix_bytes"].as_u64(),
        Some(original.len() as u64)
    );
    assert!(value["appended_bytes"].as_u64().unwrap_or(0) > 0);
    assert_eq!(value["xref_chain_verified"].as_bool(), Some(false));
    assert!(
        value["note"]
            .as_str()
            .unwrap_or_default()
            .contains("XRefStm"),
        "{}",
        stdout(&updated)
    );
    assert_eq!(value["page_count"].as_u64(), Some(2));
    assert_eq!(value["text_pages_verified"].as_u64(), Some(2));
    assert_eq!(value["title"], "Hybrid incremental");

    let saved = fs::read(&first.path).expect("saved bytes");
    assert!(saved.starts_with(&original), "prefix preserved");
    let first_len = saved.len();

    let chained = run(&[
        "update",
        first.arg(),
        "--out",
        second.arg(),
        "--subject",
        "Second revision",
    ]);
    assert!(chained.status.success(), "stderr: {}", stderr(&chained));
    assert!(
        stdout(&chained).contains(&format!("preserved_prefix: {first_len}")),
        "{}",
        stdout(&chained)
    );
}

#[test]
fn outline_add_rename_move_remove_round_trip() {
    let source = root_fixture("realworld/powerpoint-slides.pdf");
    let first = TempOut::new("pdf");
    let second = TempOut::new("pdf");
    let third = TempOut::new("pdf");
    let fourth = TempOut::new("pdf");
    let fifth = TempOut::new("pdf");
    let sixth = TempOut::new("pdf");

    let listed = run(&["outline", "list", source.to_str().expect("utf-8"), "--json"]);
    assert!(listed.status.success(), "stderr: {}", stderr(&listed));
    let value: Value = serde_json::from_str(&stdout(&listed)).expect("valid JSON");
    assert_eq!(value["present"], true);
    assert_eq!(value["count"], 3);
    assert_eq!(value["bookmarks"][0]["title"], "Slide 1: Lantern Slide");
    assert_eq!(value["bookmarks"][2]["page"], 3);

    let added = run(&[
        "outline",
        "add",
        source.to_str().expect("utf-8"),
        "--out",
        first.arg(),
        "--title",
        "Appendix",
        "--page",
        "2",
    ]);
    assert!(added.status.success(), "stderr: {}", stderr(&added));
    assert!(
        stdout(&added).contains("3  Appendix  page 2"),
        "{}",
        stdout(&added)
    );

    let child = run(&[
        "outline",
        "add",
        first.arg(),
        "--out",
        second.arg(),
        "--title",
        "Details",
        "--page",
        "1",
        "--parent",
        "3",
    ]);
    assert!(child.status.success(), "stderr: {}", stderr(&child));
    assert!(
        stdout(&child).contains("3.0  Details  page 1"),
        "{}",
        stdout(&child)
    );

    let renamed = run(&[
        "outline",
        "rename",
        second.arg(),
        "--out",
        third.arg(),
        "--path",
        "3.0",
        "--title",
        "Appendix details",
    ]);
    assert!(renamed.status.success(), "stderr: {}", stderr(&renamed));

    let moved = run(&[
        "outline",
        "move",
        third.arg(),
        "--out",
        fourth.arg(),
        "--path",
        "3.0",
        "--index",
        "0",
    ]);
    assert!(moved.status.success(), "stderr: {}", stderr(&moved));
    let moved_text = stdout(&moved);
    assert!(
        moved_text.contains("0  Appendix details  page 1"),
        "{moved_text}"
    );

    let removed = run(&[
        "outline",
        "remove",
        fourth.arg(),
        "--out",
        fifth.arg(),
        "--path",
        "4",
    ]);
    assert!(removed.status.success(), "stderr: {}", stderr(&removed));

    let final_list = run(&["outline", "list", fifth.arg(), "--json"]);
    assert!(
        final_list.status.success(),
        "stderr: {}",
        stderr(&final_list)
    );
    let value: Value = serde_json::from_str(&stdout(&final_list)).expect("valid JSON");
    assert_eq!(value["count"], 4);
    assert_eq!(value["bookmarks"][0]["title"], "Appendix details");
    assert_eq!(value["bookmarks"][0]["path"], "0");
    let titles: Vec<&str> = value["bookmarks"]
        .as_array()
        .expect("rows")
        .iter()
        .map(|row| row["title"].as_str().expect("title"))
        .collect();
    assert_eq!(
        titles,
        vec![
            "Appendix details",
            "Slide 1: Lantern Slide",
            "Slide 2: Harbor Slide",
            "Slide 3: Meadow Slide"
        ]
    );

    let bad_path = run(&[
        "outline",
        "remove",
        fifth.arg(),
        "--out",
        sixth.arg(),
        "--path",
        "9",
    ]);
    assert!(!bad_path.status.success());
    assert!(
        stderr(&bad_path).contains("bookmark"),
        "{}",
        stderr(&bad_path)
    );
}

#[test]
fn link_add_and_remove_round_trip() {
    let source = root_fixture("realworld/powerpoint-slides.pdf");
    let first = TempOut::new("pdf");
    let second = TempOut::new("pdf");
    let third = TempOut::new("pdf");

    let uri = run(&[
        "link",
        "add",
        source.to_str().expect("utf-8"),
        "--out",
        first.arg(),
        "--page",
        "1",
        "--rect",
        "72,600,240,630",
        "--uri",
        "https://example.com/a?b=1",
        "--highlight",
        "outline",
        "--json",
    ]);
    assert!(uri.status.success(), "stderr: {}", stderr(&uri));
    let value: Value = serde_json::from_str(&stdout(&uri)).expect("valid JSON");
    assert_eq!(value["page"], 1);
    assert_eq!(value["links"][0]["kind"], "uri");
    assert_eq!(value["links"][0]["uri"], "https://example.com/a?b=1");
    assert_eq!(value["links"][0]["rect"][2], 240.0);

    let internal = run(&[
        "link",
        "add",
        first.arg(),
        "--out",
        second.arg(),
        "--page",
        "2",
        "--rect",
        "50,50,150,80",
        "--to-page",
        "3",
    ]);
    assert!(internal.status.success(), "stderr: {}", stderr(&internal));
    assert!(
        stdout(&internal).contains("page 3  rect: 50 50 150 80"),
        "{}",
        stdout(&internal)
    );

    let listed = run(&["links", second.arg(), "--json"]);
    assert!(listed.status.success(), "stderr: {}", stderr(&listed));
    let value: Value = serde_json::from_str(&stdout(&listed)).expect("valid JSON");
    assert_eq!(value["count"], 2);
    assert_eq!(value["links"][0]["kind"], "uri");
    assert_eq!(value["links"][1]["kind"], "page");
    assert_eq!(value["links"][1]["dest_page"], 3);

    let removed = run(&[
        "link",
        "remove",
        second.arg(),
        "--out",
        third.arg(),
        "--page",
        "1",
        "--index",
        "0",
    ]);
    assert!(removed.status.success(), "stderr: {}", stderr(&removed));
    assert!(
        stdout(&removed).contains("links: 0"),
        "{}",
        stdout(&removed)
    );

    let out_of_range = run(&[
        "link",
        "remove",
        third.arg(),
        "--out",
        third.arg(),
        "--page",
        "1",
        "--index",
        "0",
    ]);
    assert!(!out_of_range.status.success());

    let conflicting = run(&[
        "link",
        "add",
        source.to_str().expect("utf-8"),
        "--out",
        first.arg(),
        "--page",
        "1",
        "--rect",
        "0,0,10,10",
        "--uri",
        "https://example.com",
        "--to-page",
        "2",
    ]);
    assert!(!conflicting.status.success());
}

#[test]
fn attach_add_remove_round_trip() {
    let source = authoring_fixture("authoring-page-boxes.pdf");
    let payload = TempPath::new("bin");
    let bytes = b"navigation attachment payload";
    payload.write(bytes);
    let first = TempOut::new("pdf");
    let second = TempOut::new("pdf");

    let added = run(&[
        "attach",
        "add",
        source.to_str().expect("utf-8"),
        "--out",
        first.arg(),
        "--attachment",
        payload.arg(),
        "--description",
        "navigation evidence",
        "--mime",
        "application/octet-stream",
    ]);
    assert!(added.status.success(), "stderr: {}", stderr(&added));
    assert!(stdout(&added).contains("size: 29"), "{}", stdout(&added));

    let listed = run(&["attachments", first.arg(), "--json"]);
    assert!(listed.status.success(), "stderr: {}", stderr(&listed));
    let value: Value = serde_json::from_str(&stdout(&listed)).expect("valid JSON");
    assert_eq!(value["count"], 1);
    assert_eq!(
        value["attachments"][0]["name"],
        payload
            .path
            .file_name()
            .expect("file name")
            .to_string_lossy()
            .as_ref()
    );
    assert_eq!(
        value["attachments"][0]["description"],
        "navigation evidence"
    );

    let directory = std::env::temp_dir().join(format!(
        "pdfxi-nav-attach-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    let _ = fs::remove_dir_all(&directory);
    let extraction = run(&[
        "attachments",
        first.arg(),
        "--extract",
        directory.to_str().expect("utf-8 dir"),
    ]);
    assert!(
        extraction.status.success(),
        "stderr: {}",
        stderr(&extraction)
    );
    let extracted_name = payload.path.file_name().expect("file name").to_owned();
    let extracted = fs::read(directory.join(extracted_name)).expect("extracted attachment");
    assert_eq!(extracted, bytes);
    let _ = fs::remove_dir_all(&directory);

    let duplicate = run(&[
        "attach",
        "add",
        first.arg(),
        "--out",
        second.arg(),
        "--attachment",
        payload.arg(),
    ]);
    assert!(!duplicate.status.success());
    assert!(
        stderr(&duplicate).contains("already exists"),
        "{}",
        stderr(&duplicate)
    );

    let removed = run(&[
        "attach",
        "remove",
        first.arg(),
        "--out",
        second.arg(),
        "--name",
        payload
            .path
            .file_name()
            .expect("file name")
            .to_string_lossy()
            .as_ref(),
    ]);
    assert!(removed.status.success(), "stderr: {}", stderr(&removed));
    assert!(
        stdout(&removed).contains("attachments: 0"),
        "{}",
        stdout(&removed)
    );

    let missing = run(&[
        "attach",
        "remove",
        second.arg(),
        "--out",
        second.arg(),
        "--name",
        "missing.bin",
    ]);
    assert!(!missing.status.success());
}

#[test]
fn decorate_bates_numbers_every_page() {
    let source = real_fixture("powerpoint-slides.pdf");
    let out = TempOut::new("pdf");
    let output = run(&[
        "decorate",
        "bates",
        source.to_str().expect("utf-8 source"),
        "--out",
        out.arg(),
        "--prefix",
        "EX-",
        "--digits",
        "6",
    ]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let text = stdout(&output);
    assert!(text.contains("stamped: 3"), "{text}");
    assert!(text.contains("numbers: EX-000001 .. EX-000003"), "{text}");

    let pages = page_texts(&out.path);
    assert_eq!(pages.len(), 3);
    for (index, page) in pages.iter().enumerate() {
        assert!(
            page.contains(&format!("EX-{:06}", index + 1)),
            "page {}: {page}",
            index + 1
        );
    }
    assert!(pages[0].contains("Lantern"), "original text is kept");
}

#[test]
fn decorate_bates_honours_start_increment_and_suffix() {
    let source = real_fixture("powerpoint-slides.pdf");
    let out = TempOut::new("pdf");
    let output = run(&[
        "decorate",
        "bates",
        source.to_str().expect("utf-8 source"),
        "--out",
        out.arg(),
        "--prefix",
        "B-",
        "--suffix=-A",
        "--start",
        "10",
        "--digits",
        "4",
        "--increment",
        "5",
        "--corner",
        "top-left",
        "--json",
    ]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let value: Value = serde_json::from_str(&stdout(&output)).expect("valid JSON");
    assert_eq!(value["pages"], 3);
    assert_eq!(value["stamped"], 3);
    assert_eq!(value["first"], "B-0010-A");
    assert_eq!(value["last"], "B-0020-A");
    assert_eq!(value["increment"], 5);
    assert_eq!(value["digits"], 4);

    let pages = page_texts(&out.path);
    assert!(pages[0].contains("B-0010-A"), "{:?}", pages[0]);
    assert!(pages[1].contains("B-0015-A"), "{:?}", pages[1]);
    assert!(pages[2].contains("B-0020-A"), "{:?}", pages[2]);
}

#[test]
fn decorate_bates_places_number_on_a_rotated_page() {
    let source = authoring_fixture("authoring-rotation.pdf");
    let out = TempOut::new("pdf");
    let output = run(&[
        "decorate",
        "bates",
        source.to_str().expect("utf-8 source"),
        "--out",
        out.arg(),
        "--prefix",
        "R-",
    ]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let pages = all_page_texts(&out.path);
    assert_eq!(pages.len(), 3);
    assert!(pages[0].contains("Rotated page one"), "{:?}", pages[0]);
    assert!(pages[0].contains("R-000001"), "{:?}", pages[0]);
    assert!(pages[2].contains("R-000003"), "{:?}", pages[2]);
}

#[test]
fn decorate_bates_rejects_invalid_configuration() {
    let source = real_fixture("powerpoint-slides.pdf");
    let out = TempOut::new("pdf");
    let bad_digits = run(&[
        "decorate",
        "bates",
        source.to_str().expect("utf-8 source"),
        "--out",
        out.arg(),
        "--digits",
        "0",
    ]);
    assert!(!bad_digits.status.success(), "zero digits must fail");
    assert!(
        !stderr(&bad_digits).is_empty() || !stdout(&bad_digits).is_empty(),
        "the failure must explain itself"
    );

    let bad_start = run(&[
        "decorate",
        "bates",
        source.to_str().expect("utf-8 source"),
        "--out",
        out.arg(),
        "--start",
        "0",
    ]);
    assert!(!bad_start.status.success(), "start 0 must fail");
}

#[test]
fn decorate_header_footer_stamps_every_page() {
    let source = real_fixture("powerpoint-slides.pdf");
    let out = TempOut::new("pdf");
    let output = run(&[
        "decorate",
        "header-footer",
        source.to_str().expect("utf-8 source"),
        "--out",
        out.arg(),
        "--header",
        "Quarterly Review",
        "--footer",
        "Page {page} of {pages}",
    ]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let text = stdout(&output);
    assert!(text.contains("stamped: 3"), "{text}");

    let pages = page_texts(&out.path);
    assert_eq!(pages.len(), 3);
    for (index, page) in pages.iter().enumerate() {
        assert!(
            page.contains("Quarterly Review"),
            "page {}: {page}",
            index + 1
        );
        assert!(
            page.contains(&format!("Page {} of 3", index + 1)),
            "page {}: {page}",
            index + 1
        );
    }
    assert!(pages[0].contains("Lantern"), "original text is kept");
}

#[test]
fn decorate_header_footer_json_reports_the_page_set() {
    let source = real_fixture("powerpoint-slides.pdf");
    let out = TempOut::new("pdf");
    let output = run(&[
        "decorate",
        "header-footer",
        source.to_str().expect("utf-8 source"),
        "--out",
        out.arg(),
        "--header",
        "{title}",
        "--title",
        "Board pack",
        "--side",
        "left",
        "--font-size",
        "12",
        "--json",
    ]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let value: Value = serde_json::from_str(&stdout(&output)).expect("valid JSON");
    assert_eq!(value["pages"], 3);
    assert_eq!(value["stamped"], 3);
    assert_eq!(value["suppressed"], 0);
    assert_eq!(value["font_size"], 12.0);
    assert_eq!(value["header"], "{title}");
}

#[test]
fn decorate_header_footer_rejects_bad_templates() {
    let source = real_fixture("powerpoint-slides.pdf");
    let out = TempOut::new("pdf");
    let no_template = run(&[
        "decorate",
        "header-footer",
        source.to_str().expect("utf-8 source"),
        "--out",
        out.arg(),
    ]);
    assert!(!no_template.status.success());
    assert!(
        stderr(&no_template).contains("--header"),
        "{}",
        stderr(&no_template)
    );

    let unknown = run(&[
        "decorate",
        "header-footer",
        source.to_str().expect("utf-8 source"),
        "--out",
        out.arg(),
        "--header",
        "{nope}",
    ]);
    assert!(!unknown.status.success());
    assert!(
        stderr(&unknown).contains("unknown placeholder"),
        "{}",
        stderr(&unknown)
    );
}

#[test]
fn decorate_watermark_adds_text_and_alpha() {
    let source = real_fixture("powerpoint-slides.pdf");
    let out = TempOut::new("pdf");
    let output = run(&[
        "decorate",
        "watermark",
        source.to_str().expect("utf-8 source"),
        "--out",
        out.arg(),
        "--text",
        "DRAFT",
        "--opacity",
        "0.2",
        "--json",
    ]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let value: Value = serde_json::from_str(&stdout(&output)).expect("valid JSON");
    assert_eq!(value["pages"], 3);
    assert_eq!(value["stamped"], 3);
    assert_eq!(value["text"], "DRAFT");
    assert_eq!(value["opacity"], 0.2);
    assert_eq!(value["rotation"], 45.0);

    let pages = page_texts(&out.path);
    assert!(pages.iter().all(|page| page.contains("DRAFT")), "{pages:?}");
    let raw = fs::read(&out.path).expect("read output");
    let text = String::from_utf8_lossy(&raw);
    assert!(text.contains("/ExtGState"), "alpha resource is merged");
    assert!(text.contains("/ca 0.2"), "alpha value is written");
}

#[test]
fn decorate_watermark_tiles_repeat_the_mark() {
    let source = real_fixture("powerpoint-slides.pdf");
    let out = TempOut::new("pdf");
    let output = run(&[
        "decorate",
        "watermark",
        source.to_str().expect("utf-8 source"),
        "--out",
        out.arg(),
        "--text",
        "COPY",
        "--tile",
    ]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let pages = page_texts(&out.path);
    assert!(
        pages[0].matches("COPY").count() > 1,
        "a tiled mark repeats: {:?}",
        pages[0]
    );
}

#[test]
fn decorate_watermark_rejects_bad_alpha_and_states() {
    let source = real_fixture("powerpoint-slides.pdf");
    let out = TempOut::new("pdf");
    let bad = run(&[
        "decorate",
        "watermark",
        source.to_str().expect("utf-8 source"),
        "--out",
        out.arg(),
        "--text",
        "X",
        "--opacity",
        "1.5",
    ]);
    assert!(!bad.status.success());
    assert!(stderr(&bad).contains("--opacity"), "{}", stderr(&bad));

    let empty = run(&[
        "decorate",
        "watermark",
        source.to_str().expect("utf-8 source"),
        "--out",
        out.arg(),
        "--text",
        "   ",
    ]);
    assert!(!empty.status.success());
    assert!(stderr(&empty).contains("--text"), "{}", stderr(&empty));
}

#[test]
fn optimize_rewrites_a_real_document_and_preserves_text() {
    let source = real_fixture("powerpoint-slides.pdf");
    let out = TempOut::new("pdf");
    let output = run(&[
        "optimize",
        source.to_str().expect("utf-8 source"),
        "--out",
        out.arg(),
    ]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let text = stdout(&output);
    assert!(text.contains("bytes: 119341 -> 118516 (-0.7%)"), "{text}");
    assert!(text.contains("objects: 68 -> 65"), "{text}");
    assert!(text.contains("streams compressed: 1"), "{text}");
    assert!(text.contains("deduplicated: 2"), "{text}");

    assert_eq!(page_texts(&source), page_texts(&out.path));
    let reopened = pdfxi_core::Document::from_bytes(fs::read(&out.path).expect("read output"))
        .expect("optimized file parses");
    assert_eq!(reopened.page_count().expect("page count"), 3);
}

#[test]
fn optimize_json_report_is_deterministic_and_consistent() {
    let source = real_fixture("powerpoint-slides.pdf");
    let out = TempOut::new("pdf");
    let first = run(&[
        "optimize",
        source.to_str().expect("utf-8 source"),
        "--out",
        out.arg(),
        "--json",
    ]);
    assert!(first.status.success(), "stderr: {}", stderr(&first));
    let first_json = stdout(&first);
    let value: Value = serde_json::from_str(&first_json).expect("valid JSON");
    let before = fs::metadata(&source).expect("source metadata").len() as u64;
    assert_eq!(value["bytes_before"], before);
    assert_eq!(
        value["bytes_after"],
        fs::metadata(&out.path).expect("out metadata").len()
    );
    assert_eq!(
        value["saved_bytes"].as_i64(),
        value["bytes_after"]
            .as_i64()
            .map(|after| after - before as i64)
    );
    assert!(value["objects_after"].as_u64() <= value["objects_before"].as_u64());
    assert!(
        !value["warnings"]
            .as_array()
            .expect("warnings array")
            .is_empty()
    );

    let second = run(&[
        "optimize",
        source.to_str().expect("utf-8 source"),
        "--out",
        out.arg(),
        "--json",
    ]);
    assert!(second.status.success(), "stderr: {}", stderr(&second));
    assert_eq!(
        first_json,
        stdout(&second),
        "the optimizer report must be byte-identical across runs"
    );
}

#[test]
fn optimize_remove_metadata_clears_info_and_xmp() {
    let source = real_fixture("powerpoint-slides.pdf");
    let source_document = pdfxi_core::Document::from_bytes(fs::read(&source).expect("read source"))
        .expect("source parses");
    assert!(
        source_document.info().expect("source info").is_some()
            || source_document
                .xmp_metadata()
                .expect("source xmp")
                .is_some(),
        "the fixture must carry metadata for this test to mean anything"
    );

    let out = TempOut::new("pdf");
    let output = run(&[
        "optimize",
        source.to_str().expect("utf-8 source"),
        "--out",
        out.arg(),
        "--remove-metadata",
    ]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let document = pdfxi_core::Document::from_bytes(fs::read(&out.path).expect("read output"))
        .expect("optimized file parses");
    assert!(document.info().expect("output info").is_none());
    assert!(document.xmp_metadata().expect("output xmp").is_none());
    assert_eq!(document.page_count().expect("page count"), 3);
}

#[test]
fn optimize_no_compress_still_writes_a_readable_file() {
    let source = real_fixture("powerpoint-slides.pdf");
    let out = TempOut::new("pdf");
    let output = run(&[
        "optimize",
        source.to_str().expect("utf-8 source"),
        "--out",
        out.arg(),
        "--no-compress",
        "--json",
    ]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let value: Value = serde_json::from_str(&stdout(&output)).expect("valid JSON");
    assert_eq!(value["streams_compressed"], 0);
    assert_eq!(page_texts(&source), page_texts(&out.path));
}

#[test]
fn optimize_refuses_to_overwrite_the_input() {
    let source = real_fixture("powerpoint-slides.pdf");
    let before = fs::read(&source).expect("read source");
    let output = run(&[
        "optimize",
        source.to_str().expect("utf-8 source"),
        "--out",
        source.to_str().expect("utf-8 source"),
    ]);
    assert!(!output.status.success(), "overwriting the input must fail");
    assert!(
        stderr(&output).contains("refusing to overwrite"),
        "{}",
        stderr(&output)
    );
    assert_eq!(
        fs::read(&source).expect("re-read source"),
        before,
        "the input must be untouched"
    );
}

#[test]
fn optimize_rejects_a_missing_input() {
    let output = run(&[
        "optimize",
        "target/tmp/definitely-missing.pdf",
        "--out",
        "target/tmp/unused.pdf",
    ]);
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("read target/tmp/definitely-missing.pdf"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn reflow_wraps_a_real_page_to_the_requested_width() {
    let source = real_fixture("powerpoint-slides.pdf");
    let output = run(&[
        "reflow",
        source.to_str().expect("utf-8 source"),
        "--width",
        "40",
        "--page",
        "1",
    ]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let text = stdout(&output);
    assert!(text.contains("page: 1"), "{text}");
    assert!(text.contains("Lantern"), "{text}");
    for line in text.lines() {
        let body = line.trim_start();
        if body.starts_with("page:") || body.starts_with("lines:") || body.is_empty() {
            continue;
        }
        assert!(
            body.chars().count() <= 40,
            "line wider than 40 characters: {line:?}"
        );
    }
    assert!(text.contains("lines: 2"), "{text}");
}

#[test]
fn reflow_json_reports_every_page_and_line() {
    let source = real_fixture("powerpoint-slides.pdf");
    let output = run(&[
        "reflow",
        source.to_str().expect("utf-8 source"),
        "--width",
        "40",
        "--json",
    ]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let value: Value = serde_json::from_str(&stdout(&output)).expect("valid JSON");
    assert_eq!(value["width"], 40);
    assert_eq!(value["line_count"], 6);
    let pages = value["pages"].as_array().expect("pages array");
    assert_eq!(pages.len(), 3);
    assert_eq!(pages[0]["page"], 1);
    assert_eq!(pages[0]["paragraphs"], 1);
    assert_eq!(pages[2]["page"], 3);
    assert_eq!(
        pages[2]["lines"][0],
        "Meadow Slide•The meadow slide marker is"
    );
}

#[test]
fn reflow_rejects_a_width_outside_the_range() {
    let source = real_fixture("powerpoint-slides.pdf");
    let output = run(&[
        "reflow",
        source.to_str().expect("utf-8 source"),
        "--width",
        "10",
    ]);
    assert!(!output.status.success(), "10 columns must be rejected");
    assert!(stderr(&output).contains("--width"), "{}", stderr(&output));
}

#[test]
fn read_aloud_queue_is_ordered_and_fixed_point() {
    let source = real_fixture("powerpoint-slides.pdf");
    let output = run(&["read-aloud", source.to_str().expect("utf-8 source")]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let text = stdout(&output);
    assert!(
        text.contains("utterance 0  page 1  0-832 ms  2 words"),
        "{text}"
    );
    assert!(
        text.contains("The lantern slide marker is the first landscape page."),
        "{text}"
    );
    assert!(
        text.contains("total: 6 utterance(s), 13983 ms at 180 wpm"),
        "{text}"
    );
}

#[test]
fn read_aloud_json_times_are_contiguous_and_complete() {
    let source = real_fixture("powerpoint-slides.pdf");
    let output = run(&[
        "read-aloud",
        source.to_str().expect("utf-8 source"),
        "--json",
    ]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let value: Value = serde_json::from_str(&stdout(&output)).expect("valid JSON");
    assert_eq!(value["wpm"], 180);
    assert_eq!(value["word_ms"], 333);
    assert_eq!(value["seek"], 0);
    let utterances = value["utterances"].as_array().expect("utterances array");
    assert_eq!(utterances.len(), 6);
    assert_eq!(utterances[0]["start_ms"], 0);
    for pair in utterances.windows(2) {
        assert_eq!(
            pair[0]["end_ms"], pair[1]["start_ms"],
            "the queue must have no gaps or overlaps"
        );
    }
    assert_eq!(
        utterances.last().map(|row| &row["end_ms"]),
        Some(&value["total_duration_ms"])
    );
    assert_eq!(utterances[3]["page"], 2);
}

#[test]
fn read_aloud_seek_rebases_the_remaining_queue() {
    let source = real_fixture("powerpoint-slides.pdf");
    let output = run(&[
        "read-aloud",
        source.to_str().expect("utf-8 source"),
        "--seek",
        "2",
        "--json",
    ]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let value: Value = serde_json::from_str(&stdout(&output)).expect("valid JSON");
    assert_eq!(value["seek"], 2);
    assert_eq!(value["total_duration_ms"], 9322);
    let utterances = value["utterances"].as_array().expect("utterances array");
    assert_eq!(utterances.len(), 4);
    assert_eq!(utterances[0]["index"], 2);
    assert_eq!(utterances[0]["start_ms"], 0);
    assert_eq!(utterances[0]["text"], "Harbor Slide");
}

#[test]
fn read_aloud_page_filter_limits_the_queue() {
    let source = real_fixture("powerpoint-slides.pdf");
    let output = run(&[
        "read-aloud",
        source.to_str().expect("utf-8 source"),
        "--page",
        "2",
        "--json",
    ]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let value: Value = serde_json::from_str(&stdout(&output)).expect("valid JSON");
    let utterances = value["utterances"].as_array().expect("utterances array");
    assert_eq!(utterances.len(), 2);
    assert!(utterances.iter().all(|row| row["page"] == 2));
}

#[test]
fn read_aloud_rejects_a_seek_past_the_end() {
    let source = real_fixture("powerpoint-slides.pdf");
    let output = run(&[
        "read-aloud",
        source.to_str().expect("utf-8 source"),
        "--seek",
        "99",
    ]);
    assert!(!output.status.success(), "seek past the end must fail");
    assert!(stderr(&output).contains("--seek 99"), "{}", stderr(&output));
}

#[test]
fn compare_reports_identical_documents() {
    let source = real_fixture("powerpoint-slides.pdf");
    let file = source.to_str().expect("utf-8 fixture");
    let output = run(&["compare", file, file]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let text = stdout(&output);
    assert!(
        text.contains("summary: aligned 3, changed 0, inserted 0, deleted 0"),
        "{text}"
    );
    assert!(text.contains("identical: true"), "{text}");
}

#[test]
fn compare_aligns_pages_by_content_around_a_deleted_page() {
    let source = real_fixture("powerpoint-slides.pdf");
    let out = TempOut::new("pdf");
    let delete = run(&[
        "pages",
        "delete",
        source.to_str().expect("utf-8 source"),
        "--pages",
        "2",
        "--out",
        out.arg(),
    ]);
    assert!(delete.status.success(), "stderr: {}", stderr(&delete));
    let output = run(&["compare", source.to_str().expect("utf-8 source"), out.arg()]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let text = stdout(&output);
    // Content alignment keeps pages 1 and 3 paired across the shifted numbers
    // instead of calling page 2 changed and page 3 deleted.
    assert!(text.contains("aligned  left 1  right 1  same"), "{text}");
    assert!(text.contains("deleted  left 2"), "{text}");
    assert!(text.contains("aligned  left 3  right 2  same"), "{text}");
    assert!(
        text.contains("summary: aligned 2, changed 0, inserted 0, deleted 1"),
        "{text}"
    );
    assert!(text.contains("identical: false"), "{text}");
}

#[test]
fn compare_reports_an_inserted_page_from_the_other_side() {
    let source = real_fixture("powerpoint-slides.pdf");
    let out = TempOut::new("pdf");
    let delete = run(&[
        "pages",
        "delete",
        source.to_str().expect("utf-8 source"),
        "--pages",
        "2",
        "--out",
        out.arg(),
    ]);
    assert!(delete.status.success(), "stderr: {}", stderr(&delete));
    let output = run(&["compare", out.arg(), source.to_str().expect("utf-8 source")]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let text = stdout(&output);
    assert!(text.contains("inserted right 2"), "{text}");
    assert!(
        text.contains("summary: aligned 2, changed 0, inserted 1, deleted 0"),
        "{text}"
    );
}

#[test]
fn compare_shows_changed_lines_side_by_side() {
    let left = authoring_fixture("authoring-page-boxes.pdf");
    let right = root_fixture("minimal.pdf");
    let output = run(&[
        "compare",
        left.to_str().expect("utf-8 left"),
        right.to_str().expect("utf-8 right"),
        "--lines",
    ]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let text = stdout(&output);
    assert!(
        text.contains("aligned  left 1  right 1  changed  -1 +1"),
        "{text}"
    );
    assert!(
        text.contains("- All five page boxes | + Hello, PDFXI!"),
        "{text}"
    );
    assert!(
        text.contains("summary: aligned 1, changed 1, inserted 0, deleted 0"),
        "{text}"
    );
}

#[test]
fn compare_json_has_stable_fields_and_page_rows() {
    let source = real_fixture("powerpoint-slides.pdf");
    let out = TempOut::new("pdf");
    let delete = run(&[
        "pages",
        "delete",
        source.to_str().expect("utf-8 source"),
        "--pages",
        "2",
        "--out",
        out.arg(),
    ]);
    assert!(delete.status.success(), "stderr: {}", stderr(&delete));
    let output = run(&[
        "compare",
        source.to_str().expect("utf-8 source"),
        out.arg(),
        "--json",
    ]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let value: Value = serde_json::from_str(&stdout(&output)).expect("valid JSON");
    assert_eq!(value["left_pages"], 3);
    assert_eq!(value["right_pages"], 2);
    assert_eq!(value["identical"], false);
    assert_eq!(value["summary"]["aligned"], 2);
    assert_eq!(value["summary"]["deleted"], 1);
    let pages = value["pages"].as_array().expect("pages array");
    assert_eq!(pages.len(), 3);
    assert_eq!(pages[0]["kind"], "aligned");
    assert_eq!(pages[0]["changed"], false);
    assert_eq!(pages[1]["kind"], "deleted");
    assert_eq!(pages[1]["left"], 2);
    assert_eq!(pages[2]["kind"], "aligned");
    assert_eq!(pages[2]["left"], 3);
    assert_eq!(pages[2]["right"], 2);
}

#[test]
fn compare_fail_on_difference_exits_non_zero() {
    let left = authoring_fixture("authoring-page-boxes.pdf");
    let right = root_fixture("minimal.pdf");
    let output = run(&[
        "compare",
        left.to_str().expect("utf-8 left"),
        right.to_str().expect("utf-8 right"),
        "--fail-on-difference",
    ]);
    assert!(
        !output.status.success(),
        "a difference must fail the command"
    );
    assert!(
        stderr(&output).contains("the documents differ"),
        "{}",
        stderr(&output)
    );

    let source = real_fixture("powerpoint-slides.pdf");
    let file = source.to_str().expect("utf-8 fixture");
    let same = run(&["compare", file, file, "--fail-on-difference"]);
    assert!(same.status.success(), "stderr: {}", stderr(&same));
}

#[test]
fn portfolio_lists_folder_paths_and_stream_metadata() {
    let pdf = TempPdf::new(&pdfxi_testkit::portfolio_pdf());
    let output = run(&["portfolio", pdf.arg()]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let text = stdout(&output);
    assert!(text.contains("collection: yes"), "{text}");
    assert!(text.contains("files: 3"), "{text}");
    assert!(
        text.contains("  brief.txt  size: 11  type: text/plain  description: Quarterly brief"),
        "{text}"
    );
    assert!(
        text.contains("    q1.pdf  size: 20  type: application/pdf"),
        "q1.pdf is two folders deep: {text}"
    );
    assert!(text.contains("notes.txt  size: 22"), "{text}");
}

#[test]
fn portfolio_json_is_stable_and_reports_the_collection() {
    let pdf = TempPdf::new(&pdfxi_testkit::portfolio_pdf());
    let output = run(&["portfolio", pdf.arg(), "--json"]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let value: Value = serde_json::from_str(&stdout(&output)).expect("valid JSON");
    assert_eq!(value["collection"], true);
    assert_eq!(value["count"], 3);
    let files = value["files"].as_array().expect("files array");
    assert_eq!(files.len(), 3);
    assert_eq!(files[0]["path"], "Reports/brief.txt");
    assert_eq!(files[0]["name"], "brief.txt");
    assert_eq!(files[0]["mime"], "text/plain");
    assert_eq!(files[0]["modified"], "D:20260101120000Z");
    assert_eq!(files[0]["size"], 11);
    assert_eq!(files[1]["path"], "Reports/2026/q1.pdf");
    assert_eq!(files[2]["path"], "notes.txt");
}

#[test]
fn portfolio_extract_recreates_folders_and_writes_exact_bytes() {
    let pdf = TempPdf::new(&pdfxi_testkit::portfolio_pdf());
    let directory = std::env::temp_dir().join(format!(
        "pdfxi-portfolio-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    let _ = fs::remove_dir_all(&directory);
    let target = directory.to_string_lossy().into_owned();
    let output = run(&["portfolio", pdf.arg(), "--extract", &target]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let text = stdout(&output);
    assert!(text.contains("extracted: 3 files"), "{text}");

    let brief = fs::read(directory.join("Reports").join("brief.txt")).expect("brief written");
    assert_eq!(brief, b"hello brief");
    let q1 = fs::read(directory.join("Reports").join("2026").join("q1.pdf")).expect("q1 written");
    assert_eq!(q1, b"%PDF-1.4\nq1 payload\n");
    let notes = fs::read(directory.join("notes.txt")).expect("notes written");
    assert_eq!(notes, b"notes without a folder");
    let _ = fs::remove_dir_all(&directory);
}

#[test]
fn portfolio_extract_refuses_a_path_that_escapes_the_directory() {
    let pdf = TempPdf::new(&pdfxi_testkit::hostile_portfolio_pdf());
    let parent = std::env::temp_dir().join(format!(
        "pdfxi-hostile-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    let target_dir = parent.join("out");
    let _ = fs::remove_dir_all(&parent);
    fs::create_dir_all(&target_dir).expect("create target");
    let target = target_dir.to_string_lossy().into_owned();
    let output = run(&["portfolio", pdf.arg(), "--extract", &target]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let text = stdout(&output);
    assert!(
        text.contains("refused: refused relative component"),
        "{text}"
    );
    assert!(text.contains("extracted: 0 files"), "{text}");
    assert!(
        !parent.join("escape.txt").exists(),
        "the traversal path must not be written outside the target directory"
    );
    let _ = fs::remove_dir_all(&parent);
}

#[test]
fn a_plain_pdf_is_not_a_portfolio() {
    let pdf = TempPdf::new(&pdfxi_testkit::annotated_pdf());
    let output = run(&["portfolio", pdf.arg()]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let text = stdout(&output);
    assert!(text.contains("collection: no"), "{text}");
    assert!(text.contains("files: 1"), "{text}");
}

#[test]
fn links_json_has_stable_fields() {
    let pdf = TempPdf::new(&pdfxi_testkit::annotated_pdf());
    let output = run(&["links", pdf.arg(), "--json"]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let value: Value = serde_json::from_str(&stdout(&output)).expect("valid JSON");
    assert_eq!(value["count"], 3);
    let links = value["links"].as_array().expect("links array");
    assert_eq!(links.len(), 3);
    assert_eq!(links[0]["kind"], "page");
    assert_eq!(links[1]["kind"], "uri");
    assert_eq!(links[1]["uri"], "https://example.com");
    assert_eq!(links[2]["dest_name"], "here");
}

#[test]
fn attachments_extract_writes_the_embedded_file() {
    let pdf = TempPdf::new(&pdfxi_testkit::annotated_pdf());
    let directory = std::env::temp_dir().join(format!(
        "pdfxi-extract-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or_default()
    ));
    let output = run(&[
        "attachments",
        pdf.arg(),
        "--extract",
        directory.to_str().expect("utf-8 directory"),
    ]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    assert!(
        stdout(&output).contains("extracted: 1 files"),
        "{}",
        stdout(&output)
    );
    let extracted = directory.join("notes.txt");
    assert!(extracted.exists(), "missing {}", extracted.display());
    assert_eq!(fs::read(&extracted).expect("read extracted file"), b"hello");
    let _ = fs::remove_dir_all(&directory);
}

#[test]
fn layers_report_visibility_and_nesting() {
    let pdf = TempPdf::new(&pdfxi_testkit::layered_pdf());
    let output = run(&["layers", pdf.arg()]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let text = stdout(&output);
    assert!(text.contains("layers: 3"), "{text}");
    assert!(text.contains("Background  visible: yes"), "{text}");
    assert!(text.contains("Watermark  visible: no"), "{text}");
    assert!(text.contains("Notes  visible: yes"), "{text}");
    assert!(
        text.contains("layer: 2    Watermark"),
        "nesting must indent: {text}"
    );
}

#[test]
fn layers_json_has_stable_fields() {
    let pdf = TempPdf::new(&pdfxi_testkit::layered_pdf());
    let output = run(&["layers", pdf.arg(), "--json"]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let value: Value = serde_json::from_str(&stdout(&output)).expect("valid JSON");
    assert_eq!(value["count"], 3);
    let layers = value["layers"].as_array().expect("layers array");
    assert_eq!(layers[1]["name"], "Watermark");
    assert_eq!(layers[1]["visible"], false);
    assert_eq!(layers[1]["depth"], 1);
}
