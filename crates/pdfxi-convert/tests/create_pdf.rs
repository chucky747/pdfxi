//! Integration tests for T-0058: the PDFs emitted by `pdfxi-convert::create`
//! must be structurally valid documents when reparsed by `pdfxi-core`.

use pdfxi_convert::{
    CreateError, CreateImageFormat, CreateOptions, Margins, PageSize, create_from_image,
    create_from_markdown, create_from_text, create_pdf_from_path,
};
use pdfxi_core::Document;
use std::path::Path;

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

#[test]
fn text_pdf_round_trips_through_core_with_all_lines() {
    let pdf = create_from_text(
        "Hello PDFXI.\nSecond line.\nThird line.",
        &CreateOptions::new(),
    )
    .expect("create text PDF");
    let document = Document::from_bytes(pdf).expect("core parses generated PDF");
    assert_eq!(document.page_count().expect("page count"), 1);
    assert_eq!(document.version(), "1.4");
    // Seven in-use objects (catalog, pages, three fonts, one page, one content
    // stream) plus the free-list head.
    assert_eq!(document.trailer_size().expect("size"), Some(8));

    let text = document.page_text(0).expect("extract text");
    assert!(
        text.contains("Hello PDFXI."),
        "missing first line: {text:?}"
    );
    assert!(
        text.contains("Second line."),
        "missing second line: {text:?}"
    );
    assert!(text.contains("Third line."), "missing third line: {text:?}");
    assert!(text.matches('\n').count() >= 2, "line breaks: {text:?}");
}

#[test]
fn markdown_pdf_round_trips_with_pagination() {
    let source = (0..180)
        .map(|index| format!("Paragraph line {index} with several plain words."))
        .collect::<Vec<_>>()
        .join("\n\n");
    let pdf = create_from_markdown(&source, &CreateOptions::new()).expect("create markdown PDF");
    let document = Document::from_bytes(pdf).expect("core parses generated PDF");
    let count = document.page_count().expect("page count");
    assert!(count >= 2, "long markdown input must span several pages");
    for page in 0..count {
        let text = document.page_text(page).expect("page text");
        assert!(!text.is_empty(), "page {page} unexpectedly empty");
    }
    let first = document.page_text(0).expect("first page text");
    assert!(first.contains("Paragraph line 0"), "first line: {first:?}");
}

#[test]
fn image_pdf_round_trips_with_media_box_and_dpi_scaling() {
    let pdf = create_from_image(
        &sample_jpeg(),
        CreateImageFormat::Jpeg,
        &CreateOptions::new(),
    )
    .expect("create image PDF");
    let document = Document::from_bytes(pdf.clone()).expect("core parses generated PDF");
    assert_eq!(document.page_count().expect("page count"), 1);
    let page = document.page(0).expect("first page");
    assert_eq!(page.width(), 612.0);
    assert_eq!(page.height(), 792.0);
    assert_eq!(page.contents.len(), 1);

    // Rendering twice from the same bytes is byte-for-byte identical.
    let again = create_from_image(
        &sample_jpeg(),
        CreateImageFormat::Jpeg,
        &CreateOptions::new(),
    )
    .expect("create image PDF again");
    assert_eq!(pdf, again);
}

#[test]
fn custom_page_size_and_margins_round_trip() {
    let options = CreateOptions::new()
        .with_page_size(PageSize::A4)
        .with_margins(Margins::uniform(36.0).expect("margins"))
        .with_font_size(14.0)
        .with_line_height(1.5);
    let pdf = create_from_text("Custom geometry.", &options).expect("create PDF");
    let document = Document::from_bytes(pdf).expect("core parses generated PDF");
    assert_eq!(document.page_count().expect("page count"), 1);
    let page = document.page(0).expect("first page");
    assert!((page.width() - 595.28).abs() < 0.01);
    assert!((page.height() - 841.89).abs() < 0.01);
}

#[test]
fn path_routing_creates_parseable_pdfs_and_reports_bad_input() {
    let pdf = create_pdf_from_path(
        Path::new("notes.md"),
        b"# Title\n\nBody.\n",
        &CreateOptions::new(),
    )
    .expect("route markdown");
    let document = Document::from_bytes(pdf).expect("core parses routed PDF");
    assert_eq!(document.page_count().expect("page count"), 1);

    let error = create_pdf_from_path(Path::new("bad.gif"), b"bytes", &CreateOptions::new())
        .expect_err("gif is unsupported");
    assert!(matches!(
        error,
        CreateError::UnsupportedSourceExtension { .. }
    ));
}
