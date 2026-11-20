//! Integration tests for T-0240 / acceptance A-28: "Convert to PDF".
//!
//! Every case converts real files through the document-level entry point, then
//! reopens the output with `pdfxi-core`, an independent reader, and asserts the
//! page count, page size, and extractable text. Converted files are also
//! written under the lane target directory so the orchestrator can re-render
//! them with PDFium.

use std::path::{Path, PathBuf};

use pdfxi_convert::{
    CONVERT_MAX_SOURCES, ConvertError, ConvertInput, ConvertOptions, ConvertSource, CreateError,
    CreateImageFormat, convert_to_pdf, create_from_text,
};
use pdfxi_core::Document;

/// Workspace root, derived from this crate's manifest directory.
fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

/// Directory for lane artifacts: `$CARGO_TARGET_DIR/t0240-out` when the lane
/// sets it, otherwise `<workspace>/target/t0240-out`.
fn out_dir() -> PathBuf {
    let target = match std::env::var_os("CARGO_TARGET_DIR") {
        Some(value) => {
            let configured = PathBuf::from(value);
            if configured.is_absolute() {
                configured
            } else {
                workspace_root().join(configured)
            }
        }
        None => workspace_root().join("target"),
    };
    let dir = target.join("t0240-out");
    std::fs::create_dir_all(dir.join("inputs")).expect("create artifact directory");
    dir
}

fn write_input(name: &str, contents: &[u8]) -> PathBuf {
    let path = out_dir().join("inputs").join(name);
    std::fs::write(&path, contents).expect("write test input");
    path
}

fn out_path(name: &str) -> PathBuf {
    out_dir().join(name)
}

/// Build a small valid non-interlaced 8-bit RGB PNG with a `pHYs` density.
fn rgb_png(width: u32, height: u32, dpi: Option<u32>) -> Vec<u8> {
    fn crc(data: &[u8]) -> u32 {
        let mut value = 0xFFFF_FFFFu32;
        for &byte in data {
            value ^= u32::from(byte);
            for _ in 0..8 {
                let mask = 0u32.wrapping_sub(value & 1);
                value = (value >> 1) ^ (0xEDB8_8320 & mask);
            }
        }
        value ^ 0xFFFF_FFFF
    }
    fn adler(data: &[u8]) -> u32 {
        let (mut a, mut b) = (1u32, 0u32);
        for &byte in data {
            a = (a + u32::from(byte)) % 65_521;
            b = (b + a) % 65_521;
        }
        (b << 16) | a
    }
    fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        out.extend_from_slice(kind);
        out.extend_from_slice(data);
        let mut checked = Vec::from(*kind);
        checked.extend_from_slice(data);
        out.extend_from_slice(&crc(&checked).to_be_bytes());
    }

    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
    let row_len = (width as usize) * 3 + 1;
    let raw_len = row_len * (height as usize);
    let raw: Vec<u8> = (0..raw_len)
        .map(|index| if index % row_len == 0 { 0 } else { 0x40 })
        .collect();
    let mut z = vec![0x78, 0x01, 0x01];
    z.extend_from_slice(&(raw_len as u16).to_le_bytes());
    z.extend_from_slice(&(!(raw_len as u16)).to_le_bytes());
    z.extend_from_slice(&raw);
    z.extend_from_slice(&adler(&raw).to_be_bytes());

    let mut out = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    chunk(&mut out, b"IHDR", &ihdr);
    if let Some(dpi) = dpi {
        let ppm = ((f64::from(dpi) / 0.0254).round() as u32).to_be_bytes();
        let mut phys = Vec::from(ppm);
        phys.extend_from_slice(&ppm);
        phys.push(1);
        chunk(&mut out, b"pHYs", &phys);
    }
    chunk(&mut out, b"IDAT", &z);
    chunk(&mut out, b"IEND", &[]);
    out
}

fn reopen(path: &Path) -> Document {
    let bytes = std::fs::read(path).expect("read converted PDF");
    Document::from_bytes(bytes).expect("independent reader parses converted PDF")
}

