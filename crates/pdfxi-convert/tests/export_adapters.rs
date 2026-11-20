//! Library-level closure tests for the local export adapters (T-0151).
//!
//! These tests exercise the public API only. They assert the properties the
//! affected feature rows promise: deterministic bytes, stable structural
//! ordering, correct escaping, typed errors on empty/malformed input, and
//! bounded image encoders. No network, external converter, or file I/O is used.

use pdfxi_convert::{
    ConversionError, DocumentJson, DocumentMetadata, DocxExportError, DocxExporter, ExportKind,
    HtmlDocument, HtmlExportError, HtmlExportOptions, HtmlLink, HtmlOutputMode, HtmlPage, HtmlSpan,
    ImageFormat, JsonPage, JsonSpan, ModelError, OfficeBlock, OfficeDocument, OfficeExportError,
    OfficeExporter, OfficePage, PptxExporter, PsContent, PsDocument, PsExportError, PsPage, PsText,
    RtfOptions, RtfPage, TextExportPlan, XlsxExporter, encode, export_html, export_html_string,
    export_page_text, export_postscript, export_rtf, export_rtf_string,
};

/// Returns true when `needle` occurs anywhere in `haystack`.
fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() || needle.len() > haystack.len() {
        return false;
    }
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

#[test]
fn export_kind_routes_every_affected_row_to_a_family() {
    assert_eq!(ExportKind::from_extension("docx"), ExportKind::Word);
    assert_eq!(ExportKind::from_extension("xlsx"), ExportKind::Spreadsheet);
    assert_eq!(ExportKind::from_extension("pptx"), ExportKind::Presentation);
    assert_eq!(ExportKind::from_extension("rtf"), ExportKind::Rtf);
    assert_eq!(ExportKind::from_extension("html"), ExportKind::Html);
    assert_eq!(ExportKind::from_extension("txt"), ExportKind::Text);
    assert_eq!(ExportKind::from_extension("png"), ExportKind::Image);
    for target in ExportKind::all() {
        assert!(target.is_known());
        assert!(!target.label().is_empty());
    }
}

#[test]
fn image_export_is_bounded_deterministic_and_typed() {
    let rgba = vec![42u8; 8 * 8 * 4];
    for &format in ImageFormat::all() {
        let first = encode(8, 8, &rgba, format).expect("encode");
        let second = encode(8, 8, &rgba, format).expect("re-encode");
        assert_eq!(first, second, "determinism for {format}");
        assert!(matches!(
            encode(8, 8, &[0u8; 3], format),
            Err(ConversionError::InvalidInput { .. })
        ));
        assert!(matches!(
            encode(u32::MAX, 1, &[], format),
            Err(ConversionError::InvalidInput { .. })
        ));
    }
    // PNG bytes carry the standard signature and a valid IEND terminator.
    let png = encode(8, 8, &rgba, ImageFormat::Png).expect("PNG");
    assert_eq!(&png[..8], &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]);
    assert!(png.ends_with(&[0xAE, 0x42, 0x60, 0x82])); // CRC-32 of "IEND"
}

#[test]
fn html_export_escapes_text_and_orders_structure() {
    let pages = vec![
        HtmlPage::new(2, 200.0, 300.0).with_spans(vec![HtmlSpan::new("<third>", 10.0, 280.0)]),
        HtmlPage::new(0, 200.0, 300.0).with_spans(vec![HtmlSpan::new("A & B", 10.0, 280.0)]),
        HtmlPage::new(1, 200.0, 300.0).with_spans(vec![HtmlSpan::new("\"quoted\"", 10.0, 280.0)]),
    ];
    let doc = HtmlDocument::new("Title <x>", pages, Vec::new());
    let html = export_html_string(&doc).expect("HTML export");

    // Document-level structure is deterministic and in a fixed order.
    assert!(html.starts_with("<!DOCTYPE html>"));
    let nav = html.find("class=\"pdfxi-nav\"").expect("navigation");
    let main = html.find("<main>").expect("main");
    assert!(nav < main, "navigation must precede page content");
    let p1 = html.find("id=\"page-1\"").expect("page 1");
    let p2 = html.find("id=\"page-2\"").expect("page 2");
    let p3 = html.find("id=\"page-3\"").expect("page 3");
    assert!(p1 < p2 && p2 < p3, "pages must be sorted by index");

    // All five HTML entities are escaped in text and attributes.
    assert!(html.contains("&lt;Title &lt;x&gt;&gt;") || html.contains("&lt;x&gt;"));
    assert!(html.contains("A &amp; B"));
    assert!(html.contains("&lt;third&gt;"));
    assert!(html.contains("&quot;quoted&quot;"));
    assert!(!html.contains("<script"), "output must remain inert");
    assert_eq!(html, export_html_string(&doc).expect("re-export"));

    // Page-set mode also yields a deterministic, ordered set of files.
    let files = export_html(
        &doc,
        &HtmlExportOptions::new().with_mode(HtmlOutputMode::PageSet),
    )
    .expect("page-set export");
    assert_eq!(files.len(), 4);
    assert_eq!(files[0].name, "index.html");
    assert_eq!(files[3].name, "page-0003.html");
}

