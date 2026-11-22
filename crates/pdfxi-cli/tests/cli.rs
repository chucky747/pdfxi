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
fn action_create_writes_a_valid_action_that_executes() {
    let file = TempPath::new("act");
    let created = run(&[
        "action",
        "create",
        file.arg(),
        "--name",
        "nightly",
        "--description",
        "Nightly stamp",
        "--step",
        "load:import",
        "--step",
        "stamp:watermark",
        "--param",
        "stamp:text=DRAFT",
        "--step",
        "save:export",
        "--param",
        "save:target=final.pdf",
        "--json",
    ]);
    assert!(created.status.success(), "stderr: {}", stderr(&created));
    let value: Value = serde_json::from_str(&stdout(&created)).expect("valid JSON");
    assert_eq!(value["name"], "nightly");
    assert_eq!(value["steps"], serde_json::json!(["load", "stamp", "save"]));

    let shown = run(&["action", "show", file.arg(), "nightly"]);
    assert!(shown.status.success(), "stderr: {}", stderr(&shown));
    let text = stdout(&shown);
    assert!(text.contains("stamp watermark"), "{text}");
    assert!(text.contains("text=DRAFT"), "{text}");

    let source = create_text_pdf("Created action source.");
    let out = TempDir::new();
    let executed = run(&[
        "action",
        "run",
        file.arg(),
        "nightly",
        "--input",
        source.arg(),
        "--output-dir",
        out.arg(),
        "--execute",
        "--json",
    ]);
    assert!(executed.status.success(), "stderr: {}", stderr(&executed));
    let exported = out.child("final.pdf");
    assert!(exported.exists(), "created action must export");
    let document = pdfxi_core::Document::from_bytes(fs::read(&exported).expect("read export"))
        .expect("parse export");
    let text = document.page_text(0).expect("page text");
    assert!(text.contains("Created action source."), "{text}");
    assert!(text.contains("DRAFT"), "{text}");
}

#[test]
fn action_create_rejects_bad_inputs() {
    let file = TempPath::new("act");
    let bad_param = run(&[
        "action",
        "create",
        file.arg(),
        "--name",
        "bad",
        "--step",
        "load:import",
        "--param",
        "nope:text=x",
    ]);
    assert!(!bad_param.status.success());
    assert!(
        stderr(&bad_param).contains("unknown step"),
        "{}",
        stderr(&bad_param)
    );

    let bad_step = run(&[
        "action",
        "create",
        file.arg(),
        "--name",
        "bad",
        "--step",
        "load",
    ]);
    assert!(!bad_step.status.success());
    assert!(
        stderr(&bad_step).contains("ID:KIND"),
        "{}",
        stderr(&bad_step)
    );

    let unknown_kind = run(&[
        "action",
        "create",
        file.arg(),
        "--name",
        "bad",
        "--step",
        "load:frobnicate",
    ]);
    assert!(!unknown_kind.status.success());

    let empty = run(&["action", "create", file.arg(), "--name", "bad"]);
    assert!(!empty.status.success());

    let good = run(&[
        "action",
        "create",
        file.arg(),
        "--name",
        "good",
        "--step",
        "load:import",
    ]);
    assert!(good.status.success(), "stderr: {}", stderr(&good));
    let existing = run(&[
        "action",
        "create",
        file.arg(),
        "--name",
        "other",
        "--step",
        "load:import",
    ]);
    assert!(!existing.status.success());
    assert!(
        stderr(&existing).contains("already exists"),
        "{}",
        stderr(&existing)
    );
    let forced = run(&[
        "action",
        "create",
        file.arg(),
        "--name",
        "other",
        "--step",
        "load:import",
        "--force",
    ]);
    assert!(forced.status.success(), "stderr: {}", stderr(&forced));
}

#[test]
fn action_run_execute_applies_watermark_and_writes_export() {
    let file = TempActionFile::new(&staging_action("nightly", "Stamp and export"));
    let source = create_text_pdf("Action source paragraph.");
    let out = TempDir::new();
    let result = run(&[
        "action",
        "run",
        file.arg(),
        "nightly",
        "--input",
        source.arg(),
        "--output-dir",
        out.arg(),
        "--execute",
        "--json",
    ]);
    assert!(result.status.success(), "stderr: {}", stderr(&result));
    let value: Value = serde_json::from_str(&stdout(&result)).expect("valid JSON");
    assert_eq!(value["jobs"][0]["state"], "succeeded");
    let steps = value["jobs"][0]["outcome"]["steps"]
        .as_array()
        .expect("steps");
    assert!(
        steps[0]["message"]
            .as_str()
            .unwrap_or("")
            .contains("imported"),
        "{steps:?}"
    );
    assert_eq!(steps[1]["message"], "watermarked every page with 'DRAFT'");
    assert!(
        steps[2]["message"]
            .as_str()
            .unwrap_or("")
            .contains("exported")
    );

    let exported = out.child("out.pdf");
    assert!(exported.exists(), "export must write the output");
    let bytes = fs::read(&exported).expect("read export");
    assert!(bytes.starts_with(b"%PDF-"));
    let document = pdfxi_core::Document::from_bytes(bytes).expect("parse export");
    assert_eq!(document.page_count().expect("pages"), 1);
    let text = document.page_text(0).expect("page text");
    assert!(text.contains("Action source paragraph."), "{text}");
    assert!(text.contains("DRAFT"), "{text}");
}

#[test]
fn action_run_execute_refuses_unsupported_steps_and_inputs() {
    let out = TempDir::new();
    let source = create_text_pdf("Action refusal source.");

    let security = TempActionFile::new(
        "ACTION\tsecure\tSecurity\n\
         STEP\tload\timport\n\
         STEP\tlock\tsecurity\n\
         PARAM\tpassword\ts3cret\n\
         PARAM\tpermissions\tprint\n\
         STEP\tsave\texport\n\
         PARAM\ttarget\tout.pdf\n",
    );
    let refused = run(&[
        "action",
        "run",
        security.arg(),
        "secure",
        "--input",
        source.arg(),
        "--output-dir",
        out.arg(),
        "--execute",
    ]);
    assert!(!refused.status.success());
    assert!(
        stderr(&refused).contains("1 of 1 job(s) failed"),
        "{}",
        stderr(&refused)
    );

    let convert = TempActionFile::new(
        "ACTION\tto-docx\tConvert\n\
         STEP\tload\timport\n\
         STEP\tconvert\tconvert\n\
         PARAM\tformat\tdocx\n\
         STEP\tsave\texport\n\
         PARAM\ttarget\tout.pdf\n",
    );
    let unsupported = run(&[
        "action",
        "run",
        convert.arg(),
        "to-docx",
        "--input",
        source.arg(),
        "--output-dir",
        out.arg(),
        "--execute",
        "--json",
    ]);
    assert!(!unsupported.status.success());
    assert!(
        stdout(&unsupported).contains("not supported by --execute"),
        "{}",
        stdout(&unsupported)
    );

    let custom = TempActionFile::new(
        "ACTION\tcustom\tCustom\n\
         STEP\tload\timport\n\
         STEP\trun\tcustom-command\n\
         PARAM\tcommand\tnope\n",
    );
    let refused_command = run(&[
        "action",
        "run",
        custom.arg(),
        "custom",
        "--input",
        source.arg(),
        "--execute",
    ]);
    assert!(!refused_command.status.success());

    let not_pdf = TempPath::new("txt");
    not_pdf.write(b"not a pdf");
    let bad_input = run(&[
        "action",
        "run",
        security.arg(),
        "secure",
        "--input",
        not_pdf.arg(),
        "--output-dir",
        out.arg(),
        "--execute",
        "--json",
    ]);
    assert!(!bad_input.status.success());
    assert!(
        stdout(&bad_input).contains("not a PDF"),
        "{}",
        stdout(&bad_input)
    );
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

/// A unique temporary directory, removed on drop.
struct TempDir {
    path: PathBuf,
    display: String,
}

impl TempDir {
    /// Create a unique directory.
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("pdfxi-cli-dir-{}-{nanos}-{id}", std::process::id()));
        fs::create_dir_all(&path).expect("create temporary directory");
        let display = path.to_string_lossy().into_owned();
        Self { path, display }
    }

    /// Return the directory as a command-line argument.
    fn arg(&self) -> &str {
        &self.display
    }

    /// Return a child path.
    fn child(&self, name: &str) -> PathBuf {
        self.path.join(name)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

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

/// Build a PDF with one page group and one blend-mode graphics state.
fn transparency_pdf() -> Vec<u8> {
    use pdfxi_core::{Dictionary, Object, ObjectId};
    let source = create_text_pdf("Blend mode content.");
    let bytes = fs::read(&source.path).expect("read text PDF");
    let mut document = pdfxi_core::Document::from_bytes(bytes).expect("parse text PDF");
    let state_number = document.allocate_object_number().expect("allocate state");
    let state_id = ObjectId::new(state_number, 0);
    let mut state = Dictionary::new();
    state.insert("Type".to_owned(), Object::Name("ExtGState".to_owned()));
    state.insert("BM".to_owned(), Object::Name("Multiply".to_owned()));
    state.insert("ca".to_owned(), Object::Real(0.5));
    state.insert("CA".to_owned(), Object::Real(1.0));
    document
        .set_object(state_id, Object::Dictionary(state))
        .expect("write state");
    let page = document.page(0).expect("page");
    let mut page_dictionary = document
        .object(page.id)
        .expect("page object")
        .as_dictionary()
        .cloned()
        .expect("page dictionary");
    let mut states = Dictionary::new();
    states.insert("GS1".to_owned(), Object::Reference(state_id));
    let mut resources = page_dictionary
        .get("Resources")
        .and_then(Object::as_dictionary)
        .cloned()
        .unwrap_or_default();
    resources.insert("ExtGState".to_owned(), Object::Dictionary(states));
    page_dictionary.insert("Resources".to_owned(), Object::Dictionary(resources));
    let mut group = Dictionary::new();
    group.insert("S".to_owned(), Object::Name("Transparency".to_owned()));
    group.insert("BM".to_owned(), Object::Name("Screen".to_owned()));
    group.insert("I".to_owned(), Object::Boolean(true));
    group.insert("K".to_owned(), Object::Boolean(false));
    page_dictionary.insert("Group".to_owned(), Object::Dictionary(group));
    document
        .set_object(page.id, Object::Dictionary(page_dictionary))
        .expect("write page");
    document.save().expect("save transparency PDF")
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

/// A PDF/VT-1-shaped fixture: PDF 1.6, XMP, a GTS_PDFX intent, print boxes,
/// a catalog `/DPartRoot` and a `/DPart` association on the page.
fn t0131_pdf_vt() -> Vec<u8> {
    let content = "BT (Hi) Tj ET";
    let xmp = "<?xpacket begin=\"\" id=\"W5M0\"?><x:xmpmeta xmlns:x=\"adobe:ns:meta/\"/>\
               <?xpacket end=\"w\"?>";
    t0131_pdf(
        "1.6",
        &[
            "<< /Type /Catalog /Pages 2 0 R /Metadata 5 0 R /OutputIntents [6 0 R] \
             /DPartRoot 8 0 R >>"
                .to_owned(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
             /CropBox [10 10 602 782] /BleedBox [20 20 592 772] /TrimBox [30 30 582 762] \
             /Contents 4 0 R /DPart 8 0 R >>"
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
            "<< /Type /OutputIntent /S /GTS_PDFX /DestOutputProfile 7 0 R >>".to_owned(),
            "<< /Length 0 /N 3 >>\nstream\n\nendstream".to_owned(),
            "<< /Type /DPart /RecordLevel 0 /DParts [] >>".to_owned(),
        ],
    )
}

#[test]
fn preflight_validates_pdf_vt_1_document_parts() {
    let plain = create_text_pdf("No document parts.");
    let missing = run(&["preflight", "PDF/VT-1", plain.arg(), "--json"]);
    assert_eq!(
        missing.status.code(),
        Some(2),
        "stderr: {}",
        stderr(&missing)
    );
    let value: Value = serde_json::from_str(&stdout(&missing)).expect("preflight JSON");
    let codes: Vec<&str> = value["before"]["findings"]
        .as_array()
        .expect("findings")
        .iter()
        .map(|finding| finding["code"].as_str().unwrap_or(""))
        .collect();
    assert!(
        codes.contains(&"PDFVT1.DPART_ROOT_MISSING"),
        "codes: {codes:?}"
    );
    assert!(
        codes.contains(&"PDFVT1.DPART_PAGE_UNASSIGNED"),
        "codes: {codes:?}"
    );

    let vt = TempPdf::new(&t0131_pdf_vt());
    let valid = run(&["preflight", "PDF/VT-1", vt.arg(), "--json"]);
    let value: Value = serde_json::from_str(&stdout(&valid)).expect("preflight JSON");
    let codes: Vec<&str> = value["before"]["findings"]
        .as_array()
        .expect("findings")
        .iter()
        .map(|finding| finding["code"].as_str().unwrap_or(""))
        .collect();
    assert!(
        !codes.iter().any(|code| code.starts_with("PDFVT1.DPART")),
        "document parts must satisfy the profile: {codes:?}"
    );
    assert_eq!(value["conforming"], true, "codes: {codes:?}");
    assert_eq!(valid.status.code(), Some(0), "stderr: {}", stderr(&valid));

    let unsupported = run(&["preflight", "PDF/VT-2", plain.arg()]);
    assert!(!unsupported.status.success());
    assert!(
        stderr(&unsupported).contains("unsupported preflight profile"),
        "{}",
        stderr(&unsupported)
    );
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
// production: print-production planning and application (T-0346)
// ---------------------------------------------------------------------------

#[test]
fn production_plans_and_applies_bleed_marks_and_intent() {
    let pdf = TempPdf::new(&t0131_roomy_pdf());

    let planned = run(&["production", "plan", pdf.arg(), "--json"]);
    assert!(planned.status.success(), "stderr: {}", stderr(&planned));
    let value: Value = serde_json::from_str(&stdout(&planned)).expect("plan JSON");
    assert_eq!(value["pages"], 1);
    assert_eq!(value["trim"]["width"], 552.0);
    assert_eq!(value["trim"]["height"], 732.0);
    assert_eq!(value["sheet"]["width"], 612.0);
    assert_eq!(value["marks"], 0);
    assert_eq!(value["bleed"]["x"], 0.0);
    assert_eq!(value["bleed"]["width"], 552.0);
    assert_eq!(
        value["separations"].as_array().expect("separations").len(),
        4
    );
    assert!(
        value["output_intent"]
            .as_str()
            .unwrap_or("")
            .contains("none declared")
    );
    assert_eq!(value["reparsed"], false);
    assert_eq!(value["out"], Value::Null);

    let out = TempOut::new("pdf");
    let applied = run(&[
        "production",
        "apply",
        pdf.arg(),
        "--out",
        out.arg(),
        "--bleed",
        "3",
        "--marks",
        "all",
        "--separation",
        "Cyan:process",
        "--separation",
        "PANTONE 185 C:spot",
        "--intent",
        "pdfx|Coated FOGRA39|http://www.color.org|relativecolorimetric",
        "--json",
    ]);
    assert!(applied.status.success(), "stderr: {}", stderr(&applied));
    let value: Value = serde_json::from_str(&stdout(&applied)).expect("apply JSON");
    assert_eq!(value["reparsed"], true);
    assert_eq!(value["out"], out.arg());
    assert_eq!(value["bleed"]["x"], -3.0);
    assert_eq!(value["bleed"]["width"], 558.0);
    assert!(value["marks"].as_u64().unwrap_or(0) > 0);
    let separations = value["separations"].as_array().expect("separations");
    assert_eq!(separations.len(), 2);
    assert_eq!(separations[0], "Cyan(process)");
    assert_eq!(separations[1], "PANTONE 185 C(spot)");
    assert!(
        value["output_intent"]
            .as_str()
            .unwrap_or("")
            .contains("PDF/X Coated FOGRA39")
    );

    let bytes = fs::read(&out.path).expect("production output");
    let document = pdfxi_core::Document::from_bytes(bytes).expect("output reparses");
    assert_eq!(document.page_count().expect("page count"), 1);
    let boxes = document.page_boxes(0).expect("page boxes");
    assert!(boxes.is_explicit(pdfxi_core::BoxKind::Bleed));
    let bleed = boxes.rectangle(pdfxi_core::BoxKind::Bleed);
    assert_eq!(bleed.left, 27.0);
    assert_eq!(bleed.bottom, 27.0);
    assert_eq!(bleed.right, 585.0);
    assert_eq!(bleed.top, 765.0);
    let text = String::from_utf8_lossy(&fs::read(&out.path).expect("output")).into_owned();
    assert!(text.contains("PdfxiMarks"), "printer marks are painted");
}

#[test]
fn production_rejects_bad_options_and_tight_sheets() {
    let pdf = TempPdf::new(&t0131_roomy_pdf());
    let out = TempOut::new("pdf");
    let cases = [
        (
            vec!["production", "plan", pdf.arg(), "--bleed=-1"],
            "finite and non-negative",
        ),
        (
            vec!["production", "plan", pdf.arg(), "--bleed", "100"],
            "0..=72",
        ),
        (
            vec!["production", "plan", pdf.arg(), "--marks", "sparkle"],
            "unknown mark family",
        ),
        (
            vec!["production", "plan", pdf.arg(), "--separation", "Cyan"],
            "NAME:process or NAME:spot",
        ),
        (
            vec![
                "production",
                "plan",
                pdf.arg(),
                "--intent",
                "pdfx|Name|Registry|sepia",
            ],
            "unsupported rendering intent",
        ),
        (
            vec![
                "production",
                "plan",
                pdf.arg(),
                "--intent",
                "bogus|Name|Registry|perceptual",
            ],
            "unknown output-intent subtype",
        ),
    ];
    for (args, needle) in cases {
        let output = run(&args);
        assert!(!output.status.success(), "args: {args:?}");
        assert!(
            stderr(&output).contains(needle),
            "{needle}: {}",
            stderr(&output)
        );
    }

    let tight = create_text_pdf("No bleed room.");
    let failure = run(&[
        "production",
        "apply",
        tight.arg(),
        "--out",
        out.arg(),
        "--marks",
        "crop",
    ]);
    assert!(!failure.status.success());
    assert!(
        stderr(&failure).contains("does not fit"),
        "{}",
        stderr(&failure)
    );
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
fn form_create_writes_typed_fields_and_reloads_them() {
    let first = TempOut::new("pdf");
    let second = TempOut::new("pdf");
    let create = |out: &TempOut| {
        run(&[
            "form",
            "create",
            out.arg(),
            "--field",
            "text:patient:72,700,200,24",
            "--field",
            "checkbox:agree:72,660,14,14",
            "--field",
            "radio:color:72,620,14,14:red|green",
            "--field",
            "combo:state:72,580,200,24:CA|NY|TX",
            "--field",
            "list:notes:72,500,200,60:a|b",
            "--field",
            "button:submit:72,460,100,24",
            "--field",
            "signature:sig:72,400,200,40",
        ])
    };
    let created = create(&first);
    assert!(created.status.success(), "stderr: {}", stderr(&created));
    let text = stdout(&created);
    assert!(
        text.contains("field: patient [text] rect: 72 700 200 24"),
        "{text}"
    );
    assert!(
        text.contains("field: color [radio] rect: 72 620 14 14 options: red|green"),
        "{text}"
    );
    assert!(text.contains("field: state [combo]"), "{text}");
    assert!(text.contains("fields: 7"), "{text}");

    let bytes = fs::read(&first.path).expect("form bytes");
    assert!(bytes.starts_with(b"%PDF-1.7"), "PDF header");

    let repeated = create(&second);
    assert!(repeated.status.success(), "stderr: {}", stderr(&repeated));
    assert_eq!(
        bytes,
        fs::read(&second.path).expect("second form bytes"),
        "form creation is deterministic"
    );

    let listed = run(&["form", "list", first.arg(), "--json"]);
    assert!(listed.status.success(), "stderr: {}", stderr(&listed));
    let value: Value = serde_json::from_str(&stdout(&listed)).expect("valid JSON");
    assert_eq!(value["present"], true);
    let fields = value["fields"].as_array().expect("fields");
    assert_eq!(fields.len(), 7);
    let names: Vec<&str> = fields
        .iter()
        .map(|field| field["name"].as_str().expect("name"))
        .collect();
    assert_eq!(
        names,
        vec![
            "patient", "agree", "color", "state", "notes", "submit", "sig"
        ]
    );
    let kinds: Vec<&str> = fields
        .iter()
        .map(|field| field["kind"].as_str().expect("kind"))
        .collect();
    assert_eq!(
        kinds,
        vec![
            "text",
            "checkbox",
            "radio",
            "choice",
            "choice",
            "button",
            "signature"
        ]
    );

    let info = run(&["info", first.arg()]);
    assert!(info.status.success(), "stderr: {}", stderr(&info));
    assert!(stdout(&info).contains("pages: 1"), "{}", stdout(&info));
}

#[test]
fn form_validate_and_calculate_use_typed_values() {
    let out = TempOut::new("pdf");
    let created = run(&[
        "form",
        "create",
        out.arg(),
        "--field",
        "text:a:72,700,100,24",
        "--field",
        "text:b:72,660,100,24",
        "--field",
        "text:total:72,620,100,24",
        "--field",
        "combo:state:72,560,200,24:CA|NY",
        "--field",
        "checkbox:agree:72,520,14,14",
    ]);
    assert!(created.status.success(), "stderr: {}", stderr(&created));

    let sum = run(&[
        "form",
        "calculate",
        out.arg(),
        "--set",
        "a=3",
        "--set",
        "b=4",
        "--sum",
        "total=a,b",
        "--json",
    ]);
    assert!(sum.status.success(), "stderr: {}", stderr(&sum));
    let value: Value = serde_json::from_str(&stdout(&sum)).expect("valid JSON");
    assert_eq!(value["calculations"], 1);
    assert_eq!(value["decimals"], 6);
    assert_eq!(value["results"][0]["target"], "total");
    assert_eq!(value["results"][0]["value"], "7");

    let average = run(&[
        "form",
        "calculate",
        out.arg(),
        "--set",
        "a=3",
        "--set",
        "b=4",
        "--average",
        "total=a,b",
        "--decimals",
        "1",
    ]);
    assert!(average.status.success(), "stderr: {}", stderr(&average));
    assert!(
        stdout(&average).contains("total = 3.5"),
        "{}",
        stdout(&average)
    );

    let valid = run(&[
        "form",
        "validate",
        out.arg(),
        "--set",
        "a=3",
        "--set",
        "state=CA",
        "--json",
    ]);
    assert!(valid.status.success(), "stderr: {}", stderr(&valid));
    let value: Value = serde_json::from_str(&stdout(&valid)).expect("valid JSON");
    assert_eq!(value["valid"], true);
    assert_eq!(value["error_count"], 0);
    assert_eq!(value["fields"], 5);

    let invalid = run(&["form", "validate", out.arg(), "--set", "state=ZZ", "--json"]);
    assert_eq!(invalid.status.code(), Some(2));
    let value: Value = serde_json::from_str(&stdout(&invalid)).expect("valid JSON");
    assert_eq!(value["valid"], false);
    assert_eq!(value["error_count"], 1);
    assert_eq!(value["errors"][0]["field"], "state");
    assert!(
        value["errors"][0]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("not an allowed choice"),
        "{}",
        stdout(&invalid)
    );

    let unknown_field = run(&["form", "validate", out.arg(), "--set", "missing=1"]);
    assert!(!unknown_field.status.success());

    let unknown_source = run(&[
        "form",
        "calculate",
        out.arg(),
        "--set",
        "a=3",
        "--sum",
        "total=a,missing",
    ]);
    assert!(!unknown_source.status.success());
    assert!(
        stderr(&unknown_source).contains("does not exist"),
        "{}",
        stderr(&unknown_source)
    );

    let bad_checkbox = run(&["form", "validate", out.arg(), "--set", "agree=maybe"]);
    assert!(!bad_checkbox.status.success());
    assert!(
        stderr(&bad_checkbox).contains("true or false"),
        "{}",
        stderr(&bad_checkbox)
    );
}

#[test]
fn font_add_text_embeds_a_cid_font_and_extracts_text() {
    let source = root_fixture("minimal.pdf");
    let first = TempOut::new("pdf");
    let second = TempOut::new("pdf");
    let added = run(&[
        "font",
        "add-text",
        source.to_str().expect("utf-8"),
        "--out",
        first.arg(),
        "--text",
        "Embedded font proof 2026",
        "--rect",
        "72,600,400,680",
        "--font-size",
        "14",
        "--json",
    ]);
    assert!(added.status.success(), "stderr: {}", stderr(&added));
    let value: Value = serde_json::from_str(&stdout(&added)).expect("valid JSON");
    assert_eq!(value["page"], 1);
    assert_eq!(value["lines"], 1);
    assert_eq!(value["font_size"], 14.0);
    assert_eq!(value["align"], "left");
    assert_eq!(value["words_found"], value["words_checked"]);
    assert!(value["words_checked"].as_u64().unwrap_or(0) >= 4);
    assert_eq!(value["font"]["base_font"], "CourierPrime");
    assert_eq!(value["font"]["descendant_subtype"], "CIDFontType2");
    assert_eq!(value["font"]["has_font_file"], true);
    assert_eq!(value["font"]["has_to_unicode"], true);
    assert!(
        value["resource"]
            .as_str()
            .unwrap_or_default()
            .starts_with("FXF"),
        "{}",
        stdout(&added)
    );

    let extracted = run(&["text", first.arg()]);
    assert!(extracted.status.success(), "stderr: {}", stderr(&extracted));
    let text = stdout(&extracted);
    assert!(text.contains("Embedded font proof 2026"), "{text}");

    let info = run(&["info", first.arg()]);
    assert!(info.status.success(), "stderr: {}", stderr(&info));
    assert!(stdout(&info).contains("pages: 1"), "{}", stdout(&info));

    let repeated = run(&[
        "font",
        "add-text",
        source.to_str().expect("utf-8"),
        "--out",
        second.arg(),
        "--text",
        "Embedded font proof 2026",
        "--rect",
        "72,600,400,680",
        "--font-size",
        "14",
    ]);
    assert!(repeated.status.success(), "stderr: {}", stderr(&repeated));
    assert_eq!(
        fs::read(&first.path).expect("first output"),
        fs::read(&second.path).expect("second output"),
        "font insertion is deterministic"
    );
}

#[test]
fn font_add_text_handles_an_authoring_page_and_alignment() {
    let source = authoring_fixture("authoring-page-boxes.pdf");
    let out = TempOut::new("pdf");
    let added = run(&[
        "font",
        "add-text",
        source.to_str().expect("utf-8"),
        "--out",
        out.arg(),
        "--text",
        "Centered CID font line",
        "--rect",
        "72,500,400,560",
        "--align",
        "center",
        "--json",
    ]);
    assert!(added.status.success(), "stderr: {}", stderr(&added));
    let value: Value = serde_json::from_str(&stdout(&added)).expect("valid JSON");
    assert_eq!(value["align"], "center");
    assert_eq!(value["font"]["descendant_subtype"], "CIDFontType2");
    assert_eq!(value["font"]["has_to_unicode"], true);
    let extracted = run(&["text", out.arg()]);
    assert!(extracted.status.success(), "stderr: {}", stderr(&extracted));
    assert!(stdout(&extracted).contains("Centered CID font line"));

    let blank = run(&[
        "font",
        "add-text",
        source.to_str().expect("utf-8"),
        "--out",
        out.arg(),
        "--text",
        "  ",
        "--rect",
        "72,500,400,560",
    ]);
    assert!(!blank.status.success());

    let bad_align = run(&[
        "font",
        "add-text",
        source.to_str().expect("utf-8"),
        "--out",
        out.arg(),
        "--text",
        "x",
        "--rect",
        "72,500,400,560",
        "--align",
        "justified",
    ]);
    assert!(!bad_align.status.success());
    assert!(
        stderr(&bad_align).contains("unknown --align"),
        "{}",
        stderr(&bad_align)
    );

    let off_page = run(&[
        "font",
        "add-text",
        source.to_str().expect("utf-8"),
        "--out",
        out.arg(),
        "--text",
        "x",
        "--rect",
        "600,500,900,560",
    ]);
    assert!(!off_page.status.success());

    let zero_page = run(&[
        "font",
        "add-text",
        source.to_str().expect("utf-8"),
        "--out",
        out.arg(),
        "--text",
        "x",
        "--rect",
        "72,500,400,560",
        "--page",
        "0",
    ]);
    assert!(!zero_page.status.success());
}

#[test]
fn ops_list_enumerates_every_default_operation() {
    let listed = run(&["ops", "list", "--json"]);
    assert!(listed.status.success(), "stderr: {}", stderr(&listed));
    let value: Value = serde_json::from_str(&stdout(&listed)).expect("valid JSON");
    assert_eq!(value["count"], 10);
    let operations = value["operations"].as_array().expect("operations");
    let names: Vec<&str> = operations
        .iter()
        .filter_map(|operation| operation["name"].as_str())
        .collect();
    assert_eq!(
        names,
        [
            "commands",
            "dry-run",
            "list-actions",
            "list-toolsets",
            "run-batch",
            "run-toolset",
            "show-action",
            "show-toolset",
            "validate-toolset",
            "version"
        ]
    );
    let batch = operations
        .iter()
        .find(|operation| operation["name"] == "run-batch")
        .expect("run-batch");
    let params = batch["params"].as_array().expect("params");
    assert_eq!(params.len(), 3);
    assert_eq!(params[0]["name"], "action");
    assert_eq!(params[0]["ty"], "string");
    assert_eq!(params[0]["required"], true);
    assert_eq!(params[1]["name"], "input");
    assert_eq!(params[1]["ty"], "path");
    assert_eq!(params[1]["required"], false);
    assert_eq!(params[2]["name"], "overwrite");
    assert_eq!(params[2]["ty"], "bool");
    let version = operations
        .iter()
        .find(|operation| operation["name"] == "version")
        .expect("version");
    assert_eq!(version["params"].as_array().expect("params").len(), 0);

    let repeated = run(&["ops", "list", "--json"]);
    assert_eq!(
        stdout(&listed),
        stdout(&repeated),
        "listing is deterministic"
    );

    let text = run(&["ops", "list"]);
    assert!(text.status.success(), "stderr: {}", stderr(&text));
    let printed = stdout(&text);
    assert!(printed.contains("operations: 10"), "{printed}");
    assert!(
        printed.contains("run-batch\taction:string,input?:path,overwrite?:bool"),
        "{printed}"
    );
}

#[test]
fn ops_resolve_builds_typed_plans_and_rejects_bad_arguments() {
    let resolved = run(&[
        "ops",
        "resolve",
        "run-batch",
        "action=batch.act",
        "input=in.pdf",
        "overwrite=true",
        "--json",
    ]);
    assert!(resolved.status.success(), "stderr: {}", stderr(&resolved));
    let value: Value = serde_json::from_str(&stdout(&resolved)).expect("valid JSON");
    assert_eq!(value["operation"], "run-batch");
    let arguments = value["arguments"].as_array().expect("arguments");
    assert_eq!(arguments.len(), 3);
    assert_eq!(arguments[0]["name"], "action");
    assert_eq!(arguments[0]["ty"], "string");
    assert_eq!(arguments[0]["value"], "batch.act");
    assert_eq!(arguments[1]["name"], "input");
    assert_eq!(arguments[1]["ty"], "path");
    assert_eq!(arguments[2]["name"], "overwrite");
    assert_eq!(arguments[2]["ty"], "bool");
    assert_eq!(arguments[2]["value"], "true");
    assert_eq!(
        value["summary"],
        "run-batch\taction=batch.act\tinput=in.pdf\toverwrite=true"
    );

    let version = run(&["ops", "resolve", "version", "--json"]);
    assert!(version.status.success(), "stderr: {}", stderr(&version));
    let value: Value = serde_json::from_str(&stdout(&version)).expect("valid JSON");
    assert_eq!(value["operation"], "version");
    assert_eq!(value["arguments"].as_array().expect("arguments").len(), 0);
    assert_eq!(value["summary"], "version");

    let toolset = run(&["ops", "resolve", "run-toolset", "name=demo", "limit=3"]);
    assert!(toolset.status.success(), "stderr: {}", stderr(&toolset));
    assert!(
        stdout(&toolset).contains("limit\tinteger\t3"),
        "{}",
        stdout(&toolset)
    );

    let missing = run(&["ops", "resolve", "run-batch", "input=x"]);
    assert!(!missing.status.success());
    assert!(
        stderr(&missing).contains("requires parameter 'action'"),
        "{}",
        stderr(&missing)
    );

    let unknown = run(&["ops", "resolve", "nope"]);
    assert!(!unknown.status.success());
    assert!(
        stderr(&unknown).contains("unknown operation 'nope'"),
        "{}",
        stderr(&unknown)
    );

    let unknown_param = run(&["ops", "resolve", "run-batch", "action=a", "colour=red"]);
    assert!(!unknown_param.status.success());
    assert!(
        stderr(&unknown_param).contains("has no parameter 'colour'"),
        "{}",
        stderr(&unknown_param)
    );

    let duplicate = run(&["ops", "resolve", "run-batch", "action=a", "action=b"]);
    assert!(!duplicate.status.success());
    assert!(
        stderr(&duplicate).contains("supplied more than once"),
        "{}",
        stderr(&duplicate)
    );

    let malformed = run(&["ops", "resolve", "run-batch", "action"]);
    assert!(!malformed.status.success());
    assert!(
        stderr(&malformed).contains("malformed"),
        "{}",
        stderr(&malformed)
    );

    let bad_integer = run(&["ops", "resolve", "run-toolset", "name=t", "limit=abc"]);
    assert!(!bad_integer.status.success());
    assert!(
        stderr(&bad_integer).contains("expects an integer"),
        "{}",
        stderr(&bad_integer)
    );

    let bad_bool = run(&["ops", "resolve", "run-batch", "action=a", "overwrite=maybe"]);
    assert!(!bad_bool.status.success());
    assert!(
        stderr(&bad_bool).contains("expects true or false"),
        "{}",
        stderr(&bad_bool)
    );

    let empty_name = run(&["ops", "resolve", ""]);
    assert!(!empty_name.status.success());
    assert!(
        stderr(&empty_name).contains("must not be empty"),
        "{}",
        stderr(&empty_name)
    );
}

#[test]
fn restyle_applies_style_and_verifies_text() {
    let source = create_text_pdf("Styled paragraph line one.\nLine two of the paragraph.");
    let out = TempOut::new("pdf");
    let styled = run(&[
        "restyle",
        source.arg(),
        "--out",
        out.arg(),
        "--run",
        "0",
        "--size",
        "20",
        "--scale",
        "90",
        "--char-spacing",
        "0.5",
        "--word-spacing",
        "1",
        "--color",
        "1,0,0",
        "--json",
    ]);
    assert!(styled.status.success(), "stderr: {}", stderr(&styled));
    let value: Value = serde_json::from_str(&stdout(&styled)).expect("valid JSON");
    assert_eq!(value["page"], 0);
    assert_eq!(value["run"], 0);
    assert_eq!(value["font_size"], 20.0);
    assert_eq!(value["horizontal_scaling"], 90.0);
    assert_eq!(value["character_spacing"], 0.5);
    assert_eq!(value["word_spacing"], 1.0);
    assert_eq!(value["fill_color"], serde_json::json!([1.0, 0.0, 0.0]));
    assert_eq!(value["embedded"], true);
    assert_eq!(value["verified_text"], true);

    let bytes = fs::read(&out.path).expect("restyled PDF");
    let document = pdfxi_core::Document::from_bytes(bytes.clone()).expect("reopen restyled");
    let text = document.page_text(0).expect("page text");
    assert!(text.contains("Styled paragraph line one."), "{text}");
    assert!(text.contains("Line two of the paragraph."), "{text}");
    let content = document.page_content(0).expect("content stream");
    let content = String::from_utf8_lossy(&content);
    assert!(
        content.contains("0.5000 Tc 1.0000 Tw 90.0000 Tz 0 Ts 1.0000 0.0000 0.0000 rg"),
        "{content}"
    );
    assert!(content.contains("20.0000 Tf"), "{content}");

    let again = TempOut::new("pdf");
    let repeated = run(&[
        "restyle",
        source.arg(),
        "--out",
        again.arg(),
        "--run",
        "0",
        "--size",
        "20",
        "--scale",
        "90",
        "--char-spacing",
        "0.5",
        "--word-spacing",
        "1",
        "--color",
        "1,0,0",
    ]);
    assert!(repeated.status.success(), "stderr: {}", stderr(&repeated));
    assert_eq!(
        bytes,
        fs::read(&again.path).expect("second restyled PDF"),
        "restyle is deterministic"
    );
}

#[test]
fn restyle_rejects_bad_requests() {
    let source = create_text_pdf("Restyle me.");

    let no_style = run(&["restyle", source.arg(), "--out", TempOut::new("pdf").arg()]);
    assert!(!no_style.status.success());
    assert!(
        stderr(&no_style).contains("pass at least one"),
        "{}",
        stderr(&no_style)
    );

    let zero_size = run(&[
        "restyle",
        source.arg(),
        "--out",
        TempOut::new("pdf").arg(),
        "--size",
        "0",
    ]);
    assert!(!zero_size.status.success());
    assert!(
        stderr(&zero_size).contains("font size must be finite and positive"),
        "{}",
        stderr(&zero_size)
    );

    let bad_color = run(&[
        "restyle",
        source.arg(),
        "--out",
        TempOut::new("pdf").arg(),
        "--color",
        "1,0",
    ]);
    assert!(!bad_color.status.success());
    assert!(
        stderr(&bad_color).contains("must be R,G,B"),
        "{}",
        stderr(&bad_color)
    );

    let missing_run = run(&[
        "restyle",
        source.arg(),
        "--out",
        TempOut::new("pdf").arg(),
        "--run",
        "99",
        "--size",
        "12",
    ]);
    assert!(!missing_run.status.success());
    assert!(
        stderr(&missing_run).contains("does not exist"),
        "{}",
        stderr(&missing_run)
    );

    let missing_page = run(&[
        "restyle",
        source.arg(),
        "--out",
        TempOut::new("pdf").arg(),
        "--page",
        "5",
        "--size",
        "12",
    ]);
    assert!(!missing_page.status.success());

    let missing_file = run(&[
        "restyle",
        TempPath::new("pdf").arg(),
        "--out",
        TempOut::new("pdf").arg(),
        "--size",
        "12",
    ]);
    assert!(!missing_file.status.success());
}

#[test]
fn form_recognize_writes_fields_from_hints() {
    let dir = TempDir::new();
    let manifest = dir.path.join("hints.json");
    fs::write(
        &manifest,
        br#"{"hints":[
            {"kind":"label","page":0,"x":40,"y":700,"width":80,"height":14,"text":"Name"},
            {"kind":"input","page":0,"x":130,"y":698,"width":200,"height":18},
            {"kind":"label","page":0,"x":40,"y":660,"width":80,"height":14,"text":"Agree"},
            {"kind":"checkbox","page":0,"x":130,"y":658,"width":14,"height":14},
            {"kind":"input","page":0,"x":40,"y":600,"width":200,"height":18,"name":"notes"},
            {"kind":"label","page":0,"x":40,"y":560,"width":80,"height":14,"text":"Orphan"}
        ]}"#,
    )
    .expect("write manifest");
    let out = TempOut::new("pdf");
    let created = run(&[
        "form",
        "recognize",
        manifest.to_str().expect("manifest path"),
        "--out",
        out.arg(),
        "--json",
    ]);
    assert!(created.status.success(), "stderr: {}", stderr(&created));
    let value: Value = serde_json::from_str(&stdout(&created)).expect("valid JSON");
    assert_eq!(value["hints"], 6);
    assert_eq!(value["recognized"], 3);
    assert_eq!(value["orphans"], serde_json::json!(["Orphan"]));
    assert_eq!(value["loaded"], 3);
    let fields = value["fields"].as_array().expect("fields");
    assert_eq!(fields.len(), 3);
    assert_eq!(fields[0]["name"], "name");
    assert_eq!(fields[0]["kind"], "text");
    assert_eq!(fields[0]["label"], "Name");
    assert_eq!(fields[0]["rank"], 0);
    assert_eq!(fields[1]["name"], "agree");
    assert_eq!(fields[1]["kind"], "checkbox");
    assert_eq!(fields[2]["name"], "notes");
    assert_eq!(fields[2]["kind"], "text");

    let list = run(&["form", "list", out.arg()]);
    assert!(list.status.success(), "stderr: {}", stderr(&list));
    let report = stdout(&list);
    assert!(report.contains("name  [text]"), "{report}");
    assert!(report.contains("agree  [checkbox]"), "{report}");
    assert!(report.contains("notes  [text]"), "{report}");
    assert!(report.contains("fields: 3"), "{report}");

    let bytes = fs::read(&out.path).expect("recognized form");
    let document = pdfxi_core::Document::from_bytes(bytes.clone()).expect("parse form");
    assert_eq!(document.page_count().expect("pages"), 1);

    let again = TempOut::new("pdf");
    let repeated = run(&[
        "form",
        "recognize",
        manifest.to_str().expect("manifest path"),
        "--out",
        again.arg(),
    ]);
    assert!(repeated.status.success(), "stderr: {}", stderr(&repeated));
    assert_eq!(
        bytes,
        fs::read(&again.path).expect("second recognized form"),
        "recognition is deterministic"
    );
}

#[test]
fn form_recognize_rejects_bad_hints() {
    let dir = TempDir::new();

    let labels_only = dir.path.join("labels.json");
    fs::write(
        &labels_only,
        br#"{"hints":[{"kind":"label","page":0,"x":0,"y":0,"width":10,"height":10,"text":"Only"}]}"#,
    )
    .expect("write labels manifest");
    let no_controls = run(&[
        "form",
        "recognize",
        labels_only.to_str().expect("manifest path"),
        "--out",
        TempOut::new("pdf").arg(),
    ]);
    assert!(!no_controls.status.success());
    assert!(
        stderr(&no_controls).contains("no form controls were recognized"),
        "{}",
        stderr(&no_controls)
    );

    let unknown = dir.path.join("unknown.json");
    fs::write(
        &unknown,
        br#"{"hints":[{"kind":"signature","page":0,"x":0,"y":0,"width":10,"height":10}]}"#,
    )
    .expect("write unknown manifest");
    let bad_kind = run(&[
        "form",
        "recognize",
        unknown.to_str().expect("manifest path"),
        "--out",
        TempOut::new("pdf").arg(),
    ]);
    assert!(!bad_kind.status.success());
    assert!(
        stderr(&bad_kind).contains("unknown hint kind"),
        "{}",
        stderr(&bad_kind)
    );

    let negative = dir.path.join("negative.json");
    fs::write(
        &negative,
        br#"{"hints":[{"kind":"input","page":0,"x":0,"y":0,"width":-1,"height":10}]}"#,
    )
    .expect("write negative manifest");
    let bad_rect = run(&[
        "form",
        "recognize",
        negative.to_str().expect("manifest path"),
        "--out",
        TempOut::new("pdf").arg(),
    ]);
    assert!(!bad_rect.status.success());

    let malformed = dir.path.join("malformed.json");
    fs::write(&malformed, b"not json").expect("write malformed manifest");
    assert!(
        !run(&[
            "form",
            "recognize",
            malformed.to_str().expect("manifest path"),
            "--out",
            TempOut::new("pdf").arg()
        ])
        .status
        .success()
    );

    let missing = run(&[
        "form",
        "recognize",
        dir.path.join("absent.json").to_str().expect("path"),
        "--out",
        TempOut::new("pdf").arg(),
    ]);
    assert!(!missing.status.success());
}

#[test]
fn watch_converts_text_html_and_images_to_pdf() {
    let watched = TempDir::new();
    let out = TempDir::new();
    fs::write(watched.child("note.txt"), b"Watched hello.\nSecond line.").expect("write text");
    fs::write(
        watched.child("page.html"),
        b"<h1>HTML page</h1><p>Body text.</p>",
    )
    .expect("write html");
    fs::write(watched.child("image.png"), sample_png()).expect("write image");

    let polled = run(&[
        "watch",
        watched.arg(),
        "--out",
        out.arg(),
        "--ext",
        "txt",
        "--ext",
        "html",
        "--ext",
        "png",
        "--json",
    ]);
    assert!(polled.status.success(), "stderr: {}", stderr(&polled));
    let value: Value = serde_json::from_str(&stdout(&polled)).expect("valid JSON");
    assert_eq!(value["suffix"], "pdf");
    assert_eq!(value["written"], 3);
    let report = &value["reports"][0];
    assert_eq!(report["converted"], 3);
    assert_eq!(report["failed"], 0);

    let note = fs::read(out.child("note.pdf")).expect("text PDF");
    assert!(note.starts_with(b"%PDF-"));
    let document = pdfxi_core::Document::from_bytes(note).expect("parse text PDF");
    assert_eq!(document.page_count().expect("pages"), 1);
    let text = document.page_text(0).expect("text");
    assert!(text.contains("Watched hello."), "{text}");
    assert!(text.contains("Second line."), "{text}");

    let html = fs::read(out.child("page.pdf")).expect("html PDF");
    assert!(html.starts_with(b"%PDF-"));
    let document = pdfxi_core::Document::from_bytes(html).expect("parse html PDF");
    let text = document.page_text(0).expect("html text");
    assert!(text.contains("HTML page"), "{text}");
    assert!(text.contains("Body text."), "{text}");

    let image = fs::read(out.child("image.pdf")).expect("image PDF");
    assert!(image.starts_with(b"%PDF-"));
    let document = pdfxi_core::Document::from_bytes(image).expect("parse image PDF");
    assert_eq!(document.page_count().expect("pages"), 1);
}

#[test]
fn script_run_evaluates_and_updates_fields() {
    let dir = TempDir::new();
    let script = dir.path.join("invoice.js");
    fs::write(
        &script,
        br#"var p = Number(this.getField("price").value);
var q = Number(this.getField("qty").value);
this.getField("total").value = String(p * q);
this.getField("total").value;"#,
    )
    .expect("write script");

    let run_script = run(&[
        "script",
        "run",
        script.to_str().expect("script path"),
        "--field",
        "price=12.5",
        "--field",
        "qty=4",
        "--field",
        "total=0",
        "--event-key",
        "calculate",
        "--json",
    ]);
    assert!(
        run_script.status.success(),
        "stderr: {}",
        stderr(&run_script)
    );
    let value: Value = serde_json::from_str(&stdout(&run_script)).expect("valid JSON");
    assert_eq!(value["result"], "50");
    assert_eq!(value["result_type"], "string");
    assert_eq!(value["max_steps"], 100000);
    assert_eq!(value["event"]["key"], "calculate");
    assert_eq!(value["event"]["rc"], true);
    let fields = value["fields"].as_array().expect("fields");
    let total = fields
        .iter()
        .find(|field| field["name"] == "total")
        .expect("total field");
    assert_eq!(total["value"], "50");

    let again = run(&[
        "script",
        "run",
        script.to_str().expect("script path"),
        "--field",
        "price=12.5",
        "--field",
        "qty=4",
        "--field",
        "total=0",
        "--event-key",
        "calculate",
        "--json",
    ]);
    assert!(again.status.success(), "stderr: {}", stderr(&again));
    assert_eq!(
        stdout(&run_script),
        stdout(&again),
        "sandbox execution is deterministic"
    );

    let plain = run(&[
        "script",
        "run",
        script.to_str().expect("script path"),
        "--field",
        "price=3",
        "--field",
        "qty=5",
    ]);
    assert!(plain.status.success(), "stderr: {}", stderr(&plain));
    assert!(stdout(&plain).contains("result: 15 (string)"));
    assert!(stdout(&plain).contains("field: total = 15"));
}

#[test]
fn script_run_enforces_sandbox_and_budget() {
    let dir = TempDir::new();
    let dangerous = dir.path.join("dangerous.js");
    fs::write(&dangerous, b"eval('1');").expect("write dangerous script");
    let refused = run(&["script", "run", dangerous.to_str().expect("script path")]);
    assert!(!refused.status.success());
    assert!(
        stderr(&refused).contains("dangerous global 'eval'"),
        "{}",
        stderr(&refused)
    );

    let loop_script = dir.path.join("loop.js");
    fs::write(&loop_script, b"var n = 0; while (true) { n = n + 1; } n;")
        .expect("write loop script");
    let budget = run(&[
        "script",
        "run",
        loop_script.to_str().expect("script path"),
        "--max-steps",
        "20",
    ]);
    assert!(!budget.status.success());
    assert!(
        stderr(&budget).contains("budget exceeded"),
        "{}",
        stderr(&budget)
    );

    let simple = dir.path.join("simple.js");
    fs::write(&simple, b"1 + 1;").expect("write simple script");
    let bad_field = run(&[
        "script",
        "run",
        simple.to_str().expect("script path"),
        "--field",
        "novalue",
    ]);
    assert!(!bad_field.status.success());
    assert!(
        stderr(&bad_field).contains("must be NAME=VALUE"),
        "{}",
        stderr(&bad_field)
    );

    let empty_name = run(&[
        "script",
        "run",
        simple.to_str().expect("script path"),
        "--field",
        "=value",
    ]);
    assert!(!empty_name.status.success());
    assert!(
        stderr(&empty_name).contains("empty name"),
        "{}",
        stderr(&empty_name)
    );

    let missing = run(&[
        "script",
        "run",
        dir.path.join("absent.js").to_str().expect("script path"),
    ]);
    assert!(!missing.status.success());
    assert!(
        stderr(&missing).contains("read script"),
        "{}",
        stderr(&missing)
    );
}

#[test]
fn portfolio_manage_renames_describes_adds_and_removes() {
    let dir = TempDir::new();
    fs::write(dir.child("brief.txt"), b"Case brief body.").expect("brief");
    fs::write(dir.child("notes.txt"), b"Hearing notes.").expect("notes");
    fs::write(dir.child("extra.txt"), b"Extra file.").expect("extra");
    let manifest = dir.child("manifest.json");
    fs::write(
        &manifest,
        br#"{"title":"Case file","entries":[
            {"name":"docs/brief.txt","description":"Case brief","order":0,"file":"brief.txt"},
            {"name":"docs/notes.txt","order":1,"file":"notes.txt"}
        ]}"#,
    )
    .expect("manifest");
    let source = dir.child("source.pdf");
    let created = run(&[
        "create",
        "portfolio",
        manifest.to_str().expect("manifest"),
        "--out",
        source.to_str().expect("source"),
    ]);
    assert!(created.status.success(), "stderr: {}", stderr(&created));

    let managed = dir.child("managed.pdf");
    let edited = run(&[
        "portfolio",
        source.to_str().expect("source"),
        "--out",
        managed.to_str().expect("managed"),
        "--rename",
        "docs/brief.txt=docs/renamed.txt",
        "--describe",
        "docs/renamed.txt=Renamed brief",
        "--add",
        &format!("{}=docs/extra.txt", dir.child("extra.txt").display()),
        "--remove",
        "docs/notes.txt",
        "--json",
    ]);
    assert!(edited.status.success(), "stderr: {}", stderr(&edited));
    let value: Value = serde_json::from_str(&stdout(&edited)).expect("valid JSON");
    assert_eq!(value["verified"], true);
    assert_eq!(value["operations"].as_array().expect("operations").len(), 4);
    let entries = value["entries"].as_array().expect("entries");
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0]["path"], "docs/extra.txt");
    assert_eq!(entries[1]["path"], "docs/renamed.txt");
    assert_eq!(entries[1]["description"], "Renamed brief");

    let document = pdfxi_core::Document::from_bytes(fs::read(&managed).expect("managed bytes"))
        .expect("reopen managed");
    let items = document.portfolio_items().expect("items");
    assert_eq!(items.len(), 2);
    let brief = items
        .iter()
        .find(|item| item.path == "docs/renamed.txt")
        .expect("renamed brief");
    assert_eq!(brief.description.as_deref(), Some("Renamed brief"));
    assert_eq!(
        document.attachment_bytes(&brief.key).expect("brief bytes"),
        b"Case brief body."
    );

    let extract = dir.child("out");
    let listed = run(&[
        "portfolio",
        managed.to_str().expect("managed"),
        "--extract",
        extract.to_str().expect("extract"),
    ]);
    assert!(listed.status.success(), "stderr: {}", stderr(&listed));
    assert_eq!(
        fs::read(extract.join("docs/extra.txt")).expect("extra"),
        b"Extra file."
    );
    assert!(!extract.join("docs/notes.txt").exists());
}

