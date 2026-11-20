//! Local page-capture adapters that replace the retired browser capture extension.
//!
//! ADR 0008 requires a local, file-based equivalent with no remote fetch and no
//! browser extension. This module accepts three caller-supplied, local inputs
//! and renders each through the existing bounded HTML importer in [`crate::create`]:
//!
//! - [`capture_html_file`] reads a local HTML file and resolves local images
//!   next to it;
//! - [`capture_html_markup`] renders markup the caller already holds (for
//!   example text copied from a local page);
//! - [`capture_mhtml`] extracts the first `text/html` part from an MHTML-like
//!   `multipart/related` payload. Resource parts are never fetched or embedded.
//!
//! Every entry point enforces a byte budget and a part budget, rejects remote
//! URLs and active content through the HTML importer, and returns a typed
//! [`CaptureError`]. Output is deterministic: identical input and options yield
//! byte-identical PDF bytes. No network, process, or extension boundary is used.

use crate::mime::{
    BoundedReadError, content_essence, decode_transfer, header_value, param_value, read_bounded,
    split_headers, split_multipart, strip_bom,
};
use crate::{CreateError, CreateOptions, HtmlOptions, create_from_html_with_options};
use std::path::{Component, Path};

/// Default upper bound on a single capture payload (8 MiB).
pub const CAPTURE_MAX_BYTES: usize = 8 * 1024 * 1024;

/// Upper bound on the number of MIME parts accepted by the MHTML reader.
pub const CAPTURE_MAX_PARTS: usize = 64;

/// Errors returned by the local capture adapters.
///
/// Every variant names either the rejected value or the bound that was
/// exceeded, so a caller can present a specific remediation without guessing.
#[derive(Debug, thiserror::Error)]
pub enum CaptureError {
    /// The payload exceeded the configured byte budget.
    #[error("capture input too large: {bytes} bytes (limit {limit})")]
    TooLarge {
        /// Number of bytes observed.
        bytes: usize,
        /// Budget that was exceeded.
        limit: usize,
    },
    /// Reading a caller-supplied local file failed.
    #[error("capture I/O error: {0}")]
    ReadFailed(String),
    /// The payload was not valid UTF-8 after transfer decoding.
    #[error("capture input is not UTF-8: {0}")]
    NotUtf8(#[from] std::str::Utf8Error),
    /// The supplied path was not a safe, rooted local path.
    #[error("invalid local capture path: {0}")]
    InvalidPath(String),
    /// The MHTML-like payload was structurally invalid.
    #[error("invalid MHTML payload: {0}")]
    InvalidMhtml(String),
    /// The MHTML-like payload contained no `text/html` part.
    #[error("MHTML payload has no text/html part")]
    MissingHtmlPart,
    /// A MIME part used an encoding this local reader does not support.
    #[error("unsupported MIME transfer encoding: {0}")]
    UnsupportedEncoding(String),
    /// The MHTML-like payload carried more parts than the configured bound.
    #[error("too many MIME parts (limit {limit})")]
    TooManyParts {
        /// Part budget that was exceeded.
        limit: usize,
    },
    /// The HTML importer rejected the extracted markup.
    #[error("HTML capture rendering failed: {0}")]
    Html(#[from] CreateError),
}

/// Options shared by every local capture entry point.
#[derive(Debug, Clone)]
pub struct CaptureOptions {
    max_bytes: usize,
    max_parts: usize,
    html: HtmlOptions,
    create: CreateOptions,
}

impl CaptureOptions {
    /// Use the 8 MiB byte budget, 64 MIME parts, default HTML resolution, and
    /// default page geometry.
    #[must_use]
    pub fn new() -> Self {
        Self {
            max_bytes: CAPTURE_MAX_BYTES,
            max_parts: CAPTURE_MAX_PARTS,
            html: HtmlOptions::new(),
            create: CreateOptions::new(),
        }
    }

    /// Replace the byte budget applied to files, markup, and decoded parts.
    #[must_use]
    pub const fn with_max_bytes(mut self, max_bytes: usize) -> Self {
        self.max_bytes = max_bytes;
        self
    }

    /// Replace the MIME part budget used by [`capture_mhtml`].
    #[must_use]
    pub const fn with_max_parts(mut self, max_parts: usize) -> Self {
        self.max_parts = max_parts;
        self
    }

    /// Replace the HTML-resolution options passed to the importer.
    #[must_use]
    pub fn with_html_options(mut self, html: HtmlOptions) -> Self {
        self.html = html;
        self
    }

    /// Replace the output page geometry and typography.
    #[must_use]
    pub fn with_create_options(mut self, create: CreateOptions) -> Self {
        self.create = create;
        self
    }

    /// Return the configured byte budget.
    #[must_use]
    pub const fn max_bytes(&self) -> usize {
        self.max_bytes
    }

    /// Return the configured MIME part budget.
    #[must_use]
    pub const fn max_parts(&self) -> usize {
        self.max_parts
    }

    /// Return the configured HTML options.
    #[must_use]
    pub const fn html_options(&self) -> &HtmlOptions {
        &self.html
    }

    /// Return the configured page-creation options.
    #[must_use]
    pub const fn create_options(&self) -> &CreateOptions {
        &self.create
    }
}

impl Default for CaptureOptions {
    fn default() -> Self {
        Self::new()
    }
}

/// The deterministic result of one local capture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureResult {
    pdf: Vec<u8>,
    source: String,
    html_bytes: usize,
}

impl CaptureResult {
    /// The captured PDF byte stream.
    #[must_use]
    pub fn pdf(&self) -> &[u8] {
        &self.pdf
    }

