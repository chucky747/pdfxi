//! Write an ingested PostScript subset into deterministic PDF bytes.
//!
//! [`crate::postscript_ingest`] turns DSC PostScript into [`PsContent`] pages
//! without a Turing-complete interpreter. This module is the missing writer:
//! it maps the captured text runs, rectangles and lines onto standard Type 1
//! fonts and page content streams, so a Distiller-style import produces a real
//! PDF instead of only a plan.
//!
//! Images never reach this stage: the ingester records them as unsupported
//! findings and emits no [`PsContent::Image`] item. Any image item supplied by a
//! caller is skipped rather than faked.
//!
//! Output is deterministic: fixed object numbers, fixed font resources and
//! trimmed fixed-precision numbers.

use thiserror::Error;

use crate::postscript::{PsContent, PsFont};
use crate::postscript_ingest::IngestedDocument;

/// Two decimal places is enough for page geometry and avoids locale drift.
const NUMBER_SCALE: f32 = 100.0;

/// A typed failure while writing an ingested document as PDF.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum PsPdfError {
    /// The ingested document has no pages.
    #[error("PostScript document has no pages to write")]
    EmptyDocument,
    /// A page or content coordinate was not finite.
    #[error("non-finite geometry on page {page}")]
    NonFiniteGeometry {
        /// One-based page number.
        page: usize,
    },
    /// A page extent was not positive.
    #[error("page {page} has an empty page box")]
    EmptyPageBox {
        /// One-based page number.
        page: usize,
    },
}

/// Options for the PostScript-to-PDF writer.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PsPdfOptions {
    /// Width used when the input has no bounding box or content extents.
    pub default_width: f32,
    /// Height used when the input has no bounding box or content extents.
    pub default_height: f32,
    /// Margin added around content-derived page boxes.
    pub margin: f32,
}

impl Default for PsPdfOptions {
    fn default() -> Self {
        Self {
            default_width: 612.0,
            default_height: 792.0,
            margin: 36.0,
        }
    }
}

/// Minimal deterministic PDF serializer.
struct Writer {
    objects: Vec<Vec<u8>>,
}

impl Writer {
    fn new() -> Self {
        Self {
            objects: Vec::new(),
        }
    }

    fn reserve(&mut self) -> u32 {
        self.objects.push(Vec::new());
        self.objects.len() as u32
    }

    fn set(&mut self, id: u32, body: Vec<u8>) {
        let index = usize::try_from(id).unwrap_or_default().saturating_sub(1);
        if let Some(slot) = self.objects.get_mut(index) {
            *slot = body;
        }
    }

    fn finish(&self, root: u32) -> Vec<u8> {
        let mut out = b"%PDF-1.4\n%\xE2\xE3\xCF\xD3\n".to_vec();
        let mut offsets = Vec::with_capacity(self.objects.len());
        for (index, body) in self.objects.iter().enumerate() {
            offsets.push(out.len());
            out.extend_from_slice(format!("{} 0 obj\n", index + 1).as_bytes());
            out.extend_from_slice(body);
            out.extend_from_slice(b"\nendobj\n");
        }
        let xref = out.len();
        out.extend_from_slice(format!("xref\n0 {}\n", self.objects.len() + 1).as_bytes());
        out.extend_from_slice(b"0000000000 65535 f \n");
        for offset in &offsets {
            out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        out.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root {root} 0 R >>\nstartxref\n{xref}\n%%EOF\n",
                self.objects.len() + 1
            )
            .as_bytes(),
        );
        out
    }
}

/// Trim a coordinate to two decimals without a trailing `.0`.
fn number(value: f32) -> String {
    let rounded = (f64::from(value) * f64::from(NUMBER_SCALE)).round() / f64::from(NUMBER_SCALE);
    let text = format!("{rounded:.2}");
    let trimmed = text.trim_end_matches('0').trim_end_matches('.');
    if trimmed.is_empty() || trimmed == "-0" {
        "0".to_owned()
    } else {
        trimmed.to_owned()
    }
}

/// Escape a text run into a Latin-1 literal string.
fn escape_text(text: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len());
    for character in text.chars() {
        let code = u32::from(character);
        let byte = if code <= 0xFF { code as u8 } else { b'?' };
        match byte {
            b'\\' | b'(' | b')' => {
                out.push(b'\\');
                out.push(byte);
            }
            b'\n' => out.extend_from_slice(b"\\n"),
            b'\r' => out.extend_from_slice(b"\\r"),
            b'\t' => out.extend_from_slice(b"\\t"),
            0x20..=0x7e | 0xa0..=0xFF => out.push(byte),
            _ => out.push(b'?'),
        }
    }
    out
}

/// Resource name for a standard font.
fn font_resource(font: PsFont) -> &'static str {
    match font {
        PsFont::Helvetica => "F1",
        PsFont::HelveticaBold => "F2",
        PsFont::Courier => "F3",
        PsFont::TimesRoman => "F4",
        PsFont::TimesBold => "F5",
    }
}

