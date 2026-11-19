//! A versioned, hand-written JSON serializer for structured document export.
//!
//! [`DocumentJson`] models a document snapshot with metadata, one entry per
//! page, and the positioned text spans recovered for each page. The serializer
//! is written by hand (no `serde` or any other dependency) so `pdfxi-convert`
//! stays dependency-light. It guarantees:
//!
//! - a stable [`JSON_SCHEMA_VERSION`] field;
//! - deterministic key order at every object level;
//! - pages sorted by ascending zero-based page index, regardless of input
//!   order;
//! - correct escaping of quotes, backslashes, and C0 control characters, with
//!   non-ASCII text passed through as UTF-8 (valid JSON per RFC 8259);
//! - non-finite float coordinates serialized as `0` so the output is always
//!   valid JSON.
//!
//! The model is deliberately a plain data container. Callers map their own text
//! representation into it; for example a `pdfxi_text::TextSpan` contributes its
//! `text`, `x`, `y`, `width`, and `height` fields to a [`JsonSpan`]. Keeping that
//! conversion at the boundary avoids adding a dependency on `pdfxi-text`.

/// The schema version emitted in every serialized [`DocumentJson`].
///
/// Increment this when a field is added, removed, or renamed so consumers can
/// detect breaking changes.
pub const JSON_SCHEMA_VERSION: u32 = 1;

/// Document-level metadata included in a [`DocumentJson`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DocumentMetadata {
    /// Document title, or an empty string when unknown.
    pub title: String,
    /// Document author, or an empty string when unknown.
    pub author: String,
    /// Declared page count (the number of pages in the source document).
    pub pages: usize,
}

impl DocumentMetadata {
    /// Create metadata from its title, author, and page count.
    #[must_use]
    pub fn new(title: impl Into<String>, author: impl Into<String>, pages: usize) -> Self {
        Self {
            title: title.into(),
            author: author.into(),
            pages,
        }
    }
}

/// One positioned run of text on a page.
///
/// Coordinates use PDF page units with the origin at the bottom-left, matching
/// `pdfxi_text::TextSpan`. The four numeric fields describe the span's box.
#[derive(Debug, Clone, PartialEq)]
pub struct JsonSpan {
    /// The recovered UTF-8 text of the run.
    pub text: String,
    /// Left edge of the run in page coordinates.
    pub x: f32,
    /// Baseline of the run in page coordinates.
    pub y: f32,
    /// Width of the run in page coordinates.
    pub width: f32,
    /// Height of the run in page coordinates.
    pub height: f32,
}

impl JsonSpan {
    /// Create a span with `text` and its bounding box.
    #[must_use]
    pub fn new(text: impl Into<String>, x: f32, y: f32, width: f32, height: f32) -> Self {
        Self {
            text: text.into(),
            x,
            y,
            width,
            height,
        }
    }
}

/// The text recovered from one page, together with its positioned spans.
#[derive(Debug, Clone, PartialEq)]
pub struct JsonPage {
    /// Zero-based page index.
    pub page: usize,
    /// Concatenated plain text of the page.
    pub text: String,
    /// Positioned text spans in reading order.
    pub spans: Vec<JsonSpan>,
}

impl JsonPage {
    /// Create a page from its index, plain text, and spans.
    #[must_use]
    pub fn new(page: usize, text: impl Into<String>, spans: Vec<JsonSpan>) -> Self {
        Self {
            page,
            text: text.into(),
            spans,
        }
    }
}

/// A complete structured document snapshot ready for serialization.
///
/// [`DocumentJson::new`] stores pages sorted by ascending [`JsonPage::page`], so
/// serialization is deterministic regardless of the order in which pages were
/// supplied. Ties keep their original relative order.
#[derive(Debug, Clone, PartialEq)]
pub struct DocumentJson {
    /// Document-level metadata.
    pub metadata: DocumentMetadata,
    /// Per-page text and spans, sorted by page index.
    pub pages: Vec<JsonPage>,
}