#[test]
fn portfolio_manage_rejects_bad_requests() {
    let dir = TempDir::new();
    fs::write(dir.child("brief.txt"), b"Brief.").expect("brief");
    let manifest = dir.child("manifest.json");
    fs::write(
        &manifest,
        br#"{"title":"T","entries":[{"name":"docs/brief.txt","file":"brief.txt"}]}"#,
    )
    .expect("manifest");
    let source = dir.child("source.pdf");
    let created = run(&[
        "create",
        "portfolio",
        manifest.to_str().expect("manifest"),
        "--out",
        source.to_str().expect("source"),
    ]);
    assert!(created.status.success(), "stderr: {}", stderr(&created));

    let missing = run(&[
        "portfolio",
        source.to_str().expect("source"),
        "--out",
        dir.child("x.pdf").to_str().expect("out"),
        "--rename",
        "absent.txt=new.txt",
    ]);
    assert!(!missing.status.success());
    assert!(
        stderr(&missing).contains("no entry"),
        "{}",
        stderr(&missing)
    );

    let unsafe_name = run(&[
        "portfolio",
        source.to_str().expect("source"),
        "--out",
        dir.child("x.pdf").to_str().expect("out"),
        "--add",
        &format!("{}=../escape.txt", dir.child("brief.txt").display()),
    ]);
    assert!(!unsafe_name.status.success());
    assert!(
        stderr(&unsafe_name).contains("parent component"),
        "{}",
        stderr(&unsafe_name)
    );

    let no_out = run(&[
        "portfolio",
        source.to_str().expect("source"),
        "--rename",
        "docs/brief.txt=new.txt",
    ]);
    assert!(!no_out.status.success());
    assert!(
        stderr(&no_out).contains("requires --out"),
        "{}",
        stderr(&no_out)
    );

    let not_portfolio = run(&[
        "portfolio",
        create_text_pdf("Not a portfolio.").arg(),
        "--out",
        dir.child("x.pdf").to_str().expect("out"),
        "--remove",
        "x.txt",
    ]);
    assert!(!not_portfolio.status.success());
    assert!(
        stderr(&not_portfolio).contains("not a portfolio"),
        "{}",
        stderr(&not_portfolio)
    );
}

#[test]
fn office_plan_and_convert_import_to_pdf() {
    let dir = TempDir::new();
    let manifest = dir.path.join("import.json");
    fs::write(
        &manifest,
        br#"{"family":"word","flow":"import-to-pdf","name":"Report","markup":"<h1>Report</h1><p>Hello integration.</p>"}"#,
    )
    .expect("write manifest");

    let planned = run(&[
        "office",
        "plan",
        manifest.to_str().expect("manifest path"),
        "--json",
    ]);
    assert!(planned.status.success(), "stderr: {}", stderr(&planned));
    let value: Value = serde_json::from_str(&stdout(&planned)).expect("valid JSON");
    assert_eq!(value["family"], "Word");
    assert_eq!(value["flow"], "import-to-pdf");
    assert_eq!(value["output_name"], "report.pdf");
    assert_eq!(value["page_count"], 1);
    assert_eq!(value["block_count"], 2);
    assert_eq!(value["executed"], false);
    let steps = value["steps"].as_array().expect("steps");
    assert_eq!(steps.len(), 3);
    assert_eq!(steps[0]["adapter"], "office_contract");
    assert_eq!(steps[1]["adapter"], "office_import");
    assert_eq!(steps[2]["adapter"], "create_from_text");

    let out = TempOut::new("pdf");
    let converted = run(&[
        "office",
        "convert",
        manifest.to_str().expect("manifest path"),
        "--out",
        out.arg(),
        "--json",
    ]);
    assert!(converted.status.success(), "stderr: {}", stderr(&converted));
    let value: Value = serde_json::from_str(&stdout(&converted)).expect("valid JSON");
    assert_eq!(value["executed"], true);
    assert_eq!(value["verified"], "pdf");
    assert_eq!(value["pdf_pages"], 1);
    assert!(value["text_chars"].as_u64().unwrap_or(0) > 0);
    let bytes = fs::read(&out.path).expect("integration PDF");
    assert!(bytes.starts_with(b"%PDF-"));
    let document = pdfxi_core::Document::from_bytes(bytes.clone()).expect("reopen PDF");
    assert_eq!(document.page_count().expect("page count"), 1);
    let text = document.page_text(0).expect("page text");
    assert!(text.contains("Hello integration."), "{text}");

    let again = TempOut::new("pdf");
    let repeated = run(&[
        "office",
        "convert",
        manifest.to_str().expect("manifest path"),
        "--out",
        again.arg(),
    ]);
    assert!(repeated.status.success(), "stderr: {}", stderr(&repeated));
    assert_eq!(
        bytes,
        fs::read(&again.path).expect("second integration PDF"),
        "import-to-pdf is deterministic"
    );
}

#[test]
fn office_convert_exports_ooxml_and_rejects_mismatches() {
    let dir = TempDir::new();
    let manifest = dir.path.join("export.json");
    fs::write(
        &manifest,
        br#"{"family":"excel","flow":"pdf-to-office","name":"Book","pages":[
            {"width":595,"height":842,"runs":[
                {"text":"Region","x":40,"y":800,"font_size":11},
                {"text":"Sales","x":140,"y":800,"font_size":11}
            ],"tables":[{"name":"Data","rows":[["Region","Sales"],["West","42"]]}]}
        ]}"#,
    )
    .expect("write manifest");

    let out = TempOut::new("xlsx");
    let converted = run(&[
        "office",
        "convert",
        manifest.to_str().expect("manifest path"),
        "--out",
        out.arg(),
        "--json",
    ]);
    assert!(converted.status.success(), "stderr: {}", stderr(&converted));
    let value: Value = serde_json::from_str(&stdout(&converted)).expect("valid JSON");
    assert_eq!(value["family"], "Excel");
    assert_eq!(value["flow"], "pdf-to-office");
    assert_eq!(value["output_name"], "book.xlsx");
    assert_eq!(value["executed"], true);
    assert_eq!(value["verified"], "ooxml");
    assert_eq!(value["parts"][0], "xl/workbook.xml");
    let bytes = fs::read(&out.path).expect("exported workbook");
    assert!(bytes.starts_with(b"PK\x03\x04"));
    assert!(
        bytes
            .windows(b"xl/workbook.xml".len())
            .any(|window| window == b"xl/workbook.xml")
    );

    let wrong = run(&[
        "office",
        "convert",
        manifest.to_str().expect("manifest path"),
        "--out",
        TempOut::new("pdf").arg(),
    ]);
    assert!(!wrong.status.success());
    assert!(
        stderr(&wrong).contains("must have the .xlsx extension"),
        "{}",
        stderr(&wrong)
    );

    let unknown = dir.path.join("unknown.json");
    fs::write(
        &unknown,
        br#"{"family":"visio","flow":"import-to-pdf","name":"T","markup":"<p>x</p>"}"#,
    )
    .expect("write unknown manifest");
    let bad_family = run(&["office", "plan", unknown.to_str().expect("manifest path")]);
    assert!(!bad_family.status.success());
    assert!(
        stderr(&bad_family).contains("unknown Office family"),
        "{}",
        stderr(&bad_family)
    );

    let missing_markup = dir.path.join("missing-markup.json");
    fs::write(
        &missing_markup,
        br#"{"family":"word","flow":"import-to-pdf","name":"T"}"#,
    )
    .expect("write missing markup manifest");
    let bad_payload = run(&[
        "office",
        "plan",
        missing_markup.to_str().expect("manifest path"),
    ]);
    assert!(!bad_payload.status.success());
    assert!(
        stderr(&bad_payload).contains("requires a markup string"),
        "{}",
        stderr(&bad_payload)
    );

    let empty_pages = dir.path.join("empty-pages.json");
    fs::write(
        &empty_pages,
        br#"{"family":"word","flow":"pdf-to-office","name":"T","pages":[]}"#,
    )
    .expect("write empty pages manifest");
    assert!(
        !run(&[
            "office",
            "plan",
            empty_pages.to_str().expect("manifest path")
        ])
        .status
        .success()
    );

    let malformed = dir.path.join("malformed.json");
    fs::write(&malformed, b"not json").expect("write malformed manifest");
    assert!(
        !run(&["office", "plan", malformed.to_str().expect("manifest path")])
            .status
            .success()
    );
}

#[test]
fn create_portfolio_embeds_and_round_trips_files() {
    let dir = TempDir::new();
    let brief = dir.path.join("brief.txt");
    let notes = dir.path.join("notes.txt");
    fs::write(&brief, b"Case brief body.").expect("write brief");
    fs::write(&notes, b"Hearing notes.").expect("write notes");
    let manifest = dir.path.join("manifest.json");
    fs::write(
        &manifest,
        br#"{"title":"Case file","entries":[
            {"name":"docs/brief.txt","description":"Case brief","order":0,"file":"brief.txt"},
            {"name":"docs/notes.txt","order":1,"file":"notes.txt"}
        ]}"#,
    )
    .expect("write manifest");
    let out = TempOut::new("pdf");
    let created = run(&[
        "create",
        "portfolio",
        manifest.to_str().expect("manifest path"),
        "--out",
        out.arg(),
        "--json",
    ]);
    assert!(created.status.success(), "stderr: {}", stderr(&created));
    let value: Value = serde_json::from_str(&stdout(&created)).expect("valid JSON");
    assert_eq!(value["title"], "Case file");
    assert_eq!(value["entries"], 2);
    assert_eq!(value["embedded_bytes"], 30);
    assert_eq!(value["collection"], true);
    assert_eq!(value["items"], 2);
    assert_eq!(value["verified_files"], 2);
    assert_eq!(value["files"][0]["name"], "docs/brief.txt");
    assert_eq!(value["files"][0]["description"], "Case brief");

    let bytes = fs::read(&out.path).expect("portfolio PDF");
    let document = pdfxi_core::Document::from_bytes(bytes.clone()).expect("reopen portfolio");
    assert!(document.is_portfolio().expect("collection"));
    let items = document.portfolio_items().expect("portfolio items");
    assert_eq!(items.len(), 2);
    let brief_item = items
        .iter()
        .find(|item| item.path == "docs/brief.txt")
        .expect("brief item");
    assert_eq!(
        document
            .attachment_bytes(&brief_item.key)
            .expect("brief bytes"),
        b"Case brief body."
    );

    let extract = dir.path.join("out");
    let listed = run(&[
        "portfolio",
        out.arg(),
        "--extract",
        extract.to_str().expect("extract path"),
    ]);
    assert!(listed.status.success(), "stderr: {}", stderr(&listed));
    assert_eq!(
        fs::read(extract.join("docs/brief.txt")).expect("extracted brief"),
        b"Case brief body."
    );
    assert_eq!(
        fs::read(extract.join("docs/notes.txt")).expect("extracted notes"),
        b"Hearing notes."
    );

    let again = TempOut::new("pdf");
    let repeated = run(&[
        "create",
        "portfolio",
        manifest.to_str().expect("manifest path"),
        "--out",
        again.arg(),
    ]);
    assert!(repeated.status.success(), "stderr: {}", stderr(&repeated));
    assert_eq!(
        bytes,
        fs::read(&again.path).expect("second portfolio PDF"),
        "portfolio assembly is deterministic"
    );
}