/// Content extents `(min_x, min_y, max_x, max_y)` of one page.
fn content_extents(content: &[PsContent]) -> Option<(f32, f32, f32, f32)> {
    let mut bounds: Option<(f32, f32, f32, f32)> = None;
    let mut include = |min_x: f32, min_y: f32, max_x: f32, max_y: f32| {
        bounds = Some(match bounds {
            None => (min_x, min_y, max_x, max_y),
            Some((x0, y0, x1, y1)) => (x0.min(min_x), y0.min(min_y), x1.max(max_x), y1.max(max_y)),
        });
    };
    for item in content {
        match item {
            PsContent::Text(text) => {
                let width = text.text.chars().count() as f32 * text.font_size * 0.5;
                include(text.x, text.y - text.font_size, text.x + width, text.y);
            }
            PsContent::Rect(rect) => {
                include(rect.x, rect.y, rect.x + rect.width, rect.y + rect.height)
            }
            PsContent::Line(line) => include(
                line.x1.min(line.x2),
                line.y1.min(line.y2),
                line.x1.max(line.x2),
                line.y1.max(line.y2),
            ),
            PsContent::Image(_) => {}
        }
    }
    bounds
}

/// Render one page's content stream with the `(dx, dy)` translation.
fn page_content(content: &[PsContent], dx: f32, dy: f32) -> Vec<u8> {
    let mut out = String::new();
    for item in content {
        match item {
            PsContent::Text(text) => {
                out.push_str(&format!(
                    "BT /{} {} Tf {} {} Td (",
                    font_resource(text.font),
                    number(text.font_size),
                    number(text.x + dx),
                    number(text.y + dy)
                ));
                out.push_str(&String::from_utf8_lossy(&escape_text(&text.text)));
                out.push_str(") Tj ET\n");
            }
            PsContent::Rect(rect) => {
                out.push_str(&format!(
                    "{} {} {} {} re {}\n",
                    number(rect.x + dx),
                    number(rect.y + dy),
                    number(rect.width),
                    number(rect.height),
                    if rect.fill { "f" } else { "S" }
                ));
            }
            PsContent::Line(line) => {
                out.push_str(&format!(
                    "{} w {} {} m {} {} l S\n",
                    number(line.width),
                    number(line.x1 + dx),
                    number(line.y1 + dy),
                    number(line.x2 + dx),
                    number(line.y2 + dy)
                ));
            }
            PsContent::Image(_) => {}
        }
    }
    out.into_bytes()
}

/// Write an ingested PostScript document as PDF bytes.
///
/// The document-level `%%BoundingBox`, when present, defines the page box of
/// every page; otherwise each page's content extents plus `margin` are used.
/// Coordinates are translated so the box starts at the origin.
///
/// # Errors
///
/// Returns [`PsPdfError::EmptyDocument`] when there are no pages and
/// [`PsPdfError::NonFiniteGeometry`] / [`PsPdfError::EmptyPageBox`] when the
/// resulting geometry is unusable.
pub fn write_postscript_pdf(
    document: &IngestedDocument,
    options: &PsPdfOptions,
) -> Result<Vec<u8>, PsPdfError> {
    if document.pages.is_empty() {
        return Err(PsPdfError::EmptyDocument);
    }
    let mut writer = Writer::new();
    let catalog = writer.reserve();
    let pages_id = writer.reserve();
    let f1 = writer.reserve();
    let f2 = writer.reserve();
    let f3 = writer.reserve();
    let f4 = writer.reserve();
    let f5 = writer.reserve();

    let mut planned = Vec::with_capacity(document.pages.len());
    for (index, page) in document.pages.iter().enumerate() {
        let number = index + 1;
        let (dx, dy, width, height) = match document.report.bounding_box {
            Some(bbox) => {
                let width = (bbox.urx - bbox.llx) as f32;
                let height = (bbox.ury - bbox.lly) as f32;
                (-(bbox.llx as f32), -(bbox.lly as f32), width, height)
            }
            None => match content_extents(&page.content) {
                Some((min_x, min_y, max_x, max_y)) => {
                    let width = (max_x - min_x) + options.margin * 2.0;
                    let height = (max_y - min_y) + options.margin * 2.0;
                    (
                        -min_x + options.margin,
                        -min_y + options.margin,
                        width,
                        height,
                    )
                }
                None => (0.0, 0.0, options.default_width, options.default_height),
            },
        };
        if !dx.is_finite() || !dy.is_finite() || !width.is_finite() || !height.is_finite() {
            return Err(PsPdfError::NonFiniteGeometry { page: number });
        }
        if width <= 0.0 || height <= 0.0 {
            return Err(PsPdfError::EmptyPageBox { page: number });
        }
        let page_id = writer.reserve();
        let content_id = writer.reserve();
        planned.push((
            page_id,
            content_id,
            width,
            height,
            page_content(&page.content, dx, dy),
        ));
    }
    let page_ids: Vec<u32> = planned.iter().map(|entry| entry.0).collect();
    writer.set(
        catalog,
        format!("<< /Type /Catalog /Pages {pages_id} 0 R >>").into_bytes(),
    );
    let kids = page_ids
        .iter()
        .map(|id| format!("{id} 0 R"))
        .collect::<Vec<_>>()
        .join(" ");
    writer.set(
        pages_id,
        format!(
            "<< /Type /Pages /Kids [ {kids} ] /Count {} >>",
            page_ids.len()
        )
        .into_bytes(),
    );
    for (id, base_font) in [
        (f1, "Helvetica"),
        (f2, "Helvetica-Bold"),
        (f3, "Courier"),
        (f4, "Times-Roman"),
        (f5, "Times-Bold"),
    ] {
        writer.set(
            id,
            format!(
                "<< /Type /Font /Subtype /Type1 /BaseFont /{base_font} /Encoding /WinAnsiEncoding >>"
            )
            .into_bytes(),
        );
    }
    let resources =
        format!("/Font << /F1 {f1} 0 R /F2 {f2} 0 R /F3 {f3} 0 R /F4 {f4} 0 R /F5 {f5} 0 R >>");
    for (page_id, content_id, width, height, content) in planned {
        writer.set(
            page_id,
            format!(
                "<< /Type /Page /Parent {pages_id} 0 R /MediaBox [0 0 {} {}] /Resources << {resources} >> /Contents {content_id} 0 R >>",
                number(width),
                number(height)
            )
            .into_bytes(),
        );
        let mut body = format!("<< /Length {} >>\nstream\n", content.len()).into_bytes();
        body.extend_from_slice(&content);
        body.extend_from_slice(b"\nendstream");
        writer.set(content_id, body);
    }
    Ok(writer.finish(catalog))
}

