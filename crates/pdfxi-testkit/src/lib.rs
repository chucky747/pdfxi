//! Test fixtures and helpers for PDFXI.
//!
//! The testkit builds small, deterministic PDFs in memory so unit and
//! integration tests do not depend on large binary fixtures. Real-world corpora
//! are fetched separately and are never committed.
//!
//! Every builder returns a `Vec<u8>` and writes nothing to disk. The generators
//! emit a classic cross-reference table whose offsets are computed from the
//! actual byte lengths of the objects, so the produced bytes are structurally
//! valid PDFs suitable for parser, renderer, and CLI smoke tests.
//!
//! # Object layout
//!
//! Object numbers are assigned deterministically:
//!
//! - object `1` — the document catalog;
//! - object `2` — the page tree root (`/Pages`);
//! - for page `i` (zero-based): object `3 + 2*i` is the `/Page` and object
//!   `4 + 2*i` is its content stream;
//! - the final object, `3 + 2*n` where `n` is the page count, is the shared
//!   Helvetica font resource.
//!
//! A single-page document therefore contains five objects, matching the
//! original [`minimal_pdf`] output byte for byte.

#![deny(missing_docs)]
#![forbid(unsafe_code)]

pub mod fixtures;
pub mod golden;
pub mod hash;
pub mod perf;

/// Default page width in PDF points (US Letter portrait).
const DEFAULT_WIDTH: f64 = 612.0;
/// Default page height in PDF points (US Letter portrait).
const DEFAULT_HEIGHT: f64 = 792.0;
/// Default text shown by [`minimal_pdf`] and [`pdf_with_page_size`].
const DEFAULT_TEXT: &str = "Hello, PDFXI!";

/// Description of a single generated page.
struct PageSpec {
    text: String,
    width: f64,
    height: f64,
}

impl PageSpec {
    fn new(text: &str, width: f64, height: f64) -> Self {
        Self {
            text: text.to_string(),
            width,
            height,
        }
    }
}

/// Format a finite PDF number without a trailing `.0` for integral values.
fn fmt_num(value: f64) -> String {
    if value.fract() == 0.0 {
        (value as i64).to_string()
    } else {
        value.to_string()
    }
}