#[test]
fn create_portfolio_rejects_bad_manifests() {
    let dir = TempDir::new();
    fs::write(dir.path.join("brief.txt"), b"Case brief.").expect("write brief");

    let traversal = dir.path.join("traversal.json");
    fs::write(
        &traversal,
        br#"{"title":"T","entries":[{"name":"../evil.txt","file":"brief.txt"}]}"#,
    )
    .expect("write traversal manifest");
    let refused = run(&[
        "create",
        "portfolio",
        traversal.to_str().expect("manifest path"),
        "--out",
        TempOut::new("pdf").arg(),
    ]);
    assert!(!refused.status.success());
    assert!(
        stderr(&refused).contains("parent component"),
        "{}",
        stderr(&refused)
    );

    let empty = dir.path.join("empty.json");
    fs::write(&empty, br#"{"title":"T","entries":[]}"#).expect("write empty manifest");
    let empty_out = TempOut::new("pdf");
    let empty_portfolio = run(&[
        "create",
        "portfolio",
        empty.to_str().expect("manifest path"),
        "--out",
        empty_out.arg(),
        "--json",
    ]);
    assert!(
        empty_portfolio.status.success(),
        "stderr: {}",
        stderr(&empty_portfolio)
    );
    let value: Value = serde_json::from_str(&stdout(&empty_portfolio)).expect("valid JSON");
    assert_eq!(value["entries"], 0);
    assert_eq!(value["items"], 0);
    assert_eq!(value["collection"], true);

    let missing = dir.path.join("missing.json");
    fs::write(
        &missing,
        br#"{"title":"T","entries":[{"name":"docs/a.txt","file":"absent.txt"}]}"#,
    )
    .expect("write missing manifest");
    let no_file = run(&[
        "create",
        "portfolio",
        missing.to_str().expect("manifest path"),
        "--out",
        TempOut::new("pdf").arg(),
    ]);
    assert!(!no_file.status.success());
    assert!(
        stderr(&no_file).contains("read portfolio file"),
        "{}",
        stderr(&no_file)
    );

    let malformed = dir.path.join("malformed.json");
    fs::write(&malformed, b"not json").expect("write malformed manifest");
    assert!(
        !run(&[
            "create",
            "portfolio",
            malformed.to_str().expect("manifest path"),
            "--out",
            TempOut::new("pdf").arg()
        ])
        .status
        .success()
    );
}

#[test]
fn create_office_converts_pages_and_verifies_text() {
    let bundle = TempPath::new("json");
    bundle.write(
        br#"{"kind":"word","title":"Quarterly Report","markup":"<h1>Quarterly Report</h1><p>Hello PDFXI.</p><table><tr><td>Region</td><td>Sales</td></tr></table><!--pdfxi:page--><p>Second page body.</p>"}"#,
    );
    let out = TempOut::new("pdf");
    let created = run(&[
        "create",
        "office",
        bundle.arg(),
        "--out",
        out.arg(),
        "--json",
    ]);
    assert!(created.status.success(), "stderr: {}", stderr(&created));
    let value: Value = serde_json::from_str(&stdout(&created)).expect("valid JSON");
    assert_eq!(value["kind"], "Word");
    assert_eq!(value["title"], "Quarterly Report");
    assert_eq!(value["output_name"], "quarterly_report.docx");
    assert_eq!(value["model_pages"], 2);
    assert_eq!(value["model_blocks"], 4);
    assert_eq!(value["pdf_pages"], 2);
    assert_eq!(value["expected_lines"], 4);
    assert_eq!(value["verified_lines"], 4);
    assert_eq!(value["strict"], false);
    assert!(
        value["unsupported"]
            .as_array()
            .expect("unsupported")
            .is_empty()
    );

    let bytes = fs::read(&out.path).expect("office PDF");
    let text = String::from_utf8_lossy(&bytes);
    assert!(text.contains("Quarterly Report"), "{text}");
    let extracted = run(&["text", out.arg()]);
    assert!(extracted.status.success(), "stderr: {}", stderr(&extracted));
    let text = stdout(&extracted);
    assert!(text.contains("Hello PDFXI."), "{text}");
    assert!(text.contains("Region | Sales"), "{text}");
    assert!(text.contains("Second page body."), "{text}");

    let again = TempOut::new("pdf");
    let repeated = run(&["create", "office", bundle.arg(), "--out", again.arg()]);
    assert!(repeated.status.success(), "stderr: {}", stderr(&repeated));
    assert_eq!(
        bytes,
        fs::read(&again.path).expect("second office PDF"),
        "Office conversion is deterministic"
    );
}

#[test]
fn create_office_reports_or_rejects_unsupported_bundles() {
    let unsupported = TempPath::new("json");
    unsupported
        .write(br#"{"kind":"excel","title":"Book","markup":"<p>Cell</p><img src=\"x.png\"/>"}"#);

    let strict = run(&[
        "create",
        "office",
        unsupported.arg(),
        "--out",
        TempOut::new("pdf").arg(),
        "--strict",
    ]);
    assert!(!strict.status.success());
    assert!(
        stderr(&strict).contains("unsupported Office construct"),
        "{}",
        stderr(&strict)
    );

    let out = TempOut::new("pdf");
    let reported = run(&[
        "create",
        "office",
        unsupported.arg(),
        "--out",
        out.arg(),
        "--json",
    ]);
    assert!(reported.status.success(), "stderr: {}", stderr(&reported));
    let value: Value = serde_json::from_str(&stdout(&reported)).expect("valid JSON");
    assert_eq!(value["kind"], "Excel");
    assert_eq!(value["model_pages"], 1);
    assert_eq!(value["unsupported"][0], "media");
    assert_eq!(value["verified_lines"], 1);

    let empty_title = TempPath::new("json");
    empty_title.write(br#"{"kind":"ppt","title":"","markup":"<p>x</p>"}"#);
    let bad_title = run(&[
        "create",
        "office",
        empty_title.arg(),
        "--out",
        TempOut::new("pdf").arg(),
    ]);
    assert!(!bad_title.status.success());
    assert!(
        stderr(&bad_title).contains("title must not be empty"),
        "{}",
        stderr(&bad_title)
    );

    let unknown = TempPath::new("json");
    unknown.write(br#"{"kind":"visio","title":"T","markup":"<p>x</p>"}"#);
    let bad_kind = run(&[
        "create",
        "office",
        unknown.arg(),
        "--out",
        TempOut::new("pdf").arg(),
    ]);
    assert!(!bad_kind.status.success());
    assert!(
        stderr(&bad_kind).contains("unknown Office kind"),
        "{}",
        stderr(&bad_kind)
    );

    let malformed = TempPath::new("json");
    malformed.write(b"not json");
    assert!(
        !run(&[
            "create",
            "office",
            malformed.arg(),
            "--out",
            TempOut::new("pdf").arg()
        ])
        .status
        .success()
    );

    let missing = run(&[
        "create",
        "office",
        TempPath::new("json").arg(),
        "--out",
        TempOut::new("pdf").arg(),
    ]);
    assert!(!missing.status.success());
}

#[test]
fn create_ocr_overlays_invisible_searchable_text() {
    let image = TempPath::new("png");
    image.write(&sample_png());
    let words = TempPath::new("json");
    words.write(
        br#"{"language":"eng","words":[
            {"text":"Hi","x":0,"y":0,"width":2,"height":2,"confidence":95},
            {"text":"Low","x":0,"y":0,"width":2,"height":2,"confidence":10}
        ]}"#,
    );
    let out = TempOut::new("pdf");
    let created = run(&[
        "create",
        "ocr",
        image.arg(),
        "--out",
        out.arg(),
        "--words",
        words.arg(),
        "--dpi",
        "72",
        "--json",
    ]);
    assert!(created.status.success(), "stderr: {}", stderr(&created));
    let value: Value = serde_json::from_str(&stdout(&created)).expect("valid JSON");
    assert_eq!(value["engine"], "json-words");
    assert_eq!(value["words_received"], 2);
    assert_eq!(value["kept"], 1);
    assert_eq!(value["rejected"], 1);
    assert_eq!(value["suspect"], 0);
    assert_eq!(value["pages"], 1);
    assert_eq!(value["page_width"], 2.0);
    assert_eq!(value["page_height"], 2.0);
    assert_eq!(value["verified_words"], 1);
    assert_eq!(value["extracted_chars"], 2);

    let bytes = fs::read(&out.path).expect("ocr PDF");
    let text = String::from_utf8_lossy(&bytes);
    assert!(text.contains("3 Tr"), "the layer must be invisible: {text}");
    assert!(text.contains("(Hi) Tj"), "{text}");
    assert!(text.contains("/FOcr"), "{text}");

    let extracted = run(&["text", out.arg()]);
    assert!(extracted.status.success(), "stderr: {}", stderr(&extracted));
    assert_eq!(stdout(&extracted).trim(), "Hi");

    let again = TempOut::new("pdf");
    let repeated = run(&[
        "create",
        "ocr",
        image.arg(),
        "--out",
        again.arg(),
        "--words",
        words.arg(),
        "--dpi",
        "72",
    ]);
    assert!(repeated.status.success(), "stderr: {}", stderr(&repeated));
    assert_eq!(
        bytes,
        fs::read(&again.path).expect("second OCR PDF"),
        "OCR overlay is deterministic"
    );
}

#[test]
fn create_ocr_rejects_bad_words_and_inputs() {
    let image = TempPath::new("png");
    image.write(&sample_png());

    let empty = TempPath::new("json");
    empty.write(br#"{"words":[]}"#);
    let no_words = run(&[
        "create",
        "ocr",
        image.arg(),
        "--out",
        TempOut::new("pdf").arg(),
        "--words",
        empty.arg(),
    ]);
    assert!(!no_words.status.success());
    assert!(
        stderr(&no_words).contains("contains no words"),
        "{}",
        stderr(&no_words)
    );

    let blank = TempPath::new("json");
    blank.write(br#"{"words":[{"text":" ","x":0,"y":0,"width":1,"height":1,"confidence":90}]}"#);
    let empty_text = run(&[
        "create",
        "ocr",
        image.arg(),
        "--out",
        TempOut::new("pdf").arg(),
        "--words",
        blank.arg(),
    ]);
    assert!(!empty_text.status.success());
    assert!(
        stderr(&empty_text).contains("empty text"),
        "{}",
        stderr(&empty_text)
    );

    let negative = TempPath::new("json");
    negative
        .write(br#"{"words":[{"text":"x","x":0,"y":0,"width":-1,"height":1,"confidence":90}]}"#);
    let bad_box = run(&[
        "create",
        "ocr",
        image.arg(),
        "--out",
        TempOut::new("pdf").arg(),
        "--words",
        negative.arg(),
    ]);
    assert!(!bad_box.status.success());
    assert!(
        stderr(&bad_box).contains("box must be finite"),
        "{}",
        stderr(&bad_box)
    );

    let malformed = TempPath::new("json");
    malformed.write(b"not json");
    assert!(
        !run(&[
            "create",
            "ocr",
            image.arg(),
            "--out",
            TempOut::new("pdf").arg(),
            "--words",
            malformed.arg()
        ])
        .status
        .success()
    );

    let words = TempPath::new("json");
    words.write(br#"{"words":[{"text":"Hi","x":0,"y":0,"width":2,"height":2,"confidence":90}]}"#);
    let blank_language = run(&[
        "create",
        "ocr",
        image.arg(),
        "--out",
        TempOut::new("pdf").arg(),
        "--words",
        words.arg(),
        "--language",
        " ",
    ]);
    assert!(!blank_language.status.success());
    assert!(
        stderr(&blank_language).contains("language tags must not be blank"),
        "{}",
        stderr(&blank_language)
    );

    let zero_dpi = run(&[
        "create",
        "ocr",
        image.arg(),
        "--out",
        TempOut::new("pdf").arg(),
        "--words",
        words.arg(),
        "--dpi",
        "0",
    ]);
    assert!(!zero_dpi.status.success());

    let gif = TempPath::new("gif");
    gif.write(b"GIF89a");
    let unsupported = run(&[
        "create",
        "ocr",
        gif.arg(),
        "--out",
        TempOut::new("pdf").arg(),
        "--words",
        words.arg(),
    ]);
    assert!(!unsupported.status.success());
    assert!(
        stderr(&unsupported).contains("expected png, jpg or jpeg"),
        "{}",
        stderr(&unsupported)
    );

    let missing = run(&[
        "create",
        "ocr",
        TempPath::new("png").arg(),
        "--out",
        TempOut::new("pdf").arg(),
        "--words",
        words.arg(),
    ]);
    assert!(!missing.status.success());
}

#[test]
fn create_scan_assembles_simplex_png_pages() {
    let first = TempPath::new("png");
    first.write(&sample_png());
    let second = TempPath::new("png");
    second.write(&sample_png());
    let out = TempOut::new("pdf");
    let created = run(&[
        "create",
        "scan",
        "--out",
        out.arg(),
        "--scan",
        first.arg(),
        "--scan",
        second.arg(),
        "--dpi",
        "72",
        "--json",
    ]);
    assert!(created.status.success(), "stderr: {}", stderr(&created));
    let value: Value = serde_json::from_str(&stdout(&created)).expect("valid JSON");
    assert_eq!(value["pages"], 2);
    assert_eq!(value["front"], 2);
    assert_eq!(value["back"], 0);
    assert_eq!(value["duplex"], "simplex");
    assert_eq!(value["dpi"], 72.0);
    assert_eq!(value["verified_pages"], 2);
    let rows = value["rows"].as_array().expect("rows");
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["sheet"], 1);
    assert_eq!(rows[0]["side"], "front");
    assert_eq!(rows[0]["format"], "png");
    assert_eq!(rows[0]["width_px"], 2);
    assert_eq!(rows[0]["height_px"], 2);
    assert_eq!(rows[0]["page_width"], 2.0);
    assert_eq!(rows[0]["page_height"], 2.0);
    assert_eq!(rows[1]["sheet"], 2);

    let bytes = fs::read(&out.path).expect("scan PDF");
    let document = pdfxi_core::Document::from_bytes(bytes.clone()).expect("core parses scan PDF");
    assert_eq!(document.page_count().expect("page count"), 2);
    for index in 0..2 {
        let page = document.page(index).expect("page");
        assert!((page.media_box.width() - 2.0).abs() < 0.01);
        assert!((page.media_box.height() - 2.0).abs() < 0.01);
    }

    let again = TempOut::new("pdf");
    let repeated = run(&[
        "create",
        "scan",
        "--out",
        again.arg(),
        "--scan",
        first.arg(),
        "--scan",
        second.arg(),
        "--dpi",
        "72",
    ]);
    assert!(repeated.status.success(), "stderr: {}", stderr(&repeated));
    assert_eq!(
        bytes,
        fs::read(&again.path).expect("second scan PDF"),
        "scan assembly is deterministic"
    );
}

#[test]
fn create_scan_orders_duplex_and_rejects_bad_inputs() {
    let images: Vec<TempPath> = (0..4)
        .map(|_| {
            let path = TempPath::new("png");
            path.write(&sample_png());
            path
        })
        .collect();
    let mut args: Vec<String> = vec![
        "create".to_owned(),
        "scan".to_owned(),
        "--out".to_owned(),
        TempOut::new("pdf").arg().to_owned(),
    ];
    for image in &images {
        args.push("--scan".to_owned());
        args.push(image.arg().to_owned());
    }
    args.push("--dpi".to_owned());
    args.push("72".to_owned());
    args.push("--duplex".to_owned());
    args.push("front-first".to_owned());
    args.push("--json".to_owned());
    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let front_first = run(&arg_refs);
    assert!(
        front_first.status.success(),
        "stderr: {}",
        stderr(&front_first)
    );
    let value: Value = serde_json::from_str(&stdout(&front_first)).expect("valid JSON");
    assert_eq!(value["duplex"], "duplex-front-first");
    assert_eq!(value["pages"], 4);
    assert_eq!(value["front"], 2);
    assert_eq!(value["back"], 2);
    let rows = value["rows"].as_array().expect("rows");
    let sides: Vec<(&str, u64)> = rows
        .iter()
        .map(|row| {
            (
                row["side"].as_str().unwrap_or(""),
                row["sheet"].as_u64().unwrap_or(0),
            )
        })
        .collect();
    assert_eq!(
        sides,
        [("front", 1), ("back", 1), ("front", 2), ("back", 2)]
    );

    let back_out = TempOut::new("pdf");
    let back_first = run(&[
        "create",
        "scan",
        "--out",
        back_out.arg(),
        "--scan",
        images[0].arg(),
        "--scan",
        images[1].arg(),
        "--dpi",
        "72",
        "--duplex",
        "back-first",
        "--json",
    ]);
    assert!(
        back_first.status.success(),
        "stderr: {}",
        stderr(&back_first)
    );
    let value: Value = serde_json::from_str(&stdout(&back_first)).expect("valid JSON");
    let rows = value["rows"].as_array().expect("rows");
    assert_eq!(rows[0]["side"], "back");
    assert_eq!(rows[1]["side"], "front");

    let odd = run(&[
        "create",
        "scan",
        "--out",
        TempOut::new("pdf").arg(),
        "--scan",
        images[0].arg(),
        "--dpi",
        "72",
        "--duplex",
        "front-first",
    ]);
    assert!(!odd.status.success(), "a duplex job needs complete pairs");

    let jpeg = TempPath::new("jpg");
    jpeg.write(&sample_jpeg());
    let unsupported = run(&[
        "create",
        "scan",
        "--out",
        TempOut::new("pdf").arg(),
        "--scan",
        jpeg.arg(),
    ]);
    assert!(!unsupported.status.success());
    assert!(
        stderr(&unsupported).contains("expected png"),
        "{}",
        stderr(&unsupported)
    );

    let malformed = TempPath::new("png");
    malformed.write(b"not a png");
    let bad_header = run(&[
        "create",
        "scan",
        "--out",
        TempOut::new("pdf").arg(),
        "--scan",
        malformed.arg(),
    ]);
    assert!(!bad_header.status.success());
    assert!(
        stderr(&bad_header).contains("not a PNG"),
        "{}",
        stderr(&bad_header)
    );

    let low_dpi = run(&[
        "create",
        "scan",
        "--out",
        TempOut::new("pdf").arg(),
        "--scan",
        images[0].arg(),
        "--dpi",
        "10",
    ]);
    assert!(!low_dpi.status.success());

    let no_scan = run(&["create", "scan", "--out", TempOut::new("pdf").arg()]);
    assert!(!no_scan.status.success());

    let missing = run(&[
        "create",
        "scan",
        "--out",
        TempOut::new("pdf").arg(),
        "--scan",
        TempPath::new("png").arg(),
    ]);
    assert!(!missing.status.success());
}

#[test]
fn plugin_plan_reports_capability_and_budget_gates() {
    let manifest = TempPath::new("json");
    manifest.write(
        br#"{"id":"text.plugin","name":"Text helper","version":"1.0.0","api_version":1,"capabilities":["document-text"]}"#,
    );
    let path = manifest.arg();

    let allowed = run(&[
        "plugin",
        "plan",
        path,
        "--operation",
        "extract",
        "--grant",
        "document-text",
        "--json",
    ]);
    assert!(allowed.status.success(), "stderr: {}", stderr(&allowed));
    let value: Value = serde_json::from_str(&stdout(&allowed)).expect("valid JSON");
    assert_eq!(value["plugin_id"], "text.plugin");
    assert_eq!(value["name"], "Text helper");
    assert_eq!(value["api_version"], 1);
    assert_eq!(value["capabilities"], serde_json::json!(["document-text"]));
    assert_eq!(value["operation"], "extract");
    assert_eq!(value["expected"], "text");
    assert_eq!(value["allowed"], true);
    let steps = value["steps"].as_array().expect("steps");
    assert_eq!(steps.len(), 6);
    assert_eq!(steps[0]["kind"], "registration");
    assert_eq!(steps[5]["kind"], "execute");
    assert!(steps.iter().all(|step| step["allowed"] == true));
    assert!(
        value["summary"]
            .as_str()
            .unwrap_or("")
            .contains("execute:allow"),
        "{}",
        value["summary"]
    );

    let repeated = run(&[
        "plugin",
        "plan",
        path,
        "--operation",
        "extract",
        "--grant",
        "document-text",
        "--json",
    ]);
    assert_eq!(stdout(&allowed), stdout(&repeated), "plan is deterministic");

    let denied = run(&["plugin", "plan", path, "--operation", "extract", "--json"]);
    assert_eq!(denied.status.code(), Some(2), "denied plans exit 2");
    let value: Value = serde_json::from_str(&stdout(&denied)).expect("valid JSON");
    assert_eq!(value["allowed"], false);
    let steps = value["steps"].as_array().expect("steps");
    let capabilities = steps
        .iter()
        .find(|step| step["kind"] == "capabilities")
        .expect("capabilities step");
    assert_eq!(capabilities["allowed"], false);
    assert_eq!(
        capabilities["codes"],
        serde_json::json!(["capability_not_granted"])
    );
    assert!(
        stdout(&denied).contains("upstream_denied"),
        "{}",
        stdout(&denied)
    );

    let budget = run(&[
        "plugin",
        "plan",
        path,
        "--operation",
        "extract",
        "--grant",
        "document-text",
        "--max-steps",
        "10000001",
    ]);
    assert!(!budget.status.success());
    assert!(
        stderr(&budget).contains("invocation budget"),
        "{}",
        stderr(&budget)
    );

    let output_budget = run(&[
        "plugin",
        "plan",
        path,
        "--operation",
        "extract",
        "--grant",
        "document-text",
        "--max-text-bytes",
        "0",
        "--json",
    ]);
    assert_eq!(output_budget.status.code(), Some(2));
    let value: Value = serde_json::from_str(&stdout(&output_budget)).expect("valid JSON");
    assert_eq!(value["allowed"], false);
    assert!(
        stdout(&output_budget).contains("invalid_budget"),
        "{}",
        stdout(&output_budget)
    );

    let forbidden = TempPath::new("json");
    forbidden.write(
        br#"{"id":"bad.plugin","name":"Bad","version":"1.0.0","api_version":1,"capabilities":["filesystem"]}"#,
    );
    let rejected = run(&["plugin", "plan", forbidden.arg(), "--operation", "extract"]);
    assert!(!rejected.status.success());
    assert!(
        stderr(&rejected).contains("invalid manifest"),
        "{}",
        stderr(&rejected)
    );

    let broken = TempPath::new("json");
    broken.write(b"not json");
    assert!(
        !run(&["plugin", "plan", broken.arg(), "--operation", "x"])
            .status
            .success()
    );
    assert!(
        !run(&[
            "plugin",
            "plan",
            TempPath::new("json").arg(),
            "--operation",
            "x"
        ])
        .status
        .success()
    );
}

#[test]
fn plugin_output_validates_types_and_budgets() {
    let text = run(&[
        "plugin", "output", "--type", "text", "--text", "hello", "--json",
    ]);
    assert!(text.status.success(), "stderr: {}", stderr(&text));
    let value: Value = serde_json::from_str(&stdout(&text)).expect("valid JSON");
    assert_eq!(value["ty"], "text");
    assert_eq!(value["expected"], "text");
    assert_eq!(value["encoded_len"], 5);
    assert_eq!(value["value"], "hello");
    assert_eq!(value["summary"], "host.plugin\toutput\ttext");
    assert_eq!(value["valid"], true);

    let int = run(&["plugin", "output", "--type", "int", "--int", "-3"]);
    assert!(int.status.success(), "stderr: {}", stderr(&int));
    assert!(stdout(&int).contains("value: -3"), "{}", stdout(&int));

    let boolean = run(&[
        "plugin", "output", "--type", "bool", "--bool", "true", "--json",
    ]);
    assert!(boolean.status.success(), "stderr: {}", stderr(&boolean));
    let value: Value = serde_json::from_str(&stdout(&boolean)).expect("valid JSON");
    assert_eq!(value["value"], "true");
    assert_eq!(value["encoded_len"], 5);

    let bytes = run(&[
        "plugin",
        "output",
        "--type",
        "bytes",
        "--bytes-len",
        "16",
        "--json",
    ]);
    assert!(bytes.status.success(), "stderr: {}", stderr(&bytes));
    let value: Value = serde_json::from_str(&stdout(&bytes)).expect("valid JSON");
    assert_eq!(value["value"], "bytes(16)");
    assert_eq!(value["encoded_len"], 16);

    let null = run(&["plugin", "output", "--type", "null"]);
    assert!(null.status.success(), "stderr: {}", stderr(&null));

    let mismatch = run(&[
        "plugin", "output", "--type", "text", "--text", "hello", "--expect", "int",
    ]);
    assert!(!mismatch.status.success());
    assert!(
        stderr(&mismatch).contains("does not match expected type"),
        "{}",
        stderr(&mismatch)
    );

    let too_long = run(&["plugin", "output", "--type", "text", "--text-len", "70000"]);
    assert!(!too_long.status.success());
    assert!(
        stderr(&too_long).contains("maximum is 65536"),
        "{}",
        stderr(&too_long)
    );

    let bytes_long = run(&[
        "plugin",
        "output",
        "--type",
        "bytes",
        "--bytes-len",
        "1048577",
    ]);
    assert!(!bytes_long.status.success());

    let empty = run(&["plugin", "output", "--type", "text", "--text", ""]);
    assert!(!empty.status.success());

    let empty_id = run(&[
        "plugin",
        "output",
        "--type",
        "text",
        "--text",
        "x",
        "--plugin-id",
        " ",
    ]);
    assert!(!empty_id.status.success());

    let missing_value = run(&["plugin", "output", "--type", "int"]);
    assert!(!missing_value.status.success());
}

#[test]
fn toolset_lists_validates_and_shows_tools() {
    let dir = TempDir::new();
    fs::write(
        dir.child("actions.txt"),
        b"ACTION\tnightly\tNightly staging\nSTEP\tload\timport\nSTEP\tsave\texport\n\nACTION\tweekly\tWeekly\nSTEP\tload\timport\n",
    )
    .expect("write actions");
    fs::write(
        dir.child("tools.json"),
        br#"{
  "version": 1,
  "name": "maintenance",
  "tools": {
    "daily": { "file": "actions.txt", "action": "nightly", "defaults": { "target": "daily.pdf" } },
    "weekly": { "file": "actions.txt", "action": "weekly" }
  }
}"#,
    )
    .expect("write toolset");
    let file = dir.child("tools.json");
    let file_arg = file.to_str().expect("utf-8");

    let listed = run(&["toolset", "list", file_arg, "--json"]);
    assert!(listed.status.success(), "stderr: {}", stderr(&listed));
    let value: Value = serde_json::from_str(&stdout(&listed)).expect("valid JSON");
    assert_eq!(value["name"], "maintenance");
    assert_eq!(value["count"], 2);
    let tools = value["tools"].as_array().expect("tools");
    let aliases: Vec<&str> = tools
        .iter()
        .filter_map(|tool| tool["alias"].as_str())
        .collect();
    assert_eq!(aliases, ["daily", "weekly"]);
    assert_eq!(tools[0]["action"], "nightly");
    assert_eq!(tools[0]["defaults"]["target"], "daily.pdf");
    assert_eq!(tools[1]["defaults"], serde_json::json!({}));

    let text = run(&["toolset", "list", file_arg]);
    assert!(text.status.success(), "stderr: {}", stderr(&text));
    assert!(stdout(&text).contains("tools: 2"), "{}", stdout(&text));
    assert!(
        stdout(&text).contains("defaults: target=daily.pdf"),
        "{}",
        stdout(&text)
    );

    let validated = run(&["toolset", "validate", file_arg, "--json"]);
    assert!(validated.status.success(), "stderr: {}", stderr(&validated));
    let value: Value = serde_json::from_str(&stdout(&validated)).expect("valid JSON");
    assert_eq!(value["valid"], true);
    assert_eq!(value["tools"], 2);

    let shown = run(&["toolset", "show", file_arg, "daily", "--json"]);
    assert!(shown.status.success(), "stderr: {}", stderr(&shown));
    let value: Value = serde_json::from_str(&stdout(&shown)).expect("valid JSON");
    assert_eq!(value["alias"], "daily");
    assert_eq!(value["file"], "actions.txt");
    assert_eq!(value["action"], "nightly");
    assert_eq!(value["defaults"]["target"], "daily.pdf");

    let unknown = run(&["toolset", "show", file_arg, "nope"]);
    assert!(!unknown.status.success());
    assert!(
        stderr(&unknown).contains("available tools: daily, weekly"),
        "{}",
        stderr(&unknown)
    );

    fs::write(dir.child("broken.json"), b"not json").expect("write broken");
    let broken = run(&[
        "toolset",
        "validate",
        dir.child("broken.json").to_str().expect("utf-8"),
    ]);
    assert!(!broken.status.success());

    fs::write(
        dir.child("dangerous.json"),
        br#"{"version":1,"name":"bad","tools":{"shell":{"file":"actions.txt","action":"nightly"}}}"#,
    )
    .expect("write dangerous");
    let dangerous = run(&[
        "toolset",
        "validate",
        dir.child("dangerous.json").to_str().expect("utf-8"),
    ]);
    assert!(!dangerous.status.success());
    assert!(
        stderr(&dangerous).contains("dangerous"),
        "{}",
        stderr(&dangerous)
    );
}

#[test]
fn toolset_run_applies_defaults_and_rejects_dangerous_actions() {
    let dir = TempDir::new();
    fs::write(
        dir.child("actions.txt"),
        b"ACTION\tnightly\tNightly staging\nSTEP\tload\timport\nSTEP\tsave\texport\n\nACTION\tunsafe\tUnsafe\nSTEP\tsec\tsecurity\nPARAM\tpassword\tsecret\n",
    )
    .expect("write actions");
    fs::write(
        dir.child("tools.json"),
        br#"{
  "version": 1,
  "name": "maintenance",
  "tools": {
    "daily": { "file": "actions.txt", "action": "nightly", "defaults": { "target": "daily.pdf" } },
    "danger": { "file": "actions.txt", "action": "unsafe" },
    "escape": { "file": "../outside.txt", "action": "nightly" }
  }
}"#,
    )
    .expect("write toolset");
    let file = dir.child("tools.json");
    let file_arg = file.to_str().expect("utf-8");
    let out = TempDir::new();
    let input = root_fixture("minimal.pdf");

    let run_json = run(&[
        "toolset",
        "run",
        file_arg,
        "daily",
        "--input",
        input.to_str().expect("utf-8"),
        "--output-dir",
        out.arg(),
        "--json",
    ]);
    assert!(run_json.status.success(), "stderr: {}", stderr(&run_json));
    let value: Value = serde_json::from_str(&stdout(&run_json)).expect("valid JSON");
    assert_eq!(value["action"], "nightly");
    let jobs = value["jobs"].as_array().expect("jobs");
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0]["state"], "succeeded");
    let steps = jobs[0]["outcome"]["steps"].as_array().expect("steps");
    assert_eq!(steps.len(), 2);
    let save = steps
        .iter()
        .find(|step| step["step_id"] == "save")
        .expect("save step");
    assert_eq!(save["artifact_kind"], "path");
    let artifact = save["artifact"].as_str().unwrap_or("");
    assert!(
        artifact.ends_with("daily.pdf"),
        "the tool default supplies the export target: {artifact}"
    );

    let run_text = run(&[
        "toolset",
        "run",
        file_arg,
        "daily",
        "--input",
        input.to_str().expect("utf-8"),
    ]);
    assert!(run_text.status.success(), "stderr: {}", stderr(&run_text));
    assert!(
        stdout(&run_text).contains("succeeded"),
        "{}",
        stdout(&run_text)
    );

    let danger = run(&[
        "toolset",
        "run",
        file_arg,
        "danger",
        "--input",
        input.to_str().expect("utf-8"),
    ]);
    assert!(!danger.status.success());
    assert!(
        stderr(&danger).contains("dangerous operation"),
        "{}",
        stderr(&danger)
    );

    let escaped = run(&[
        "toolset",
        "run",
        file_arg,
        "escape",
        "--input",
        input.to_str().expect("utf-8"),
    ]);
    assert!(!escaped.status.success());
    assert!(
        stderr(&escaped).contains("escapes the toolset directory"),
        "{}",
        stderr(&escaped)
    );

    let missing_input = run(&[
        "toolset",
        "run",
        file_arg,
        "daily",
        "--input",
        TempPath::new("pdf").arg(),
    ]);
    assert!(!missing_input.status.success());

    let no_inputs = run(&["toolset", "run", file_arg, "daily"]);
    assert!(!no_inputs.status.success(), "--input is required");
}

#[test]
fn watch_processes_files_and_persists_markers() {
    let watched = TempDir::new();
    let out = TempDir::new();
    let markers = TempPath::new("json");
    fs::write(watched.child("a.pdf"), b"AAA").expect("write a");
    fs::write(watched.child("b.pdf"), b"BBBB").expect("write b");
    fs::write(watched.child("notes.txt"), b"note").expect("write txt");

    let first = run(&[
        "watch",
        watched.arg(),
        "--out",
        out.arg(),
        "--markers",
        markers.arg(),
        "--json",
    ]);
    assert!(first.status.success(), "stderr: {}", stderr(&first));
    let value: Value = serde_json::from_str(&stdout(&first)).expect("valid JSON");
    assert_eq!(value["extensions"], serde_json::json!(["pdf"]));
    assert_eq!(value["suffix"], "pdf");
    assert_eq!(value["ticks"], 1);
    assert_eq!(value["polls"], 1);
    assert_eq!(value["written"], 2);
    assert_eq!(value["marker_count"], 2);
    assert_eq!(value["decisions"][0]["decision"], "due");
    let reports = value["reports"].as_array().expect("reports");
    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0]["converted"], 2);
    assert_eq!(reports[0]["skipped"], 0);
    assert_eq!(reports[0]["unsupported"], 1);
    assert_eq!(reports[0]["failed"], 0);
    let files = reports[0]["files"].as_array().expect("files");
    let states: Vec<&str> = files
        .iter()
        .filter_map(|file| file["state"].as_str())
        .collect();
    assert_eq!(states, ["converted", "converted", "unsupported"]);

    assert_eq!(
        fs::read(out.child("a.pdf")).expect("a output"),
        b"AAA".to_vec()
    );
    assert_eq!(
        fs::read(out.child("b.pdf")).expect("b output"),
        b"BBBB".to_vec()
    );
    assert!(!out.child("notes.pdf").exists());
    let snapshot = fs::read_to_string(&markers.path).expect("markers");
    assert!(snapshot.contains("\"a.pdf\""), "{snapshot}");
    assert!(snapshot.contains("\"b.pdf\""), "{snapshot}");

    let second = run(&[
        "watch",
        watched.arg(),
        "--out",
        out.arg(),
        "--markers",
        markers.arg(),
        "--json",
    ]);
    assert!(second.status.success(), "stderr: {}", stderr(&second));
    let value: Value = serde_json::from_str(&stdout(&second)).expect("valid JSON");
    assert_eq!(value["written"], 0);
    assert_eq!(value["marker_count"], 2);
    assert_eq!(value["reports"][0]["converted"], 0);
    assert_eq!(value["reports"][0]["skipped"], 2);
    assert_eq!(value["reports"][0]["unsupported"], 1);
    assert_eq!(
        fs::read_to_string(&markers.path).expect("markers again"),
        snapshot,
        "unchanged markers are byte-identical"
    );

    let scheduled = run(&[
        "watch",
        watched.arg(),
        "--out",
        out.arg(),
        "--markers",
        markers.arg(),
        "--poll-every",
        "2",
        "--ticks",
        "3",
        "--json",
    ]);
    assert!(scheduled.status.success(), "stderr: {}", stderr(&scheduled));
    let value: Value = serde_json::from_str(&stdout(&scheduled)).expect("valid JSON");
    assert_eq!(value["ticks"], 3);
    assert_eq!(value["polls"], 1);
    let decisions = value["decisions"].as_array().expect("decisions");
    assert_eq!(decisions.len(), 3);
    assert_eq!(decisions[0]["decision"], "wait");
    assert_eq!(decisions[0]["wait_ticks"], 1);
    assert_eq!(decisions[1]["decision"], "due");
    assert_eq!(decisions[2]["decision"], "wait");
    assert!(
        value["schedule"]
            .as_str()
            .unwrap_or("")
            .contains("interval=every_ticks:2"),
        "{}",
        value["schedule"]
    );
}

#[test]
fn watch_rejects_unsafe_or_invalid_setups() {
    let watched = TempDir::new();
    let out = TempDir::new();
    fs::write(watched.child("a.pdf"), b"AAA").expect("write a");

    let missing = run(&["watch", TempPath::new("dir").arg(), "--out", out.arg()]);
    assert!(!missing.status.success());

    let inside = watched.child("inside");
    fs::create_dir_all(&inside).expect("create nested out");
    let inside_arg = inside.to_str().expect("utf-8");
    let rejected = run(&["watch", watched.arg(), "--out", inside_arg]);
    assert!(!rejected.status.success());
    assert!(
        stderr(&rejected).contains("inside the watched folder"),
        "{}",
        stderr(&rejected)
    );

    let zero_ticks = run(&["watch", watched.arg(), "--out", out.arg(), "--ticks", "0"]);
    assert!(!zero_ticks.status.success());

    let zero_base = run(&[
        "watch",
        watched.arg(),
        "--out",
        out.arg(),
        "--base-ticks",
        "0",
    ]);
    assert!(!zero_base.status.success());
    assert!(
        stderr(&zero_base).contains("base_ticks must be positive"),
        "{}",
        stderr(&zero_base)
    );

    let bad_multiplier = run(&[
        "watch",
        watched.arg(),
        "--out",
        out.arg(),
        "--multiplier",
        "999",
    ]);
    assert!(!bad_multiplier.status.success());

    let bad_ext = run(&["watch", watched.arg(), "--out", out.arg(), "--ext", "p df"]);
    assert!(!bad_ext.status.success());

    let markers = TempPath::new("json");
    markers.write(b"not a snapshot");
    let bad_markers = run(&[
        "watch",
        watched.arg(),
        "--out",
        out.arg(),
        "--markers",
        markers.arg(),
    ]);
    assert!(!bad_markers.status.success());
    assert!(
        stderr(&bad_markers).contains("parse markers"),
        "{}",
        stderr(&bad_markers)
    );
}

#[test]
fn measure_distance_area_and_perimeter_on_real_pages() {
    let page = root_fixture("minimal.pdf");
    let distance = run(&[
        "measure",
        page.to_str().expect("utf-8"),
        "--points",
        "0,0;3,4",
        "--json",
    ]);
    assert!(distance.status.success(), "stderr: {}", stderr(&distance));
    let value: Value = serde_json::from_str(&stdout(&distance)).expect("valid JSON");
    assert_eq!(value["page"], 1);
    assert_eq!(
        value["page_box"],
        serde_json::json!([0.0, 0.0, 612.0, 792.0])
    );
    assert_eq!(value["points"], 2);
    assert_eq!(value["unit"], "pt");
    assert_eq!(value["units_per_point"], 1.0);
    let results = value["results"].as_array().expect("results");
    assert_eq!(results.len(), 1);
    assert_eq!(results[0]["kind"], "distance");
    assert_eq!(results[0]["value"], 5.0);
    assert_eq!(results[0]["unit"], "pt");
    assert_eq!(results[0]["summary"], "distance 5.00 pt");
    assert_eq!(results[0]["commands"], 2);

    let polygon = run(&[
        "measure",
        page.to_str().expect("utf-8"),
        "--points",
        "100,100;200,100;200,150",
        "--unit",
        "in",
        "--json",
    ]);
    assert!(polygon.status.success(), "stderr: {}", stderr(&polygon));
    let value: Value = serde_json::from_str(&stdout(&polygon)).expect("valid JSON");
    assert_eq!(value["points"], 3);
    assert_eq!(value["unit"], "in");
    let results = value["results"].as_array().expect("results");
    assert_eq!(results.len(), 2);
    assert_eq!(results[0]["kind"], "area");
    assert_eq!(results[0]["value"], 0.48);
    assert_eq!(results[0]["unit"], "in²");
    assert_eq!(results[0]["summary"], "area 0.48 in²");
    assert_eq!(results[0]["commands"], 4);
    assert_eq!(results[1]["kind"], "perimeter");
    assert_eq!(results[1]["value"], 3.64);
    assert_eq!(results[1]["summary"], "perimeter 3.64 in");

    let dpi = run(&[
        "measure",
        page.to_str().expect("utf-8"),
        "--points",
        "0,0;150,0",
        "--dpi",
        "150",
        "--json",
    ]);
    assert!(dpi.status.success(), "stderr: {}", stderr(&dpi));
    let value: Value = serde_json::from_str(&stdout(&dpi)).expect("valid JSON");
    assert_eq!(value["unit"], "in");
    assert_eq!(value["results"][0]["value"], 1.0);

    let slides = root_fixture("realworld/powerpoint-slides.pdf");
    let landscape = run(&[
        "measure",
        slides.to_str().expect("utf-8"),
        "--points",
        "0,0;100,0",
        "--unit",
        "cm",
        "--json",
    ]);
    assert!(landscape.status.success(), "stderr: {}", stderr(&landscape));
    let value: Value = serde_json::from_str(&stdout(&landscape)).expect("valid JSON");
    assert_eq!(value["results"][0]["value"], 3.53);
    let box_values = value["page_box"].as_array().expect("page box");
    assert_eq!(box_values.len(), 4);
    assert!(
        box_values[2].as_f64().unwrap_or(0.0) > box_values[3].as_f64().unwrap_or(0.0),
        "first slide is landscape: {}",
        stdout(&landscape)
    );
}

#[test]
fn measure_rejects_out_of_box_points_and_bad_options() {
    let page = root_fixture("minimal.pdf");
    let path = page.to_str().expect("utf-8");
    let outside = run(&["measure", path, "--points", "0,0;900,0"]);
    assert!(!outside.status.success());
    assert!(
        stderr(&outside).contains("outside the page box"),
        "{}",
        stderr(&outside)
    );

    let one = run(&["measure", path, "--points", "10,10"]);
    assert!(!one.status.success());
    assert!(
        stderr(&one).contains("at least two points"),
        "{}",
        stderr(&one)
    );

    let unit = run(&["measure", path, "--points", "0,0;1,1", "--unit", "parsec"]);
    assert!(!unit.status.success());
    assert!(
        stderr(&unit).contains("unknown --unit"),
        "{}",
        stderr(&unit)
    );

    let both = run(&[
        "measure", path, "--points", "0,0;1,1", "--unit", "in", "--dpi", "72",
    ]);
    assert!(!both.status.success());
    assert!(
        stderr(&both).contains("mutually exclusive"),
        "{}",
        stderr(&both)
    );

    let zero_dpi = run(&["measure", path, "--points", "0,0;1,1", "--dpi", "0"]);
    assert!(!zero_dpi.status.success());

    let decimals = run(&["measure", path, "--points", "0,0;1,1", "--decimals", "7"]);
    assert!(!decimals.status.success());
    assert!(
        stderr(&decimals).contains("between 0 and 6"),
        "{}",
        stderr(&decimals)
    );

    let zero_page = run(&["measure", path, "--points", "0,0;1,1", "--page", "0"]);
    assert!(!zero_page.status.success());

    let missing_page = run(&["measure", path, "--points", "0,0;1,1", "--page", "9"]);
    assert!(!missing_page.status.success());
    assert!(
        stderr(&missing_page).contains("does not exist"),
        "{}",
        stderr(&missing_page)
    );

    let missing_file = run(&["measure", TempPath::new("pdf").arg(), "--points", "0,0;1,1"]);
    assert!(!missing_file.status.success());
}

#[test]
fn create_postscript_writes_text_and_vector_pdf() {
    let source = TempPath::new("ps");
    source.write(
        b"%!PS-Adobe-3.0\n%%BoundingBox: 0 0 300 200\n%%Pages: 1\n%%Page: 1 1\n/F1 findfont 18 scalefont setfont\n20 150 moveto (Hello PostScript) show\n10 10 100 40 rectfill\n5 5 moveto 150 5 lineto 2 setlinewidth stroke\nshowpage\n%%Trailer\n",
    );
    let out = TempOut::new("pdf");
    let created = run(&["create", "postscript", source.arg(), out.arg(), "--json"]);
    assert!(created.status.success(), "stderr: {}", stderr(&created));
    let value: Value = serde_json::from_str(&stdout(&created)).expect("valid JSON");
    assert_eq!(value["kind"], "PS");
    assert_eq!(value["pages"], 1);
    assert_eq!(value["declared_pages"], 1);
    assert_eq!(value["bounding_box"], serde_json::json!([0, 0, 300, 200]));
    assert_eq!(value["text_runs"], 1);
    assert_eq!(value["lines"], 1);
    assert_eq!(value["rectangles"], 1);
    assert_eq!(
        value["unsupported"].as_array().expect("unsupported").len(),
        0
    );
    assert_eq!(value["clean"], true);
    assert_eq!(value["verified_pages"], 1);

    let info = run(&["info", out.arg()]);
    assert!(info.status.success(), "stderr: {}", stderr(&info));
    assert!(stdout(&info).contains("pages: 1"), "{}", stdout(&info));
    let text = run(&["text", out.arg()]);
    assert!(text.status.success(), "stderr: {}", stderr(&text));
    assert!(
        stdout(&text).contains("Hello PostScript"),
        "{}",
        stdout(&text)
    );

    let again = TempOut::new("pdf");
    let repeated = run(&["create", "postscript", source.arg(), again.arg()]);
    assert!(repeated.status.success(), "stderr: {}", stderr(&repeated));
    assert_eq!(
        fs::read(&out.path).expect("first"),
        fs::read(&again.path).expect("second"),
        "PostScript import is deterministic"
    );
    assert_ne!(
        fs::read(&source.path).expect("source"),
        fs::read(&out.path).expect("output")
    );
}