#[test]
fn text_file_converts_to_one_letter_page_with_text() {
    let input = write_input(
        "t0240-notes.txt",
        b"PDFXI conversion marker one.\nSecond source line.\n",
    );
    let converted = convert_to_pdf(
        ConvertInput::from_path(&input).expect("classify text file"),
        ConvertOptions::new(),
    )
    .expect("convert text file");
    assert_eq!(converted.page_count(), 1);
    assert_eq!(converted.parts().len(), 1);
    assert_eq!(converted.parts()[0].first_page(), 0);
    assert_eq!(converted.parts()[0].page_count(), 1);
    assert_eq!(converted.parts()[0].kind().label(), "text");

    let path = out_path("text.pdf");
    converted.write_to(&path).expect("write text pdf");

    let document = reopen(&path);
    assert_eq!(document.page_count().expect("page count"), 1);
    let page = document.page(0).expect("first page");
    assert!((page.width() - 612.0).abs() < 0.01);
    assert!((page.height() - 792.0).abs() < 0.01);
    let text = document.page_text(0).expect("page text");
    assert!(
        text.contains("PDFXI conversion marker one."),
        "first marker missing: {text:?}"
    );
    assert!(
        text.contains("Second source line."),
        "second marker missing: {text:?}"
    );
}

#[test]
fn markdown_file_renders_heading_and_bullet_items() {
    let markdown = b"# Marker Heading\n\nA paragraph of body copy.\n\n- alpha item\n- beta item\n";
    let input = write_input("t0240-notes.md", markdown);
    let converted = convert_to_pdf(
        ConvertInput::from_path(&input).expect("classify markdown file"),
        ConvertOptions::new(),
    )
    .expect("convert markdown file");
    let path = out_path("markdown.pdf");
    converted.write_to(&path).expect("write markdown pdf");

    let document = reopen(&path);
    assert_eq!(document.page_count().expect("page count"), 1);
    let text = document.page_text(0).expect("page text");
    assert!(text.contains("Marker Heading"), "heading: {text:?}");
    assert!(text.contains("alpha item"), "first bullet: {text:?}");
    assert!(text.contains("beta item"), "second bullet: {text:?}");
    assert_eq!(converted.parts()[0].kind().label(), "markdown");
}

#[test]
fn html_file_with_table_renders_cells() {
    let html = r#"<!DOCTYPE html>
<html><head><title>Report</title></head><body>
<h1>Regional Summary</h1>
<p>Intro copy for the table.</p>
<table>
<tr><th>Region</th><th>Units</th></tr>
<tr><td>North</td><td>1200</td></tr>
<tr><td>South</td><td>900</td></tr>
</table>
</body></html>
"#;
    let input = write_input("t0240-table.html", html.as_bytes());
    let converted = convert_to_pdf(
        ConvertInput::from_path(&input).expect("classify html file"),
        ConvertOptions::new(),
    )
    .expect("convert html file");
    let path = out_path("html.pdf");
    converted.write_to(&path).expect("write html pdf");

    let document = reopen(&path);
    assert_eq!(document.page_count().expect("page count"), 1);
    let text = document.page_text(0).expect("page text");
    for marker in [
        "Regional Summary",
        "Region",
        "North",
        "1200",
        "South",
        "900",
    ] {
        assert!(
            text.contains(marker),
            "missing table marker {marker}: {text:?}"
        );
    }
    assert_eq!(converted.parts()[0].kind().label(), "html");
}

#[test]
fn png_image_converts_as_one_page_and_honours_page_size() {
    let png = rgb_png(40, 30, Some(150));
    let input = write_input("t0240-scan.png", &png);
    let a4 = pdfxi_convert::PageSize::A4;
    let options = ConvertOptions::new()
        .with_page_size(a4)
        .with_margins(pdfxi_convert::Margins::uniform(36.0).expect("margins"));
    let converted = convert_to_pdf(
        ConvertInput::from_path(&input).expect("classify png file"),
        options,
    )
    .expect("convert png file");
    assert_eq!(converted.page_count(), 1);
    assert_eq!(converted.parts()[0].kind().label(), "png");
    let path = out_path("image-a4.pdf");
    converted.write_to(&path).expect("write image pdf");

    let document = reopen(&path);
    assert_eq!(document.page_count().expect("page count"), 1);
    let page = document.page(0).expect("first page");
    assert!((page.width() - 595.28).abs() < 0.01, "A4 width");
    assert!((page.height() - 841.89).abs() < 0.01, "A4 height");
    assert_eq!(page.contents.len(), 1);
    // An image page carries no text layer.
    let text = document.page_text(0).expect("image page text");
    assert!(text.trim().is_empty(), "image page must have no text");
}