impl DocumentJson {
    /// Create a document snapshot, sorting pages by their zero-based index.
    #[must_use]
    pub fn new(metadata: DocumentMetadata, mut pages: Vec<JsonPage>) -> Self {
        pages.sort_by_key(|page| page.page);
        Self { metadata, pages }
    }

    /// Serialize this snapshot to a compact JSON string with a stable key order.
    ///
    /// The exact layout is:
    ///
    /// ```text
    /// {"json_schema_version":1,"metadata":{"title":"...","author":"...","pages":N},"pages":[{"page":0,"text":"...","spans":[{"text":"...","x":0,"y":0,"width":0,"height":0}]}]}
    /// ```
    ///
    /// No insignificant whitespace is emitted and no trailing newline is added.
    #[must_use]
    pub fn to_json_string(&self) -> String {
        let mut out = String::new();
        out.push('{');
        out.push_str("\"json_schema_version\":");
        push_usize(&mut out, JSON_SCHEMA_VERSION as usize);

        out.push_str(",\"metadata\":{");
        out.push_str("\"title\":");
        push_json_string(&mut out, &self.metadata.title);
        out.push_str(",\"author\":");
        push_json_string(&mut out, &self.metadata.author);
        out.push_str(",\"pages\":");
        push_usize(&mut out, self.metadata.pages);
        out.push('}');

        out.push_str(",\"pages\":[");
        for (index, page) in self.pages.iter().enumerate() {
            if index != 0 {
                out.push(',');
            }
            out.push('{');
            out.push_str("\"page\":");
            push_usize(&mut out, page.page);
            out.push_str(",\"text\":");
            push_json_string(&mut out, &page.text);
            out.push_str(",\"spans\":[");
            for (span_index, span) in page.spans.iter().enumerate() {
                if span_index != 0 {
                    out.push(',');
                }
                out.push('{');
                out.push_str("\"text\":");
                push_json_string(&mut out, &span.text);
                out.push_str(",\"x\":");
                push_f32(&mut out, span.x);
                out.push_str(",\"y\":");
                push_f32(&mut out, span.y);
                out.push_str(",\"width\":");
                push_f32(&mut out, span.width);
                out.push_str(",\"height\":");
                push_f32(&mut out, span.height);
                out.push('}');
            }
            out.push_str("]}");
        }
        out.push_str("]}");
        out
    }

    /// Serialize this snapshot as UTF-8 bytes (JSON is always UTF-8).
    #[must_use]
    pub fn to_json_bytes(&self) -> Vec<u8> {
        self.to_json_string().into_bytes()
    }
}

/// Append a JSON string literal, escaping all mandatory characters.
///
/// `"` and `\` are escaped with a backslash; the C0 control characters use the
/// short forms where defined (`\b`, `\f`, `\n`, `\r`, `\t`) or a `\u00XX`
/// escape otherwise. Non-ASCII characters are written verbatim as UTF-8.
fn push_json_string(out: &mut String, value: &str) {
    out.push('"');
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0C}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                // Every remaining C0 control character needs a \u escape.
                out.push_str("\\u00");
                let code = c as u32;
                let digits = b"0123456789abcdef";
                out.push(digits[((code >> 4) & 0xF) as usize] as char);
                out.push(digits[(code & 0xF) as usize] as char);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// Append an unsigned integer as a JSON number.
fn push_usize(out: &mut String, value: usize) {
    out.push_str(&value.to_string());
}

