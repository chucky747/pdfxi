//! Test fixtures and helpers for PDFXI.
//!
//! The testkit builds small, deterministic PDFs in memory so unit and
//! integration tests do not depend on large binary fixtures. Real-world corpora
//! are fetched separately and are never committed.

#![deny(missing_docs)]
#![forbid(unsafe_code)]

/// Build a minimal single-page PDF that renders the text `Hello, PDFXI!`.
///
/// The cross-reference offsets are computed from the actual byte lengths, so
/// the result is a structurally valid PDF suitable for parser and CLI smoke
/// tests.
pub fn minimal_pdf() -> Vec<u8> {
    let mut objects: Vec<String> = Vec::new();
    objects.push("<< /Type /Catalog /Pages 2 0 R >>".to_string());
    objects.push("<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string());
    objects.push(
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R \
         /Resources << /Font << /F1 5 0 R >> >> >>"
            .to_string(),
    );
    let content = "BT /F1 24 Tf 72 720 Td (Hello, PDFXI!) Tj ET";
    let stream = format!(
        "<< /Length {} >>\nstream\n{}\nendstream",
        content.len(),
        content
    );
    objects.push(stream);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minimal_pdf_has_header_and_xref() {
        let pdf = minimal_pdf();
        assert!(pdf.starts_with(b"%PDF-1.7"));
        let text = String::from_utf8_lossy(&pdf);
        assert!(text.contains("xref"));
        assert!(text.contains("trailer"));
        assert!(text.ends_with("%%EOF\n"));
    }

    #[test]
    fn minimal_pdf_has_five_objects() {
        let text = String::from_utf8_lossy(&minimal_pdf()).to_string();
        for id in 1..=5 {
            assert!(text.contains(&format!("{} 0 obj", id)));
        }
    }
}