#[test]
fn create_postscript_handles_eps_pages_and_unsupported_constructs() {
    let eps = TempPath::new("eps");
    eps.write(
        b"%!PS-Adobe-3.0 EPSF-3.0\n%%BoundingBox: 0 0 200 100\n/Helvetica findfont 12 scalefont setfont\n10 40 moveto (EPS body) show\n",
    );
    let eps_out = TempOut::new("pdf");
    let created = run(&["create", "postscript", eps.arg(), eps_out.arg(), "--json"]);
    assert!(created.status.success(), "stderr: {}", stderr(&created));
    let value: Value = serde_json::from_str(&stdout(&created)).expect("valid JSON");
    assert_eq!(value["kind"], "EPS");
    assert_eq!(value["pages"], 1);
    assert_eq!(value["text_runs"], 1);
    let text = run(&["text", eps_out.arg()]);
    assert!(stdout(&text).contains("EPS body"), "{}", stdout(&text));

    let multipage = TempPath::new("ps");
    multipage.write(
        b"%!PS-Adobe-3.0\n%%BoundingBox: 0 0 200 100\n%%Pages: 2\n%%Page: one 1\n/Courier findfont 10 scalefont setfont\n10 50 moveto (first page) show\nshowpage\n%%Page: two 2\n10 50 moveto (second page) show\nshowpage\n%%Trailer\n",
    );
    let multi_out = TempOut::new("pdf");
    let created = run(&[
        "create",
        "postscript",
        multipage.arg(),
        multi_out.arg(),
        "--json",
    ]);
    assert!(created.status.success(), "stderr: {}", stderr(&created));
    let value: Value = serde_json::from_str(&stdout(&created)).expect("valid JSON");
    assert_eq!(value["pages"], 2);
    assert_eq!(value["declared_pages"], 2);
    assert_eq!(value["verified_pages"], 2);
    let info = run(&["info", multi_out.arg()]);
    assert!(stdout(&info).contains("pages: 2"), "{}", stdout(&info));

    let bad = TempPath::new("ps");
    bad.write(
        b"%!PS-Adobe-3.0\n%%BoundingBox: 0 0 100 100\nnewpath 10 10 moveto 50 50 80 10 0 360 arc fill\nshowpage\n",
    );
    let bad_out = TempOut::new("pdf");
    let reported = run(&["create", "postscript", bad.arg(), bad_out.arg(), "--json"]);
    assert!(reported.status.success(), "stderr: {}", stderr(&reported));
    let value: Value = serde_json::from_str(&stdout(&reported)).expect("valid JSON");
    assert_eq!(value["clean"], false);
    let unsupported = value["unsupported"].as_array().expect("unsupported");
    assert!(!unsupported.is_empty());
    assert!(
        unsupported.iter().any(|finding| finding["token"] == "arc"),
        "{}",
        stdout(&reported)
    );

    let strict = run(&[
        "create",
        "postscript",
        bad.arg(),
        TempOut::new("pdf").arg(),
        "--strict",
    ]);
    assert!(!strict.status.success());
    assert!(
        stderr(&strict).contains("unsupported PostScript construct"),
        "{}",
        stderr(&strict)
    );

    let malformed = TempPath::new("ps");
    malformed.write(b"not postscript");
    let failed = run(&[
        "create",
        "postscript",
        malformed.arg(),
        TempOut::new("pdf").arg(),
    ]);
    assert!(!failed.status.success());
    assert!(
        stderr(&failed).contains("missing %!PS-Adobe DSC header"),
        "{}",
        stderr(&failed)
    );

    let missing = run(&[
        "create",
        "postscript",
        TempPath::new("ps").arg(),
        TempOut::new("pdf").arg(),
    ]);
    assert!(!missing.status.success());
}

#[test]
fn form_distribute_exports_a_round_tripping_bundle() {
    let form = TempOut::new("pdf");
    let created = run(&[
        "form",
        "create",
        form.arg(),
        "--field",
        "text:customer:72,700,200,20",
        "--field",
        "text:agree:72,660,200,20",
    ]);
    assert!(created.status.success(), "stderr: {}", stderr(&created));

    let bundle = TempPath::new("txt");
    let distributed = run(&[
        "form",
        "distribute",
        form.arg(),
        "--out",
        bundle.arg(),
        "--form-id",
        "order-form",
        "--title",
        "Order",
        "--version",
        "3",
        "--json",
    ]);
    assert!(
        distributed.status.success(),
        "stderr: {}",
        stderr(&distributed)
    );
    let value: Value = serde_json::from_str(&stdout(&distributed)).expect("valid JSON");
    assert_eq!(value["form_id"], "order-form");
    assert_eq!(value["title"], "Order");
    assert_eq!(value["version"], 3);
    assert_eq!(value["field_count"], 2);
    let fields = value["fields"].as_array().expect("fields");
    assert_eq!(fields[0]["name"], "customer");
    assert_eq!(fields[0]["kind"], "text");
    assert_eq!(fields[0]["required"], false);
    assert_eq!(fields[1]["name"], "agree");

    let written = fs::read_to_string(&bundle.path).expect("bundle text");
    assert!(written.starts_with("PDFXI-BUNDLE-1\n"), "{written}");
    assert!(written.contains("ID\torder-form"), "{written}");
    assert!(written.contains("TITLE\tOrder"), "{written}");
    assert!(written.contains("FIELD\tcustomer\ttext\t0\t"), "{written}");
    assert!(written.contains("FIELD\tagree\ttext\t0\t"), "{written}");

    let again = TempPath::new("txt");
    let repeated = run(&[
        "form",
        "distribute",
        form.arg(),
        "--out",
        again.arg(),
        "--form-id",
        "order-form",
        "--title",
        "Order",
        "--version",
        "3",
    ]);
    assert!(repeated.status.success(), "stderr: {}", stderr(&repeated));
    assert_eq!(
        fs::read(&bundle.path).expect("first"),
        fs::read(&again.path).expect("second"),
        "bundle export is deterministic"
    );

    let empty_id = run(&[
        "form",
        "distribute",
        form.arg(),
        "--out",
        TempPath::new("txt").arg(),
        "--form-id",
        " ",
    ]);
    assert!(!empty_id.status.success());
    assert!(
        stderr(&empty_id).contains("--form-id must not be empty"),
        "{}",
        stderr(&empty_id)
    );

    let no_form = run(&[
        "form",
        "distribute",
        root_fixture("minimal.pdf").to_str().expect("utf-8"),
        "--out",
        TempPath::new("txt").arg(),
    ]);
    assert!(!no_form.status.success());
    assert!(
        stderr(&no_form).contains("has no /AcroForm"),
        "{}",
        stderr(&no_form)
    );
}

#[test]
fn form_collect_merges_responses_and_reports_conflicts() {
    let form = TempOut::new("pdf");
    let created = run(&[
        "form",
        "create",
        form.arg(),
        "--field",
        "text:customer:72,700,200,20",
        "--field",
        "text:agree:72,660,200,20",
    ]);
    assert!(created.status.success(), "stderr: {}", stderr(&created));

    let alice = TempPath::new("fdf");
    alice.write(
        b"%FDF-1.7\n1 0 obj\n<< /FDF << /Fields [\n<< /T (customer) /V (Ada) >>\n<< /T (agree) /V (yes) >>\n] >> >>\nendobj\ntrailer\n<< /Root 1 0 R >>\n%%EOF\n",
    );
    let bob = TempPath::new("fdf");
    bob.write(
        b"%FDF-1.7\n1 0 obj\n<< /FDF << /Fields [\n<< /T (customer) /V (Bob) >>\n] >> >>\nendobj\ntrailer\n<< /Root 1 0 R >>\n%%EOF\n",
    );
    let carol = TempPath::new("xfdf");
    carol.write(
        b"<?xml version=\"1.0\"?>\n<xfdf xmlns=\"http://ns.adobe.com/xfdf/\"><fields><field name=\"customer\"><value>Ada</value></field></fields></xfdf>",
    );

    let alice_arg = format!("alice={}", alice.arg());
    let bob_arg = format!("bob={}", bob.arg());
    let carol_arg = format!("carol={}", carol.arg());
    let collected = run(&[
        "form",
        "collect",
        form.arg(),
        "--response",
        &alice_arg,
        "--response",
        &bob_arg,
        "--response",
        &carol_arg,
        "--json",
    ]);
    assert!(collected.status.success(), "stderr: {}", stderr(&collected));
    let value: Value = serde_json::from_str(&stdout(&collected)).expect("valid JSON");
    assert_eq!(value["pdf_fields"], 2);
    assert_eq!(value["summary"]["responses_received"], 3);
    assert_eq!(value["summary"]["responses_accepted"], 3);
    assert_eq!(value["summary"]["responses_rejected"], 0);
    assert_eq!(value["summary"]["fields_merged"], 2);
    assert_eq!(value["summary"]["conflicts"], 1);
    let fields = value["fields"].as_array().expect("fields");
    let customer = fields
        .iter()
        .find(|field| field["name"] == "customer")
        .expect("customer row");
    assert_eq!(customer["value"], "Ada");
    assert_eq!(customer["conflict"], true);
    assert_eq!(
        customer["contributors"],
        serde_json::json!(["alice", "bob", "carol"])
    );
    let conflicts = value["conflicts"].as_array().expect("conflicts");
    assert_eq!(conflicts.len(), 1);
    assert_eq!(conflicts[0]["name"], "customer");
    assert_eq!(conflicts[0]["values"].as_array().expect("values").len(), 3);
    assert_eq!(value["missing_required"], serde_json::json!([]));

    let reversed = run(&[
        "form",
        "collect",
        form.arg(),
        "--response",
        &carol_arg,
        "--response",
        &bob_arg,
        "--response",
        &alice_arg,
        "--json",
    ]);
    assert!(reversed.status.success(), "stderr: {}", stderr(&reversed));
    assert_eq!(
        stdout(&collected),
        stdout(&reversed),
        "collection is deterministic under input reordering"
    );

    let bad = TempPath::new("fdf");
    bad.write(b"not an fdf");
    let bad_arg = format!("bad={}", bad.arg());
    let partial = run(&[
        "form",
        "collect",
        form.arg(),
        "--response",
        &alice_arg,
        "--response",
        &bad_arg,
        "--json",
    ]);
    assert!(partial.status.success(), "stderr: {}", stderr(&partial));
    let value: Value = serde_json::from_str(&stdout(&partial)).expect("valid JSON");
    assert_eq!(value["summary"]["responses_received"], 2);
    assert_eq!(value["summary"]["responses_accepted"], 1);
    assert_eq!(value["summary"]["responses_rejected"], 1);
    let rejected = value["rejected"].as_array().expect("rejected");
    assert_eq!(rejected.len(), 1);
    assert!(
        rejected[0]["reason"].as_str().unwrap_or("").len() > 4,
        "{}",
        stdout(&partial)
    );

    let ghost = TempPath::new("fdf");
    ghost.write(
        b"%FDF-1.7\n1 0 obj\n<< /FDF << /Fields [\n<< /T (ghost) /V (x) >>\n] >> >>\nendobj\ntrailer\n<< /Root 1 0 R >>\n%%EOF\n",
    );
    let unknown = run(&["form", "collect", form.arg(), "--response", ghost.arg()]);
    assert!(!unknown.status.success());
    assert!(
        stderr(&unknown).contains("does not define"),
        "{}",
        stderr(&unknown)
    );

    let empty = run(&["form", "collect", form.arg()]);
    assert!(
        !empty.status.success(),
        "at least one --response is required"
    );
}

#[test]
fn form_reader_enable_writes_rights_and_fill_save_decisions() {
    let source = root_fixture("minimal.pdf");
    let out = TempOut::new("pdf");
    let enabled = run(&[
        "form",
        "reader-enable",
        source.to_str().expect("utf-8"),
        "--out",
        out.arg(),
        "--rights",
        "fill,save",
        "--reason",
        "local trial",
        "--json",
    ]);
    assert!(enabled.status.success(), "stderr: {}", stderr(&enabled));
    let value: Value = serde_json::from_str(&stdout(&enabled)).expect("valid JSON");
    assert_eq!(value["pages"], 1);
    assert_eq!(value["rights"], serde_json::json!(["fill-in", "save"]));
    assert_eq!(value["filter"], "PDFXI.ReaderEnable.Local");
    assert_eq!(value["placeholder_bytes"], 4096);
    assert_eq!(value["reason"], "local trial");
    assert_eq!(value["can_fill"], true);
    assert_eq!(value["can_save"], true);
    assert_eq!(value["certified"], false);

    let bytes = fs::read(&out.path).expect("enabled bytes");
    let text = String::from_utf8_lossy(&bytes);
    for key in ["/Perms", "/UR", "/Ff", "PDFXI.ReaderEnable.Local"] {
        assert!(text.contains(key), "saved bytes are missing {key}");
    }
    assert_ne!(fs::read(&source).expect("source"), bytes);

    let save_only = TempOut::new("pdf");
    let second = run(&[
        "form",
        "reader-enable",
        out.arg(),
        "--out",
        save_only.arg(),
        "--rights",
        "save",
        "--json",
    ]);
    assert!(second.status.success(), "stderr: {}", stderr(&second));
    let value: Value = serde_json::from_str(&stdout(&second)).expect("valid JSON");
    assert_eq!(value["rights"], serde_json::json!(["save"]));
    assert_eq!(value["can_fill"], false);
    assert_eq!(value["can_save"], true);

    let info = run(&["info", out.arg()]);
    assert!(info.status.success(), "stderr: {}", stderr(&info));
    assert!(stdout(&info).contains("pages: 1"), "{}", stdout(&info));

    let bad = run(&[
        "form",
        "reader-enable",
        source.to_str().expect("utf-8"),
        "--out",
        TempOut::new("pdf").arg(),
        "--rights",
        "fill,teleport",
    ]);
    assert!(!bad.status.success());
    assert!(stderr(&bad).contains("unknown right"), "{}", stderr(&bad));

    let zero = run(&[
        "form",
        "reader-enable",
        source.to_str().expect("utf-8"),
        "--out",
        TempOut::new("pdf").arg(),
        "--placeholder-bytes",
        "0",
    ]);
    assert!(!zero.status.success());
    assert!(
        stderr(&zero).contains("outside 1..=65536"),
        "{}",
        stderr(&zero)
    );
}

#[test]
fn form_reader_enable_keeps_fields_and_handles_every_right() {
    let form = TempOut::new("pdf");
    let created = run(&[
        "form",
        "create",
        form.arg(),
        "--field",
        "text:Name:72,600,168,20",
    ]);
    assert!(created.status.success(), "stderr: {}", stderr(&created));

    let out = TempOut::new("pdf");
    let enabled = run(&[
        "form",
        "reader-enable",
        form.arg(),
        "--out",
        out.arg(),
        "--rights",
        "all",
        "--placeholder-bytes",
        "128",
        "--json",
    ]);
    assert!(enabled.status.success(), "stderr: {}", stderr(&enabled));
    let value: Value = serde_json::from_str(&stdout(&enabled)).expect("valid JSON");
    assert_eq!(value["placeholder_bytes"], 128);
    let rights: Vec<&str> = value["rights"]
        .as_array()
        .expect("rights")
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert_eq!(
        rights,
        [
            "fill-in",
            "save",
            "annotate",
            "assemble",
            "copy-extract",
            "accessibility"
        ]
    );
    assert_eq!(value["can_fill"], true);
    assert_eq!(value["can_save"], true);

    let listed = run(&["form", "list", out.arg(), "--json"]);
    assert!(listed.status.success(), "stderr: {}", stderr(&listed));
    let value: Value = serde_json::from_str(&stdout(&listed)).expect("valid JSON");
    let fields = value["fields"].as_array().expect("fields");
    assert_eq!(fields.len(), 1, "{}", stdout(&listed));
    assert_eq!(fields[0]["name"], "Name");

    let repeated = TempOut::new("pdf");
    let again = run(&[
        "form",
        "reader-enable",
        form.arg(),
        "--out",
        repeated.arg(),
        "--rights",
        "all",
        "--placeholder-bytes",
        "128",
    ]);
    assert!(again.status.success(), "stderr: {}", stderr(&again));
    assert_eq!(
        fs::read(&out.path).expect("first"),
        fs::read(&repeated.path).expect("second"),
        "reader-enable output is deterministic"
    );
}

#[test]
fn replace_rewrites_text_and_reports_remaining_matches() {
    let source = root_fixture("minimal.pdf");
    let out = TempOut::new("pdf");
    let replaced = run(&[
        "replace",
        source.to_str().expect("utf-8"),
        "--out",
        out.arg(),
        "--find",
        "Hello",
        "--replace",
        "Goodbye",
        "--case-sensitive",
        "--json",
    ]);
    assert!(replaced.status.success(), "stderr: {}", stderr(&replaced));
    let value: Value = serde_json::from_str(&stdout(&replaced)).expect("valid JSON");
    assert_eq!(value["pages"], 1);
    assert_eq!(value["initial_matches"], 1);
    assert_eq!(value["replaced"], 1);
    assert_eq!(value["remaining"], 0);
    assert_eq!(value["limit_reached"], false);
    assert_eq!(value["case_sensitive"], true);
    let rows = value["rows"].as_array().expect("rows");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["page"], 1);

    let text = run(&["text", out.arg()]);
    assert!(text.status.success(), "stderr: {}", stderr(&text));
    assert!(stdout(&text).contains("Goodbye, PDFXI!"));
    assert!(!stdout(&text).contains("Hello"));

    let again = TempOut::new("pdf");
    let repeated = run(&[
        "replace",
        source.to_str().expect("utf-8"),
        "--out",
        again.arg(),
        "--find",
        "Hello",
        "--replace",
        "Goodbye",
        "--case-sensitive",
    ]);
    assert!(repeated.status.success(), "stderr: {}", stderr(&repeated));
    assert_eq!(
        fs::read(&out.path).expect("first"),
        fs::read(&again.path).expect("second"),
        "replacement is deterministic"
    );

    let miss = run(&[
        "replace",
        source.to_str().expect("utf-8"),
        "--out",
        TempOut::new("pdf").arg(),
        "--find",
        "hello",
        "--replace",
        "x",
        "--case-sensitive",
        "--json",
    ]);
    assert!(miss.status.success(), "stderr: {}", stderr(&miss));
    let value: Value = serde_json::from_str(&stdout(&miss)).expect("valid JSON");
    assert_eq!(value["initial_matches"], 0);
    assert_eq!(value["replaced"], 0);

    let empty = run(&[
        "replace",
        source.to_str().expect("utf-8"),
        "--out",
        TempOut::new("pdf").arg(),
        "--find",
        "",
        "--replace",
        "x",
    ]);
    assert!(!empty.status.success());
    assert!(
        stderr(&empty).contains("--find must not be empty"),
        "{}",
        stderr(&empty)
    );
}

#[test]
fn replace_spans_pages_options_and_unicode() {
    let content: String = (0..120)
        .map(|index| {
            if index % 30 == 7 {
                format!("line {index:03} alpha alphabet marker\n")
            } else {
                format!("line {index:03} marker\n")
            }
        })
        .collect();
    let text = TempPath::new("txt");
    text.write(content.as_bytes());
    let source = TempOut::new("pdf");
    let created = run(&["create", "text", text.arg(), source.arg()]);
    assert!(created.status.success(), "stderr: {}", stderr(&created));
    let info = run(&["info", source.arg()]);
    assert!(info.status.success(), "stderr: {}", stderr(&info));
    assert!(
        !stdout(&info).contains("pages: 1\n"),
        "fixture must span more than one page: {}",
        stdout(&info)
    );

    let out = TempOut::new("pdf");
    let replaced = run(&[
        "replace",
        source.arg(),
        "--out",
        out.arg(),
        "--find",
        "marker",
        "--replace",
        "label",
        "--json",
    ]);
    assert!(replaced.status.success(), "stderr: {}", stderr(&replaced));
    let value: Value = serde_json::from_str(&stdout(&replaced)).expect("valid JSON");
    assert_eq!(value["initial_matches"], 120);
    assert_eq!(value["replaced"], 120);
    assert_eq!(value["remaining"], 0);
    assert_eq!(value["limit_reached"], false);
    let pages: Vec<u64> = value["rows"]
        .as_array()
        .expect("rows")
        .iter()
        .filter_map(|row| row["page"].as_u64())
        .collect();
    assert!(
        pages.iter().any(|page| *page > 1),
        "replacement must span pages"
    );

    let scanned = run(&["search", out.arg(), "label", "--json"]);
    assert!(scanned.status.success(), "stderr: {}", stderr(&scanned));
    let value: Value = serde_json::from_str(&stdout(&scanned)).expect("valid JSON");
    let matches = value["matches"].as_array().expect("matches");
    assert_eq!(matches.len(), 120, "{}", stdout(&scanned));

    let whole = TempOut::new("pdf");
    let whole_word = run(&[
        "replace",
        source.arg(),
        "--out",
        whole.arg(),
        "--find",
        "alpha",
        "--replace",
        "ALPHA",
        "--whole-word",
        "--case-sensitive",
        "--json",
    ]);
    assert!(
        whole_word.status.success(),
        "stderr: {}",
        stderr(&whole_word)
    );
    let value: Value = serde_json::from_str(&stdout(&whole_word)).expect("valid JSON");
    assert_eq!(value["initial_matches"], 4);
    assert_eq!(value["replaced"], 4);
    let text = run(&["text", whole.arg()]);
    assert!(
        stdout(&text).contains("alphabet"),
        "whole-word keeps the substring"
    );

    let limited = TempOut::new("pdf");
    let limit = run(&[
        "replace",
        source.arg(),
        "--out",
        limited.arg(),
        "--find",
        "marker",
        "--replace",
        "label",
        "--limit",
        "3",
        "--json",
    ]);
    assert!(limit.status.success(), "stderr: {}", stderr(&limit));
    let value: Value = serde_json::from_str(&stdout(&limit)).expect("valid JSON");
    assert_eq!(value["replaced"], 3);
    assert_eq!(value["remaining"], 117);
    assert_eq!(value["limit_reached"], true);
    assert_eq!(value["limit"], 3);

    let unicode = TempOut::new("pdf");
    let accented = run(&[
        "replace",
        source.arg(),
        "--out",
        unicode.arg(),
        "--find",
        "line 001",
        "--replace",
        "Grüße 001",
        "--case-sensitive",
        "--json",
    ]);
    assert!(accented.status.success(), "stderr: {}", stderr(&accented));
    let value: Value = serde_json::from_str(&stdout(&accented)).expect("valid JSON");
    assert_eq!(value["replaced"], 1);
    let text = run(&["text", unicode.arg()]);
    assert!(stdout(&text).contains("Grüße 001"), "{}", stdout(&text));
}

#[test]
fn replace_runs_on_a_real_world_document() {
    let source = root_fixture("realworld/powerpoint-slides.pdf");
    let out = TempOut::new("pdf");
    let replaced = run(&[
        "replace",
        source.to_str().expect("utf-8"),
        "--out",
        out.arg(),
        "--find",
        "marker",
        "--replace",
        "label",
        "--json",
    ]);
    assert!(replaced.status.success(), "stderr: {}", stderr(&replaced));
    let value: Value = serde_json::from_str(&stdout(&replaced)).expect("valid JSON");
    assert_eq!(value["pages"], 3);
    assert_eq!(value["initial_matches"], 3);
    assert_eq!(value["replaced"], 3);
    assert_eq!(value["remaining"], 0);
    assert!(
        value["embedded_font_blocks"].as_u64().unwrap_or(0) > 0,
        "the real fixture needs the embedded fallback font: {}",
        stdout(&replaced)
    );
    let text = run(&["text", out.arg()]);
    assert!(text.status.success(), "stderr: {}", stderr(&text));
    assert_eq!(stdout(&text).matches("label").count(), 3);
    let info = run(&["info", out.arg()]);
    assert!(stdout(&info).contains("pages: 3"), "{}", stdout(&info));
}

#[test]
fn structure_apply_writes_a_tagged_tree_on_a_real_page() {
    let source = root_fixture("minimal.pdf");
    let out = TempOut::new("pdf");
    let tree = TempPath::new("json");
    tree.write(
        br#"{
  "lang": "en-US",
  "nodes": [
    { "id": 1, "role": "Document", "page": 0, "children": [2, 3] },
    { "id": 2, "role": "H1", "page": 0, "bbox": [72, 700, 400, 730], "text": "Hello, PDFXI!" },
    { "id": 3, "role": "P", "page": 0, "bbox": [72, 650, 400, 690], "text": "Tagged structure proof" }
  ],
  "roots": [1]
}"#,
    );

    let applied = run(&[
        "structure",
        "apply",
        source.to_str().expect("utf-8"),
        "--out",
        out.arg(),
        "--tree",
        tree.arg(),
        "--json",
    ]);
    assert!(applied.status.success(), "stderr: {}", stderr(&applied));
    let value: Value = serde_json::from_str(&stdout(&applied)).expect("valid JSON");
    assert_eq!(value["lang"], "en-US");
    assert_eq!(value["pages"], 1);
    assert_eq!(value["nodes"], 3);
    assert_eq!(value["struct_elements"], 3);
    assert_eq!(value["pages_tagged"], 1);
    assert_eq!(value["certification_claimed"], false);
    let roles: Vec<&str> = value["roles"]
        .as_array()
        .expect("roles")
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert_eq!(roles, ["Document", "H1", "P"]);
    assert!(value["struct_tree_root"].as_u64().unwrap_or(0) > 0);

    let bytes = fs::read(&out.path).expect("tagged bytes");
    let text = String::from_utf8_lossy(&bytes);
    for key in ["/StructTreeRoot", "/StructParents", "/ParentTree", "/MCID"] {
        assert!(text.contains(key), "saved bytes are missing {key}");
    }
    assert_ne!(
        fs::read(&source).expect("source"),
        bytes,
        "tagging must write a new document"
    );

    let info = run(&["info", out.arg()]);
    assert!(info.status.success(), "stderr: {}", stderr(&info));
    assert!(stdout(&info).contains("pages: 1"), "{}", stdout(&info));

    let checked = run(&["accessibility", "check", out.arg(), "--json"]);
    assert!(
        !checked.status.success(),
        "the title finding keeps exit code 1"
    );
    let report: Value = serde_json::from_str(&stdout(&checked)).expect("valid JSON");
    let findings = report["findings"].as_array().expect("findings");
    let codes: Vec<&str> = findings
        .iter()
        .filter_map(|finding| finding["code"].as_str())
        .collect();
    assert_eq!(codes, ["UA-0001"], "tagging clears the structure findings");
    for cleared in [
        "PDFUA1.NOT_MARKED",
        "PDFUA1.NO_LANG",
        "PDFUA1.NO_STRUCT_TREE_ROOT",
    ] {
        assert!(!codes.contains(&cleared), "{cleared} should be cleared");
    }
}

#[test]
fn accessibility_order_and_move_edit_reading_order() {
    let source = root_fixture("minimal.pdf");
    let tagged = TempOut::new("pdf");
    let tree = TempPath::new("json");
    tree.write(
        br#"{"nodes":[
            {"id":1,"role":"Document","page":0,"children":[2,3]},
            {"id":2,"role":"P","page":0,"bbox":[72,700,400,730],"text":"Paragraph"},
            {"id":3,"role":"Table","page":0,"children":[4,5]},
            {"id":4,"role":"TH","page":0,"bbox":[72,650,200,670],"text":"Header"},
            {"id":5,"role":"TD","page":0,"bbox":[72,620,200,640],"text":"Cell"}
        ],"roots":[1]}"#,
    );
    let applied = run(&[
        "structure",
        "apply",
        source.to_str().expect("utf-8"),
        "--out",
        tagged.arg(),
        "--tree",
        tree.arg(),
    ]);
    assert!(applied.status.success(), "stderr: {}", stderr(&applied));

    let order = run(&["accessibility", "order", tagged.arg(), "--json"]);
    assert!(order.status.success(), "stderr: {}", stderr(&order));
    let value: Value = serde_json::from_str(&stdout(&order)).expect("valid JSON");
    assert_eq!(value["count"], 5);
    let ids: Vec<u64> = value["order"]
        .as_array()
        .expect("order")
        .iter()
        .filter_map(|row| row["node"].as_u64())
        .collect();
    assert_eq!(ids, [1, 2, 3, 4, 5]);

    let moved = TempOut::new("pdf");
    let move_table = run(&[
        "accessibility",
        "move",
        tagged.arg(),
        "--out",
        moved.arg(),
        "--node",
        "3",
        "--before",
        "2",
        "--json",
    ]);
    assert!(
        move_table.status.success(),
        "stderr: {}",
        stderr(&move_table)
    );
    let value: Value = serde_json::from_str(&stdout(&move_table)).expect("valid JSON");
    assert_eq!(value["order"], serde_json::json!([1, 3, 4, 5, 2]));
    let reordered = run(&["accessibility", "order", moved.arg(), "--json"]);
    assert!(reordered.status.success(), "stderr: {}", stderr(&reordered));
    let value: Value = serde_json::from_str(&stdout(&reordered)).expect("valid JSON");
    let ids: Vec<u64> = value["order"]
        .as_array()
        .expect("order")
        .iter()
        .filter_map(|row| row["node"].as_u64())
        .collect();
    assert_eq!(ids, [1, 3, 4, 5, 2]);

    let end = TempOut::new("pdf");
    let move_end = run(&[
        "accessibility",
        "move",
        moved.arg(),
        "--out",
        end.arg(),
        "--node",
        "4",
        "--json",
    ]);
    assert!(move_end.status.success(), "stderr: {}", stderr(&move_end));
    let value: Value = serde_json::from_str(&stdout(&move_end)).expect("valid JSON");
    assert_eq!(value["position"], 3);
    assert_eq!(value["order"], serde_json::json!([1, 3, 5, 4, 2]));
}

#[test]
fn accessibility_move_rejects_bad_targets() {
    let source = root_fixture("minimal.pdf");
    let tagged = TempOut::new("pdf");
    let tree = TempPath::new("json");
    tree.write(
        br#"{"nodes":[
            {"id":1,"role":"Document","page":0,"children":[2,3]},
            {"id":2,"role":"P","page":0,"bbox":[72,700,400,730],"text":"Paragraph"},
            {"id":3,"role":"Table","page":0,"children":[4]},
            {"id":4,"role":"TD","page":0,"bbox":[72,600,200,620],"text":"Cell"}
        ],"roots":[1]}"#,
    );
    let applied = run(&[
        "structure",
        "apply",
        source.to_str().expect("utf-8"),
        "--out",
        tagged.arg(),
        "--tree",
        tree.arg(),
    ]);
    assert!(applied.status.success(), "stderr: {}", stderr(&applied));

    let missing = run(&[
        "accessibility",
        "move",
        tagged.arg(),
        "--out",
        TempOut::new("pdf").arg(),
        "--node",
        "99",
    ]);
    assert!(!missing.status.success());
    assert!(
        stderr(&missing).contains("no structure element"),
        "{}",
        stderr(&missing)
    );

    let not_sibling = run(&[
        "accessibility",
        "move",
        tagged.arg(),
        "--out",
        TempOut::new("pdf").arg(),
        "--node",
        "2",
        "--before",
        "4",
    ]);
    assert!(!not_sibling.status.success());
    assert!(
        stderr(&not_sibling).contains("not a sibling"),
        "{}",
        stderr(&not_sibling)
    );

    let untagged = run(&["accessibility", "order", source.to_str().expect("utf-8")]);
    assert!(!untagged.status.success());
}

#[test]
fn accessibility_alt_and_table_edit_tagged_structure() {
    let source = root_fixture("minimal.pdf");
    let tagged = TempOut::new("pdf");
    let tree = TempPath::new("json");
    tree.write(
        br#"{
  "lang": "en-US",
  "nodes": [
    {"id":1,"role":"Document","page":0,"children":[2,3]},
    {"id":2,"role":"P","page":0,"bbox":[72,700,400,730],"text":"Paragraph","alt":"Old paragraph"},
    {"id":3,"role":"Table","page":0,"children":[4,5]},
    {"id":4,"role":"TH","page":0,"bbox":[72,650,200,670],"text":"Header"},
    {"id":5,"role":"TD","page":0,"bbox":[72,620,200,640],"text":"Cell"}
  ],
  "roots": [1]
}"#,
    );
    let applied = run(&[
        "structure",
        "apply",
        source.to_str().expect("utf-8"),
        "--out",
        tagged.arg(),
        "--tree",
        tree.arg(),
    ]);
    assert!(applied.status.success(), "stderr: {}", stderr(&applied));

    let alt = TempOut::new("pdf");
    let changed = run(&[
        "accessibility",
        "alt",
        tagged.arg(),
        "--out",
        alt.arg(),
        "--node",
        "2",
        "--text",
        "Updated paragraph alt",
        "--json",
    ]);
    assert!(changed.status.success(), "stderr: {}", stderr(&changed));
    let value: Value = serde_json::from_str(&stdout(&changed)).expect("valid JSON");
    assert_eq!(value["node"], 2);
    assert_eq!(value["field"], "alt");
    assert_eq!(value["value"], "Updated paragraph alt");
    let bytes = fs::read(&alt.path).expect("alt PDF");
    let text = String::from_utf8_lossy(&bytes);
    assert!(
        text.contains("557064617465642070617261677261706820616C74"),
        "{text}"
    );
    assert!(!text.contains("4F6C6420706172616772617068"), "{text}");
    pdfxi_core::Document::from_bytes(bytes).expect("alt PDF parses");

    let cleared = TempOut::new("pdf");
    let clear = run(&[
        "accessibility",
        "alt",
        alt.arg(),
        "--out",
        cleared.arg(),
        "--node",
        "2",
        "--clear",
        "--json",
    ]);
    assert!(clear.status.success(), "stderr: {}", stderr(&clear));
    let value: Value = serde_json::from_str(&stdout(&clear)).expect("valid JSON");
    assert_eq!(value["value"], Value::Null);
    let bytes = fs::read(&cleared.path).expect("cleared PDF");
    assert!(
        !String::from_utf8_lossy(&bytes).contains("557064617465642070617261677261706820616C74")
    );

    let scoped = TempOut::new("pdf");
    let scope = run(&[
        "accessibility",
        "table",
        tagged.arg(),
        "--out",
        scoped.arg(),
        "--node",
        "4",
        "--scope",
        "column",
        "--json",
    ]);
    assert!(scope.status.success(), "stderr: {}", stderr(&scope));
    let value: Value = serde_json::from_str(&stdout(&scope)).expect("valid JSON");
    assert_eq!(value["role"], "tableheader");
    assert_eq!(value["value"], "Column");
    let bytes = fs::read(&scoped.path).expect("scoped PDF");
    assert!(String::from_utf8_lossy(&bytes).contains("/Scope /Column"));
}

#[test]
fn accessibility_alt_and_table_reject_bad_targets() {
    let source = root_fixture("minimal.pdf");
    let tagged = TempOut::new("pdf");
    let tree = TempPath::new("json");
    tree.write(
        br#"{"nodes":[
            {"id":1,"role":"Document","page":0,"children":[2,3]},
            {"id":2,"role":"P","page":0,"text":"Paragraph"},
            {"id":3,"role":"Table","page":0,"children":[4]},
            {"id":4,"role":"TD","page":0,"bbox":[72,600,200,620],"text":"Cell"}
        ],"roots":[1]}"#,
    );
    let applied = run(&[
        "structure",
        "apply",
        source.to_str().expect("utf-8"),
        "--out",
        tagged.arg(),
        "--tree",
        tree.arg(),
    ]);
    assert!(applied.status.success(), "stderr: {}", stderr(&applied));

    let missing = run(&[
        "accessibility",
        "alt",
        tagged.arg(),
        "--out",
        TempOut::new("pdf").arg(),
        "--node",
        "99",
        "--text",
        "x",
    ]);
    assert!(!missing.status.success());
    assert!(
        stderr(&missing).contains("no structure element"),
        "{}",
        stderr(&missing)
    );

    let wrong_role = run(&[
        "accessibility",
        "table",
        tagged.arg(),
        "--out",
        TempOut::new("pdf").arg(),
        "--node",
        "2",
        "--scope",
        "row",
    ]);
    assert!(!wrong_role.status.success());
    assert!(
        stderr(&wrong_role).contains("not a table header or data cell"),
        "{}",
        stderr(&wrong_role)
    );

    let bad_scope = run(&[
        "accessibility",
        "table",
        tagged.arg(),
        "--out",
        TempOut::new("pdf").arg(),
        "--node",
        "4",
        "--scope",
        "sideways",
    ]);
    assert!(!bad_scope.status.success());
    assert!(
        stderr(&bad_scope).contains("unknown scope"),
        "{}",
        stderr(&bad_scope)
    );

    let untagged = run(&[
        "accessibility",
        "alt",
        source.to_str().expect("utf-8"),
        "--out",
        TempOut::new("pdf").arg(),
        "--node",
        "1",
        "--text",
        "x",
    ]);
    assert!(!untagged.status.success());
}

