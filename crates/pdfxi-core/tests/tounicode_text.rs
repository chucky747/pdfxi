//! Integration coverage for `/ToUnicode`-aware text extraction.
//!
//! The synthetic document exercises every destination form required by
//! ISO 32000-1 section 9.10.3 that the Edge-produced subset font uses, plus an
//! unmapped code. The real-world case mirrors the corpus marker check without
//! depending on the corpus manifest.

use std::path::PathBuf;

use pdfxi_core::Document;

/// Build a classic-xref PDF from object bodies `1..=N`.
fn pdf_from_objects(objects: &[String]) -> Vec<u8> {
    let mut out = String::from("%PDF-1.7\n");
    let mut offsets = Vec::with_capacity(objects.len());
    for (index, body) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.push_str(&format!("{} 0 obj\n{}\nendobj\n", index + 1, body));
    }
    let xref = out.len();
    out.push_str(&format!("xref\n0 {}\n", objects.len() + 1));
    out.push_str("0000000000 65535 f \n");
    for offset in &offsets {
        out.push_str(&format!("{offset:010} 00000 n \n"));
    }
    out.push_str(&format!(
        "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
        objects.len() + 1
    ));
    out.into_bytes()
}

/// Wrap `body` in a stream object with an accurate `/Length`.
fn stream(body: &str) -> String {
    format!("<< /Length {} >>\nstream\n{}\nendstream", body.len(), body)
}

/// Build a one-page document whose single font is `/F1`.
fn one_font_pdf(font: &str, content: &str) -> Vec<u8> {
    pdf_from_objects(&[
        "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
         /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>"
            .to_owned(),
        stream(content),
        font.to_owned(),
    ])
}

const SAMPLE_CMAP: &str = "/CIDInit /ProcSet findresource begin
12 dict begin
begincmap
1 begincodespacerange
<0000> <FFFF>
endcodespacerange
2 beginbfchar
<0001> <0041>
<0003> <D83DDE00>
endbfchar
2 beginbfrange
<0004> <0006> <0061>
<0008> <000A> [<006B> <006C> <006D>]
endbfrange
endcmap
end
end";

#[test]
fn page_text_applies_bfchar_bfrange_and_array_destinations() {
    let font = "<< /Type /Font /Subtype /Type0 /BaseFont /TestSubset \
                /Encoding /Identity-H /ToUnicode 6 0 R >>";
    let content = "BT /F1 12 Tf <0001000300040005000600080009000A0002> Tj ET";
    let objects = vec![
        "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
         /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>"
            .to_owned(),
        stream(content),
        font.to_owned(),
        stream(SAMPLE_CMAP),
    ];
    let document = Document::from_bytes(pdf_from_objects(&objects)).expect("valid PDF");
    assert_eq!(
        document.page_text(0).expect("page text"),
        "A\u{1F600}abcklm\u{FFFD}"
    );
}

#[test]
fn page_text_switches_fonts_with_tf() {
    let cmap_a = "1 begincodespacerange <0000> <FFFF> endcodespacerange \
                  1 beginbfchar <0001> <0041> endbfchar";
    let cmap_b = "1 begincodespacerange <0000> <FFFF> endcodespacerange \
                  1 beginbfchar <0001> <0042> endbfchar";
    let content = "BT /F1 12 Tf <0001> Tj /F2 12 Tf <0001> Tj ET";
    let objects = vec![
        "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
         /Resources << /Font << /F1 5 0 R /F2 6 0 R >> >> /Contents 4 0 R >>"
            .to_owned(),
        stream(content),
        "<< /Type /Font /Subtype /Type0 /BaseFont /A /Encoding /Identity-H \
         /ToUnicode 7 0 R >>"
            .to_owned(),
        "<< /Type /Font /Subtype /Type0 /BaseFont /B /Encoding /Identity-H \
         /ToUnicode 8 0 R >>"
            .to_owned(),
        stream(cmap_a),
        stream(cmap_b),
    ];
    let document = Document::from_bytes(pdf_from_objects(&objects)).expect("valid PDF");
    assert_eq!(document.page_text(0).expect("page text"), "AB");
}

#[test]
fn page_text_without_to_unicode_is_unchanged() {
    let font = "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>";
    let document = Document::from_bytes(one_font_pdf(font, "BT /F1 12 Tf (Hello, PDFXI!) Tj ET"))
        .expect("valid PDF");
    assert_eq!(document.page_text(0).expect("page text"), "Hello, PDFXI!");

    let minimal = Document::from_bytes(pdfxi_testkit::minimal_pdf()).expect("valid fixture");
    assert_eq!(minimal.page_text(0).expect("page text"), "Hello, PDFXI!");
}

#[test]
fn browser_print_fixture_extracts_unicode_markers() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/realworld/browser-print.pdf");
    let bytes = std::fs::read(&path).expect("read browser-print.pdf");
    let document = Document::from_bytes(bytes).expect("valid PDF");
    let text = document.page_text(0).expect("page text");
    for marker in ["kestrel browser", "kestrel anchor"] {
        assert!(text.contains(marker), "missing {marker:?} in {text:?}");
    }
    assert!(
        !text.contains('\u{0}'),
        "subset glyph codes leaked into the text: {text:?}"
    );
}