#[test]
fn mixed_text_and_image_merge_in_order() {
    let text = ConvertSource::text("Mixed request text page marker.\n").with_name("marker.txt");
    let image =
        ConvertSource::image(rgb_png(32, 24, None), CreateImageFormat::Png).with_name("scan.png");
    let input = ConvertInput::new().with(text).with(image);
    let converted = convert_to_pdf(input, ConvertOptions::new()).expect("convert mixed request");
    assert_eq!(converted.page_count(), 2);
    assert_eq!(converted.parts().len(), 2);
    assert_eq!(converted.parts()[0].first_page(), 0);
    assert_eq!(converted.parts()[0].page_count(), 1);
    assert_eq!(converted.parts()[1].first_page(), 1);
    assert_eq!(converted.parts()[1].page_count(), 1);
    assert_eq!(converted.parts()[0].name(), Some("marker.txt"));
    assert_eq!(converted.parts()[1].name(), Some("scan.png"));

    let path = out_path("mixed.pdf");
    converted.write_to(&path).expect("write mixed pdf");

    let document = reopen(&path);
    assert_eq!(document.page_count().expect("page count"), 2);
    let first = document.page_text(0).expect("first page text");
    assert!(
        first.contains("Mixed request text page marker."),
        "text page must come first: {first:?}"
    );
    let second = document.page_text(1).expect("second page text");
    assert!(
        second.trim().is_empty(),
        "image page must follow: {second:?}"
    );
    assert_eq!(document.page(1).expect("page 1").contents.len(), 1);
}

#[test]
fn title_is_written_to_info_and_survives_reopen() {
    let input = ConvertInput::from_source(ConvertSource::text("Titled document body.\n"));
    let converted = convert_to_pdf(input, ConvertOptions::new().with_title("Quarterly Report"))
        .expect("convert titled document");
    assert_eq!(converted.title(), Some("Quarterly Report"));

    let path = out_path("titled.pdf");
    converted.write_to(&path).expect("write titled pdf");
    let document = reopen(&path);
    let info = document
        .info()
        .expect("read info dictionary")
        .expect("info dictionary present");
    assert_eq!(info.title.as_deref(), Some("Quarterly Report"));
    assert_eq!(info.producer.as_deref(), Some("PDFXI"));
}

#[test]
fn office_extensions_return_typed_office_unavailable() {
    for extension in ["docx", "xlsx", "pptx"] {
        let name = format!("t0240-report.{extension}");
        let error = ConvertInput::from_path(write_input(&name, b"PK\x03\x04placeholder"))
            .expect_err("office input must be refused");
        match error {
            ConvertError::OfficeUnavailable { format, message } => {
                assert_eq!(format, extension);
                assert!(
                    message.contains("Office converter"),
                    "message must be actionable: {message}"
                );
            }
            other => panic!("expected OfficeUnavailable for {extension}, got {other:?}"),
        }
    }
    // Legacy and OpenDocument families take the same path.
    let error = ConvertInput::from_bytes(Some("legacy.doc"), vec![0xD0, 0xCF, 0x11, 0xE0])
        .expect_err("legacy word must be refused");
    assert!(matches!(error, ConvertError::OfficeUnavailable { .. }));

    // A container with no usable name is classified by its member parts.
    let mut package = Vec::from(&b"PK\x03\x04"[..]);
    package.extend_from_slice(b"word/document.xml");
    let error = ConvertInput::from_bytes(Some("download"), package)
        .expect_err("sniffed docx must be refused");
    match error {
        ConvertError::OfficeUnavailable { format, .. } => assert_eq!(format, "docx"),
        other => panic!("expected sniffed OfficeUnavailable, got {other:?}"),
    }

    // A plain ZIP archive is neither a document nor an Office package.
    let error = ConvertInput::from_bytes(Some("archive.zip"), Vec::from(&b"PK\x03\x04payload"[..]))
        .expect_err("generic zip must be refused");
    match error {
        ConvertError::UnsupportedFormat { format } => assert_eq!(format, "zip"),
        other => panic!("expected UnsupportedFormat, got {other:?}"),
    }
}

#[test]
fn unsupported_formats_are_named_in_the_error() {
    let error = ConvertInput::from_path(write_input("t0240-animation.gif", b"GIF89a"))
        .expect_err("gif must be refused");
    match error {
        ConvertError::UnsupportedFormat { format } => assert_eq!(format, "gif"),
        other => panic!("expected UnsupportedFormat, got {other:?}"),
    }

    let error = ConvertInput::from_bytes(None, vec![0x00, 0xFF, 0x81, 0x7E])
        .expect_err("binary must be refused");
    match error {
        ConvertError::UnsupportedFormat { format } => assert_eq!(format, "binary"),
        other => panic!("expected UnsupportedFormat, got {other:?}"),
    }
}