#[cfg(test)]
mod tests {
    use super::{PsPdfError, PsPdfOptions, escape_text, number, write_postscript_pdf};
    use crate::postscript::{PsContent, PsLine, PsRect, PsText};
    use crate::postscript_ingest::{
        IngestedDocument, IngestedPage, PostScriptKind, PsIngestReport,
    };

    fn document(content: Vec<PsContent>) -> IngestedDocument {
        IngestedDocument {
            pages: vec![IngestedPage {
                label: "1".to_owned(),
                ordinal: 1,
                content,
            }],
            report: PsIngestReport {
                kind: PostScriptKind::Ps,
                page_count: 1,
                declared_pages: Some(1),
                bounding_box: None,
                unsupported: Vec::new(),
            },
        }
    }

    #[test]
    fn numbers_are_trimmed_and_negative_zero_is_normalized() {
        assert_eq!(number(1.0), "1");
        assert_eq!(number(1.5), "1.5");
        assert_eq!(number(1.234), "1.23");
        assert_eq!(number(-0.001), "0");
        assert_eq!(number(72.0), "72");
    }

    #[test]
    fn text_escapes_delimiters_and_replaces_non_latin1() {
        assert_eq!(escape_text("a(b)\\c"), b"a\\(b\\)\\\\c".to_vec());
        assert_eq!(escape_text("A\u{20ac}B"), b"A?B".to_vec());
    }

    #[test]
    fn writer_emits_text_vector_and_pages() {
        let doc = document(vec![
            PsContent::Text(PsText::new("Hello", 10.0, 20.0, 12.0)),
            PsContent::Rect(PsRect::new(1.0, 2.0, 30.0, 40.0).filled()),
            PsContent::Line(PsLine::new(0.0, 0.0, 5.0, 5.0, 2.0)),
        ]);
        let first = write_postscript_pdf(&doc, &PsPdfOptions::default()).expect("pdf");
        let second = write_postscript_pdf(&doc, &PsPdfOptions::default()).expect("pdf");
        assert_eq!(first, second, "writer is deterministic");
        let text = String::from_utf8_lossy(&first);
        assert!(text.starts_with("%PDF-1.4"), "{text}");
        assert!(text.contains("(Hello) Tj"), "{text}");
        assert!(text.contains(" re f"), "{text}");
        assert!(text.contains(" l S"), "{text}");
        assert!(text.contains("/Type /Page"), "{text}");
        assert!(text.ends_with("%%EOF\n"), "{text}");
    }

    #[test]
    fn empty_document_and_bad_boxes_are_typed_errors() {
        let empty = IngestedDocument {
            pages: Vec::new(),
            report: PsIngestReport {
                kind: PostScriptKind::Ps,
                page_count: 0,
                declared_pages: None,
                bounding_box: None,
                unsupported: Vec::new(),
            },
        };
        assert_eq!(
            write_postscript_pdf(&empty, &PsPdfOptions::default()),
            Err(PsPdfError::EmptyDocument)
        );

        let mut zero = document(Vec::new());
        zero.report.bounding_box = Some(crate::postscript_ingest::PsBoundingBox {
            llx: 0,
            lly: 0,
            urx: 0,
            ury: 0,
        });
        assert!(matches!(
            write_postscript_pdf(&zero, &PsPdfOptions::default()),
            Err(PsPdfError::EmptyPageBox { .. })
        ));
    }
}