    /// The caller-supplied source label, echoed for diagnostics.
    #[must_use]
    pub fn source(&self) -> &str {
        &self.source
    }

    /// The number of UTF-8 bytes of markup that were rendered.
    #[must_use]
    pub const fn html_bytes(&self) -> usize {
        self.html_bytes
    }

    /// Consume the result and return the PDF bytes.
    #[must_use]
    pub fn into_pdf(self) -> Vec<u8> {
        self.pdf
    }
}

/// Validate that a caller-supplied path is a rooted, traversal-free local path.
fn validate_local_path(path: &Path) -> Result<(), CaptureError> {
    let text = path
        .to_str()
        .ok_or_else(|| CaptureError::InvalidPath("path is not valid UTF-8".to_owned()))?;
    if text.is_empty() || text.contains('\0') {
        return Err(CaptureError::InvalidPath(format!(
            "'{text}' is not a usable path"
        )));
    }
    if path
        .components()
        .any(|part| matches!(part, Component::ParentDir | Component::CurDir))
    {
        return Err(CaptureError::InvalidPath(format!(
            "'{text}' contains a traversal or current-directory component"
        )));
    }
    if !path.is_absolute() {
        return Err(CaptureError::InvalidPath(format!(
            "'{text}' must be an absolute local path"
        )));
    }
    Ok(())
}

/// Render already-supplied HTML markup without touching the file system.
///
/// `source` is an opaque label echoed in [`CaptureResult::source`]. The markup
/// is validated by the same importer used for local files, so remote URLs,
/// active content, and oversized documents are rejected with a typed error.
///
/// # Errors
///
/// Returns [`CaptureError::TooLarge`] when `markup` exceeds the byte budget,
/// [`CaptureError::Html`] when the importer rejects it, and the corresponding
/// page-options error when geometry is invalid.
pub fn capture_html_markup(
    source: &str,
    markup: &str,
    options: &CaptureOptions,
) -> Result<CaptureResult, CaptureError> {
    if markup.len() > options.max_bytes {
        return Err(CaptureError::TooLarge {
            bytes: markup.len(),
            limit: options.max_bytes,
        });
    }
    let pdf = create_from_html_with_options(markup, &options.html, &options.create)?;
    Ok(CaptureResult {
        pdf,
        source: source.to_owned(),
        html_bytes: markup.len(),
    })
}

/// Read a local HTML file and render it through the bounded HTML importer.
///
/// Relative `img` references resolve against the file's parent directory unless
/// the caller supplied an explicit base directory through
/// [`CaptureOptions::with_html_options`]. No remote resource is ever fetched.
///
/// # Errors
///
/// Returns [`CaptureError::InvalidPath`] for a relative or traversal path,
/// [`CaptureError::ReadFailed`] when the file cannot be read,
/// [`CaptureError::TooLarge`] over the byte budget, [`CaptureError::NotUtf8`]
/// for non-UTF-8 content, and [`CaptureError::Html`] for rejected markup.
pub fn capture_html_file(
    path: &Path,
    options: &CaptureOptions,
) -> Result<CaptureResult, CaptureError> {
    validate_local_path(path)?;
    let bytes = match read_bounded(path, options.max_bytes) {
        Ok(bytes) => bytes,
        Err(BoundedReadError::TooLarge { bytes, limit }) => {
            return Err(CaptureError::TooLarge { bytes, limit });
        }
        Err(BoundedReadError::Io(message)) => return Err(CaptureError::ReadFailed(message)),
    };
    let markup = std::str::from_utf8(strip_bom(&bytes))?;
    let html_options = if options.html.base_dir() == Path::new(".") {
        let base = path.parent().unwrap_or_else(|| Path::new("."));
        options.html.clone().with_base_dir(base)
    } else {
        options.html.clone()
    };
    let effective = CaptureOptions {
        html: html_options,
        ..options.clone()
    };
    let pdf = create_from_html_with_options(markup, &effective.html, &effective.create)?;
    Ok(CaptureResult {
        pdf,
        source: path.display().to_string(),
        html_bytes: markup.len(),
    })
}

/// Extract the first `text/html` part of an MHTML-like payload and render it.
///
/// The payload must be a `multipart/related` MIME document with a `boundary`
/// parameter. Only the first `text/html` part is used; image and other resource
/// parts are ignored, never fetched, and never embedded. Both
/// quoted-printable and base64 transfer encodings are supported, along with the
/// identity `7bit`/`8bit`/`binary` encodings. Decoded text must be UTF-8.
///
/// # Errors
///
/// Returns [`CaptureError::InvalidMhtml`] for a missing boundary or malformed
/// multipart body, [`CaptureError::MissingHtmlPart`] when no HTML part exists,
/// [`CaptureError::UnsupportedEncoding`] for an unknown transfer encoding,
/// [`CaptureError::TooManyParts`] over the part budget, and
/// [`CaptureError::TooLarge`] over the byte budget.
pub fn capture_mhtml(
    payload: &str,
    options: &CaptureOptions,
) -> Result<CaptureResult, CaptureError> {
    if payload.len() > options.max_bytes {
        return Err(CaptureError::TooLarge {
            bytes: payload.len(),
            limit: options.max_bytes,
        });
    }
    let (headers, body) = split_headers(payload);
    let content_type = content_essence(header_value(headers, "content-type").as_deref());
    if content_type != "multipart/related" && content_type != "multipart/alternative" {
        return Err(CaptureError::InvalidMhtml(format!(
            "expected multipart/related, found '{content_type}'"
        )));
    }
    let raw_type = header_value(headers, "content-type").unwrap_or_default();
    let boundary = param_value(&raw_type, "boundary")
        .ok_or_else(|| CaptureError::InvalidMhtml("missing boundary parameter".to_owned()))?;
    let parts =
        split_multipart(body, &boundary, options.max_parts).map_err(CaptureError::InvalidMhtml)?;

    let mut html: Option<String> = None;
    for part in parts {
        let (part_headers, part_body) = split_headers(part);
        let part_type = content_essence(header_value(part_headers, "content-type").as_deref());
        if part_type != "text/html" {
            continue;
        }
        let encoding = header_value(part_headers, "content-transfer-encoding");
        let decoded = match decode_transfer(encoding.as_deref(), part_body, options.max_bytes) {
            Ok(decoded) => decoded,
            Err(message) if message.contains("unsupported Content-Transfer-Encoding") => {
                return Err(CaptureError::UnsupportedEncoding(message));
            }
            Err(message) if message.contains("too large") => {
                return Err(CaptureError::TooLarge {
                    bytes: options.max_bytes.saturating_add(1),
                    limit: options.max_bytes,
                });
            }
            Err(message) => return Err(CaptureError::InvalidMhtml(message)),
        };
        let text = std::str::from_utf8(strip_bom(&decoded))?;
        html = Some(text.to_owned());
        break;
    }
    let html = html.ok_or(CaptureError::MissingHtmlPart)?;
    let pdf = create_from_html_with_options(&html, &options.html, &options.create)?;
    Ok(CaptureResult {
        pdf,
        source: "mhtml".to_owned(),
        html_bytes: html.len(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn options() -> CaptureOptions {
        CaptureOptions::new()
    }

    #[test]
    fn markup_is_rendered_through_the_existing_html_subset() {
        let markup = "<h1>Hello</h1><p>Local page</p>";
        let result = capture_html_markup("clipboard", markup, &options()).expect("capture markup");
        assert_eq!(result.source(), "clipboard");
        assert_eq!(result.html_bytes(), markup.len());
        assert!(result.pdf().starts_with(b"%PDF-1.4"));
    }

    #[test]
    fn remote_urls_are_rejected_with_a_typed_html_error() {
        let error = capture_html_markup(
            "page",
            "<img src=\"https://example.invalid/a.png\">",
            &options(),
        )
        .expect_err("remote resource must be rejected");
        assert!(matches!(
            error,
            CaptureError::Html(CreateError::HtmlRemoteResource(_))
        ));
    }

    #[test]
    fn oversized_markup_is_rejected_before_parsing() {
        let markup = "x".repeat(33);
        let error = capture_html_markup("page", &markup, &options().with_max_bytes(32))
            .expect_err("over-budget markup must be rejected");
        assert!(matches!(
            error,
            CaptureError::TooLarge {
                bytes: 33,
                limit: 32
            }
        ));
    }

    #[test]
    fn local_file_capture_rejects_relative_and_traversal_paths() {
        assert!(matches!(
            capture_html_file(Path::new("page.html"), &options()),
            Err(CaptureError::InvalidPath(_))
        ));
        assert!(matches!(
            capture_html_file(Path::new(r"C:\pages\..\page.html"), &options()),
            Err(CaptureError::InvalidPath(_))
        ));
    }

    #[test]
    fn mhtml_extracts_quoted_printable_html_and_ignores_resources() {
        let payload = concat!(
            "From: <saved@example.invalid>\r\n",
            "Content-Type: multipart/related; boundary=\"part-boundary\"\r\n",
            "\r\n",
            "--part-boundary\r\n",
            "Content-Type: text/plain\r\n",
            "\r\n",
            "ignored resource\r\n",
            "--part-boundary\r\n",
            "Content-Type: text/html; charset=utf-8\r\n",
            "Content-Transfer-Encoding: quoted-printable\r\n",
            "\r\n",
            "<h1>Caf=C3=A9</h1>=\r\n",
            "<p>saved page</p>\r\n",
            "--part-boundary--\r\n",
        );
        let result = capture_mhtml(payload, &options()).expect("capture mhtml");
        assert_eq!(result.source(), "mhtml");
        assert!(result.pdf().starts_with(b"%PDF-1.4"));
        assert_eq!(
            result.html_bytes(),
            "<h1>Café</h1><p>saved page</p>\n".len()
        );
    }

    #[test]
    fn mhtml_rejects_payloads_without_an_html_part() {
        let payload = concat!(
            "Content-Type: multipart/related; boundary=b\r\n\r\n",
            "--b\r\nContent-Type: text/plain\r\n\r\nhello\r\n--b--\r\n",
        );
        assert!(matches!(
            capture_mhtml(payload, &options()),
            Err(CaptureError::MissingHtmlPart)
        ));
    }

    #[test]
    fn mhtml_rejects_unsupported_transfer_encoding() {
        let payload = concat!(
            "Content-Type: multipart/related; boundary=b\r\n\r\n",
            "--b\r\nContent-Type: text/html\r\nContent-Transfer-Encoding: x-uuencode\r\n\r\nhi\r\n--b--\r\n",
        );
        assert!(matches!(
            capture_mhtml(payload, &options()),
            Err(CaptureError::UnsupportedEncoding(_))
        ));
    }

    #[test]
    fn mhtml_requires_multipart_related_with_a_boundary() {
        let error = capture_mhtml("Content-Type: text/html\r\n\r\n<p>hi</p>", &options())
            .expect_err("non-multipart payload must be rejected");
        assert!(matches!(error, CaptureError::InvalidMhtml(_)));
        let error = capture_mhtml("Content-Type: multipart/related\r\n\r\n--b--", &options())
            .expect_err("missing boundary must be rejected");
        assert!(error.to_string().contains("boundary"));
    }

    #[test]
    fn local_file_capture_renders_a_saved_html_file() {
        use std::fs;
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "pdfxi-t0107-capture-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).expect("create temp dir");
        let path: PathBuf = dir.join("page.html");
        fs::write(&path, "<h1>Saved</h1><p>from disk</p>").expect("write html");
        let result = capture_html_file(&path, &options()).expect("capture file");
        assert!(result.pdf().starts_with(b"%PDF-1.4"));
        assert_eq!(result.source(), path.display().to_string());
        let _ = fs::remove_dir_all(&dir);
    }
}