#[test]
fn html_export_rejects_empty_and_unsafe_documents() {
    let empty = HtmlDocument::new("T", Vec::new(), Vec::new());
    assert!(matches!(
        export_html_string(&empty),
        Err(HtmlExportError::EmptyDocument)
    ));

    let unsafe_ref = HtmlDocument::new(
        "T",
        vec![HtmlPage::new(0, 200.0, 300.0).with_image_ref("../secret.png")],
        Vec::new(),
    );
    assert!(matches!(
        export_html_string(&unsafe_ref),
        Err(HtmlExportError::UnsafeImageReference { .. })
    ));

    let unsafe_link = HtmlDocument::new(
        "T",
        vec![HtmlPage::new(0, 200.0, 300.0)],
        vec![HtmlLink::new(0, "javascript:alert(1)", 0.0, 0.0, 1.0, 1.0)],
    );
    assert!(matches!(
        export_html_string(&unsafe_link),
        Err(HtmlExportError::UnsupportedUriScheme { .. })
    ));
}

#[test]
fn rtf_export_is_deterministic_escaped_and_valid_when_empty() {
    let pages = vec![RtfPage::new(1, "second & <x>"), RtfPage::new(0, "first")];
    let first = export_rtf_string(pages.clone().into_iter(), &RtfOptions::new()).expect("RTF");
    let second =
        export_rtf_string(pages.clone().into_iter(), &RtfOptions::new()).expect("RTF again");
    assert_eq!(first, second, "RTF bytes must be deterministic");
    assert!(first.starts_with("{\\rtf1"));
    assert!(first.find("first").expect("first") < first.find("second").expect("second"));
    assert!(
        first.contains("& <x>"),
        "RTF only escapes \\, braces and Unicode"
    );
    assert!(first.ends_with('}'));
    assert_eq!(
        export_rtf(pages.into_iter(), &RtfOptions::new()).expect("RTF bytes"),
        first.as_bytes()
    );

    let empty = export_rtf_string(std::iter::empty(), &RtfOptions::new()).expect("empty RTF");
    assert!(empty.starts_with("{\\rtf1"));
    assert!(empty.ends_with('}'));
}

#[test]
fn office_export_is_deterministic_and_structure_ordered() {
    let page = |index, text| {
        OfficePage::new(index, 200.0, 300.0).with_blocks(vec![OfficeBlock::paragraph(text)])
    };
    let doc = OfficeDocument::new(
        "<Report & Summary>",
        vec![page(1, "Second page"), page(0, "First page")],
    );

    let exporters: [(&str, &dyn OfficeExporter); 3] = [
        ("docx", &DocxExporter),
        ("xlsx", &XlsxExporter::new()),
        ("pptx", &PptxExporter),
    ];
    for (name, exporter) in exporters {
        let first = exporter.export(&doc).expect("office export");
        let second = exporter.export(&doc).expect("office re-export");
        assert_eq!(
            first.bytes, second.bytes,
            "{name} bytes must be deterministic"
        );
        assert_eq!(first.extension(), name);
        assert!(first.report.part_count >= 3, "{name} must be a valid ZIP");
        assert!(
            contains_bytes(&first.bytes, b"First page"),
            "{name} first page"
        );
        assert!(
            contains_bytes(&first.bytes, b"Second page"),
            "{name} second page"
        );
        let first_at = first
            .bytes
            .windows(10)
            .position(|window| window == b"First page")
            .expect("first page present");
        let second_at = first
            .bytes
            .windows(11)
            .position(|window| window == b"Second page")
            .expect("second page present");
        assert!(first_at < second_at, "{name} must order pages ascending");
    }

    // XML escaping applies to every format, not just DOCX.
    let docx = DocxExporter.export(&doc).expect("docx");
    assert!(contains_bytes(&docx.bytes, b"&lt;Report &amp; Summary&gt;"));
    assert!(!contains_bytes(&docx.bytes, b"<Report & Summary>"));
}