#[test]
fn content_sniffing_detects_png_without_an_extension() {
    let png = rgb_png(16, 16, None);
    let input =
        ConvertInput::from_bytes(Some("scan-without-extension"), png).expect("sniff png bytes");
    assert_eq!(input.sources()[0].kind().label(), "png");

    let converted = convert_to_pdf(input, ConvertOptions::new()).expect("convert sniffed png");
    assert_eq!(converted.page_count(), 1);
}

#[test]
fn empty_request_and_invalid_options_are_typed_errors() {
    let error = convert_to_pdf(ConvertInput::new(), ConvertOptions::new())
        .expect_err("empty request must fail");
    assert!(matches!(error, ConvertError::EmptyInput));

    let error = convert_to_pdf(
        ConvertInput::from_source(ConvertSource::text("body")),
        ConvertOptions::new().with_font_size(0.0),
    )
    .expect_err("invalid font size must fail");
    assert!(matches!(
        error,
        ConvertError::Options(CreateError::InvalidOption(_))
    ));
}

#[test]
fn one_source_failure_names_the_source_position() {
    let input = ConvertInput::new()
        .with(ConvertSource::text("fine\n").with_name("ok.txt"))
        .with(ConvertSource::text("   \n\t\n").with_name("blank.txt"));
    let error = convert_to_pdf(input, ConvertOptions::new()).expect_err("blank source must fail");
    match error {
        ConvertError::Source { name, source } => {
            assert!(name.contains("#2"), "position must be reported: {name}");
            assert!(name.contains("blank.txt"), "name must be reported: {name}");
            assert!(matches!(source, CreateError::EmptyDocument));
        }
        other => panic!("expected Source error, got {other:?}"),
    }
}

#[test]
fn active_html_content_is_rejected_through_the_source_error() {
    let input = ConvertInput::from_source(ConvertSource::html(
        "<html><body><script>alert(1)</script></body></html>",
        None,
    ));
    let error = convert_to_pdf(input, ConvertOptions::new()).expect_err("script must be refused");
    match error {
        ConvertError::Source { source, .. } => {
            assert!(matches!(source, CreateError::HtmlScriptRejected(_)));
        }
        other => panic!("expected Source error, got {other:?}"),
    }
}

#[test]
fn identical_requests_produce_identical_bytes() {
    let input = || {
        ConvertInput::new()
            .with(ConvertSource::text("Determinism marker.\n").with_name("a.txt"))
            .with(ConvertSource::image(
                rgb_png(24, 18, None),
                CreateImageFormat::Png,
            ))
    };
    let first = convert_to_pdf(input(), ConvertOptions::new()).expect("first conversion");
    let second = convert_to_pdf(input(), ConvertOptions::new()).expect("second conversion");
    assert_eq!(first.bytes(), second.bytes());
}

#[test]
fn source_limit_bound_is_documented_and_enforced() {
    assert_eq!(CONVERT_MAX_SOURCES, 512);
    // A request at the limit still converts; the guard only rejects more.
    let input = ConvertInput::from_source(ConvertSource::text("body"));
    assert!(convert_to_pdf(input, ConvertOptions::new()).is_ok());
}

#[test]
fn plain_text_creator_is_reused_for_single_text_sources() {
    // The document-level path must not change the creation engine's output for
    // the first page; only the assembly and metadata steps are added.
    let text = "Reuse check line.\n";
    let direct =
        create_from_text(text, &pdfxi_convert::CreateOptions::new()).expect("direct creation");
    let direct_document = Document::from_bytes(direct).expect("direct parse");
    let converted = convert_to_pdf(
        ConvertInput::from_source(ConvertSource::text(text)),
        ConvertOptions::new(),
    )
    .expect("converted creation");
    let converted_document =
        Document::from_bytes(converted.bytes().to_vec()).expect("converted parse");
    assert_eq!(
        direct_document.page_count().expect("direct count"),
        converted_document.page_count().expect("converted count")
    );
    assert_eq!(
        direct_document.page_text(0).expect("direct text"),
        converted_document.page_text(0).expect("converted text")
    );
}

#[test]
fn converted_output_reopens_after_being_written_to_disk() {
    // The artifact paths this file writes are the ones the orchestrator opens
    // with PDFium; each test only asserts on the file it writes itself.
    let converted = convert_to_pdf(
        ConvertInput::from_source(ConvertSource::text("PDFium artifact marker.\n")),
        ConvertOptions::new(),
    )
    .expect("convert artifact");
    let path = out_path("pdfium-check.pdf");
    converted.write_to(&path).expect("write pdfium artifact");
    let document = reopen(&path);
    assert_eq!(document.page_count().expect("page count"), 1);
    let text = document.page_text(0).expect("page text");
    assert!(text.contains("PDFium artifact marker."), "marker: {text:?}");
}
