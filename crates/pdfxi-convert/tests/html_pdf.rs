//! Integration tests for T-0084: PDFs emitted by the HTML importer must be
//! structurally valid documents when reparsed by `pdfxi-core`.

use pdfxi_convert::{
    CreateError, CreateOptions, HtmlOptions, create_from_html, create_from_html_with_options,
};
use pdfxi_core::Document;
use std::fs;

fn sample_png() -> Vec<u8> {
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
    let mut ihdr = Vec::from(&8u32.to_be_bytes()[..]);
    ihdr.extend_from_slice(&8u32.to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
    let raw = vec![0u8; 8 * 3 + 1];
    let mut z = vec![0x78, 0x01, 0x01];
    z.extend_from_slice(&(raw.len() as u16).to_le_bytes());
    z.extend_from_slice(&(!(raw.len() as u16)).to_le_bytes());
    z.extend_from_slice(&raw);
    z.extend_from_slice(&adler(&raw).to_be_bytes());
    let mut out = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"IDAT", &z);
    chunk(&mut out, b"IEND", &[]);
    out
}

#[test]
fn html_pdf_round_trips_with_heading_and_body_text() {
    let html = "<h1>Quarterly Report</h1><p>All <strong>three</strong> lines are present.</p>\
                <ul><li>alpha</li><li>beta</li></ul>";
    let pdf = create_from_html(html, &CreateOptions::new()).expect("create HTML PDF");
    let document = Document::from_bytes(pdf).expect("core parses HTML PDF");
    assert_eq!(document.version(), "1.4");
    assert_eq!(document.page_count().expect("page count"), 1);
    let text = document.page_text(0).expect("page text");
    assert!(text.contains("Quarterly Report"), "heading: {text:?}");
    assert!(
        text.contains("All three lines are present."),
        "body: {text:?}"
    );
    assert!(
        text.contains("alpha") && text.contains("beta"),
        "list: {text:?}"
    );
}

#[test]
fn long_html_document_paginates_and_every_page_has_text() {
    let html = (0..160)
        .map(|index| format!("<p>Paragraph {index} with several plain words.</p>"))
        .collect::<String>();
    let pdf = create_from_html(&html, &CreateOptions::new()).expect("create long HTML PDF");
    let document = Document::from_bytes(pdf).expect("core parses long HTML PDF");
    let count = document.page_count().expect("page count");
    assert!(count >= 2, "160 paragraphs must span several pages");
    for page in 0..count {
        let text = document.page_text(page).expect("page text");
        assert!(!text.is_empty(), "empty page {page}");
    }
    let first = document.page_text(0).expect("first page text");
    assert!(first.contains("Paragraph 0"), "first paragraph: {first:?}");
}

#[test]
fn html_pdf_with_local_png_round_trips() {
    let dir = std::env::temp_dir().join(format!("pdfxi-html-rt-{}", std::process::id()));
    fs::create_dir_all(&dir).expect("temp dir");
    let image = dir.join("figure.png");
    fs::write(&image, sample_png()).expect("write png");
    let html = "<p>Figure below.</p><img src=\"figure.png\"><p>Figure above.</p>";
    let options = HtmlOptions::new().with_base_dir(&dir);
    let pdf = create_from_html_with_options(html, &options, &CreateOptions::new())
        .expect("create image HTML PDF");
    let document = Document::from_bytes(pdf.clone()).expect("core parses image HTML PDF");
    assert_eq!(document.page_count().expect("page count"), 1);
    let again = create_from_html_with_options(html, &options, &CreateOptions::new())
        .expect("create image HTML PDF again");
    assert_eq!(pdf, again, "image placement is deterministic");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn unsafe_html_is_rejected_with_typed_errors() {
    let script = create_from_html("<script>alert(1)</script>", &CreateOptions::new())
        .expect_err("script rejected");
    assert!(matches!(script, CreateError::HtmlScriptRejected(_)));

    let remote = create_from_html(
        "<img src=\"https://example.org/a.png\">",
        &CreateOptions::new(),
    )
    .expect_err("remote rejected");
    assert!(matches!(remote, CreateError::HtmlRemoteResource(_)));

    let traversal =
        create_from_html("<img src=\"../a.png\">", &CreateOptions::new()).expect_err("traversal");
    assert!(matches!(traversal, CreateError::HtmlPathTraversal(_)));
}