/// Escape a string for use inside a PDF literal string `(...)`.
fn escape_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '(' => out.push_str("\\("),
            ')' => out.push_str("\\)"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => out.push_str(&format!("\\{:03o}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

/// Assemble a complete PDF document from a list of page descriptions.
fn build_pdf(pages: &[PageSpec]) -> Vec<u8> {
    let count = pages.len();
    let font_id = 3 + 2 * count;

    let kids: String = (0..count).map(|i| format!("{} 0 R ", 3 + 2 * i)).collect();

    let mut objects: Vec<String> = Vec::with_capacity(3 + 2 * count);
    objects.push("<< /Type /Catalog /Pages 2 0 R >>".to_string());
    objects.push(format!(
        "<< /Type /Pages /Kids [{}] /Count {} >>",
        kids.trim_end(),
        count
    ));

    for (i, page) in pages.iter().enumerate() {
        let content_id = 3 + 2 * i + 1;
        objects.push(format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {} {}] /Contents {} 0 R \
             /Resources << /Font << /F1 {} 0 R >> >> >>",
            fmt_num(page.width),
            fmt_num(page.height),
            content_id,
            font_id
        ));
        let content = format!("BT /F1 24 Tf 72 720 Td ({}) Tj ET", escape_text(&page.text));
        objects.push(format!(
            "<< /Length {} >>\nstream\n{}\nendstream",
            content.len(),
            content
        ));
    }

    objects.push("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string());

    let mut out = String::from("%PDF-1.7\n%PDFXI\n");
    let mut offsets = Vec::with_capacity(objects.len());
    for (index, body) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.push_str(&format!("{} 0 obj\n{}\nendobj\n", index + 1, body));
    }
    let xref = out.len();
    out.push_str(&format!("xref\n0 {}\n", objects.len() + 1));
    out.push_str("0000000000 65535 f \n");
    for offset in offsets {
        out.push_str(&format!("{:010} 00000 n \n", offset));
    }
    out.push_str(&format!(
        "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{}\n%%EOF\n",
        objects.len() + 1,
        xref
    ));
    out.into_bytes()
}

/// Build a minimal single-page PDF that renders the text `Hello, PDFXI!`.
///
/// The page uses the default US Letter size (612 x 792 points). The
/// cross-reference offsets are computed from the actual byte lengths, so the
/// result is a structurally valid PDF suitable for parser and CLI smoke tests.
/// This is the original builder and its output is byte-for-byte unchanged.
pub fn minimal_pdf() -> Vec<u8> {
    build_pdf(&[PageSpec::new(DEFAULT_TEXT, DEFAULT_WIDTH, DEFAULT_HEIGHT)])
}

/// Build a single-page PDF whose content stream draws `text`.
///
/// The page uses the default US Letter size (612 x 792 points). Characters that
/// are special inside a PDF literal string (`\`, `(`, and `)`) are escaped, and
/// the stream `/Length` reflects the escaped byte count.
pub fn text_pdf(text: &str) -> Vec<u8> {
    build_pdf(&[PageSpec::new(text, DEFAULT_WIDTH, DEFAULT_HEIGHT)])
}

/// Build a PDF containing exactly `pages` pages.
///
/// Each page is labelled `Page N of TOTAL` so tests can assert both the page
/// count and the presence of distinct per-page text. Passing `0` yields a valid
/// zero-page document with an empty `/Kids` array and `/Count 0`.
pub fn multipage_pdf(pages: usize) -> Vec<u8> {
    let specs: Vec<PageSpec> = (1..=pages)
        .map(|n| {
            PageSpec::new(
                &format!("Page {n} of {pages}"),
                DEFAULT_WIDTH,
                DEFAULT_HEIGHT,
            )
        })
        .collect();
    build_pdf(&specs)
}

/// Build a single-page PDF with a custom media box.
///
/// `width` and `height` are expressed in PDF points. Non-finite or non-positive
/// values are replaced with the US Letter defaults (612 x 792) so the builder
/// always emits a structurally valid document. The drawn text is the same as
/// [`minimal_pdf`].
pub fn pdf_with_page_size(width: f64, height: f64) -> Vec<u8> {
    let width = if width.is_finite() && width > 0.0 {
        width
    } else {
        DEFAULT_WIDTH
    };
    let height = if height.is_finite() && height > 0.0 {
        height
    } else {
        DEFAULT_HEIGHT
    };
    build_pdf(&[PageSpec::new(DEFAULT_TEXT, width, height)])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn as_text(pdf: &[u8]) -> String {
        String::from_utf8_lossy(pdf).into_owned()
    }

    /// Read the byte offset written after the `startxref` keyword.
    fn startxref(pdf: &[u8]) -> usize {
        let text = as_text(pdf);
        let marker = "startxref\n";
        let at = text.find(marker).expect("startxref keyword present");
        let tail = &text[at + marker.len()..];
        let digits: String = tail.chars().take_while(|c| c.is_ascii_digit()).collect();
        digits.parse::<usize>().expect("numeric startxref")
    }

    /// `startxref` must point at the literal `xref` keyword.
    fn assert_startxref_points_at_xref(pdf: &[u8]) {
        let offset = startxref(pdf);
        assert!(offset < pdf.len(), "startxref offset out of range");
        assert_eq!(
            &pdf[offset..offset + 4],
            b"xref",
            "startxref must point at the xref keyword"
        );
    }

    /// Verify every in-use xref entry points at its `N 0 obj` header.
    fn assert_xref_offsets_valid(pdf: &[u8]) {
        let text = as_text(pdf);
        let xref_at = startxref(pdf);
        let section = &text[xref_at..];
        let mut lines = section.lines();
        assert_eq!(lines.next(), Some("xref"));
        let header = lines.next().expect("xref subsection header");
        let size: usize = header
            .split_whitespace()
            .nth(1)
            .expect("xref size")
            .parse()
            .expect("numeric xref size");

        let mut object_id = 0usize;
        for line in lines {
            if line == "trailer" {
                break;
            }
            if line.trim().is_empty() {
                continue;
            }
            let offset: usize = line
                .split_whitespace()
                .next()
                .expect("xref offset")
                .parse()
                .expect("numeric xref offset");
            if object_id != 0 {
                let header = format!("{object_id} 0 obj");
                let end = (offset + header.len()).min(pdf.len());
                assert_eq!(
                    &pdf[offset..end],
                    header.as_bytes(),
                    "xref entry for object {object_id} points at the wrong offset"
                );
            }
            object_id += 1;
        }
        assert_eq!(object_id, size, "xref entry count must equal /Size");
    }

    /// Verify each declared stream `/Length` matches the bytes before `endstream`.
    fn assert_stream_lengths_valid(pdf: &[u8]) {
        let text = as_text(pdf);
        let marker = ">>\nstream\n";
        let mut cursor = 0usize;
        let mut streams = 0usize;
        while let Some(rel) = text[cursor..].find(marker) {
            let stream_at = cursor + rel + ">>\n".len();
            let before = &text[..stream_at];
            let len_at =
                before.rfind("/Length ").expect("/Length before stream") + "/Length ".len();
            let len_end = len_at + text[len_at..].find(' ').expect("space after length");
            let declared: usize = text[len_at..len_end].parse().expect("numeric length");

            let data_start = stream_at + "stream\n".len();
            let data_end = data_start + declared;
            assert!(data_end + "\nendstream".len() <= text.len());
            assert_eq!(
                &text[data_end..data_end + "\nendstream".len()],
                "\nendstream",
                "declared stream length does not match its data"
            );
            streams += 1;
            cursor = data_end;
        }
        assert!(streams > 0, "expected at least one stream");
    }

    fn assert_header_and_eof(pdf: &[u8]) {
        assert!(pdf.starts_with(b"%PDF-1.7"), "missing PDF header");
        assert!(pdf.ends_with(b"%%EOF\n"), "missing PDF trailer marker");
    }

    #[test]
    fn minimal_pdf_has_header_and_xref() {
        let pdf = minimal_pdf();
        assert_header_and_eof(&pdf);
        let text = as_text(&pdf);
        assert!(text.contains("xref"));
        assert!(text.contains("trailer"));
    }

    #[test]
    fn minimal_pdf_has_five_objects() {
        let text = as_text(&minimal_pdf());
        assert_eq!(text.matches(" 0 obj").count(), 5);
        for id in 1..=5 {
            assert!(text.contains(&format!("{id} 0 obj")));
        }
    }

    #[test]
    fn minimal_pdf_is_byte_stable() {
        assert_eq!(minimal_pdf(), minimal_pdf());
    }

    #[test]
    fn minimal_pdf_startxref_points_at_xref() {
        assert_startxref_points_at_xref(&minimal_pdf());
    }

    #[test]
    fn all_builders_have_valid_xref_offsets_and_lengths() {
        for pdf in [
            minimal_pdf(),
            text_pdf("custom content"),
            multipage_pdf(3),
            pdf_with_page_size(595.28, 841.89),
        ] {
            assert_header_and_eof(&pdf);
            assert_startxref_points_at_xref(&pdf);
            assert_xref_offsets_valid(&pdf);
            assert_stream_lengths_valid(&pdf);
        }
    }

    #[test]
    fn text_pdf_contains_requested_text() {
        let pdf = text_pdf("The quick brown fox");
        let text = as_text(&pdf);
        assert!(text.contains("(The quick brown fox) Tj"), "text missing");
        assert_eq!(text.matches(" 0 obj").count(), 5);
        assert_startxref_points_at_xref(&pdf);
    }

    #[test]
    fn text_pdf_escapes_special_literal_characters() {
        let pdf = text_pdf("a(b)\\c)");
        let text = as_text(&pdf);
        assert!(text.contains(r"a\(b\)\\c\)"), "special chars not escaped");
        assert_stream_lengths_valid(&pdf);
    }

    #[test]
    fn multipage_pdf_reports_requested_page_count() {
        let pdf = multipage_pdf(4);
        let text = as_text(&pdf);
        assert!(text.contains("/Count 4"), "page count missing");
        // 2 root objects + 2 objects per page + 1 font.
        assert_eq!(text.matches(" 0 obj").count(), 2 + 2 * 4 + 1);
        assert_startxref_points_at_xref(&pdf);
    }

    #[test]
    fn multipage_pdf_has_distinct_text_on_each_page() {
        let text = as_text(&multipage_pdf(3));
        for n in 1..=3 {
            assert!(text.contains(&format!("(Page {n} of 3) Tj")));
        }
    }

    #[test]
    fn multipage_pdf_zero_pages_is_valid() {
        let pdf = multipage_pdf(0);
        let text = as_text(&pdf);
        assert!(text.contains("/Kids [] /Count 0"));
        assert_eq!(text.matches(" 0 obj").count(), 3);
        assert_startxref_points_at_xref(&pdf);
        assert_xref_offsets_valid(&pdf);
    }

    #[test]
    fn pdf_with_page_size_embeds_requested_mediabox() {
        let pdf = pdf_with_page_size(595.0, 842.0);
        let text = as_text(&pdf);
        assert!(text.contains("/MediaBox [0 0 595 842]"));
        assert!(text.contains("(Hello, PDFXI!) Tj"));
        assert_startxref_points_at_xref(&pdf);
    }

    #[test]
    fn pdf_with_page_size_supports_fractional_dimensions() {
        let pdf = pdf_with_page_size(595.28, 841.89);
        let text = as_text(&pdf);
        assert!(text.contains("/MediaBox [0 0 595.28 841.89]"));
        assert_xref_offsets_valid(&pdf);
    }

    #[test]
    fn pdf_with_page_size_rejects_invalid_dimensions() {
        let pdf = pdf_with_page_size(f64::NAN, -10.0);
        let text = as_text(&pdf);
        assert!(text.contains("/MediaBox [0 0 612 792]"));
        assert_xref_offsets_valid(&pdf);
    }
}