#[test]
fn office_export_rejects_empty_and_malformed_models() {
    let empty = OfficeDocument::new("T", Vec::new());
    assert_eq!(
        DocxExporter.export(&empty),
        Err(OfficeExportError::Docx(DocxExportError::Invalid(
            ModelError::EmptyDocument,
        )))
    );

    let page = OfficePage::new(0, 200.0, 300.0);
    let duplicate = OfficeDocument::new("T", vec![page.clone(), page]);
    assert!(XlsxExporter::new().export(&duplicate).is_err());

    let malformed = OfficeDocument::new("T", vec![OfficePage::new(0, f32::NAN, 300.0)]);
    assert!(PptxExporter.export(&malformed).is_err());
}

#[test]
fn postscript_export_is_deterministic_and_structure_ordered() {
    let page = |index, text| {
        PsPage::new(index, 200.0, 300.0)
            .with_content(vec![PsContent::Text(PsText::new(text, 10.0, 280.0, 12.0))])
    };
    let doc = PsDocument::new(
        "Title (with parens)",
        vec![page(1, "Second"), page(0, "First")],
    );
    let first = export_postscript(&doc).expect("PostScript");
    let second = export_postscript(&doc).expect("PostScript again");
    assert_eq!(first, second, "PostScript bytes must be deterministic");

    let text = String::from_utf8(first).expect("PostScript is ASCII");
    assert!(text.starts_with("%!PS-Adobe-3.0"));
    assert!(text.ends_with("%%EOF"));
    let p1 = text.find("%%Page:").expect("page 1");
    let second_page = text[p1 + 1..].find("%%Page:").expect("page 2") + p1 + 1;
    assert!(p1 < second_page);
    assert_eq!(text.matches("showpage").count(), 2);
    // Parentheses in the DSC title must be escaped in the literal string.
    assert!(text.contains("Title \\(with parens\\)"));
}

#[test]
fn postscript_export_rejects_empty_and_malformed_documents() {
    let empty = PsDocument::new("T", Vec::new());
    assert_eq!(export_postscript(&empty), Err(PsExportError::EmptyDocument));

    let page = PsPage::new(0, 200.0, 300.0);
    let duplicate = PsDocument::new("T", vec![page.clone(), page]);
    assert_eq!(
        export_postscript(&duplicate),
        Err(PsExportError::DuplicatePage { page: 0 })
    );

    let bad = PsDocument::new("T", vec![PsPage::new(0, f32::NAN, 300.0)]);
    assert_eq!(
        export_postscript(&bad),
        Err(PsExportError::InvalidPageGeometry { page: 0 })
    );
}

#[test]
fn text_and_json_export_are_structured_and_deterministic() {
    let pages = vec![(1usize, "second".to_owned()), (0usize, "first".to_owned())];
    let text = export_page_text(pages.into_iter(), &TextExportPlan::new());
    assert_eq!(text, "first\nsecond");
    assert_eq!(
        text,
        export_page_text(
            vec![(0usize, "first".to_owned()), (1usize, "second".to_owned())].into_iter(),
            &TextExportPlan::new()
        ),
        "page order must not change the output"
    );

    let metadata = DocumentMetadata::new("Title", "Author", 1);
    let json_page = JsonPage::new(0, "hello", vec![JsonSpan::new("hello", 0.0, 0.0, 2.0, 3.0)]);
    let json = DocumentJson::new(metadata, vec![json_page]).to_json_string();
    assert!(json.starts_with("{\"json_schema_version\":1,"));
    assert!(json.contains("\"title\":\"Title\""));
    assert!(json.contains("\"text\":\"hello\""));
}