/// Append a finite `f32` as a JSON number, substituting `0` for non-finite values.
///
/// Rust's `Display` for floats produces a shortest round-trip decimal string
/// without exponent notation, which is always a valid JSON number.
fn push_f32(out: &mut String, value: f32) {
    if value.is_finite() {
        out.push_str(&value.to_string());
    } else {
        out.push('0');
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_escapes_quotes_backslashes_and_control_characters() {
        let doc = DocumentJson::new(
            DocumentMetadata::new("", "", 0),
            vec![JsonPage::new(
                0,
                "quote:\" backslash:\\ nl:\n cr:\r tab:\t bs:\u{08} ff:\u{0C} nul:\u{00}",
                vec![],
            )],
        );
        let json = doc.to_json_string();
        assert!(
            json.contains("quote:\\\" backslash:\\\\ nl:\\n cr:\\r tab:\\t"),
            "{json}"
        );
        assert!(json.contains("bs:\\b ff:\\f nul:\\u0000"), "{json}");
    }

    #[test]
    fn json_preserves_non_ascii_as_utf8() {
        let doc = DocumentJson::new(
            DocumentMetadata::new("Café 中文", "José", 1),
            vec![JsonPage::new(0, "naïve — 日本語 🚀", vec![])],
        );
        let json = doc.to_json_string();
        assert!(json.contains("Café 中文"), "{json}");
        assert!(json.contains("naïve — 日本語 🚀"), "{json}");
        assert_eq!(doc.to_json_bytes(), json.as_bytes());
    }

    #[test]
    fn json_key_order_and_layout_are_deterministic() {
        let span = JsonSpan::new("Hi", 1.5, 2.0, 10.0, 12.0);
        let doc = DocumentJson::new(
            DocumentMetadata::new("T", "A", 1),
            vec![JsonPage::new(0, "Hi", vec![span])],
        );
        assert_eq!(
            doc.to_json_string(),
            "{\"json_schema_version\":1,\"metadata\":{\"title\":\"T\",\"author\":\"A\",\"pages\":1},\"pages\":[{\"page\":0,\"text\":\"Hi\",\"spans\":[{\"text\":\"Hi\",\"x\":1.5,\"y\":2,\"width\":10,\"height\":12}]}]}"
        );
    }

    #[test]
    fn json_schema_version_is_stable_and_serialized_first() {
        assert_eq!(JSON_SCHEMA_VERSION, 1);
        let doc = DocumentJson::new(DocumentMetadata::default(), vec![]);
        let json = doc.to_json_string();
        assert!(json.starts_with("{\"json_schema_version\":1,"), "{json}");
    }

    #[test]
    fn json_serializes_spans_with_boxes_and_sorts_pages() {
        let doc = DocumentJson::new(
            DocumentMetadata::new("", "", 2),
            vec![
                JsonPage::new(1, "two", vec![JsonSpan::new("two", 0.0, 0.0, 3.0, 4.0)]),
                JsonPage::new(0, "one", vec![]),
            ],
        );
        assert_eq!(doc.pages[0].page, 0, "pages are sorted at construction");
        let json = doc.to_json_string();
        let first = json.find("\"page\":0").expect("page 0 present");
        let second = json.find("\"page\":1").expect("page 1 present");
        assert!(first < second, "serialized pages ascend by index");
        assert!(json.contains("\"width\":3,\"height\":4"), "{json}");
    }

    #[test]
    fn json_serializes_empty_document_and_non_finite_coordinates() {
        let empty = DocumentJson::new(DocumentMetadata::new("", "", 0), vec![]);
        assert_eq!(
            empty.to_json_string(),
            "{\"json_schema_version\":1,\"metadata\":{\"title\":\"\",\"author\":\"\",\"pages\":0},\"pages\":[]}"
        );
        let strange = JsonSpan::new("x", f32::NAN, f32::INFINITY, -1.5, f32::NEG_INFINITY);
        let doc = DocumentJson::new(
            DocumentMetadata::default(),
            vec![JsonPage::new(0, "x", vec![strange])],
        );
        let json = doc.to_json_string();
        assert!(
            json.contains("\"x\":0,\"y\":0,\"width\":-1.5,\"height\":0"),
            "{json}"
        );
    }
}