#[test]
fn structure_apply_tags_every_page_and_rejects_bad_trees() {
    let source = root_fixture("realworld/powerpoint-slides.pdf");
    let out = TempOut::new("pdf");
    let tree = TempPath::new("json");
    tree.write(
        br#"{
  "lang": "en-GB",
  "nodes": [
    { "id": 1, "role": "Document", "page": 0, "children": [2, 3, 4] },
    { "id": 2, "role": "H1", "page": 0, "bbox": [72, 700, 500, 740], "text": "Slide one" },
    { "id": 3, "role": "H2", "page": 1, "bbox": [72, 700, 500, 740], "text": "Slide two" },
    { "id": 4, "role": "P", "page": 2, "bbox": [72, 660, 500, 700], "text": "Slide three" }
  ],
  "roots": [1]
}"#,
    );

    let applied = run(&[
        "structure",
        "apply",
        source.to_str().expect("utf-8"),
        "--out",
        out.arg(),
        "--tree",
        tree.arg(),
        "--json",
    ]);
    assert!(applied.status.success(), "stderr: {}", stderr(&applied));
    let value: Value = serde_json::from_str(&stdout(&applied)).expect("valid JSON");
    assert_eq!(value["pages"], 3);
    assert_eq!(value["pages_tagged"], 3);
    assert_eq!(value["lang"], "en-GB");
    let roles: Vec<&str> = value["roles"]
        .as_array()
        .expect("roles")
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert_eq!(roles, ["Document", "H1", "H2", "P"]);

    let before = fs::read(&source).expect("source bytes");
    let bad = TempPath::new("json");
    bad.write(br#"{"nodes":[{"id":1,"role":"Widget","page":0}],"roots":[1]}"#);
    let rejected = run(&[
        "structure",
        "apply",
        source.to_str().expect("utf-8"),
        "--out",
        TempOut::new("pdf").arg(),
        "--tree",
        bad.arg(),
    ]);
    assert!(!rejected.status.success());
    assert!(
        stderr(&rejected).contains("unknown structure role"),
        "{}",
        stderr(&rejected)
    );
    assert_eq!(
        before,
        fs::read(&source).expect("source bytes"),
        "input untouched"
    );

    let malformed = TempPath::new("json");
    malformed.write(br#"{"nodes":[{"id":1,"role":"P","page":0,"children":[2]}],"roots":[1]}"#);
    let broken = run(&[
        "structure",
        "apply",
        source.to_str().expect("utf-8"),
        "--out",
        TempOut::new("pdf").arg(),
        "--tree",
        malformed.arg(),
    ]);
    assert!(
        !broken.status.success(),
        "dangling children must be rejected"
    );

    let no_roots = TempPath::new("json");
    no_roots.write(br#"{"nodes":[{"id":1,"role":"P","page":0}],"roots":[]}"#);
    let empty = run(&[
        "structure",
        "apply",
        source.to_str().expect("utf-8"),
        "--out",
        TempOut::new("pdf").arg(),
        "--tree",
        no_roots.arg(),
    ]);
    assert!(
        !empty.status.success(),
        "a tree with no roots must be rejected"
    );

    let out_of_range = TempPath::new("json");
    out_of_range.write(br#"{"nodes":[{"id":1,"role":"P","page":9}],"roots":[1]}"#);
    let bad_page = run(&[
        "structure",
        "apply",
        source.to_str().expect("utf-8"),
        "--out",
        TempOut::new("pdf").arg(),
        "--tree",
        out_of_range.arg(),
    ]);
    assert!(
        !bad_page.status.success(),
        "a page out of range must be rejected"
    );

    let missing = run(&[
        "structure",
        "apply",
        source.to_str().expect("utf-8"),
        "--out",
        TempOut::new("pdf").arg(),
        "--tree",
        TempPath::new("json").arg(),
    ]);
    assert!(
        !missing.status.success(),
        "an absent tree file must be rejected"
    );
}

#[test]
fn versions_snapshot_list_diff_and_remove() {
    let left_text = TempPath::new("txt");
    let right_text = TempPath::new("txt");
    left_text.write(b"alpha\nbravo\ncharlie\ndelta\n");
    right_text.write(b"alpha\nBRAVO\ncharlie\ndelta\nextra\n");
    let left_pdf = TempOut::new("pdf");
    let right_pdf = TempOut::new("pdf");
    for (text, pdf) in [(&left_text, &left_pdf), (&right_text, &right_pdf)] {
        let created = run(&["create", "text", text.arg(), pdf.arg()]);
        assert!(created.status.success(), "stderr: {}", stderr(&created));
    }
    let store = TempPath::new("json");

    let first = run(&[
        "versions",
        "snapshot",
        left_pdf.arg(),
        "--store",
        store.arg(),
        "--label",
        "draft",
        "--json",
    ]);
    assert!(first.status.success(), "stderr: {}", stderr(&first));
    let value: Value = serde_json::from_str(&stdout(&first)).expect("valid JSON");
    assert_eq!(value["id"], 1);
    assert_eq!(value["label"], "draft");
    assert_eq!(value["pages"], 1);
    assert_eq!(value["lines"], 4);

    let second = run(&[
        "versions",
        "snapshot",
        right_pdf.arg(),
        "--store",
        store.arg(),
        "--json",
    ]);
    assert!(second.status.success(), "stderr: {}", stderr(&second));
    let value: Value = serde_json::from_str(&stdout(&second)).expect("valid JSON");
    assert_eq!(value["id"], 2);
    assert_eq!(value["lines"], 5);

    let state = fs::read_to_string(&store.path).expect("store bytes");
    assert!(state.contains("\"version\": 1"), "{state}");

    let listed = run(&["versions", "list", "--store", store.arg(), "--json"]);
    assert!(listed.status.success(), "stderr: {}", stderr(&listed));
    let rows: Value = serde_json::from_str(&stdout(&listed)).expect("valid JSON");
    let rows = rows.as_array().expect("rows");
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["id"], 1);
    assert_eq!(rows[0]["label"], "draft");
    assert_eq!(rows[1]["id"], 2);

    let diff = run(&[
        "versions",
        "diff",
        "1",
        "2",
        "--store",
        store.arg(),
        "--json",
    ]);
    assert!(diff.status.success(), "stderr: {}", stderr(&diff));
    let value: Value = serde_json::from_str(&stdout(&diff)).expect("valid JSON");
    assert_eq!(value["identical"], false);
    assert_eq!(value["summary"]["changed"], 1);
    assert_eq!(value["summary"]["added"], 1);
    assert_eq!(value["left"]["kind"], "snapshot");
    assert_eq!(value["left"]["id"], 1);
    assert_eq!(value["hunks"][0]["old_text"], "bravo");
    assert_eq!(value["hunks"][0]["new_text"], "BRAVO");

    let file_diff = run(&[
        "versions",
        "diff",
        right_pdf.arg(),
        "1",
        "--store",
        store.arg(),
        "--json",
    ]);
    assert!(file_diff.status.success(), "stderr: {}", stderr(&file_diff));
    let value: Value = serde_json::from_str(&stdout(&file_diff)).expect("valid JSON");
    assert_eq!(value["left"]["kind"], "file");
    assert_eq!(value["summary"]["removed"], 1);
    assert_eq!(value["summary"]["changed"], 1);

    let same = run(&[
        "versions",
        "diff",
        "2",
        right_pdf.arg(),
        "--store",
        store.arg(),
        "--json",
    ]);
    assert!(same.status.success(), "stderr: {}", stderr(&same));
    let value: Value = serde_json::from_str(&stdout(&same)).expect("valid JSON");
    assert_eq!(value["identical"], true);
    assert_eq!(value["summary"]["hunks"], 0);

    let removed = run(&["versions", "remove", "1", "--store", store.arg(), "--json"]);
    assert!(removed.status.success(), "stderr: {}", stderr(&removed));
    let value: Value = serde_json::from_str(&stdout(&removed)).expect("valid JSON");
    assert_eq!(value["removed"], 1);
    assert_eq!(value["snapshots"], 1);

    let after = run(&["versions", "list", "--store", store.arg(), "--json"]);
    let rows: Value = serde_json::from_str(&stdout(&after)).expect("valid JSON");
    assert_eq!(rows.as_array().expect("rows").len(), 1);

    let unknown = run(&["versions", "diff", "9", "2", "--store", store.arg()]);
    assert!(!unknown.status.success());
    assert!(
        stderr(&unknown).contains("no snapshot 9"),
        "{}",
        stderr(&unknown)
    );

    let empty = TempPath::new("json");
    let empty_list = run(&["versions", "list", "--store", empty.arg(), "--json"]);
    assert!(
        empty_list.status.success(),
        "stderr: {}",
        stderr(&empty_list)
    );
    let rows: Value = serde_json::from_str(&stdout(&empty_list)).expect("valid JSON");
    assert_eq!(rows.as_array().expect("rows").len(), 0);
}

#[test]
fn versions_snapshot_real_world_document() {
    let source = root_fixture("realworld/powerpoint-slides.pdf");
    let store = TempPath::new("json");
    let snapshot = run(&[
        "versions",
        "snapshot",
        source.to_str().expect("utf-8"),
        "--store",
        store.arg(),
        "--label",
        "slides",
        "--json",
    ]);
    assert!(snapshot.status.success(), "stderr: {}", stderr(&snapshot));
    let value: Value = serde_json::from_str(&stdout(&snapshot)).expect("valid JSON");
    assert_eq!(value["pages"], 3);
    assert!(value["lines"].as_u64().unwrap_or(0) > 0);

    let same = run(&[
        "versions",
        "diff",
        "1",
        source.to_str().expect("utf-8"),
        "--store",
        store.arg(),
        "--json",
    ]);
    assert!(same.status.success(), "stderr: {}", stderr(&same));
    let value: Value = serde_json::from_str(&stdout(&same)).expect("valid JSON");
    assert_eq!(value["identical"], true);
    assert_eq!(value["summary"]["hunks"], 0);
}

#[test]
fn compare_hunks_report_line_changes_with_page_numbers() {
    let left_text = TempPath::new("txt");
    let right_text = TempPath::new("txt");
    left_text.write(b"alpha\nbravo\ncharlie\ndelta\n");
    right_text.write(b"alpha\nBRAVO\ncharlie\nepsilon\nextra\n");
    let left_pdf = TempOut::new("pdf");
    let right_pdf = TempOut::new("pdf");
    for (text, pdf) in [(&left_text, &left_pdf), (&right_text, &right_pdf)] {
        let created = run(&["create", "text", text.arg(), pdf.arg()]);
        assert!(created.status.success(), "stderr: {}", stderr(&created));
    }

    let compared = run(&[
        "compare",
        left_pdf.arg(),
        right_pdf.arg(),
        "--hunks",
        "--json",
    ]);
    assert!(compared.status.success(), "stderr: {}", stderr(&compared));
    let value: Value = serde_json::from_str(&stdout(&compared)).expect("valid JSON");
    assert_eq!(value["identical"], false);
    assert_eq!(value["summary"]["changed"], 2);
    assert_eq!(value["summary"]["added"], 1);
    assert_eq!(value["summary"]["removed"], 0);
    assert_eq!(value["summary"]["pages_examined"], 1);
    let hunks = value["hunks"].as_array().expect("hunks");
    assert_eq!(hunks.len(), 3);
    assert_eq!(hunks[0]["kind"], "changed");
    assert_eq!(hunks[0]["page"], 1);
    assert_eq!(hunks[0]["old_line"], 2);
    assert_eq!(hunks[0]["new_line"], 2);
    assert_eq!(hunks[0]["old_text"], "bravo");
    assert_eq!(hunks[0]["new_text"], "BRAVO");
    assert_eq!(hunks[2]["kind"], "added");
    assert_eq!(hunks[2]["new_text"], "extra");

    let repeated = run(&[
        "compare",
        left_pdf.arg(),
        right_pdf.arg(),
        "--hunks",
        "--json",
    ]);
    assert_eq!(
        stdout(&compared),
        stdout(&repeated),
        "hunks are deterministic"
    );

    let identical = run(&[
        "compare",
        left_pdf.arg(),
        left_pdf.arg(),
        "--hunks",
        "--json",
    ]);
    assert!(identical.status.success(), "stderr: {}", stderr(&identical));
    let value: Value = serde_json::from_str(&stdout(&identical)).expect("valid JSON");
    assert_eq!(value["identical"], true);
    assert_eq!(value["summary"]["hunks"], 0);
    assert_eq!(value["hunks"].as_array().expect("hunks").len(), 0);

    let failing = run(&[
        "compare",
        left_pdf.arg(),
        right_pdf.arg(),
        "--hunks",
        "--fail-on-difference",
    ]);
    assert_eq!(failing.status.code(), Some(1));

    let conflicting = run(&[
        "compare",
        left_pdf.arg(),
        right_pdf.arg(),
        "--hunks",
        "--lines",
    ]);
    assert!(!conflicting.status.success());
}

#[test]
fn compare_hunks_reference_later_pages() {
    let left_text = TempPath::new("txt");
    let right_text = TempPath::new("txt");
    let left: String = (0..120).map(|index| format!("line {index:03}\n")).collect();
    let mut right: String = (0..120).map(|index| format!("line {index:03}\n")).collect();
    right = right.replace("line 110", "line 110 changed");
    left_text.write(left.as_bytes());
    right_text.write(right.as_bytes());
    let left_pdf = TempOut::new("pdf");
    let right_pdf = TempOut::new("pdf");
    for (text, pdf) in [(&left_text, &left_pdf), (&right_text, &right_pdf)] {
        let created = run(&["create", "text", text.arg(), pdf.arg()]);
        assert!(created.status.success(), "stderr: {}", stderr(&created));
    }
    let info = run(&["info", left_pdf.arg()]);
    assert!(info.status.success(), "stderr: {}", stderr(&info));
    assert!(
        !stdout(&info).contains("pages: 1\n"),
        "fixture must span more than one page: {}",
        stdout(&info)
    );

    let compared = run(&[
        "compare",
        left_pdf.arg(),
        right_pdf.arg(),
        "--hunks",
        "--json",
    ]);
    assert!(compared.status.success(), "stderr: {}", stderr(&compared));
    let value: Value = serde_json::from_str(&stdout(&compared)).expect("valid JSON");
    let hunks = value["hunks"].as_array().expect("hunks");
    assert_eq!(hunks.len(), 1, "{}", stdout(&compared));
    assert_eq!(hunks[0]["kind"], "changed");
    assert!(
        hunks[0]["page"].as_u64().unwrap_or(0) >= 2,
        "change must reference a later page: {}",
        stdout(&compared)
    );
    assert_eq!(hunks[0]["old_text"], "line 110");
    assert_eq!(hunks[0]["new_text"], "line 110 changed");
}

#[test]
fn accessibility_check_reports_metadata_and_pdfua_gaps() {
    let source = root_fixture("minimal.pdf");
    let first = run(&[
        "accessibility",
        "check",
        source.to_str().expect("utf-8"),
        "--json",
    ]);
    assert_eq!(first.status.code(), Some(2));
    let value: Value = serde_json::from_str(&stdout(&first)).expect("valid JSON");
    assert_eq!(value["valid"], false);
    assert_eq!(value["certification_claimed"], false);
    assert_eq!(value["pages"], 1);
    assert!(value["errors"].as_u64().unwrap_or(0) >= 2);
    let findings = value["findings"].as_array().expect("findings");
    let codes: Vec<&str> = findings
        .iter()
        .map(|finding| finding["code"].as_str().expect("code"))
        .collect();
    assert!(codes.contains(&"UA-0001"), "{codes:?}");
    assert!(codes.contains(&"UA-0002"), "{codes:?}");
    assert!(codes.contains(&"PDFUA1.NO_STRUCT_TREE_ROOT"), "{codes:?}");
    assert!(
        findings.iter().any(|finding| finding["source"] == "pdfua"),
        "{}",
        stdout(&first)
    );
    assert!(
        !value["fix_plan"].as_array().expect("fix plan").is_empty(),
        "{}",
        stdout(&first)
    );

    let repeated = run(&[
        "accessibility",
        "check",
        source.to_str().expect("utf-8"),
        "--json",
    ]);
    assert_eq!(stdout(&first), stdout(&repeated), "report is deterministic");

    let overridden = run(&[
        "accessibility",
        "check",
        source.to_str().expect("utf-8"),
        "--title",
        "Minimal",
        "--lang",
        "en-US",
        "--json",
    ]);
    assert_eq!(overridden.status.code(), Some(2));
    let value: Value = serde_json::from_str(&stdout(&overridden)).expect("valid JSON");
    assert_eq!(value["title"], "Minimal");
    assert_eq!(value["lang"], "en-US");
    let codes: Vec<&str> = value["findings"]
        .as_array()
        .expect("findings")
        .iter()
        .map(|finding| finding["code"].as_str().expect("code"))
        .collect();
    assert!(!codes.contains(&"UA-0001"), "{codes:?}");
    assert!(!codes.contains(&"UA-0002"), "{codes:?}");
    assert!(codes.contains(&"PDFUA1.NO_STRUCT_TREE_ROOT"), "{codes:?}");

    let missing = run(&["accessibility", "check", "no-such-file.pdf"]);
    assert!(!missing.status.success());
    assert_eq!(missing.status.code(), Some(1));
}

#[test]
fn accessibility_check_accepts_a_form_document() {
    let out = TempOut::new("pdf");
    let created = run(&[
        "form",
        "create",
        out.arg(),
        "--field",
        "text:patient:72,700,200,24",
    ]);
    assert!(created.status.success(), "stderr: {}", stderr(&created));
    let checked = run(&["accessibility", "check", out.arg(), "--json"]);
    assert_eq!(checked.status.code(), Some(2));
    let value: Value = serde_json::from_str(&stdout(&checked)).expect("valid JSON");
    assert_eq!(value["pages"], 1);
    assert_eq!(value["valid"], false);
    assert!(
        value["findings"]
            .as_array()
            .expect("findings")
            .iter()
            .any(|finding| finding["code"] == "UA-0001"),
        "{}",
        stdout(&checked)
    );
}

#[test]
fn form_tabs_orders_widgets_and_navigates() {
    let out = TempOut::new("pdf");
    let created = run(&[
        "form",
        "create",
        out.arg(),
        "--field",
        "text:patient:72,700,200,24",
        "--field",
        "checkbox:agree:72,660,14,14",
        "--field",
        "radio:color:72,620,14,14:red|green",
        "--field",
        "combo:state:72,580,200,24:CA|NY",
    ]);
    assert!(created.status.success(), "stderr: {}", stderr(&created));

    let tabs = run(&["form", "tabs", out.arg(), "--json"]);
    assert!(tabs.status.success(), "stderr: {}", stderr(&tabs));
    let value: Value = serde_json::from_str(&stdout(&tabs)).expect("valid JSON");
    assert_eq!(value["tabs_name"], "R");
    assert_eq!(value["policy"], "rows");
    assert_eq!(value["widgets"], 4);
    let order: Vec<&str> = value["order"]
        .as_array()
        .expect("order")
        .iter()
        .map(|row| row["name"].as_str().expect("name"))
        .collect();
    assert_eq!(order, vec!["patient", "agree", "color", "state"]);
    assert_eq!(value["order"][0]["page"], 1);
    assert_eq!(value["order"][0]["rect"][0], 72.0);
    assert_eq!(value["navigation"][0]["next"], "agree");
    assert_eq!(value["navigation"][0]["previous"], "state");

    let no_wrap = run(&["form", "tabs", out.arg(), "--no-wrap", "--json"]);
    assert!(no_wrap.status.success(), "stderr: {}", stderr(&no_wrap));
    let value: Value = serde_json::from_str(&stdout(&no_wrap)).expect("valid JSON");
    assert_eq!(value["navigation"][0]["previous"], Value::Null);
    assert_eq!(value["navigation"][3]["next"], Value::Null);

    let columns = run(&["form", "tabs", out.arg(), "--policy", "columns", "--json"]);
    assert!(columns.status.success(), "stderr: {}", stderr(&columns));
    let value: Value = serde_json::from_str(&stdout(&columns)).expect("valid JSON");
    assert_eq!(value["tabs_name"], "C");
    assert_eq!(value["policy"], "columns");
    let column_order: Vec<&str> = value["order"]
        .as_array()
        .expect("order")
        .iter()
        .map(|row| row["name"].as_str().expect("name"))
        .collect();
    assert_ne!(column_order, order, "columns policy reorders widgets");

    let bad_policy = run(&["form", "tabs", out.arg(), "--policy", "diagonal"]);
    assert!(!bad_policy.status.success());
    assert!(
        stderr(&bad_policy).contains("unknown tab policy"),
        "{}",
        stderr(&bad_policy)
    );

    let minimal = root_fixture("minimal.pdf");
    let no_form = run(&["form", "tabs", minimal.to_str().expect("utf-8"), "--json"]);
    assert!(no_form.status.success(), "stderr: {}", stderr(&no_form));
    let value: Value = serde_json::from_str(&stdout(&no_form)).expect("valid JSON");
    assert_eq!(value["widgets"], 0);
}

#[test]
fn form_actions_report_supported_and_rejected_entries() {
    use pdfxi_core::{Dictionary, Document, Object, decode_text_string};

    fn action(subtype: &str) -> Object {
        let mut dictionary = Dictionary::new();
        dictionary.insert("S".to_owned(), Object::Name(subtype.to_owned()));
        Object::Dictionary(dictionary)
    }

    let out = TempOut::new("pdf");
    let created = run(&[
        "form",
        "create",
        out.arg(),
        "--field",
        "text:patient:72,700,200,24",
        "--field",
        "combo:state:72,660,200,24:CA|NY",
    ]);
    assert!(created.status.success(), "stderr: {}", stderr(&created));

    // Stage a main /A on patient and a /AA with K + F on state.
    let mut document =
        Document::from_bytes(fs::read(&out.path).expect("form bytes")).expect("form parses");
    let root = document.root().expect("catalog");
    let catalog = document
        .object(root)
        .expect("catalog object")
        .as_dictionary()
        .expect("catalog dictionary")
        .clone();
    let acroform_id = catalog
        .get("AcroForm")
        .and_then(Object::as_reference)
        .expect("AcroForm reference");
    let acroform = document
        .object(acroform_id)
        .expect("AcroForm object")
        .as_dictionary()
        .expect("AcroForm dictionary")
        .clone();
    let fields = acroform
        .get("Fields")
        .and_then(Object::as_array)
        .expect("Fields array")
        .to_vec();
    let mut patched = 0;
    for field in fields {
        let Some(id) = field.as_reference() else {
            continue;
        };
        let mut dictionary = document
            .object(id)
            .expect("field object")
            .as_dictionary()
            .expect("field dictionary")
            .clone();
        let name = dictionary
            .get("T")
            .and_then(|value| match value {
                Object::String(bytes) => Some(decode_text_string(bytes)),
                _ => None,
            })
            .unwrap_or_default();
        match name.as_str() {
            "patient" => {
                let mut reset = Dictionary::new();
                reset.insert("S".to_owned(), Object::Name("ResetForm".to_owned()));
                reset.insert(
                    "Fields".to_owned(),
                    Object::Array(vec![Object::String(b"patient".to_vec())]),
                );
                dictionary.insert("A".to_owned(), Object::Dictionary(reset));
                patched += 1;
            }
            "state" => {
                let mut additional = Dictionary::new();
                additional.insert("K".to_owned(), action("ResetForm"));
                additional.insert("F".to_owned(), action("JavaScript"));
                dictionary.insert("AA".to_owned(), Object::Dictionary(additional));
                patched += 1;
            }
            _ => {}
        }
        document
            .set_object(id, Object::Dictionary(dictionary))
            .expect("stage field");
    }
    assert_eq!(patched, 2, "both fields patched");
    let patched_bytes = document.save().expect("save patched form");
    let patched_pdf = TempPath::new("pdf");
    patched_pdf.write(&patched_bytes);
    let artifact = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/lanes/forms/t0297");
    if fs::create_dir_all(&artifact).is_ok() {
        let _ = fs::write(artifact.join("actions.pdf"), &patched_bytes);
    }

    let actions = run(&["form", "actions", patched_pdf.arg(), "--json"]);
    assert!(actions.status.success(), "stderr: {}", stderr(&actions));
    let value: Value = serde_json::from_str(&stdout(&actions)).expect("valid JSON");
    assert_eq!(value["present"], true);
    let steps = value["steps"].as_array().expect("steps");
    assert_eq!(steps.len(), 2, "{}", stdout(&actions));
    assert_eq!(steps[0]["field"], "patient");
    assert_eq!(steps[0]["site"], "A");
    assert_eq!(steps[0]["action"], "ResetForm");
    assert_eq!(steps[0]["fields"][0], "patient");
    assert_eq!(steps[1]["field"], "state");
    assert_eq!(steps[1]["site"], "K");
    let unsupported = value["unsupported"].as_array().expect("unsupported");
    assert_eq!(unsupported.len(), 1, "{}", stdout(&actions));
    assert_eq!(unsupported[0]["field"], "state");
    assert_eq!(unsupported[0]["site"], "F");
    assert_eq!(unsupported[0]["action_type"], "JavaScript");
    assert!(
        unsupported[0]["reason"]
            .as_str()
            .unwrap_or_default()
            .contains("JavaScript"),
        "{}",
        stdout(&actions)
    );

    let minimal = root_fixture("minimal.pdf");
    let no_form = run(&[
        "form",
        "actions",
        minimal.to_str().expect("utf-8"),
        "--json",
    ]);
    assert!(no_form.status.success(), "stderr: {}", stderr(&no_form));
    let value: Value = serde_json::from_str(&stdout(&no_form)).expect("valid JSON");
    assert_eq!(value["present"], false);
    assert_eq!(value["steps"].as_array().expect("steps").len(), 0);
}

#[test]
fn form_create_and_list_reject_bad_input() {
    let out = TempOut::new("pdf");
    let no_fields = run(&["form", "create", out.arg()]);
    assert!(!no_fields.status.success());
    assert!(
        stderr(&no_fields).contains("--field"),
        "{}",
        stderr(&no_fields)
    );

    let bad_rect = run(&[
        "form",
        "create",
        out.arg(),
        "--field",
        "text:name:72,700,200",
    ]);
    assert!(!bad_rect.status.success());
    assert!(
        stderr(&bad_rect).contains("rectangle"),
        "{}",
        stderr(&bad_rect)
    );

    let duplicate = run(&[
        "form",
        "create",
        out.arg(),
        "--field",
        "text:name:72,700,200,24",
        "--field",
        "checkbox:name:72,660,14,14",
    ]);
    assert!(!duplicate.status.success());
    assert!(
        stderr(&duplicate).contains("duplicate field name"),
        "{}",
        stderr(&duplicate)
    );

    let text_options = run(&[
        "form",
        "create",
        out.arg(),
        "--field",
        "text:name:72,700,200,24:unexpected",
    ]);
    assert!(!text_options.status.success());

    let radio_without_options = run(&[
        "form",
        "create",
        out.arg(),
        "--field",
        "radio:color:72,620,14,14",
    ]);
    assert!(!radio_without_options.status.success());
    assert!(
        stderr(&radio_without_options).contains("option"),
        "{}",
        stderr(&radio_without_options)
    );

    let off_page = run(&[
        "form",
        "create",
        out.arg(),
        "--field",
        "text:name:600,700,200,24",
    ]);
    assert!(!off_page.status.success());
    assert!(
        stderr(&off_page).contains("outside"),
        "{}",
        stderr(&off_page)
    );

    let minimal = root_fixture("minimal.pdf");
    let no_form = run(&["form", "list", minimal.to_str().expect("utf-8"), "--json"]);
    assert!(no_form.status.success(), "stderr: {}", stderr(&no_form));
    let value: Value = serde_json::from_str(&stdout(&no_form)).expect("valid JSON");
    assert_eq!(value["present"], false);
    assert_eq!(value["fields"].as_array().expect("fields").len(), 0);
}

#[test]
fn export_postscript_writes_positioned_dsc() {
    let source = create_text_pdf("PostScript export line one.\nSecond line.");
    let out = TempOut::new("ps");
    let exported = run(&[
        "export",
        "postscript",
        source.arg(),
        "--out",
        out.arg(),
        "--json",
    ]);
    assert!(exported.status.success(), "stderr: {}", stderr(&exported));
    let value: Value = serde_json::from_str(&stdout(&exported)).expect("valid JSON");
    assert_eq!(value["pages"], 1);
    assert_eq!(value["runs"], 2);
    assert!(
        value["title"]
            .as_str()
            .unwrap_or("")
            .starts_with("pdfxi-cli-path"),
        "{value}"
    );
    assert!(value["bytes"].as_u64().unwrap_or(0) > 0);
    let omitted: Vec<&str> = value["omitted"]
        .as_array()
        .expect("omitted")
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert_eq!(omitted, ["vector art", "images", "annotations"]);

    let bytes = fs::read(&out.path).expect("PostScript output");
    let text = String::from_utf8_lossy(&bytes);
    assert!(text.starts_with("%!PS-Adobe-3.0"), "{text}");
    assert!(text.contains("%%Pages: 1"), "{text}");
    assert!(text.contains("%%BoundingBox: 0 0 612 792"), "{text}");
    assert!(text.contains("PostScript export line one."), "{text}");
    assert!(text.contains("Second line."), "{text}");

    let again = TempOut::new("ps");
    let repeated = run(&["export", "postscript", source.arg(), "--out", again.arg()]);
    assert!(repeated.status.success(), "stderr: {}", stderr(&repeated));
    assert_eq!(
        bytes,
        fs::read(&again.path).expect("second PostScript output"),
        "PostScript export is deterministic"
    );
}

#[test]
fn export_postscript_handles_pages_and_missing_inputs() {
    let source = root_fixture("realworld/powerpoint-slides.pdf");
    let out = TempOut::new("ps");
    let exported = run(&[
        "export",
        "postscript",
        source.to_str().expect("utf-8"),
        "--out",
        out.arg(),
        "--json",
    ]);
    assert!(exported.status.success(), "stderr: {}", stderr(&exported));
    let value: Value = serde_json::from_str(&stdout(&exported)).expect("valid JSON");
    assert_eq!(value["pages"], 3);
    assert!(value["runs"].as_u64().unwrap_or(0) > 0);
    let bytes = fs::read(&out.path).expect("PostScript");
    let text = String::from_utf8_lossy(&bytes);
    assert!(text.contains("%%Pages: 3"), "{text}");
    assert!(text.contains("%%Page: 1 1"), "{text}");
    assert!(text.contains("%%Page: 3 3"), "{text}");

    let missing = run(&[
        "export",
        "postscript",
        TempPath::new("pdf").arg(),
        "--out",
        TempOut::new("ps").arg(),
    ]);
    assert!(!missing.status.success());
    assert!(stderr(&missing).contains("read"), "{}", stderr(&missing));
}

#[test]
fn home_state_tracks_recents_favorites_and_collections() {
    let dir = TempDir::new();
    let state = dir.child("home.txt");
    let first = create_text_pdf("First home document.");
    let second = create_text_pdf("Second home document.");

    let added = run(&[
        "home",
        "add",
        first.arg(),
        "--state",
        state.to_str().expect("state"),
        "--json",
    ]);
    assert!(added.status.success(), "stderr: {}", stderr(&added));
    let added = run(&[
        "home",
        "add",
        second.arg(),
        "--state",
        state.to_str().expect("state"),
        "--json",
    ]);
    assert!(added.status.success(), "stderr: {}", stderr(&added));
    let favorite = run(&[
        "home",
        "favorite",
        first.arg(),
        "--state",
        state.to_str().expect("state"),
        "--json",
    ]);
    assert!(favorite.status.success(), "stderr: {}", stderr(&favorite));
    let collection = run(&[
        "home",
        "collection",
        "Case files",
        "--add",
        first.arg(),
        "--state",
        state.to_str().expect("state"),
        "--json",
    ]);
    assert!(
        collection.status.success(),
        "stderr: {}",
        stderr(&collection)
    );

    let listed = run(&[
        "home",
        "list",
        "--state",
        state.to_str().expect("state"),
        "--json",
    ]);
    assert!(listed.status.success(), "stderr: {}", stderr(&listed));
    let value: Value = serde_json::from_str(&stdout(&listed)).expect("valid JSON");
    let recents = value["recents"].as_array().expect("recents");
    assert_eq!(recents.len(), 2);
    assert!(
        recents[0]["title"]
            .as_str()
            .unwrap_or("")
            .starts_with("pdfxi-cli-path"),
        "{recents:?}"
    );
    assert_eq!(value["favorites"].as_array().expect("favorites").len(), 1);
    let collections = value["collections"].as_array().expect("collections");
    assert_eq!(collections[0]["name"], "Case files");
    assert_eq!(
        collections[0]["members"].as_array().expect("members").len(),
        1
    );

    let removed = run(&[
        "home",
        "favorite",
        first.arg(),
        "--remove",
        "--state",
        state.to_str().expect("state"),
        "--json",
    ]);
    assert!(removed.status.success(), "stderr: {}", stderr(&removed));
    let deleted = run(&[
        "home",
        "collection",
        "Case files",
        "--delete",
        "--state",
        state.to_str().expect("state"),
        "--json",
    ]);
    assert!(deleted.status.success(), "stderr: {}", stderr(&deleted));
    let value: Value = serde_json::from_str(&stdout(&deleted)).expect("valid JSON");
    assert!(value["favorites"].as_array().expect("favorites").is_empty());
    assert!(
        value["collections"]
            .as_array()
            .expect("collections")
            .is_empty()
    );

    let reloaded = run(&[
        "home",
        "list",
        "--state",
        state.to_str().expect("state"),
        "--json",
    ]);
    assert!(reloaded.status.success(), "stderr: {}", stderr(&reloaded));
    let value: Value = serde_json::from_str(&stdout(&reloaded)).expect("valid JSON");
    assert_eq!(value["recents"].as_array().expect("recents").len(), 2);
}

#[test]
fn home_rejects_bad_state_and_missing_documents() {
    let dir = TempDir::new();
    let state = dir.child("home.txt");

    let missing = run(&[
        "home",
        "add",
        dir.child("absent.pdf").to_str().expect("path"),
        "--state",
        state.to_str().expect("state"),
    ]);
    assert!(!missing.status.success());
    assert!(
        stderr(&missing).contains("does not exist"),
        "{}",
        stderr(&missing)
    );

    let empty_name = run(&[
        "home",
        "collection",
        "",
        "--state",
        state.to_str().expect("state"),
    ]);
    assert!(!empty_name.status.success());

    fs::write(&state, b"garbage\n").expect("write corrupt state");
    let corrupt = run(&["home", "list", "--state", state.to_str().expect("state")]);
    assert!(!corrupt.status.success());
    assert!(
        stderr(&corrupt).contains("malformed"),
        "{}",
        stderr(&corrupt)
    );
}

#[test]
fn unicode_analyze_reports_scripts_and_fallback() {
    let text = "hello \u{05e9}\u{05dc}\u{05d5}\u{05dd} \u{4e16}\u{754c}";
    let analyzed = run(&[
        "unicode",
        "analyze",
        "--text",
        text,
        "--font",
        "Latin=Latin",
        "--font",
        "Hebrew=Hebrew",
        "--font",
        "CJK=CJK",
        "--font",
        "Neutral=Other",
        "--json",
    ]);
    assert!(analyzed.status.success(), "stderr: {}", stderr(&analyzed));
    let value: Value = serde_json::from_str(&stdout(&analyzed)).expect("valid JSON");
    assert_eq!(value["source"], "--text");
    assert_eq!(value["base_direction"], "ltr");
    assert!(value["dominant"].as_str().is_some());
    let runs = value["runs"].as_array().expect("runs");
    assert!(
        runs.iter()
            .any(|run| run["script"] == "hebrew" && run["direction"] == "rtl"),
        "{runs:?}"
    );
    assert!(runs.iter().any(|run| run["script"] == "cjk"), "{runs:?}");
    let fallback = value["fallback"].as_array().expect("fallback");
    let hebrew = fallback
        .iter()
        .find(|step| step["script"] == "hebrew")
        .expect("hebrew fallback");
    assert_eq!(hebrew["chain"], serde_json::json!(["Hebrew"]));
    assert!(
        value["fallback_summary"]
            .as_str()
            .unwrap_or("")
            .contains("hebrew:Hebrew")
    );

    let pdf = create_text_pdf("hello world");
    let analyzed = run(&["unicode", "analyze", pdf.arg(), "--json"]);
    assert!(analyzed.status.success(), "stderr: {}", stderr(&analyzed));
    let value: Value = serde_json::from_str(&stdout(&analyzed)).expect("valid JSON");
    assert!(value["source"].as_str().unwrap_or("").ends_with(".pdf"));
    assert_eq!(value["dominant"], "latin");
    assert_eq!(value["base_direction"], "ltr");
    assert!(value["characters"].as_u64().unwrap_or(0) >= 11);
}

#[test]
fn unicode_analyze_rejects_bad_inputs() {
    let unknown = run(&[
        "unicode",
        "analyze",
        "--text",
        "abc",
        "--font",
        "Latin=Klingon",
    ]);
    assert!(!unknown.status.success());
    assert!(
        stderr(&unknown).contains("unknown script"),
        "{}",
        stderr(&unknown)
    );

    let blank = run(&["unicode", "analyze", "--text", "   "]);
    assert!(!blank.status.success());
    assert!(stderr(&blank).contains("empty"), "{}", stderr(&blank));

    let missing_font = run(&[
        "unicode",
        "analyze",
        "--text",
        "\u{05e9}\u{05dc}\u{05d5}\u{05dd}",
        "--font",
        "Latin=Latin",
    ]);
    assert!(!missing_font.status.success());
    assert!(
        stderr(&missing_font).contains("no font candidate"),
        "{}",
        stderr(&missing_font)
    );

    let missing = run(&["unicode", "analyze", TempPath::new("pdf").arg()]);
    assert!(!missing.status.success());

    let both = run(&[
        "unicode",
        "analyze",
        TempPath::new("pdf").arg(),
        "--text",
        "abc",
    ]);
    assert!(!both.status.success());
}

#[test]
fn transparency_inspect_reads_groups_and_blend_states() {
    let source = TempPath::new("pdf");
    source.write(&transparency_pdf());
    let inspected = run(&["transparency", "inspect", source.arg(), "--json"]);
    assert!(inspected.status.success(), "stderr: {}", stderr(&inspected));
    let value: Value = serde_json::from_str(&stdout(&inspected)).expect("valid JSON");
    assert_eq!(value["pages"], 1);
    assert_eq!(value["groups"], 1);
    assert_eq!(value["states"], 1);
    assert_eq!(value["modes"], serde_json::json!(["Multiply", "Screen"]));
    let rows = value["rows"].as_array().expect("rows");
    assert_eq!(rows.len(), 2);
    let group = rows
        .iter()
        .find(|row| row["source"] == "group")
        .expect("group row");
    assert_eq!(group["mode"], "Screen");
    assert_eq!(group["isolation"], "Isolated");
    assert_eq!(group["knockout"], "NonKnockout");
    assert_eq!(group["descriptor"], "Isolated|NonKnockout|Screen");
    let state = rows
        .iter()
        .find(|row| row["source"] == "ExtGState /GS1")
        .expect("state row");
    assert_eq!(state["mode"], "Multiply");
    assert_eq!(state["source_alpha"], 128);
    assert_eq!(state["backdrop_alpha"], 256);
    assert_eq!(state["descriptor"], "Multiply ca=128 CA=256");
}

#[test]
fn transparency_inspect_rejects_unknown_modes_and_missing_files() {
    let source = create_text_pdf("Bogus mode.");
    let bytes = fs::read(&source.path).expect("read text PDF");
    let mut document = pdfxi_core::Document::from_bytes(bytes).expect("parse");
    let page = document.page(0).expect("page");
    let mut page_dictionary = document
        .object(page.id)
        .expect("page object")
        .as_dictionary()
        .cloned()
        .expect("page dictionary");
    let mut group = pdfxi_core::Dictionary::new();
    group.insert(
        "S".to_owned(),
        pdfxi_core::Object::Name("Transparency".to_owned()),
    );
    group.insert(
        "BM".to_owned(),
        pdfxi_core::Object::Name("Bogus".to_owned()),
    );
    page_dictionary.insert("Group".to_owned(), pdfxi_core::Object::Dictionary(group));
    document
        .set_object(page.id, pdfxi_core::Object::Dictionary(page_dictionary))
        .expect("write page");
    let path = TempPath::new("pdf");
    path.write(&document.save().expect("save"));
    let rejected = run(&["transparency", "inspect", path.arg()]);
    assert!(!rejected.status.success());
    assert!(
        stderr(&rejected).contains("Unknown blend mode")
            || stderr(&rejected).contains("blend mode"),
        "{}",
        stderr(&rejected)
    );

    let missing = run(&["transparency", "inspect", TempPath::new("pdf").arg()]);
    assert!(!missing.status.success());
    assert!(stderr(&missing).contains("read"), "{}", stderr(&missing));
}

#[test]
fn find_files_ranks_and_groups_matches() {
    let dir = TempDir::new();
    let one = dir.child("one.pdf");
    let two = dir.child("two.pdf");
    let one_source = create_text_pdf("needle here and needle again.");
    fs::write(&one, fs::read(&one_source.path).expect("one")).expect("write one");
    let two_source = create_text_pdf("one needle only.");
    fs::write(&two, fs::read(&two_source.path).expect("two")).expect("write two");

    let found = run(&["find-files", dir.arg(), "--query", "needle", "--json"]);
    assert!(found.status.success(), "stderr: {}", stderr(&found));
    let value: Value = serde_json::from_str(&stdout(&found)).expect("valid JSON");
    assert_eq!(value["files_scanned"], 2);
    assert_eq!(value["files_matched"], 2);
    assert_eq!(value["total_matches"], 3);
    assert_eq!(value["reported_matches"], 3);
    assert_eq!(value["truncated"], false);
    let files = value["files"].as_array().expect("files");
    assert!(files[0]["file"].as_str().unwrap_or("").ends_with("one.pdf"));
    assert_eq!(files[0]["matches"], 2);
    assert_eq!(files[0]["pages"][0]["page"], 1);
    assert_eq!(files[0]["pages"][0]["matches"], 2);
    assert!(
        files[0]["pages"][0]["snippet"]
            .as_str()
            .unwrap_or("")
            .contains("needle here")
    );
    assert_eq!(files[1]["matches"], 1);

    let text = run(&["find-files", dir.arg(), "--query", "needle"]);
    assert!(text.status.success(), "stderr: {}", stderr(&text));
    let lines = stdout(&text);
    assert!(lines.contains("files_matched: 2"), "{lines}");
    assert!(lines.contains("page 1"), "{lines}");
}

#[test]
fn find_files_applies_caps_and_rejects_bad_inputs() {
    let dir = TempDir::new();
    let one = dir.child("one.pdf");
    let one_source = create_text_pdf("needle here and needle again.");
    fs::write(&one, fs::read(&one_source.path).expect("one")).expect("write one");
    let two = dir.child("two.pdf");
    let two_source = create_text_pdf("one needle only.");
    fs::write(&two, fs::read(&two_source.path).expect("two")).expect("write two");

    let capped = run(&[
        "find-files",
        dir.arg(),
        "--query",
        "needle",
        "--max-results",
        "2",
        "--json",
    ]);
    assert!(capped.status.success(), "stderr: {}", stderr(&capped));
    let value: Value = serde_json::from_str(&stdout(&capped)).expect("valid JSON");
    assert_eq!(value["reported_matches"], 2);
    assert_eq!(value["truncated"], true);
    assert_eq!(value["files_matched"], 1);

    let per_file = run(&[
        "find-files",
        dir.arg(),
        "--query",
        "needle",
        "--max-per-file",
        "1",
        "--json",
    ]);
    assert!(per_file.status.success(), "stderr: {}", stderr(&per_file));
    let value: Value = serde_json::from_str(&stdout(&per_file)).expect("valid JSON");
    assert_eq!(value["reported_matches"], 2);
    assert_eq!(value["truncated"], true);

    let case = run(&[
        "find-files",
        dir.arg(),
        "--query",
        "NEEDLE",
        "--case-sensitive",
        "--json",
    ]);
    assert!(case.status.success(), "stderr: {}", stderr(&case));
    let value: Value = serde_json::from_str(&stdout(&case)).expect("valid JSON");
    assert_eq!(value["total_matches"], 0);

    let explicit = run(&[
        "find-files",
        one.to_str().expect("one"),
        "--query",
        "needle",
        "--json",
    ]);
    assert!(explicit.status.success(), "stderr: {}", stderr(&explicit));
    let value: Value = serde_json::from_str(&stdout(&explicit)).expect("valid JSON");
    assert_eq!(value["files_scanned"], 1);
    assert_eq!(value["total_matches"], 2);

    let missing = run(&[
        "find-files",
        dir.child("absent").to_str().expect("path"),
        "--query",
        "needle",
    ]);
    assert!(!missing.status.success());
    assert!(
        stderr(&missing).contains("does not exist"),
        "{}",
        stderr(&missing)
    );

    let empty = run(&["find-files", dir.arg(), "--query", ""]);
    assert!(!empty.status.success());
}

#[test]
fn export_conformance_plan_and_profiles_preserve_text() {
    let source = root_fixture("minimal.pdf");
    let plan = run(&[
        "export",
        "conformance",
        source.to_str().expect("utf-8"),
        "--profile",
        "pdfa-2b",
        "--plan",
        "--json",
    ]);
    assert!(plan.status.success(), "stderr: {}", stderr(&plan));
    let value: Value = serde_json::from_str(&stdout(&plan)).expect("valid JSON");
    assert_eq!(value["target"], "PDF/A-2b");
    assert_eq!(value["minimum_version"], "1.7");
    assert_eq!(value["certification_claimed"], false);
    assert!(
        value["steps"].as_array().expect("steps").len() >= 5,
        "{}",
        stdout(&plan)
    );

    let source_text = run(&["text", source.to_str().expect("utf-8")]);
    let pdfa = TempOut::new("pdf");
    let normalized = run(&[
        "export",
        "conformance",
        source.to_str().expect("utf-8"),
        "--out",
        pdfa.arg(),
        "--profile",
        "pdfa-2b",
        "--json",
    ]);
    assert!(
        normalized.status.success(),
        "stderr: {}",
        stderr(&normalized)
    );
    let value: Value = serde_json::from_str(&stdout(&normalized)).expect("valid JSON");
    assert_eq!(value["target"], "PDF/A-2b");
    assert_eq!(value["certification_claimed"], false);
    assert_eq!(value["pages_verified"], 1);
    assert_eq!(value["output_version"], "1.7");
    let enforced = value["enforced"].as_array().expect("enforced");
    assert!(enforced.iter().any(|item| item == "xmp_metadata_presence"));
    assert!(enforced.iter().any(|item| item == "output_intent_presence"));
    assert!(
        value["findings"]
            .as_array()
            .expect("findings")
            .iter()
            .any(|finding| finding["code"] == "EXPORT.XMP_PLACEHOLDER_ADDED")
    );
    let saved_text = run(&["text", pdfa.arg()]);
    assert!(
        saved_text.status.success(),
        "stderr: {}",
        stderr(&saved_text)
    );
    assert_eq!(stdout(&source_text), stdout(&saved_text));

    let pdfe = TempOut::new("pdf");
    let engineering = run(&[
        "export",
        "conformance",
        source.to_str().expect("utf-8"),
        "--out",
        pdfe.arg(),
        "--profile",
        "pdfe-1",
        "--json",
    ]);
    assert!(
        engineering.status.success(),
        "stderr: {}",
        stderr(&engineering)
    );
    let value: Value = serde_json::from_str(&stdout(&engineering)).expect("valid JSON");
    assert_eq!(value["target"], "PDF/E-1");
    assert_eq!(value["enforced"], serde_json::json!(["minimum_version"]));

    let unsupported = run(&[
        "export",
        "conformance",
        source.to_str().expect("utf-8"),
        "--out",
        pdfa.arg(),
        "--profile",
        "pdfa-9",
    ]);
    assert!(!unsupported.status.success());
    assert!(
        stderr(&unsupported).contains("PDF/A-1b"),
        "{}",
        stderr(&unsupported)
    );

    let contradictory = run(&[
        "export",
        "conformance",
        source.to_str().expect("utf-8"),
        "--out",
        pdfa.arg(),
        "--profile",
        "pdfe-1",
        "--output-intent",
        "GTS_PDFA1",
    ]);
    assert!(!contradictory.status.success());
    assert!(
        stderr(&contradictory).contains("does not use output intents"),
        "{}",
        stderr(&contradictory)
    );

    let missing_out = run(&[
        "export",
        "conformance",
        source.to_str().expect("utf-8"),
        "--profile",
        "pdfa-2b",
    ]);
    assert!(!missing_out.status.success());
}

#[test]
fn export_conformance_forbidden_entry_policy_is_typed() {
    use pdfxi_core::{Document, Object};

    let mut document =
        Document::from_bytes(pdfxi_testkit::minimal_pdf()).expect("minimal fixture parses");
    let root = document.root().expect("catalog id");
    let mut catalog = document
        .object(root)
        .expect("catalog object")
        .as_dictionary()
        .expect("catalog dictionary")
        .clone();
    catalog.insert("JavaScript".to_owned(), Object::Name("Example".to_owned()));
    document
        .set_object(root, Object::Dictionary(catalog))
        .expect("stage catalog");
    let bytes = document.save().expect("save fixture with /JavaScript");
    let source = TempPath::new("pdf");
    source.write(&bytes);
    let out = TempOut::new("pdf");

    let rejected = run(&[
        "export",
        "conformance",
        source.arg(),
        "--out",
        out.arg(),
        "--profile",
        "pdfa-1b",
        "--policy",
        "reject",
    ]);
    eprintln!(
        "REJECTED status={:?} stdout={} stderr={}",
        rejected.status,
        stdout(&rejected),
        stderr(&rejected)
    );
    assert!(!rejected.status.success());
    assert!(
        stderr(&rejected).contains("forbids"),
        "{}",
        stderr(&rejected)
    );

    let reported = run(&[
        "export",
        "conformance",
        source.arg(),
        "--out",
        out.arg(),
        "--profile",
        "pdfa-1b",
        "--policy",
        "report",
        "--json",
    ]);
    assert!(reported.status.success(), "stderr: {}", stderr(&reported));
    let value: Value = serde_json::from_str(&stdout(&reported)).expect("valid JSON");
    let entries = value["present_forbidden_entries"]
        .as_array()
        .expect("forbidden entries");
    assert!(
        entries.iter().any(|entry| entry == "/JavaScript"),
        "{}",
        stdout(&reported)
    );
    assert_eq!(value["certification_claimed"], false);

    let bad_policy = run(&[
        "export",
        "conformance",
        source.arg(),
        "--out",
        out.arg(),
        "--profile",
        "pdfa-1b",
        "--policy",
        "ignore",
    ]);
    assert!(!bad_policy.status.success());
    assert!(
        stderr(&bad_policy).contains("--policy"),
        "{}",
        stderr(&bad_policy)
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

#[cfg(windows)]
#[test]
fn read_aloud_lists_installed_system_voices() {
    let output = run(&["read-aloud", "--voices", "--json"]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let value: Value = serde_json::from_str(&stdout(&output)).expect("valid JSON");
    let voices = value["voices"].as_array().expect("voices array");
    assert!(
        !voices.is_empty(),
        "System.Speech reported no installed voice: {value}"
    );
    assert!(
        voices
            .iter()
            .all(|voice| { voice.as_str().is_some_and(|name| !name.trim().is_empty()) })
    );
}

#[cfg(windows)]
#[test]
fn read_aloud_speaks_the_plan_through_system_speech() {
    let pdf = create_text_pdf("Read aloud works.");
    let output = run(&[
        "read-aloud",
        pdf.arg(),
        "--wpm",
        "400",
        "--speak",
        "--voice",
        "default",
        "--json",
    ]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let value: Value = serde_json::from_str(&stdout(&output)).expect("valid JSON");
    assert_eq!(value["spoken"].as_bool(), Some(true));
    assert_eq!(
        value["utterances"].as_array().map(|rows| rows.len()),
        Some(1)
    );
}

#[test]
fn read_aloud_rejects_invalid_system_speech_requests() {
    let pdf = create_text_pdf("Speech request.");
    let no_file = run(&["read-aloud"]);
    assert!(!no_file.status.success());
    assert!(
        stderr(&no_file).contains("pass a PDF file"),
        "{}",
        stderr(&no_file)
    );

    let conflict = run(&["read-aloud", pdf.arg(), "--voices"]);
    assert!(!conflict.status.success());
    assert!(
        stderr(&conflict).contains("cannot be used with"),
        "{}",
        stderr(&conflict)
    );

    let quoted = run(&["read-aloud", pdf.arg(), "--voice", "Bad'Voice"]);
    assert!(!quoted.status.success());
    assert!(stderr(&quoted).contains("quote"), "{}", stderr(&quoted));
}

// ---------------------------------------------------------------------------
// color: ICC profiles and output intents (T-0331)
// ---------------------------------------------------------------------------

/// Write a big-endian four-character signature.
fn t0331_sig(buf: &mut [u8], offset: usize, sig: &[u8; 4]) {
    buf[offset..offset + 4].copy_from_slice(sig);
}

/// Write a big-endian `u32`.
fn t0331_u32(buf: &mut [u8], offset: usize, value: u32) {
    buf[offset..offset + 4].copy_from_slice(&value.to_be_bytes());
}

/// Write an ICC `s15Fixed16` number.
fn t0331_fixed(buf: &mut [u8], offset: usize, value: f32) {
    t0331_u32(buf, offset, (f64::from(value) * 65536.0) as u32);
}

/// An `XYZType` tag payload.
fn t0331_xyz(x: f32, y: f32, z: f32) -> Vec<u8> {
    let mut data = vec![0u8; 20];
    t0331_sig(&mut data, 0, b"XYZ ");
    t0331_fixed(&mut data, 8, x);
    t0331_fixed(&mut data, 12, y);
    t0331_fixed(&mut data, 16, z);
    data
}

/// A single-entry `curv` tag payload.
fn t0331_gamma(gamma: f32) -> Vec<u8> {
    let mut data = vec![0u8; 14];
    t0331_sig(&mut data, 0, b"curv");
    t0331_u32(&mut data, 8, 1);
    data[12..14].copy_from_slice(&((f64::from(gamma) * 256.0) as u16).to_be_bytes());
    data
}

/// A `textDescriptionType` payload with only the ASCII section.
fn t0331_desc(text: &str) -> Vec<u8> {
    let ascii = text.as_bytes();
    let mut data = vec![0u8; 12 + ascii.len() + 1];
    t0331_sig(&mut data, 0, b"desc");
    t0331_u32(&mut data, 8, (ascii.len() + 1) as u32);
    data[12..12 + ascii.len()].copy_from_slice(ascii);
    data
}

/// Assemble a synthetic ICC profile from tag payloads.
fn t0331_profile(class: &[u8; 4], space: &[u8; 4], tags: &[([u8; 4], Vec<u8>)]) -> Vec<u8> {
    let mut cursor = 128 + 4 + tags.len() * 12;
    let mut offsets = Vec::with_capacity(tags.len());
    for (_, data) in tags {
        offsets.push(cursor);
        cursor += data.len();
    }
    let mut buf = vec![0u8; cursor];
    t0331_u32(&mut buf, 0, cursor as u32);
    t0331_sig(&mut buf, 4, b"ADBE");
    t0331_sig(&mut buf, 8, b"\x02\x40\x00\x00");
    t0331_sig(&mut buf, 12, class);
    t0331_sig(&mut buf, 16, space);
    t0331_sig(&mut buf, 20, b"XYZ ");
    t0331_sig(&mut buf, 36, b"acsp");
    t0331_u32(&mut buf, 64, 1);
    t0331_fixed(&mut buf, 68, 0.9642);
    t0331_fixed(&mut buf, 72, 1.0);
    t0331_fixed(&mut buf, 76, 0.8249);
    t0331_u32(&mut buf, 128, tags.len() as u32);
    for (index, ((sig, data), offset)) in tags.iter().zip(offsets).enumerate() {
        let base = 128 + 4 + index * 12;
        t0331_sig(&mut buf, base, sig);
        t0331_u32(&mut buf, base + 4, offset as u32);
        t0331_u32(&mut buf, base + 8, data.len() as u32);
        buf[offset..offset + data.len()].copy_from_slice(data);
    }
    buf
}

/// A synthetic RGB display profile with a colourant matrix and gamma TRCs.
fn t0331_rgb_profile() -> Vec<u8> {
    t0331_profile(
        b"mntr",
        b"RGB ",
        &[
            (*b"desc", t0331_desc("Synthetic sRGB-like")),
            (*b"wtpt", t0331_xyz(0.9642, 1.0, 0.8249)),
            (*b"rXYZ", t0331_xyz(0.4360, 0.2225, 0.0139)),
            (*b"gXYZ", t0331_xyz(0.3851, 0.7169, 0.0971)),
            (*b"bXYZ", t0331_xyz(0.1431, 0.0606, 0.7141)),
            (*b"rTRC", t0331_gamma(2.2)),
            (*b"gTRC", t0331_gamma(2.2)),
            (*b"bTRC", t0331_gamma(2.2)),
        ],
    )
}

/// A synthetic gray print profile with one gamma TRC.
fn t0331_gray_profile() -> Vec<u8> {
    t0331_profile(
        b"prtr",
        b"GRAY",
        &[
            (*b"desc", t0331_desc("Synthetic gray gamma 2.2")),
            (*b"wtpt", t0331_xyz(0.9642, 1.0, 0.8249)),
            (*b"kTRC", t0331_gamma(2.2)),
        ],
    )
}

/// Assemble a classic-xref PDF from object bodies and one binary stream.
fn t0331_stream_pdf(bodies: &[Vec<u8>], stream_object: usize, stream: &[u8]) -> Vec<u8> {
    let mut out = b"%PDF-1.7\n%PDFXI\n".to_vec();
    let mut offsets = Vec::with_capacity(bodies.len() + 1);
    for (index, body) in bodies.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n", index + 1).as_bytes());
        out.extend_from_slice(body);
        out.extend_from_slice(b"\nendobj\n");
    }
    offsets.push(out.len());
    out.extend_from_slice(
        format!(
            "{stream_object} 0 obj\n<< /N 3 /Length {} >>\nstream\n",
            stream.len()
        )
        .as_bytes(),
    );
    out.extend_from_slice(stream);
    out.extend_from_slice(b"\nendstream\nendobj\n");
    let xref = out.len();
    let size = offsets.len() + 1;
    out.extend_from_slice(format!("xref\n0 {size}\n0000000000 65535 f \n").as_bytes());
    for offset in &offsets {
        out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!("trailer\n<< /Size {size} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n").as_bytes(),
    );
    out
}

/// A one-page PDF with a `GTS_PDFX` output intent carrying `icc`.
fn t0331_intent_pdf(icc: &[u8]) -> Vec<u8> {
    let bodies = vec![
        b"<< /Type /Catalog /Pages 2 0 R /OutputIntents [4 0 R] >>".to_vec(),
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>".to_vec(),
        b"<< /Type /OutputIntent /S /GTS_PDFX /OutputConditionIdentifier (Synthetic RGB) \
          /Info (Colour host fixture) /RegistryName (http://example.test) \
          /DestOutputProfile 5 0 R >>"
            .to_vec(),
    ];
    t0331_stream_pdf(&bodies, 5, icc)
}

#[test]
fn color_icc_reports_header_tags_and_plan() {
    let profile = TempPath::new("icc");
    profile.write(&t0331_rgb_profile());
    let output = run(&["color", "icc", profile.arg(), "--json"]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let value: Value = serde_json::from_str(&stdout(&output)).expect("valid JSON");
    let facts = &value["profile"];
    assert_eq!(facts["version"], "2.4");
    assert_eq!(facts["cmm"], "ADBE");
    assert_eq!(facts["device_class"], "display");
    assert_eq!(facts["colour_space"], "RGB ");
    assert_eq!(facts["pcs"], "XYZ");
    assert_eq!(facts["rendering_intent"], "relative-colorimetric");
    assert_eq!(facts["tags"], 8);
    assert_eq!(facts["description"], "Synthetic sRGB-like");
    assert_eq!(facts["plan"], "rgb-matrix-trc");
    let whitepoint = facts["whitepoint"].as_array().expect("whitepoint");
    assert!((whitepoint[1].as_f64().expect("y") - 1.0).abs() < 1e-4);
    assert!(
        value["file"]
            .as_str()
            .is_some_and(|path| path.ends_with(".icc"))
    );
}

#[test]
fn color_intents_reads_embedded_profiles_from_real_pdfs() {
    let icc = t0331_rgb_profile();
    let pdf = TempPdf::new(&t0331_intent_pdf(&icc));
    let output = run(&["color", "intents", pdf.arg(), "--json"]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let value: Value = serde_json::from_str(&stdout(&output)).expect("valid JSON");
    assert_eq!(value["intents"], 1);
    assert_eq!(value["usable"], true);
    assert!(value["issues"].as_array().expect("issues").is_empty());
    let row = &value["rows"][0];
    assert_eq!(row["subtype"], "GTS_PDFX");
    assert_eq!(row["output_condition_identifier"], "Synthetic RGB");
    assert_eq!(row["info"], "Colour host fixture");
    assert_eq!(row["registry_name"], "http://example.test");
    assert_eq!(row["profile_bytes"], icc.len());
    assert_eq!(row["profile"]["plan"], "rgb-matrix-trc");
    assert_eq!(row["profile"]["colour_space"], "RGB ");

    let plain = create_text_pdf("No output intents here.");
    let output = run(&["color", "intents", plain.arg(), "--json"]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let value: Value = serde_json::from_str(&stdout(&output)).expect("valid JSON");
    assert_eq!(value["intents"], 0);
    assert_eq!(value["usable"], false);
    assert_eq!(value["issues"][0], "missing");
}

#[test]
fn color_convert_runs_the_analytic_plan() {
    let rgb = TempPath::new("icc");
    rgb.write(&t0331_rgb_profile());
    let output = run(&[
        "color",
        "convert",
        rgb.arg(),
        "--space",
        "rgb",
        "--values",
        "0.2,0.5,0.8",
        "--json",
    ]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let value: Value = serde_json::from_str(&stdout(&output)).expect("valid JSON");
    assert_eq!(value["plan"], "rgb-matrix-trc");
    assert_eq!(value["output_space"], "PCS-XYZ");
    assert_eq!(value["input"], serde_json::json!([0.2, 0.5, 0.8]));
    let converted = value["output"].as_array().expect("output");
    assert_eq!(converted.len(), 3);
    for (actual, expected) in converted.iter().zip([0.1841_f64, 0.1997, 0.4587]) {
        assert!(
            (actual.as_f64().expect("component") - expected).abs() < 1e-3,
            "{converted:?}"
        );
    }

    let gray = TempPath::new("icc");
    gray.write(&t0331_gray_profile());
    let output = run(&[
        "color",
        "convert",
        gray.arg(),
        "--space",
        "gray",
        "--values",
        "0.5",
        "--json",
    ]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let value: Value = serde_json::from_str(&stdout(&output)).expect("valid JSON");
    assert!(
        value["plan"]
            .as_str()
            .unwrap_or("")
            .starts_with("gray-gamma:"),
        "{value}"
    );
    let luminance = value["output"][0].as_f64().expect("luminance");
    assert!((luminance - 0.5_f64.powf(2.2)).abs() < 1e-3, "{luminance}");
}

#[test]
fn color_rejects_bad_profiles_and_inputs() {
    let rgb = TempPath::new("icc");
    let bytes = t0331_rgb_profile();
    rgb.write(&bytes);

    let truncated = TempPath::new("icc");
    truncated.write(&bytes[..100]);
    let output = run(&["color", "icc", truncated.arg()]);
    assert!(!output.status.success());
    assert!(stderr(&output).contains("truncated"), "{}", stderr(&output));

    let unknown_space = run(&[
        "color",
        "convert",
        rgb.arg(),
        "--space",
        "hsl",
        "--values",
        "0.1",
    ]);
    assert!(!unknown_space.status.success());
    assert!(
        stderr(&unknown_space).contains("unknown space"),
        "{}",
        stderr(&unknown_space)
    );

    let out_of_range = run(&["color", "convert", rgb.arg(), "--values", "1.5,0,0"]);
    assert!(!out_of_range.status.success());
    assert!(
        stderr(&out_of_range).contains("outside 0..=1"),
        "{}",
        stderr(&out_of_range)
    );

    let mismatch = run(&[
        "color",
        "convert",
        rgb.arg(),
        "--space",
        "cmyk",
        "--values",
        "0.1,0.2,0.3,0.4",
    ]);
    assert!(!mismatch.status.success());
    assert!(
        stderr(&mismatch).contains("expects 3 components"),
        "{}",
        stderr(&mismatch)
    );

    let missing = run(&["color", "icc", TempPath::new("icc").arg()]);
    assert!(!missing.status.success());
}

#[test]
fn accessibility_tree_reflects_real_pages_and_focus() {
    let source = real_fixture("powerpoint-slides.pdf");
    let output = run(&[
        "accessibility",
        "tree",
        source.to_str().expect("utf-8 source"),
        "--json",
    ]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let value: Value = serde_json::from_str(&stdout(&output)).expect("valid JSON");
    assert_eq!(value["pages"], 3);
    assert_eq!(value["current_page"], 1);
    assert_eq!(value["well_formed"], true);
    assert_eq!(value["focus"], 9);
    assert_eq!(value["focus_announcement"], "powerpoint-slides, Document");
    let nodes = value["nodes"].as_array().expect("nodes");
    assert!(nodes.iter().any(|node| node["role"] == "Window"));
    assert!(
        nodes
            .iter()
            .any(|node| node["label"] == "Toggle high contrast")
    );
    assert!(
        nodes
            .iter()
            .any(|node| node["label"] == "Page 1" && node["role"] == "Group")
    );

    let toolbar = run(&[
        "accessibility",
        "tree",
        source.to_str().expect("utf-8 source"),
        "--focus",
        "toolbar",
        "--high-contrast",
        "--json",
    ]);
    assert!(toolbar.status.success(), "stderr: {}", stderr(&toolbar));
    let value: Value = serde_json::from_str(&stdout(&toolbar)).expect("valid JSON");
    assert_eq!(value["focus"], 3);
    assert_eq!(value["focus_announcement"], "Toolbar, Toolbar");
    assert_eq!(value["high_contrast"], true);
    let nodes = value["nodes"].as_array().expect("nodes");
    assert!(nodes.iter().any(|node| node["label"] == "High contrast on"));
}

#[test]
fn accessibility_tree_rejects_bad_pages_and_regions() {
    let source = create_text_pdf("One page.");
    let page = run(&["accessibility", "tree", source.arg(), "--page", "9"]);
    assert!(!page.status.success());
    assert!(stderr(&page).contains("outside 1..=1"), "{}", stderr(&page));

    let region = run(&["accessibility", "tree", source.arg(), "--focus", "sidebar"]);
    assert!(!region.status.success());
    assert!(
        stderr(&region).contains("unknown focus target"),
        "{}",
        stderr(&region)
    );
}

#[test]
fn accessibility_nav_audits_contrast_and_traversal() {
    let high = run(&[
        "accessibility",
        "nav",
        "--theme",
        "high-contrast",
        "--tabs",
        "3",
        "--json",
    ]);
    assert!(high.status.success(), "stderr: {}", stderr(&high));
    let value: Value = serde_json::from_str(&stdout(&high)).expect("valid JSON");
    assert_eq!(value["theme"], "high-contrast");
    assert_eq!(value["contrast_ok"], true);
    let contrasts = value["contrasts"].as_array().expect("contrasts");
    assert_eq!(contrasts.len(), 5);
    for row in contrasts {
        assert_eq!(row["required"], 7.0);
        assert_eq!(row["passes"], true);
        assert!(row["ratio"].as_f64().expect("ratio") >= 7.0);
    }
    assert!(
        contrasts
            .iter()
            .any(|row| row["name"] == "foreground/background")
    );
    let order = value["focus_order"].as_array().expect("focus order");
    assert_eq!(order.len(), 11);
    assert_eq!(order[0], "Menu bar");
    assert_eq!(
        value["traversal"],
        serde_json::json!(["Toolbar", "Document tabs", "Page thumbnails"])
    );
    let shortcuts = value["shortcuts"].as_array().expect("shortcuts");
    assert!(
        shortcuts
            .iter()
            .any(|row| row["shortcut"] == "Ctrl+Shift+H")
    );
    assert!(shortcuts.iter().any(|row| row["shortcut"] == "F6"));

    let standard = run(&[
        "accessibility",
        "nav",
        "--theme",
        "standard",
        "--tabs",
        "2",
        "--reverse",
        "--json",
    ]);
    assert!(standard.status.success(), "stderr: {}", stderr(&standard));
    let value: Value = serde_json::from_str(&stdout(&standard)).expect("valid JSON");
    assert_eq!(value["theme"], "standard");
    assert_eq!(value["contrasts"][0]["required"], 4.5);
    assert_eq!(
        value["traversal"],
        serde_json::json!(["Status bar", "Links panel"])
    );

    let neon = run(&["accessibility", "nav", "--theme", "neon"]);
    assert!(!neon.status.success());
    assert!(stderr(&neon).contains("unknown theme"), "{}", stderr(&neon));
}

#[test]
fn accessibility_layout_uses_real_page_geometry() {
    let source = create_text_pdf("Reflow geometry.");
    let output = run(&[
        "accessibility",
        "layout",
        source.arg(),
        "--viewport",
        "400x700",
        "--font-size",
        "14",
        "--margin",
        "20",
        "--json",
    ]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let value: Value = serde_json::from_str(&stdout(&output)).expect("valid JSON");
    assert_eq!(value["page_width"], 612.0);
    assert_eq!(value["page_height"], 792.0);
    assert_eq!(value["content_width"], 360.0);
    assert_eq!(value["content_height"], 660.0);
    let scale = value["scale"].as_f64().expect("scale");
    assert!((scale - 360.0 / 612.0).abs() < 1e-6, "{scale}");
    assert!(value["approx_chars_per_line"].as_u64().unwrap_or(0) > 0);
    assert!(value["max_lines_per_page"].as_u64().unwrap_or(0) > 0);

    let empty = run(&[
        "accessibility",
        "layout",
        source.arg(),
        "--viewport",
        "0x700",
    ]);
    assert!(!empty.status.success());
    assert!(
        stderr(&empty).contains("must be positive"),
        "{}",
        stderr(&empty)
    );

    let malformed = run(&[
        "accessibility",
        "layout",
        source.arg(),
        "--viewport",
        "wide",
    ]);
    assert!(!malformed.status.success());
    assert!(
        stderr(&malformed).contains("WIDTHxHEIGHT"),
        "{}",
        stderr(&malformed)
    );

    let page = run(&["accessibility", "layout", source.arg(), "--page", "4"]);
    assert!(!page.status.success());
    assert!(stderr(&page).contains("outside 1..=1"), "{}", stderr(&page));
}

// ---------------------------------------------------------------------------
// geo: geospatial viewports (T-0333)
// ---------------------------------------------------------------------------

/// A one-page PDF with one real `/VP` geospatial viewport.
fn t0333_geo_pdf() -> Vec<u8> {
    t0131_pdf(
        "1.7",
        &[
            "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /VP [4 0 R] >>".to_owned(),
            "<< /BBox [0 0 612 792] /Measure 5 0 R >>".to_owned(),
            "<< /Subtype /GEO /GPTS [-10 40 -5 45] /XStep 1 /YStep 1 /Units (degree) \
              /GCS << /Type /GEOGCS /EPSG 4326 >> >>"
                .to_owned(),
        ],
    )
}

#[test]
fn geo_info_reports_real_viewports_and_plan() {
    let pdf = TempPdf::new(&t0333_geo_pdf());
    let output = run(&["geo", "info", pdf.arg(), "--json"]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let value: Value = serde_json::from_str(&stdout(&output)).expect("valid JSON");
    assert_eq!(value["page"], 1);
    assert_eq!(value["gcs"], "EPSG:4326");
    assert_eq!(value["epsg"], 4326);
    assert_eq!(value["projection"], "geographic");
    assert_eq!(value["supported"], true);
    let viewports = value["viewports"].as_array().expect("viewports");
    assert_eq!(viewports.len(), 1);
    assert_eq!(
        viewports[0]["pdf"],
        serde_json::json!([0.0, 0.0, 612.0, 792.0])
    );
    assert_eq!(
        viewports[0]["gpts"],
        serde_json::json!([-10.0, 40.0, -5.0, 45.0])
    );
    assert_eq!(viewports[0]["unit"], "degree");
    assert!(
        value["plan"]
            .as_str()
            .unwrap_or("")
            .starts_with("geospatial-plan/v1|supported=1")
    );
    assert!(value["lpts"].is_null());
}

#[test]
fn geo_maps_between_pdf_and_geographic_space() {
    let pdf = TempPdf::new(&t0333_geo_pdf());
    let to_geo = run(&[
        "geo",
        "to-geo",
        pdf.arg(),
        "--x",
        "306",
        "--y",
        "396",
        "--json",
    ]);
    assert!(to_geo.status.success(), "stderr: {}", stderr(&to_geo));
    let value: Value = serde_json::from_str(&stdout(&to_geo)).expect("valid JSON");
    assert_eq!(value["direction"], "pdf-to-geo");
    assert_eq!(value["viewport"], 0);
    let output = value["output"].as_array().expect("output");
    assert!((output[0].as_f64().expect("lon") - -7.5).abs() < 1e-9);
    assert!((output[1].as_f64().expect("lat") - 42.5).abs() < 1e-9);

    let to_pdf = run(&[
        "geo",
        "to-pdf",
        pdf.arg(),
        "--lon",
        "-7.5",
        "--lat",
        "42.5",
        "--json",
    ]);
    assert!(to_pdf.status.success(), "stderr: {}", stderr(&to_pdf));
    let value: Value = serde_json::from_str(&stdout(&to_pdf)).expect("valid JSON");
    assert_eq!(value["direction"], "geo-to-pdf");
    let output = value["output"].as_array().expect("output");
    assert!((output[0].as_f64().expect("x") - 306.0).abs() < 1e-6);
    assert!((output[1].as_f64().expect("y") - 396.0).abs() < 1e-6);
}

#[test]
fn geo_rejects_absent_bounds_and_outside_points() {
    let plain = create_text_pdf("No geospatial viewports.");
    let absent = run(&["geo", "info", plain.arg()]);
    assert!(!absent.status.success());
    assert!(
        stderr(&absent).contains("no /VP viewports"),
        "{}",
        stderr(&absent)
    );

    let pdf = TempPdf::new(&t0333_geo_pdf());
    let outside = run(&["geo", "to-geo", pdf.arg(), "--x", "700", "--y", "396"]);
    assert!(!outside.status.success());
    assert!(
        stderr(&outside).contains("outside the selected viewport"),
        "{}",
        stderr(&outside)
    );

    let no_viewport = run(&["geo", "to-pdf", pdf.arg(), "--lon", "10", "--lat", "10"]);
    assert!(!no_viewport.status.success());
    assert!(
        stderr(&no_viewport).contains("no viewport contains"),
        "{}",
        stderr(&no_viewport)
    );

    let page = run(&["geo", "info", pdf.arg(), "--page", "2"]);
    assert!(!page.status.success());
    assert!(stderr(&page).contains("outside 1..=1"), "{}", stderr(&page));
}

// ---------------------------------------------------------------------------
// media3d: 3D annotations (T-0334)
// ---------------------------------------------------------------------------

/// A one-page PDF with a `/3D` annotation, two views and a payload stream.
fn t0334_three_d_pdf() -> (Vec<u8>, Vec<u8>) {
    let payload = b"U3D\0fake-payload-0123456789".to_vec();
    let bodies: Vec<Vec<u8>> = vec![
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Annots [4 0 R] >>".to_vec(),
        b"<< /Type /Annot /Subtype /3D /Rect [72 72 300 300] /3DD 5 0 R /3DV [6 0 R 7 0 R] \
          /3DA << /A /PI /D /L /AN true >> >>"
            .to_vec(),
    ];
    let tail: Vec<Vec<u8>> = vec![
        b"<< /Type /3DView /XN (Overview) /IN true >>".to_vec(),
        b"<< /Type /3DView /XN (Close-up) >>".to_vec(),
    ];
    let mut out = b"%PDF-1.7\n%PDFXI\n".to_vec();
    let mut offsets = Vec::new();
    for (index, body) in bodies.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n", index + 1).as_bytes());
        out.extend_from_slice(body);
        out.extend_from_slice(b"\nendobj\n");
    }
    offsets.push(out.len());
    out.extend_from_slice(
        format!(
            "5 0 obj\n<< /Type /3D /Subtype /U3D /Length {} >>\nstream\n",
            payload.len()
        )
        .as_bytes(),
    );
    out.extend_from_slice(&payload);
    out.extend_from_slice(b"\nendstream\nendobj\n");
    for (index, body) in tail.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n", index + 6).as_bytes());
        out.extend_from_slice(body);
        out.extend_from_slice(b"\nendobj\n");
    }
    let xref = out.len();
    let size = offsets.len() + 1;
    out.extend_from_slice(format!("xref\n0 {size}\n0000000000 65535 f \n").as_bytes());
    for offset in &offsets {
        out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!("trailer\n<< /Size {size} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n").as_bytes(),
    );
    (out, payload)
}

#[test]
fn media3d_reports_views_activation_and_plan() {
    let (pdf, payload) = t0334_three_d_pdf();
    let file = TempPdf::new(&pdf);
    let output = run(&["media3d", file.arg(), "--json"]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let value: Value = serde_json::from_str(&stdout(&output)).expect("valid JSON");
    assert_eq!(value["pages"], 1);
    assert_eq!(value["annotations"], 1);
    let row = &value["rows"][0];
    assert_eq!(row["page"], 1);
    assert_eq!(row["annotation"], 4);
    assert_eq!(row["format"], "u3d");
    assert_eq!(row["payload_bytes"], payload.len());
    assert_eq!(row["views"], serde_json::json!(["Overview", "Close-up"]));
    assert_eq!(row["active_view"], "Overview");
    assert_eq!(row["activation_event"], "PI");
    assert_eq!(row["activation_state"], "L");
    assert_eq!(row["animate"], true);
    let requirements = row["requirements"].as_array().expect("requirements");
    assert!(requirements.iter().any(|req| req == "decoder:u3d"));
    assert!(requirements.iter().any(|req| req == "animation"));
    assert!(
        row["plan"]
            .as_str()
            .unwrap_or("")
            .starts_with("three-d-plan/v1|stream=object:5:0|format=u3d")
    );
}

#[cfg(windows)]
#[test]
fn media3d_extracts_the_decoded_payload() {
    let (pdf, payload) = t0334_three_d_pdf();
    let file = TempPdf::new(&pdf);
    let dir = TempDir::new();
    let out = dir.child("payload.u3d");
    let output = run(&[
        "media3d",
        file.arg(),
        "--extract",
        out.to_str().expect("extract path"),
    ]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    assert!(
        stdout(&output).contains("extracted:"),
        "{}",
        stdout(&output)
    );
    assert_eq!(fs::read(&out).expect("payload written"), payload);
}

#[test]
fn media3d_rejects_documents_without_3d_annotations() {
    let plain = create_text_pdf("No 3D annotations.");
    let output = run(&["media3d", plain.arg()]);
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("no /3D annotations"),
        "{}",
        stderr(&output)
    );
}

// ---------------------------------------------------------------------------
// interactions: /AA additional actions (T-0335)
// ---------------------------------------------------------------------------

/// A one-page PDF with catalog, page and annotation `/AA` actions.
fn t0335_interactions_pdf() -> Vec<u8> {
    t0131_pdf(
        "1.7",
        &[
            "<< /Type /Catalog /Pages 2 0 R /AA << /PV 6 0 R >> >>".to_owned(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
             /AA << /E 7 0 R /X 8 0 R >> /Annots [4 0 R] >>"
                .to_owned(),
            "<< /Type /Annot /Subtype /Link /Rect [72 72 200 100] \
             /AA << /D 10 0 R >> >>"
                .to_owned(),
            "<< /Type /Annot /Subtype /Text /Rect [300 300 320 320] >>".to_owned(),
            "<< /S /GoTo /D [3 0 R /Fit] >>".to_owned(),
            "<< /S /NextPage >>".to_owned(),
            "<< /S /JavaScript /JS (app.alert(1)) >>".to_owned(),
            "<< /S /URI /URI (https://example.test/) >>".to_owned(),
            "<< /S /FullScreen >>".to_owned(),
        ],
    )
}

#[test]
fn interactions_audits_catalog_page_and_annotation_actions() {
    let pdf = TempPdf::new(&t0335_interactions_pdf());
    let output = run(&["interactions", pdf.arg(), "--json"]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let value: Value = serde_json::from_str(&stdout(&output)).expect("valid JSON");
    assert_eq!(value["sites"], 3);
    assert_eq!(value["actions"], 4);
    assert_eq!(value["accepted"], 3);
    assert_eq!(value["rejected"], 1);
    let rows = value["rows"].as_array().expect("rows");
    assert!(rows.iter().any(|row| {
        row["site"] == "catalog"
            && row["event"] == "PV"
            && row["kind"] == "GoTo"
            && row["code"] == "goto:0"
            && row["target"] == "0"
    }));
    assert!(rows.iter().any(|row| {
        row["site"] == "page 1" && row["event"] == "E" && row["code"] == "next-page"
    }));
    let rejected = rows
        .iter()
        .find(|row| row["status"] == "rejected")
        .expect("rejected row");
    assert_eq!(rejected["kind"], "JavaScript");
    assert!(
        rejected["error"]
            .as_str()
            .unwrap_or("")
            .contains("dangerous action"),
        "{rejected}"
    );
    assert!(rows.iter().any(|row| {
        row["site"] == "page 1 annotation 4" && row["code"] == "toggle-full-screen"
    }));
}

#[test]
fn interactions_strict_mode_fails_on_rejected_actions() {
    let pdf = TempPdf::new(&t0335_interactions_pdf());
    let strict = run(&["interactions", pdf.arg(), "--strict"]);
    assert!(!strict.status.success());
    assert!(
        stderr(&strict).contains("rejected by the safe-action whitelist"),
        "{}",
        stderr(&strict)
    );

    let clean = create_text_pdf("No additional actions.");
    let output = run(&["interactions", clean.arg(), "--strict", "--json"]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let value: Value = serde_json::from_str(&stdout(&output)).expect("valid JSON");
    assert_eq!(value["sites"], 0);
    assert_eq!(value["actions"], 0);
    assert_eq!(value["rejected"], 0);
}

#[test]
fn cli_version_starts_with_the_full_command_tree() {
    let output = run(&["--version"]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    assert!(stdout(&output).contains("pdfxi"), "{}", stdout(&output));
}

// ---------------------------------------------------------------------------
// media: media annotations and HTML5 envelope (T-0336)
// ---------------------------------------------------------------------------

/// A one-page PDF with a `/Sound` stream annotation and a `/Movie` annotation.
fn t0336_media_pdf() -> (Vec<u8>, Vec<u8>) {
    let audio = b"RIFF....WAVEfmt fake-audio-payload".to_vec();
    let bodies: Vec<Vec<u8>> = vec![
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Annots [4 0 R 5 0 R] >>".to_vec(),
        b"<< /Type /Annot /Subtype /Sound /Rect [72 600 92 620] /Sound 6 0 R >>".to_vec(),
        b"<< /Type /Annot /Subtype /Movie /Rect [100 400 400 600] \
          /Movie << /F (clip.mp4) /T (Intro clip) >> /Volume 0.5 >>"
            .to_vec(),
    ];
    let mut out = b"%PDF-1.7\n%PDFXI\n".to_vec();
    let mut offsets = Vec::new();
    for (index, body) in bodies.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n", index + 1).as_bytes());
        out.extend_from_slice(body);
        out.extend_from_slice(b"\nendobj\n");
    }
    offsets.push(out.len());
    out.extend_from_slice(
        format!("6 0 obj\n<< /R 8000 /Length {} >>\nstream\n", audio.len()).as_bytes(),
    );
    out.extend_from_slice(&audio);
    out.extend_from_slice(b"\nendstream\nendobj\n");
    let xref = out.len();
    let size = offsets.len() + 1;
    out.extend_from_slice(format!("xref\n0 {size}\n0000000000 65535 f \n").as_bytes());
    for offset in &offsets {
        out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!("trailer\n<< /Size {size} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n").as_bytes(),
    );
    (out, audio)
}

#[test]
fn media_list_reports_sound_and_movie_annotations() {
    let (pdf, payload) = t0336_media_pdf();
    let file = TempPdf::new(&pdf);
    let output = run(&["media", "list", file.arg(), "--json"]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let value: Value = serde_json::from_str(&stdout(&output)).expect("valid JSON");
    assert_eq!(value["pages"], 1);
    assert_eq!(value["annotations"], 2);
    let rows = value["rows"].as_array().expect("rows");
    let sound = &rows[0];
    assert_eq!(sound["kind"], "Sound");
    assert_eq!(sound["annotation"], 4);
    assert_eq!(sound["source"], "object:6:0");
    assert_eq!(sound["mime_type"], "audio/x-pdf-sound");
    assert_eq!(sound["volume"], 1.0);
    assert_eq!(sound["needs_host_player"], true);
    assert!(
        sound["requirements"]
            .as_array()
            .expect("requirements")
            .iter()
            .any(|req| req == "media-player")
    );
    let movie = &rows[1];
    assert_eq!(movie["kind"], "Movie");
    assert_eq!(movie["source"], "file:clip.mp4");
    assert_eq!(movie["mime_type"], "video/mp4");
    assert_eq!(movie["volume"], 0.5);
    assert!(
        movie["descriptor"]
            .as_str()
            .unwrap_or("")
            .starts_with("media-plan/v1|kind=Movie|source=file:clip.mp4")
    );
    assert!(
        sound["descriptor"]
            .as_str()
            .unwrap_or("")
            .contains("kind=Sound")
    );
    assert!(!payload.is_empty());
}

#[test]
fn media_extracts_the_embedded_sound_payload() {
    let (pdf, payload) = t0336_media_pdf();
    let file = TempPdf::new(&pdf);
    let dir = TempDir::new();
    let output = run(&["media", "list", file.arg(), "--extract", dir.arg()]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    assert!(
        stdout(&output).contains("extracted:"),
        "{}",
        stdout(&output)
    );
    let extracted = fs::read(dir.child("4-Sound.bin")).expect("sound payload");
    assert_eq!(extracted, payload);
}

#[test]
fn media_html5_writes_a_local_only_envelope() {
    let dir = TempDir::new();
    let out = dir.child("player.html");
    let output = run(&[
        "media",
        "html5",
        "--source",
        "clip.mp4=video/mp4",
        "--source",
        "@embedded.mp4=video/mp4",
        "--poster",
        "cover.png",
        "--controls",
        "--loop",
        "--width",
        "640",
        "--height",
        "360",
        "--out",
        out.to_str().expect("out"),
    ]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let page = fs::read_to_string(&out).expect("HTML written");
    assert!(page.starts_with("<!DOCTYPE html>"), "{page}");
    assert!(
        page.contains("default-src 'none'; media-src 'self'; img-src 'self'"),
        "{page}"
    );
    assert!(
        page.contains("<video controls loop poster=\"cover.png\" width=\"640\" height=\"360\">"),
        "{page}"
    );
    assert!(
        page.contains("<source src=\"clip.mp4\" type=\"video/mp4\"/>"),
        "{page}"
    );
    assert!(
        page.contains("<source src=\"@embedded.mp4\" type=\"video/mp4\"/>"),
        "{page}"
    );
    assert!(page.contains("</video>"), "{page}");

    let json = run(&[
        "media",
        "html5",
        "--source",
        "clip.mp4=video/mp4",
        "--out",
        out.to_str().expect("out"),
        "--json",
    ]);
    assert!(json.status.success(), "stderr: {}", stderr(&json));
    let value: Value = serde_json::from_str(&stdout(&json)).expect("valid JSON");
    assert_eq!(value["element"], "video");
    assert!(
        value["descriptor"]
            .as_str()
            .unwrap_or("")
            .starts_with("html5-plan/v1|element=video|sources=relative:clip.mp4")
    );
    assert!(value["markup"].as_str().unwrap_or("").contains("<video"));
}

#[test]
fn media_html5_rejects_remote_traversal_and_oversize_input() {
    let remote = run(&[
        "media",
        "html5",
        "--source",
        "https://evil.test/a.mp4=video/mp4",
    ]);
    assert!(!remote.status.success());
    assert!(
        stderr(&remote).contains("remote media source"),
        "{}",
        stderr(&remote)
    );

    let traversal = run(&["media", "html5", "--source", "../a.mp4=video/mp4"]);
    assert!(!traversal.status.success());
    assert!(
        stderr(&traversal).contains("parent-directory traversal"),
        "{}",
        stderr(&traversal)
    );

    let oversized = run(&[
        "media",
        "html5",
        "--source",
        "clip.mp4=video/mp4",
        "--width",
        "9000",
    ]);
    assert!(!oversized.status.success());
    assert!(
        stderr(&oversized).contains("8192"),
        "{}",
        stderr(&oversized)
    );

    let none = run(&["media", "html5"]);
    assert!(!none.status.success());
}

#[test]
fn media_list_rejects_documents_without_media() {
    let plain = create_text_pdf("No media annotations.");
    let output = run(&["media", "list", plain.arg()]);
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("no /Sound, /Movie or /Screen"),
        "{}",
        stderr(&output)
    );
}

// ---------------------------------------------------------------------------
// separations: print plates from content (T-0337)
// ---------------------------------------------------------------------------

/// A one-page PDF with a spot separation, CMYK, RGB and gray paint operations.
fn t0337_separations_pdf() -> Vec<u8> {
    let content = "q /CS0 cs 1 scn Q 0.1 0.2 0.3 0.4 k 0.5 0.6 0.7 rg 0.4 g";
    t0131_pdf(
        "1.7",
        &[
            "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R \
             /Resources << /ColorSpace << /CS0 [/Separation (Spot Orange) /DeviceCMYK 5 0 R] >> >> >>"
                .to_owned(),
            format!(
                "<< /Length {} >>\nstream\n{content}\nendstream",
                content.len()
            ),
            "<< /FunctionType 2 /Domain [0 1] /C0 [0 0 0 0] /C1 [0 0.5 1 0] /N 1 >>".to_owned(),
        ],
    )
}

#[test]
fn separations_reports_process_and_spot_plates() {
    let pdf = TempPdf::new(&t0337_separations_pdf());
    let output = run(&["separations", pdf.arg(), "--json"]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let value: Value = serde_json::from_str(&stdout(&output)).expect("valid JSON");
    assert_eq!(value["pages"], 1);
    assert_eq!(value["device_rgb_operations"], 1);
    assert_eq!(value["device_gray_operations"], 1);
    let plates = value["plates"].as_array().expect("plates");
    assert_eq!(plates.len(), 5);
    assert_eq!(plates[0]["name"], "Black");
    assert_eq!(plates[0]["process"].as_bool(), Some(true));
    assert_eq!(plates[0]["operations"], 2);
    assert_eq!(plates[0]["pages"], serde_json::json!([1]));
    for name in ["Cyan", "Magenta", "Yellow"] {
        let row = plates
            .iter()
            .find(|row| row["name"] == name)
            .expect("process plate");
        assert_eq!(row["process"].as_bool(), Some(true));
        assert_eq!(row["operations"], 1);
    }
    let spot = plates
        .iter()
        .find(|row| row["name"] == "Spot Orange")
        .expect("spot plate");
    assert_eq!(spot["process"].as_bool(), Some(false));
    assert_eq!(spot["operations"], 1);
    assert_eq!(spot["pages"], serde_json::json!([1]));
    assert!(
        value["summary"]
            .as_str()
            .unwrap_or("")
            .contains("4 process, 1 spot")
    );
    assert!(
        value["descriptor"]
            .as_str()
            .unwrap_or("")
            .contains("5:spot:1:Spot Orange")
    );
}

#[test]
fn separations_plate_query_is_the_output_preview() {
    let pdf = TempPdf::new(&t0337_separations_pdf());
    let output = run(&["separations", pdf.arg(), "--plate", "Spot Orange", "--json"]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let value: Value = serde_json::from_str(&stdout(&output)).expect("valid JSON");
    assert_eq!(value["name"], "Spot Orange");
    assert_eq!(value["process"].as_bool(), Some(false));
    assert_eq!(value["operations"], 1);
    assert_eq!(value["pages"], serde_json::json!([1]));

    let text = run(&["separations", pdf.arg(), "--plate", "cyan"]);
    assert!(text.status.success(), "stderr: {}", stderr(&text));
    assert!(
        stdout(&text).contains("plate: Cyan  process: true  operations: 1"),
        "{}",
        stdout(&text)
    );

    let missing = run(&["separations", pdf.arg(), "--plate", "Varnish"]);
    assert!(!missing.status.success());
    assert!(
        stderr(&missing).contains("no plate named"),
        "{}",
        stderr(&missing)
    );
}

#[test]
fn separations_reports_no_plates_for_colourless_content() {
    let pdf = TempPdf::new(&t0131_pdf(
        "1.7",
        &[
            "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R >>".to_owned(),
            "<< /Length 4 >>\nstream\nq Q\nendstream".to_owned(),
        ],
    ));
    let output = run(&["separations", pdf.arg(), "--json"]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let value: Value = serde_json::from_str(&stdout(&output)).expect("valid JSON");
    assert!(value["plates"].as_array().expect("plates").is_empty());
    assert_eq!(value["summary"], "separations: none");
    assert_eq!(value["descriptor"], "separations-plan/v1");
}

// ---------------------------------------------------------------------------
// ink: alternates and overprint preview (T-0338)
// ---------------------------------------------------------------------------

/// A one-page PDF that paints CMYK and a spot ink under an overprint state.
fn t0338_overprint_pdf() -> Vec<u8> {
    let content = "q /GS0 gs 0.1 0.2 0.3 0.4 k /CS0 cs 1 scn Q";
    t0131_pdf(
        "1.7",
        &[
            "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R \
             /Resources << /ColorSpace << /CS0 [/Separation (Spot Orange) /DeviceCMYK 5 0 R] >> \
             /ExtGState << /GS0 << /OP true /op true /OPM 1 >> >> >> >>"
                .to_owned(),
            format!(
                "<< /Length {} >>\nstream\n{content}\nendstream",
                content.len()
            ),
            "<< /FunctionType 2 /Domain [0 1] /C0 [0 0 0 0] /C1 [0 0.5 1 0] /N 1 >>".to_owned(),
        ],
    )
}

#[test]
fn ink_reports_alternates_and_overprint_counts() {
    let pdf = TempPdf::new(&t0338_overprint_pdf());
    let output = run(&["ink", pdf.arg(), "--json"]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let value: Value = serde_json::from_str(&stdout(&output)).expect("valid JSON");
    let plates = value["plates"].as_array().expect("plates");
    assert_eq!(plates.len(), 5);
    for plate in plates {
        assert_eq!(
            plate["overprint_operations"], 1,
            "plate {} not overprinted: {plate}",
            plate["name"]
        );
    }
    let spot = plates
        .iter()
        .find(|plate| plate["name"] == "Spot Orange")
        .expect("spot plate");
    assert_eq!(spot["alternate"], "DeviceCMYK");
    assert_eq!(value["alternates"]["Spot Orange"], "DeviceCMYK");
    let states = value["overprint_states"].as_array().expect("states");
    assert_eq!(states.len(), 1);
    assert_eq!(states[0]["name"], "GS0");
    assert_eq!(states[0]["fill"].as_bool(), Some(true));
    assert_eq!(states[0]["stroke"].as_bool(), Some(true));
    assert_eq!(states[0]["mode"], 1);
}

#[test]
fn ink_without_overprint_states_reports_zero_counts() {
    let pdf = TempPdf::new(&t0337_separations_pdf());
    let output = run(&["ink", pdf.arg(), "--json"]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let value: Value = serde_json::from_str(&stdout(&output)).expect("valid JSON");
    let plates = value["plates"].as_array().expect("plates");
    assert!(
        plates
            .iter()
            .all(|plate| plate["overprint_operations"] == 0)
    );
    assert!(
        value["overprint_states"]
            .as_array()
            .expect("states")
            .is_empty()
    );
    let spot = plates
        .iter()
        .find(|plate| plate["name"] == "Spot Orange")
        .expect("spot plate");
    assert_eq!(spot["alternate"], "DeviceCMYK");

    let text = run(&["ink", pdf.arg()]);
    assert!(text.status.success(), "stderr: {}", stderr(&text));
    assert!(
        stdout(&text).contains("Spot Orange  spot  1 op(s)  overprint 0  alternate DeviceCMYK"),
        "{}",
        stdout(&text)
    );
}

// ---------------------------------------------------------------------------
// policy: deployment preferences (T-0339)
// ---------------------------------------------------------------------------

#[test]
fn policy_write_show_and_check_round_trip() {
    let dir = TempDir::new();
    let policy = dir.child("policy.txt");
    let output = run(&[
        "policy",
        "write",
        "--out",
        policy.to_str().expect("policy"),
        "--theme",
        "dark",
        "--locale",
        "de-DE",
        "--channel",
        "beta",
        "--toggle",
        "telemetry=false",
        "--toggle",
        "autosave=true",
        "--allow-path",
        "/data/corpus",
        "--json",
    ]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let value: Value = serde_json::from_str(&stdout(&output)).expect("valid JSON");
    assert_eq!(value["theme"], "dark");
    assert_eq!(value["locale"], "de-DE");
    assert_eq!(value["channel"], "beta");
    assert_eq!(value["toggles"]["telemetry"], false);
    assert_eq!(value["allowed_paths"][0], "/data/corpus");
    let document = value["document"].as_str().unwrap_or("");
    assert!(document.contains("base.theme=dark"), "{document}");
    assert!(document.contains("toggle.telemetry=false"), "{document}");

    let shown = run(&["policy", "show", policy.to_str().expect("policy"), "--json"]);
    assert!(shown.status.success(), "stderr: {}", stderr(&shown));
    let value: Value = serde_json::from_str(&stdout(&shown)).expect("valid JSON");
    assert_eq!(value["channel"], "beta");
    assert_eq!(value["toggles"]["autosave"], true);

    let allowed = run(&[
        "policy",
        "check",
        policy.to_str().expect("policy"),
        "--theme",
        "dark",
        "--channel",
        "beta",
        "--toggle",
        "autosave",
        "--path",
        "/data/corpus/docs/a.pdf",
    ]);
    assert!(allowed.status.success(), "stderr: {}", stderr(&allowed));
    assert!(stdout(&allowed).contains("allowed: true"));

    let refused = run(&[
        "policy",
        "check",
        policy.to_str().expect("policy"),
        "--theme",
        "light",
        "--toggle",
        "telemetry",
        "--path",
        "/etc/passwd",
        "--json",
    ]);
    assert!(!refused.status.success());
    let value: Value = serde_json::from_str(&stdout(&refused)).expect("valid JSON");
    assert_eq!(value["allowed"], false);
    let failures = value["failures"].as_array().expect("failures");
    assert_eq!(failures.len(), 3);
    assert!(failures.iter().any(|failure| {
        failure
            .as_str()
            .unwrap_or("")
            .contains("is not the deployment theme")
    }));
    assert!(failures.iter().any(|failure| {
        failure
            .as_str()
            .unwrap_or("")
            .contains("is not enabled by policy")
    }));
    assert!(failures.iter().any(|failure| {
        failure
            .as_str()
            .unwrap_or("")
            .contains("outside every allowed path")
    }));
}

#[test]
fn policy_merge_uses_overlay_precedence() {
    let dir = TempDir::new();
    let base = dir.child("base.txt");
    let overlay = dir.child("overlay.txt");
    let merged = dir.child("merged.txt");
    assert!(
        run(&[
            "policy",
            "write",
            "--out",
            base.to_str().expect("base"),
            "--theme",
            "dark",
            "--channel",
            "beta",
            "--toggle",
            "telemetry=false",
            "--toggle",
            "autosave=true",
            "--allow-path",
            "/data/corpus",
        ])
        .status
        .success()
    );
    assert!(
        run(&[
            "policy",
            "write",
            "--out",
            overlay.to_str().expect("overlay"),
            "--theme",
            "light",
            "--channel",
            "stable",
            "--toggle",
            "autosave=false",
            "--toggle",
            "plugins=true",
            "--allow-path",
            "/work",
            "--allow-path",
            "/data/corpus",
        ])
        .status
        .success()
    );
    let output = run(&[
        "policy",
        "merge",
        base.to_str().expect("base"),
        overlay.to_str().expect("overlay"),
        "--out",
        merged.to_str().expect("merged"),
        "--json",
    ]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let value: Value = serde_json::from_str(&stdout(&output)).expect("valid JSON");
    assert_eq!(value["theme"], "light");
    assert_eq!(value["channel"], "stable");
    assert_eq!(value["toggles"]["telemetry"], false);
    assert_eq!(value["toggles"]["autosave"], false);
    assert_eq!(value["toggles"]["plugins"], true);
    assert_eq!(
        value["allowed_paths"],
        serde_json::json!(["/data/corpus", "/work"])
    );

    let reloaded = run(&["policy", "show", merged.to_str().expect("merged"), "--json"]);
    assert!(reloaded.status.success(), "stderr: {}", stderr(&reloaded));
    let value: Value = serde_json::from_str(&stdout(&reloaded)).expect("valid JSON");
    assert_eq!(value["theme"], "light");
    assert_eq!(value["allowed_paths"].as_array().expect("paths").len(), 2);
}

#[test]
fn policy_rejects_malformed_documents_and_inputs() {
    let dir = TempDir::new();
    let bad_toggle = run(&[
        "policy",
        "write",
        "--out",
        dir.child("p.txt").to_str().expect("path"),
        "--toggle",
        "telemetry=yes",
    ]);
    assert!(!bad_toggle.status.success());
    assert!(
        stderr(&bad_toggle).contains("invalid boolean"),
        "{}",
        stderr(&bad_toggle)
    );

    let relative = run(&[
        "policy",
        "write",
        "--out",
        dir.child("p.txt").to_str().expect("path"),
        "--allow-path",
        "relative/dir",
    ]);
    assert!(!relative.status.success());

    let malformed = dir.child("bad.txt");
    fs::write(&malformed, "base.theme=dark\n").expect("write malformed policy");
    let output = run(&["policy", "show", malformed.to_str().expect("path")]);
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("parse policy"),
        "{}",
        stderr(&output)
    );

    let unknown = dir.child("unknown.txt");
    fs::write(
        &unknown,
        "base.theme=dark\nbase.locale=en-US\nbase.recent_count=10\n\
         base.show_toolbar=true\nbase.default_zoom_percent=100\n\
         base.autosave_seconds=300\nbase.version=1\nchannel=stable\nsurprise=1\n",
    )
    .expect("write unknown key");
    let output = run(&["policy", "show", unknown.to_str().expect("path")]);
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("unknown preferences key"),
        "{}",
        stderr(&output)
    );
}

#[test]
fn policy_parses_a_plain_preferences_document() {
    let dir = TempDir::new();
    let prefs = dir.child("prefs.txt");
    fs::write(
        &prefs,
        "autosave_seconds=120\ndefault_zoom_percent=125\nlocale=en-GB\n\
         recent_count=5\nshow_toolbar=false\ntheme=light\nversion=1\n",
    )
    .expect("write preferences");
    let output = run(&[
        "policy",
        "preferences",
        prefs.to_str().expect("path"),
        "--json",
    ]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let value: Value = serde_json::from_str(&stdout(&output)).expect("valid JSON");
    assert_eq!(value["theme"], "light");
    assert_eq!(value["locale"], "en-GB");
    assert_eq!(value["recent_count"], 5);
    assert_eq!(value["show_toolbar"], false);
    assert_eq!(value["default_zoom_percent"], 125);
    assert_eq!(value["autosave_seconds"], 120);
}

// ---------------------------------------------------------------------------
// shell: file associations (T-0340)
// ---------------------------------------------------------------------------

#[test]
fn shell_plan_exports_descriptor_document() {
    let exe = std::env::current_exe().expect("current test executable");
    let output = run(&[
        "shell",
        "plan",
        "--exe",
        exe.to_str().expect("exe"),
        "--json",
    ]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let value: Value = serde_json::from_str(&stdout(&output)).expect("valid JSON");
    let lines = value["lines"].as_array().expect("lines");
    assert_eq!(lines.len(), 2);
    assert!(
        lines[0]
            .as_str()
            .unwrap_or("")
            .starts_with("assoc.ext=pdf; progid=PDFXI.pdf; command="),
        "{lines:?}"
    );
    assert!(
        lines[1]
            .as_str()
            .unwrap_or("")
            .starts_with("proto.scheme=pdfxi; progid=PDFXI.protocol;"),
        "{lines:?}"
    );
    assert!(
        value["summary"]
            .as_str()
            .unwrap_or("")
            .starts_with("associations=1; protocols=1;")
    );
}

#[cfg(windows)]
#[test]
fn shell_register_status_and_unregister_round_trip() {
    let root = format!(
        "HKCU\\Software\\PDFXI-Test-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    );
    let exe = std::env::current_exe().expect("current test executable");
    let registered = run(&[
        "shell",
        "register",
        "--root",
        &root,
        "--exe",
        exe.to_str().expect("exe"),
        "--json",
    ]);
    assert!(
        registered.status.success(),
        "stderr: {}",
        stderr(&registered)
    );
    let value: Value = serde_json::from_str(&stdout(&registered)).expect("valid JSON");
    assert_eq!(value["association_installed"], true);
    assert_eq!(value["association_command_installed"], true);
    assert_eq!(value["protocol_installed"], true);
    assert_eq!(value["extension"], ".pdf");
    assert_eq!(value["scheme"], "pdfxi");

    let status = run(&[
        "shell",
        "status",
        "--root",
        &root,
        "--exe",
        exe.to_str().expect("exe"),
        "--json",
    ]);
    assert!(status.status.success(), "stderr: {}", stderr(&status));
    let value: Value = serde_json::from_str(&stdout(&status)).expect("valid JSON");
    assert_eq!(value["association_installed"], true);
    assert_eq!(value["protocol_installed"], true);

    let removed = run(&[
        "shell",
        "unregister",
        "--root",
        &root,
        "--exe",
        exe.to_str().expect("exe"),
        "--json",
    ]);
    assert!(removed.status.success(), "stderr: {}", stderr(&removed));
    let value: Value = serde_json::from_str(&stdout(&removed)).expect("valid JSON");
    assert_eq!(value["association_installed"], false);
    assert_eq!(value["association_command_installed"], false);
    assert_eq!(value["protocol_installed"], false);

    // Remove the empty sandbox root so the test leaves no registry residue.
    let _ = Command::new("reg").args(["delete", &root, "/f"]).status();
}

#[test]
fn shell_rejects_machine_wide_roots_and_missing_executables() {
    let exe = std::env::current_exe().expect("current test executable");
    let machine = run(&[
        "shell",
        "register",
        "--root",
        "HKLM\\Software\\Classes",
        "--exe",
        exe.to_str().expect("exe"),
    ]);
    assert!(!machine.status.success());
    assert!(
        stderr(&machine).contains("per-user path under HKCU"),
        "{}",
        stderr(&machine)
    );

    let missing = run(&[
        "shell",
        "register",
        "--root",
        "HKCU\\Software\\PDFXI-Test",
        "--exe",
        "definitely-missing.exe",
    ]);
    assert!(!missing.status.success());
    assert!(
        stderr(&missing).contains("does not exist"),
        "{}",
        stderr(&missing)
    );

    let outside = run(&[
        "shell",
        "status",
        "--root",
        "HKCU\\Software\\Classes",
        "--exe",
        exe.to_str().expect("exe"),
    ]);
    assert!(outside.status.success(), "stderr: {}", stderr(&outside));
}

// ---------------------------------------------------------------------------
// handoff: browser integration (T-0341)
// ---------------------------------------------------------------------------

#[test]
fn handoff_open_resolves_local_html_and_mime_decisions() {
    let dir = TempDir::new();
    let page = dir.child("export.html");
    fs::write(&page, "<!DOCTYPE html><title>Export</title>").expect("write HTML");

    let output = run(&["handoff", "open", page.to_str().expect("path"), "--json"]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let value: Value = serde_json::from_str(&stdout(&output)).expect("valid JSON");
    assert_eq!(value["decision"], "open");
    assert_eq!(value["launched"], false);
    assert!(
        value["file_url"]
            .as_str()
            .unwrap_or("")
            .starts_with("file:///")
    );
    assert!(
        value["file_url"]
            .as_str()
            .unwrap_or("")
            .ends_with("export.html")
    );

    let download = run(&[
        "handoff",
        "open",
        page.to_str().expect("path"),
        "--mime",
        "application/octet-stream",
        "--json",
    ]);
    assert!(download.status.success(), "stderr: {}", stderr(&download));
    let value: Value = serde_json::from_str(&stdout(&download)).expect("valid JSON");
    assert_eq!(value["decision"], "download");
    assert_eq!(value["mime"], "application/octet-stream");

    let missing = run(&[
        "handoff",
        "open",
        dir.child("missing.html").to_str().expect("path"),
    ]);
    assert!(!missing.status.success());
    assert!(
        stderr(&missing).contains("does not exist"),
        "{}",
        stderr(&missing)
    );

    let bad_mime = run(&[
        "handoff",
        "open",
        page.to_str().expect("path"),
        "--mime",
        "application/x-msdownload",
    ]);
    assert!(!bad_mime.status.success());
    assert!(
        stderr(&bad_mime).contains("unsupported handoff MIME"),
        "{}",
        stderr(&bad_mime)
    );
}

#[test]
fn handoff_link_builds_file_url_with_fragment() {
    let dir = TempDir::new();
    let page = dir.child("export.html");
    fs::write(&page, "<!DOCTYPE html>").expect("write HTML");
    let output = run(&[
        "handoff",
        "link",
        page.to_str().expect("path"),
        "--fragment",
        "section-2",
        "--json",
    ]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let value: Value = serde_json::from_str(&stdout(&output)).expect("valid JSON");
    let url = value["file_url"].as_str().expect("file URL");
    assert!(url.starts_with("file:///"), "{url}");
    assert!(url.ends_with("export.html#section-2"), "{url}");
    assert!(
        value["summary"]
            .as_str()
            .unwrap_or("")
            .contains("fragment=section-2")
    );

    let pdf = create_text_pdf("Not HTML.");
    let non_html = run(&["handoff", "link", pdf.arg()]);
    assert!(!non_html.status.success());
    assert!(stderr(&non_html).contains(".html"), "{}", stderr(&non_html));
}

#[test]
fn handoff_parse_accepts_file_urls_and_rejects_remote_schemes() {
    let dir = TempDir::new();
    let page = dir.child("export.html");
    fs::write(&page, "<!DOCTYPE html>").expect("write HTML");
    let url = format!(
        "file:///{}",
        page.to_str().expect("path").replace('\\', "/")
    );
    let output = run(&["handoff", "parse", &url, "--json"]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let value: Value = serde_json::from_str(&stdout(&output)).expect("valid JSON");
    assert_eq!(value["decision"], "open");
    assert!(
        value["path"]
            .as_str()
            .unwrap_or("")
            .ends_with("export.html")
    );

    let remote = run(&["handoff", "parse", "https://evil.test/a.html"]);
    assert!(!remote.status.success());
    assert!(
        stderr(&remote).contains("unsupported handoff URL scheme"),
        "{}",
        stderr(&remote)
    );

    let traversal = run(&["handoff", "parse", "D:\\work\\..\\Windows\\system.ini"]);
    assert!(!traversal.status.success());
    assert!(
        stderr(&traversal).contains("traversal"),
        "{}",
        stderr(&traversal)
    );
}

// ---------------------------------------------------------------------------
// signature: electronic signature fields (T-0342)
// ---------------------------------------------------------------------------

#[test]
fn signature_create_plans_and_verifies_a_real_field() {
    let dir = TempDir::new();
    let out = dir.child("sig.pdf");
    let created = run(&[
        "signature",
        "create",
        "--out",
        out.to_str().expect("out"),
        "--signer",
        "Ada Lovelace",
        "--reason",
        "Approval",
        "--json",
    ]);
    assert!(created.status.success(), "stderr: {}", stderr(&created));
    let value: Value = serde_json::from_str(&stdout(&created)).expect("valid JSON");
    assert_eq!(value["field"], "Signature1");
    assert_eq!(value["placeholder_bytes"], 8192);
    assert_eq!(value["reloaded"], true);
    assert!(
        value["signature_line"]
            .as_str()
            .unwrap_or("")
            .contains("Digitally signed by Ada Lovelace")
    );
    let bytes = fs::read(&out).expect("signature PDF written");
    let document = pdfxi_core::Document::from_bytes(bytes).expect("core parses the signature PDF");
    assert_eq!(document.page_count().expect("page count"), 1);

    let planned = run(&["signature", "plan", out.to_str().expect("out"), "--json"]);
    assert!(planned.status.success(), "stderr: {}", stderr(&planned));
    let value: Value = serde_json::from_str(&stdout(&planned)).expect("valid JSON");
    let range = value["byte_range"].as_array().expect("byte range");
    assert_eq!(range.len(), 4);
    assert_eq!(range[0], 0);
    assert!(range[1].as_u64().unwrap_or(0) > 0);
    assert!(range[2].as_u64().unwrap_or(0) > range[1].as_u64().unwrap_or(0));
    assert!(range[3].as_u64().unwrap_or(0) > 0);
    assert!(value["signed_bytes"].as_u64().unwrap_or(0) > 0);
    assert_eq!(value["filter"], "Adobe.PPKLite");
    assert_eq!(value["subfilter"], "adbe.pkcs7.detached");
    assert_eq!(value["placeholder_bytes"], 8192);

    let verified = run(&[
        "signature",
        "verify",
        out.to_str().expect("out"),
        "--contents",
        "deadbeef01020304",
        "--json",
    ]);
    assert!(verified.status.success(), "stderr: {}", stderr(&verified));
    let value: Value = serde_json::from_str(&stdout(&verified)).expect("valid JSON");
    assert_eq!(value["field"], "Signature1");
    assert_eq!(value["valid"], true);
    assert_eq!(value["authenticity"], "structure-only");
    assert_eq!(value["contents_bytes"], 8);
    assert!(
        value["detail"]
            .as_str()
            .unwrap_or("")
            .starts_with("structure-ok:Adobe.PPKLite:adbe.pkcs7.detached:")
    );

    let placeholder = run(&[
        "signature",
        "verify",
        out.to_str().expect("out"),
        "--contents",
        "0000",
    ]);
    assert!(!placeholder.status.success());
    assert!(
        stderr(&placeholder).contains("unsigned placeholder"),
        "{}",
        stderr(&placeholder)
    );
}

#[test]
fn signature_create_rejects_bad_rects_and_sizes() {
    let dir = TempDir::new();
    let out = dir.child("sig.pdf").to_str().expect("out").to_owned();
    let off_page = run(&[
        "signature",
        "create",
        "--out",
        &out,
        "--rect",
        "600,700,100,100",
    ]);
    assert!(!off_page.status.success());
    assert!(
        stderr(&off_page).contains("outside the 612x792 page"),
        "{}",
        stderr(&off_page)
    );

    let odd = run(&[
        "signature",
        "create",
        "--out",
        &out,
        "--placeholder-bytes",
        "1001",
    ]);
    assert!(!odd.status.success());
    assert!(
        stderr(&odd).contains("not an even number"),
        "{}",
        stderr(&odd)
    );

    let bad_rect = run(&["signature", "create", "--out", &out, "--rect", "1,2,3"]);
    assert!(!bad_rect.status.success());
    assert!(
        stderr(&bad_rect).contains("four comma-separated"),
        "{}",
        stderr(&bad_rect)
    );
}

#[test]
fn signature_plan_rejects_documents_without_a_placeholder() {
    let plain = create_text_pdf("No signature field.");
    let output = run(&["signature", "plan", plain.arg()]);
    assert!(!output.status.success());
    assert!(
        stderr(&output).contains("/Contents placeholder not found"),
        "{}",
        stderr(&output)
    );

    let bad_hex = run(&["signature", "verify", plain.arg(), "--contents", "xyz"]);
    assert!(!bad_hex.status.success());
    assert!(
        stderr(&bad_hex).contains("hex string"),
        "{}",
        stderr(&bad_hex)
    );
}

// ---------------------------------------------------------------------------
// protect: standard security handler (T-0343)
// ---------------------------------------------------------------------------

#[test]
fn protect_and_unprotect_round_trip() {
    let dir = TempDir::new();
    let out = dir.child("protected.pdf");
    let plain = create_text_pdf("Protected round trip.");
    let protected = run(&[
        "protect",
        plain.arg(),
        "--out",
        out.to_str().expect("out"),
        "--password",
        "test-password",
        "--owner",
        "owner-password",
        "--permissions",
        "read-only",
        "--json",
    ]);
    assert!(protected.status.success(), "stderr: {}", stderr(&protected));
    let value: Value = serde_json::from_str(&stdout(&protected)).expect("valid JSON");
    assert_eq!(value["method"], "aes-256");
    assert_eq!(value["revision"], 6);
    assert_eq!(value["key_length_bits"], 256);
    assert_eq!(value["owner_password"], true);
    assert_eq!(value["pages"], 1);
    assert!(value["first_page_chars"].as_u64().unwrap_or(0) > 0);
    assert_eq!(
        value["permissions"],
        serde_json::json!(["print", "accessibility", "print-high"])
    );
    let encrypted = fs::read(&out).expect("protected file");
    assert!(
        encrypted
            .windows(b"/Encrypt".len())
            .any(|window| window == b"/Encrypt"),
        "the protected file has no /Encrypt entry"
    );

    let unprotect_out = dir.child("unprotected.pdf");
    let unproteced = run(&[
        "unprotect",
        out.to_str().expect("out"),
        "--out",
        unprotect_out.to_str().expect("out"),
        "--password",
        "test-password",
        "--json",
    ]);
    assert!(
        unproteced.status.success(),
        "stderr: {}",
        stderr(&unproteced)
    );
    let value: Value = serde_json::from_str(&stdout(&unproteced)).expect("valid JSON");
    assert_eq!(value["encrypted"], false);
    assert_eq!(value["pages"], 1);
    let decrypted = fs::read(&unprotect_out).expect("unprotected file");
    assert!(
        !decrypted
            .windows(b"/Encrypt".len())
            .any(|window| window == b"/Encrypt"),
        "the unprotect output still has /Encrypt"
    );
    let document = pdfxi_core::Document::from_bytes(decrypted).expect("core parses the output");
    assert_eq!(
        document.page_text(0).expect("page text"),
        "Protected round trip.\n"
    );
}

#[test]
fn protect_supports_legacy_rc4() {
    let dir = TempDir::new();
    let out = dir.child("rc4.pdf");
    let plain = create_text_pdf("RC4 round trip.");
    let protected = run(&[
        "protect",
        plain.arg(),
        "--out",
        out.to_str().expect("out"),
        "--password",
        "test-password",
        "--method",
        "rc4-40",
        "--json",
    ]);
    assert!(protected.status.success(), "stderr: {}", stderr(&protected));
    let value: Value = serde_json::from_str(&stdout(&protected)).expect("valid JSON");
    assert_eq!(value["method"], "rc4-40");
    assert_eq!(value["revision"], 2);
    assert_eq!(value["key_length_bits"], 40);
    assert_eq!(value["owner_password"], false);

    let round_trip = dir.child("rc4-plain.pdf");
    let unprotect = run(&[
        "unprotect",
        out.to_str().expect("out"),
        "--out",
        round_trip.to_str().expect("out"),
        "--password",
        "test-password",
    ]);
    assert!(unprotect.status.success(), "stderr: {}", stderr(&unprotect));
    let document =
        pdfxi_core::Document::from_bytes(fs::read(&round_trip).expect("output")).expect("parses");
    assert_eq!(
        document.page_text(0).expect("page text"),
        "RC4 round trip.\n"
    );
}

#[test]
fn protect_rejects_encrypted_input_and_bad_options() {
    let dir = TempDir::new();
    let out = dir.child("protected.pdf");
    let plain = create_text_pdf("Options.");
    assert!(
        run(&[
            "protect",
            plain.arg(),
            "--out",
            out.to_str().expect("out"),
            "--password",
            "test-password",
        ])
        .status
        .success()
    );

    let already = run(&[
        "protect",
        out.to_str().expect("out"),
        "--out",
        dir.child("again.pdf").to_str().expect("out"),
        "--password",
        "test-password",
    ]);
    assert!(!already.status.success());
    assert!(
        stderr(&already).contains("already encrypted"),
        "{}",
        stderr(&already)
    );

    let wrong = run(&[
        "unprotect",
        out.to_str().expect("out"),
        "--out",
        dir.child("wrong.pdf").to_str().expect("out"),
        "--password",
        "not-the-password",
    ]);
    assert!(!wrong.status.success());
    assert!(
        stderr(&wrong).contains("password is incorrect"),
        "{}",
        stderr(&wrong)
    );

    let conflict = run(&[
        "protect",
        plain.arg(),
        "--out",
        dir.child("conflict.pdf").to_str().expect("out"),
        "--password",
        "test-password",
        "--permissions",
        "annotate",
    ]);
    assert!(!conflict.status.success());
    assert!(
        stderr(&conflict).contains("annotation requires modification"),
        "{}",
        stderr(&conflict)
    );

    let method = run(&[
        "protect",
        plain.arg(),
        "--out",
        dir.child("method.pdf").to_str().expect("out"),
        "--password",
        "test-password",
        "--method",
        "rot13",
    ]);
    assert!(!method.status.success());
    assert!(
        stderr(&method).contains("unknown method"),
        "{}",
        stderr(&method)
    );

    let empty = run(&[
        "protect",
        plain.arg(),
        "--out",
        dir.child("empty.pdf").to_str().expect("out"),
        "--password",
        "",
    ]);
    assert!(!empty.status.success());
}

// ---------------------------------------------------------------------------
// trust: offline chain and revocation evaluation (T-0347)
// ---------------------------------------------------------------------------

struct TrustFixtures {
    ca_pem: PathBuf,
    other_ca_pem: PathBuf,
    leaf_pem: PathBuf,
    good_crl: PathBuf,
    revoked_crl: PathBuf,
}

fn write_trust_fixtures(dir: &TempDir) -> TrustFixtures {
    use std::str::FromStr;

    use der::Encode;
    use der::EncodePem;
    use der::asn1::UtcTime;
    use der::pem::LineEnding;
    use rsa::pkcs1v15::Signature as RsaRawSignature;
    use rsa::pkcs1v15::SigningKey;
    use rsa::signature::{Keypair, SignatureEncoding, Signer};
    use sha2::Sha256;
    use spki::SubjectPublicKeyInfoOwned;
    use x509_cert::builder::{Builder as _, CertificateBuilder, Profile};
    use x509_cert::name::Name;
    use x509_cert::serial_number::SerialNumber;
    use x509_cert::time::{Time, Validity};

    fn validity() -> Validity {
        Validity {
            not_before: Time::UtcTime(
                UtcTime::from_unix_duration(std::time::Duration::from_secs(1_600_000_000))
                    .expect("notBefore"),
            ),
            not_after: Time::UtcTime(
                UtcTime::from_unix_duration(std::time::Duration::from_secs(2_000_000_000))
                    .expect("notAfter"),
            ),
        }
    }

    let mut rng = rand::rngs::OsRng;
    let ca_key = rsa::RsaPrivateKey::new(&mut rng, 2048).expect("CA key");
    let ca_signer = SigningKey::<Sha256>::new(ca_key);
    let ca_name = Name::from_str("CN=PDFXI Test CA,O=PDFXI").expect("CA name");
    let ca_spki =
        SubjectPublicKeyInfoOwned::from_key(ca_signer.verifying_key()).expect("CA public key");
    let ca_cert = CertificateBuilder::new(
        Profile::Root,
        SerialNumber::from(1_u32),
        validity(),
        ca_name.clone(),
        ca_spki,
        &ca_signer,
    )
    .expect("CA builder")
    .build::<RsaRawSignature>()
    .expect("CA certificate");

    let leaf_key = rsa::RsaPrivateKey::new(&mut rng, 2048).expect("leaf key");
    let leaf_signer = SigningKey::<Sha256>::new(leaf_key);
    let leaf_name = Name::from_str("CN=PDFXI Test Leaf,O=PDFXI").expect("leaf name");
    let leaf_spki =
        SubjectPublicKeyInfoOwned::from_key(leaf_signer.verifying_key()).expect("leaf key");
    let leaf_cert = CertificateBuilder::new(
        Profile::Leaf {
            issuer: ca_name.clone(),
            enable_key_agreement: false,
            enable_key_encipherment: false,
        },
        SerialNumber::from(7_u32),
        validity(),
        leaf_name,
        leaf_spki,
        &ca_signer,
    )
    .expect("leaf builder")
    .build::<RsaRawSignature>()
    .expect("leaf certificate");

    let other_key = rsa::RsaPrivateKey::new(&mut rng, 2048).expect("other CA key");
    let other_signer = SigningKey::<Sha256>::new(other_key);
    let other_name = Name::from_str("CN=Unrelated Root,O=PDFXI").expect("other name");
    let other_spki =
        SubjectPublicKeyInfoOwned::from_key(other_signer.verifying_key()).expect("other key");
    let other_cert = CertificateBuilder::new(
        Profile::Root,
        SerialNumber::from(99_u32),
        validity(),
        other_name,
        other_spki,
        &other_signer,
    )
    .expect("other builder")
    .build::<RsaRawSignature>()
    .expect("other certificate");

    let issuer_der = ca_cert
        .tbs_certificate
        .subject
        .to_der()
        .expect("issuer name DER");
    let make_crl = |revoked: Vec<pdfxi_security::RevokedCertificate>| {
        let crl = pdfxi_security::Crl::new(&issuer_der, 1_650_000_000, 1_850_000_000, revoked)
            .expect("CRL");
        let signature = ca_signer.sign(crl.tbs_der());
        crl.with_signature(
            pdfxi_security::SignatureAlgorithm::Sha256WithRsa,
            signature.to_bytes().to_vec(),
        )
        .to_der()
    };
    let good_crl = make_crl(Vec::new());
    let revoked_crl = make_crl(vec![pdfxi_security::RevokedCertificate::new(
        7,
        1_650_000_000,
        pdfxi_security::RevocationReason::KeyCompromise,
    )]);

    let ca_pem = dir.child("ca.pem");
    let other_ca_pem = dir.child("other-ca.pem");
    let leaf_pem = dir.child("leaf.pem");
    let good_crl_path = dir.child("good.crl");
    let revoked_crl_path = dir.child("revoked.crl");
    fs::write(&ca_pem, ca_cert.to_pem(LineEnding::LF).expect("CA PEM")).expect("write CA");
    fs::write(
        &other_ca_pem,
        other_cert.to_pem(LineEnding::LF).expect("other PEM"),
    )
    .expect("write other CA");
    fs::write(
        &leaf_pem,
        leaf_cert.to_pem(LineEnding::LF).expect("leaf PEM"),
    )
    .expect("write leaf");
    fs::write(&good_crl_path, good_crl).expect("write good CRL");
    fs::write(&revoked_crl_path, revoked_crl).expect("write revoked CRL");
    TrustFixtures {
        ca_pem,
        other_ca_pem,
        leaf_pem,
        good_crl: good_crl_path,
        revoked_crl: revoked_crl_path,
    }
}

#[test]
fn trust_verifies_a_chain_and_revocation() {
    let dir = TempDir::new();
    let fixtures = write_trust_fixtures(&dir);
    let ca = fixtures.ca_pem.to_str().expect("ca");
    let leaf = fixtures.leaf_pem.to_str().expect("leaf");

    let plain = run(&[
        "trust",
        "verify",
        "--anchor",
        ca,
        "--leaf",
        leaf,
        "--time",
        "1700000000",
        "--json",
    ]);
    assert!(plain.status.success(), "stderr: {}", stderr(&plain));
    let value: Value = serde_json::from_str(&stdout(&plain)).expect("trust JSON");
    assert_eq!(value["trusted"], true);
    assert_eq!(value["reason"], Value::Null);
    assert_eq!(value["chain_serials"], serde_json::json!([7, 1]));
    assert_eq!(value["anchor_serial"], 1);
    assert_eq!(value["evaluated_at"], 1_700_000_000_u64);
    assert_eq!(value["strict"], false);
    let revocation = value["revocation"].as_array().expect("revocation");
    assert_eq!(revocation.len(), 1);
    assert_eq!(revocation[0]["serial"], 7);
    assert_eq!(revocation[0]["status"], "unknown");
    assert_eq!(revocation[0]["signature_verified"], false);

    let good = run(&[
        "trust",
        "verify",
        "--anchor",
        ca,
        "--leaf",
        leaf,
        "--time",
        "1700000000",
        "--crl",
        fixtures.good_crl.to_str().expect("crl"),
        "--strict",
        "--json",
    ]);
    assert!(good.status.success(), "stderr: {}", stderr(&good));
    let value: Value = serde_json::from_str(&stdout(&good)).expect("trust JSON");
    assert_eq!(value["trusted"], true);
    assert_eq!(value["revocation"][0]["status"], "good");
    assert_eq!(value["revocation"][0]["signature_verified"], true);

    let revoked = run(&[
        "trust",
        "verify",
        "--anchor",
        ca,
        "--leaf",
        leaf,
        "--time",
        "1700000000",
        "--crl",
        fixtures.revoked_crl.to_str().expect("crl"),
        "--strict",
        "--json",
    ]);
    assert!(!revoked.status.success());
    let value: Value = serde_json::from_str(&stdout(&revoked)).expect("trust JSON");
    assert_eq!(value["trusted"], false);
    assert_eq!(value["reason"], "revoked:7");
    assert_eq!(value["revocation"][0]["status"], "revoked");
    assert_eq!(value["revocation"][0]["revoked_at"], 1_650_000_000_u64);
    assert_eq!(value["revocation"][0]["signature_verified"], true);
    assert!(
        stderr(&revoked).contains("not trusted"),
        "{}",
        stderr(&revoked)
    );

    let strict_missing = run(&[
        "trust",
        "verify",
        "--anchor",
        ca,
        "--leaf",
        leaf,
        "--time",
        "1700000000",
        "--strict",
    ]);
    assert!(!strict_missing.status.success());
    assert!(
        stderr(&strict_missing).contains("revocation_unknown:7"),
        "{}",
        stderr(&strict_missing)
    );

    let other = run(&[
        "trust",
        "verify",
        "--anchor",
        fixtures.other_ca_pem.to_str().expect("ca"),
        "--leaf",
        leaf,
        "--time",
        "1700000000",
        "--json",
    ]);
    assert!(!other.status.success());
    let value: Value = serde_json::from_str(&stdout(&other)).expect("trust JSON");
    assert_eq!(value["trusted"], false);
    assert!(!value["reason"].as_str().unwrap_or("").is_empty());
}

#[test]
fn trust_rejects_bad_inputs() {
    let dir = TempDir::new();
    let fixtures = write_trust_fixtures(&dir);
    let ca = fixtures.ca_pem.to_str().expect("ca");
    let leaf = fixtures.leaf_pem.to_str().expect("leaf");

    let missing = run(&[
        "trust",
        "verify",
        "--anchor",
        dir.child("missing.pem").to_str().expect("path"),
        "--leaf",
        leaf,
    ]);
    assert!(!missing.status.success());
    assert!(stderr(&missing).contains("read"), "{}", stderr(&missing));

    let garbage = dir.child("garbage.pem");
    fs::write(&garbage, b"not a certificate").expect("write garbage");
    let bad_anchor = run(&[
        "trust",
        "verify",
        "--anchor",
        garbage.to_str().expect("path"),
        "--leaf",
        leaf,
    ]);
    assert!(!bad_anchor.status.success());
    assert!(
        stderr(&bad_anchor).contains("parse"),
        "{}",
        stderr(&bad_anchor)
    );

    let both = dir.child("both.pem");
    let mut bundle = fs::read(&fixtures.ca_pem).expect("ca bytes");
    bundle.extend_from_slice(&fs::read(&fixtures.leaf_pem).expect("leaf bytes"));
    fs::write(&both, bundle).expect("write bundle");
    let multi = run(&[
        "trust",
        "verify",
        "--anchor",
        ca,
        "--leaf",
        both.to_str().expect("path"),
    ]);
    assert!(!multi.status.success());
    assert!(stderr(&multi).contains("exactly one"), "{}", stderr(&multi));

    let bad_crl = dir.child("bad.crl");
    fs::write(&bad_crl, b"not a crl").expect("write bad CRL");
    let crl_error = run(&[
        "trust",
        "verify",
        "--anchor",
        ca,
        "--leaf",
        leaf,
        "--crl",
        bad_crl.to_str().expect("path"),
    ]);
    assert!(!crl_error.status.success());
    assert!(
        stderr(&crl_error).contains("parse CRL"),
        "{}",
        stderr(&crl_error)
    );
}

// ---------------------------------------------------------------------------
// id: digital-ID metadata catalog (T-0348)
// ---------------------------------------------------------------------------

#[test]
fn id_imports_lists_and_removes_a_pem_identity() {
    let dir = TempDir::new();
    let (key, cert) = write_test_identity(&dir, "PDFXI Catalog Signer");
    let key = key.to_str().expect("key");
    let cert = cert.to_str().expect("cert");
    let store = dir.child("ids.json");
    let store_arg = store.to_str().expect("store");

    let inspected = run(&[
        "id",
        "inspect",
        "--key",
        key,
        "--cert",
        cert,
        "--label",
        "Signer",
        "--time",
        "1700000000",
        "--json",
    ]);
    assert!(inspected.status.success(), "stderr: {}", stderr(&inspected));
    let value: Value = serde_json::from_str(&stdout(&inspected)).expect("inspect JSON");
    assert_eq!(value["label"], "Signer");
    assert!(
        value["subject"]
            .as_str()
            .unwrap_or("")
            .contains("PDFXI Catalog Signer")
    );
    assert_eq!(value["fingerprint_sha256"].as_str().unwrap_or("").len(), 64);
    assert_eq!(value["key_algorithm"], "1.2.840.113549.1.1.1");
    assert_eq!(value["not_after"], 2_000_000_000_u64);
    assert_eq!(value["validity"], "valid");
    assert!(!store.exists(), "inspect must not create the catalog");

    let imported = run(&[
        "id",
        "import",
        "--store",
        store_arg,
        "--label",
        "Signer",
        "--key",
        key,
        "--cert",
        cert,
        "--time",
        "1700000000",
        "--json",
    ]);
    assert!(imported.status.success(), "stderr: {}", stderr(&imported));
    let value: Value = serde_json::from_str(&stdout(&imported)).expect("import JSON");
    assert_eq!(value["count"], 1);
    assert_eq!(value["ids"][0]["label"], "Signer");
    assert_eq!(value["ids"][0]["validity"], "valid");
    let catalog = fs::read_to_string(&store).expect("catalog written");
    assert!(
        !catalog.contains("PRIVATE KEY"),
        "no key material is stored"
    );
    assert!(catalog.contains("fingerprint_sha256"));

    let listed = run(&[
        "id",
        "list",
        "--store",
        store_arg,
        "--time",
        "1700000000",
        "--json",
    ]);
    assert!(listed.status.success(), "stderr: {}", stderr(&listed));
    let value: Value = serde_json::from_str(&stdout(&listed)).expect("list JSON");
    assert_eq!(value["count"], 1);
    assert_eq!(value["ids"][0]["label"], "Signer");

    let expired = run(&[
        "id",
        "list",
        "--store",
        store_arg,
        "--time",
        "2100000000",
        "--json",
    ]);
    assert!(expired.status.success(), "stderr: {}", stderr(&expired));
    let value: Value = serde_json::from_str(&stdout(&expired)).expect("list JSON");
    assert_eq!(value["ids"][0]["validity"], "expired");

    let duplicate = run(&[
        "id",
        "import",
        "--store",
        store_arg,
        "--label",
        "SameCert",
        "--key",
        key,
        "--cert",
        cert,
        "--time",
        "1700000000",
    ]);
    assert!(!duplicate.status.success());
    assert!(
        stderr(&duplicate).contains("already catalogued"),
        "{}",
        stderr(&duplicate)
    );

    let removed = run(&[
        "id", "remove", "--store", store_arg, "--label", "Signer", "--json",
    ]);
    assert!(removed.status.success(), "stderr: {}", stderr(&removed));
    let value: Value = serde_json::from_str(&stdout(&removed)).expect("remove JSON");
    assert_eq!(value["removed"], "Signer");
    assert_eq!(value["count"], 0);

    let again = run(&["id", "remove", "--store", store_arg, "--label", "Signer"]);
    assert!(!again.status.success());
    assert!(
        stderr(&again).contains("no digital ID"),
        "{}",
        stderr(&again)
    );
}

#[test]
fn id_rejects_bad_identity_inputs() {
    let dir = TempDir::new();
    let (key, cert) = write_test_identity(&dir, "Catalog Errors");
    let key = key.to_str().expect("key");
    let cert = cert.to_str().expect("cert");
    let store = dir.child("ids.json");
    let store_arg = store.to_str().expect("store");

    let missing = run(&["id", "inspect", "--label", "X"]);
    assert!(!missing.status.success());
    assert!(
        stderr(&missing).contains("supply --pfx"),
        "{}",
        stderr(&missing)
    );

    let conflict = run(&[
        "id", "inspect", "--label", "X", "--pfx", cert, "--key", key, "--cert", cert,
    ]);
    assert!(!conflict.status.success());
    assert!(
        stderr(&conflict).contains("cannot be combined"),
        "{}",
        stderr(&conflict)
    );

    let bad_label = run(&[
        "id",
        "import",
        "--store",
        store_arg,
        "--label",
        "bad/label",
        "--key",
        key,
        "--cert",
        cert,
    ]);
    assert!(!bad_label.status.success());
    assert!(
        stderr(&bad_label).contains("path separators"),
        "{}",
        stderr(&bad_label)
    );

    let garbage_key = dir.child("garbage-key.pem");
    fs::write(&garbage_key, b"not a private key").expect("write garbage key");
    let bad_key = run(&[
        "id",
        "inspect",
        "--label",
        "X",
        "--key",
        garbage_key.to_str().expect("path"),
        "--cert",
        cert,
    ]);
    assert!(!bad_key.status.success());
    assert!(
        stderr(&bad_key).contains("parse private key"),
        "{}",
        stderr(&bad_key)
    );

    let empty = run(&["id", "list", "--store", store_arg, "--json"]);
    assert!(empty.status.success(), "stderr: {}", stderr(&empty));
    let value: Value = serde_json::from_str(&stdout(&empty)).expect("list JSON");
    assert_eq!(value["count"], 0);
}

// ---------------------------------------------------------------------------
// sign: real CMS/RSA signatures (T-0344)
// ---------------------------------------------------------------------------

/// Write a fresh self-signed RSA identity as PEM key + certificate.
fn write_test_identity(dir: &TempDir, common_name: &str) -> (PathBuf, PathBuf) {
    use std::str::FromStr;

    use der::EncodePem;
    use der::asn1::UtcTime;
    use der::pem::LineEnding;
    use rsa::pkcs1v15::Signature as RsaRawSignature;
    use rsa::pkcs1v15::SigningKey;
    use rsa::pkcs8::EncodePrivateKey;
    use rsa::signature::Keypair;
    use sha2::Sha256;
    use spki::SubjectPublicKeyInfoOwned;
    use x509_cert::builder::{Builder as _, CertificateBuilder, Profile};
    use x509_cert::name::Name;
    use x509_cert::serial_number::SerialNumber;
    use x509_cert::time::{Time, Validity};

    let mut rng = rand::rngs::OsRng;
    let key = rsa::RsaPrivateKey::new(&mut rng, 2048).expect("generate test RSA key");
    let signer = SigningKey::<Sha256>::new(key.clone());
    let subject = Name::from_str(&format!("CN={common_name},O=PDFXI")).expect("subject name");
    let spki =
        SubjectPublicKeyInfoOwned::from_key(signer.verifying_key()).expect("public key info");
    let builder = CertificateBuilder::new(
        Profile::Leaf {
            issuer: subject.clone(),
            enable_key_agreement: false,
            enable_key_encipherment: false,
        },
        SerialNumber::from(7_u32),
        Validity {
            not_before: Time::UtcTime(
                UtcTime::from_unix_duration(std::time::Duration::from_secs(1_600_000_000))
                    .expect("notBefore"),
            ),
            not_after: Time::UtcTime(
                UtcTime::from_unix_duration(std::time::Duration::from_secs(2_000_000_000))
                    .expect("notAfter"),
            ),
        },
        subject,
        spki,
        &signer,
    )
    .expect("certificate builder");
    let certificate = builder
        .build::<RsaRawSignature>()
        .expect("self-signed certificate");
    let key_path = dir.child("signer-key.pem");
    let cert_path = dir.child("signer-cert.pem");
    fs::write(
        &key_path,
        key.to_pkcs8_pem(LineEnding::LF)
            .expect("encode key")
            .as_bytes(),
    )
    .expect("write key");
    fs::write(
        &cert_path,
        certificate.to_pem(LineEnding::LF).expect("encode cert"),
    )
    .expect("write certificate");
    (key_path, cert_path)
}

#[test]
fn sign_creates_a_cms_signature_and_verifies_it() {
    let dir = TempDir::new();
    let (key, cert) = write_test_identity(&dir, "PDFXI CLI Signer");
    let plain = create_text_pdf("Signed content.");
    let signed = dir.child("signed.pdf");
    let created = run(&[
        "sign",
        "create",
        plain.arg(),
        "--out",
        signed.to_str().expect("out"),
        "--key",
        key.to_str().expect("key"),
        "--cert",
        cert.to_str().expect("cert"),
        "--reason",
        "Approval",
        "--time",
        "1700000000",
        "--json",
    ]);
    assert!(created.status.success(), "stderr: {}", stderr(&created));
    let value: Value = serde_json::from_str(&stdout(&created)).expect("valid JSON");
    assert_eq!(value["field"], "Signature1");
    assert_eq!(value["page"], 1);
    assert_eq!(value["digest"], "sha256");
    assert_eq!(value["signing_time"], 1_700_000_000_u64);
    assert_eq!(value["fully_valid"], true);
    assert!(
        value["signer"]
            .as_str()
            .unwrap_or("")
            .contains("PDFXI CLI Signer")
    );
    assert!(value["cms_bytes"].as_u64().unwrap_or(0) > 0);
    assert!(value["contents_reservation"].as_u64().unwrap_or(0) >= 4096);
    let range = value["byte_range"].as_array().expect("byte range");
    assert_eq!(range.len(), 4);
    assert_eq!(range[0], 0);
    let bytes = fs::read(&signed).expect("signed file");
    assert_eq!(bytes.len(), value["bytes"].as_u64().unwrap_or(0) as usize);
    let document = pdfxi_core::Document::from_bytes(bytes).expect("core parses the signed PDF");
    assert_eq!(document.page_count().expect("page count"), 1);

    let verified = run(&["sign", "verify", signed.to_str().expect("out"), "--json"]);
    assert!(verified.status.success(), "stderr: {}", stderr(&verified));
    let value: Value = serde_json::from_str(&stdout(&verified)).expect("valid JSON");
    assert_eq!(value["status"], "valid");
    assert_eq!(value["fully_valid"], true);
    assert_eq!(value["signature_valid"], true);
    assert_eq!(value["bytes_match"], true);
    assert_eq!(value["covers_entire_file"], true);
    assert_eq!(value["appended_after_signature"], false);
    assert_eq!(value["sub_filter"], "adbe.pkcs7.detached");
    assert_eq!(value["fingerprint_sha256"].as_str().unwrap_or("").len(), 64);
    assert_eq!(value["chain_trusted"], Value::Null);
    assert_eq!(value["signing_time"], 1_700_000_000_u64);

    // Tampering with a covered byte must be reported, never accepted.
    let mut tampered = fs::read(&signed).expect("signed file");
    let position = tampered
        .windows(b"Signed content.".len())
        .position(|window| window == b"Signed content.")
        .expect("text present in the signed file");
    tampered[position] = b'X';
    let tampered_path = dir.child("tampered.pdf");
    fs::write(&tampered_path, &tampered).expect("write tampered file");
    let rejected = run(&["sign", "verify", tampered_path.to_str().expect("out")]);
    assert!(!rejected.status.success());
    assert!(
        stderr(&rejected).contains("not fully valid"),
        "{}",
        stderr(&rejected)
    );

    // Appending bytes leaves the signature valid for what it covers but not
    // for the whole file.
    let mut appended = fs::read(&signed).expect("signed file");
    appended.extend_from_slice(b"\n% appended after signing\n");
    let appended_path = dir.child("appended.pdf");
    fs::write(&appended_path, &appended).expect("write appended file");
    let covered = run(&[
        "sign",
        "verify",
        appended_path.to_str().expect("out"),
        "--json",
    ]);
    assert!(!covered.status.success());
    let value: Value = serde_json::from_str(&stdout(&covered)).expect("valid JSON");
    assert_eq!(value["status"], "valid");
    assert_eq!(value["appended_after_signature"], true);
    assert_eq!(value["covers_entire_file"], false);
    assert_eq!(value["fully_valid"], false);
}

#[test]
fn sign_rejects_bad_identity_options_and_unsigned_input() {
    let dir = TempDir::new();
    let (key, cert) = write_test_identity(&dir, "Options Signer");
    let plain = create_text_pdf("No signature yet.");
    let out = dir.child("out.pdf");
    let out = out.to_str().expect("out");

    let missing = run(&["sign", "create", plain.arg(), "--out", out]);
    assert!(!missing.status.success());
    assert!(
        stderr(&missing).contains("--key/--cert or with --pfx"),
        "{}",
        stderr(&missing)
    );

    let conflict = run(&[
        "sign",
        "create",
        plain.arg(),
        "--out",
        out,
        "--key",
        key.to_str().expect("key"),
        "--cert",
        cert.to_str().expect("cert"),
        "--pfx",
        cert.to_str().expect("cert"),
    ]);
    assert!(!conflict.status.success());
    assert!(
        stderr(&conflict).contains("--pfx cannot be combined"),
        "{}",
        stderr(&conflict)
    );

    for (page, needle) in [("0", "one-based"), ("9", "outside")] {
        let page_error = run(&[
            "sign",
            "create",
            plain.arg(),
            "--out",
            out,
            "--key",
            key.to_str().expect("key"),
            "--cert",
            cert.to_str().expect("cert"),
            "--page",
            page,
        ]);
        assert!(!page_error.status.success());
        assert!(
            stderr(&page_error).contains(needle),
            "{needle}: {}",
            stderr(&page_error)
        );
    }

    let unsigned = run(&["sign", "verify", plain.arg()]);
    assert!(!unsigned.status.success());
    assert!(
        stderr(&unsigned).to_lowercase().contains("signature"),
        "{}",
        stderr(&unsigned)
    );

    let garbage_key = dir.child("garbage-key.pem");
    fs::write(&garbage_key, b"not a pem document").expect("write garbage key");
    let bad = run(&[
        "sign",
        "create",
        plain.arg(),
        "--out",
        out,
        "--key",
        garbage_key.to_str().expect("key"),
        "--cert",
        cert.to_str().expect("cert"),
    ]);
    assert!(!bad.status.success());
    assert!(
        stderr(&bad).contains("load PEM identity"),
        "{}",
        stderr(&bad)
    );
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
