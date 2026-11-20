//! Local email-to-PDF ingestion for saved `.eml` message files.
//!
//! ADR 0008 replaces the retired email capture service with a local, file-based
//! equivalent: the caller supplies a message saved by their mail client and
//! this module renders its `text/plain` or `text/html` body through the existing
//! deterministic creation engine. No network, mail client, or mail server is
//! contacted.
//!
//! The parser handles RFC 5322-style headers (including folded values) and MIME
//! `multipart/*` entities with a bounded part count, nesting depth, and byte
//! budget. `text/html` bodies are rendered through [`crate::create::create_from_html`];
//! `text/plain` bodies use [`crate::create::create_from_text`]. When both are
//! present (for example `multipart/alternative`), the HTML body is preferred.
//! Attachments are **listed by name only**: their content is never decoded or
//! embedded. All failures are typed [`EmailError`] values.

use crate::mime::{
    BoundedReadError, content_essence, decode_transfer, header_value, param_value, read_bounded,
    split_headers, split_multipart, strip_bom,
};
use crate::{
    CreateError, CreateOptions, HtmlOptions, create_from_html_with_options, create_from_text,
};
use std::path::Path;

/// Default upper bound on a saved message (4 MiB).
pub const EMAIL_MAX_BYTES: usize = 4 * 1024 * 1024;

/// Default upper bound on the number of MIME entities in a message.
pub const EMAIL_MAX_PARTS: usize = 128;

/// Default upper bound on MIME nesting depth.
pub const EMAIL_MAX_DEPTH: usize = 8;

/// Default upper bound on the number of attachment names reported.
pub const EMAIL_MAX_ATTACHMENTS: usize = 64;

/// Errors returned while parsing or rendering a saved `.eml` message.
#[derive(Debug, thiserror::Error)]
pub enum EmailError {
    /// The message exceeded the configured byte budget.
    #[error("email input too large: {bytes} bytes (limit {limit})")]
    TooLarge {
        /// Number of bytes observed.
        bytes: usize,
        /// Budget that was exceeded.
        limit: usize,
    },
    /// Reading a caller-supplied message file failed.
    #[error("email I/O error: {0}")]
    ReadFailed(String),
    /// A decoded MIME part was not valid UTF-8.
    #[error("email part is not UTF-8: {0}")]
    NotUtf8(#[from] std::str::Utf8Error),
    /// The message contained more MIME entities than the configured bound.
    #[error("email has too many MIME parts (limit {limit})")]
    TooManyParts {
        /// Part budget that was exceeded.
        limit: usize,
    },
    /// MIME nesting exceeded the configured depth.
    #[error("email MIME nesting too deep (limit {limit})")]
    TooDeep {
        /// Depth budget that was exceeded.
        limit: usize,
    },
    /// More attachment names were present than the configured bound.
    #[error("email has too many attachments (limit {limit})")]
    TooManyAttachments {
        /// Attachment budget that was exceeded.
        limit: usize,
    },
    /// The message was structurally malformed.
    #[error("malformed email: {0}")]
    Malformed(String),
    /// The PDF creation engine rejected the extracted body.
    #[error("email PDF rendering failed: {0}")]
    Html(#[from] CreateError),
}

/// Bounds applied while parsing an untrusted saved message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EmailLimits {
    max_bytes: usize,
    max_parts: usize,
    max_depth: usize,
    max_attachments: usize,
}

impl EmailLimits {
    /// Bounds of 4 MiB, 128 parts, depth 8, and 64 attachment names.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            max_bytes: EMAIL_MAX_BYTES,
            max_parts: EMAIL_MAX_PARTS,
            max_depth: EMAIL_MAX_DEPTH,
            max_attachments: EMAIL_MAX_ATTACHMENTS,
        }
    }

    /// Replace the byte budget.
    #[must_use]
    pub const fn with_max_bytes(mut self, max_bytes: usize) -> Self {
        self.max_bytes = max_bytes;
        self
    }

    /// Replace the MIME part budget.
    #[must_use]
    pub const fn with_max_parts(mut self, max_parts: usize) -> Self {
        self.max_parts = max_parts;
        self
    }

    /// Replace the MIME nesting-depth budget.
    #[must_use]
    pub const fn with_max_depth(mut self, max_depth: usize) -> Self {
        self.max_depth = max_depth;
        self
    }

    /// Replace the attachment-name budget.
    #[must_use]
    pub const fn with_max_attachments(mut self, max_attachments: usize) -> Self {
        self.max_attachments = max_attachments;
        self
    }

    /// Return the byte budget.
    #[must_use]
    pub const fn max_bytes(&self) -> usize {
        self.max_bytes
    }

    /// Return the MIME part budget.
    #[must_use]
    pub const fn max_parts(&self) -> usize {
        self.max_parts
    }

    /// Return the MIME nesting-depth budget.
    #[must_use]
    pub const fn max_depth(&self) -> usize {
        self.max_depth
    }

    /// Return the attachment-name budget.
    #[must_use]
    pub const fn max_attachments(&self) -> usize {
        self.max_attachments
    }
}

impl Default for EmailLimits {
    fn default() -> Self {
        Self::new()
    }
}

/// Which body representation was chosen for rendering.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmailBodyKind {
    /// An HTML body was present and preferred.
    Html,
    /// Only a plain-text body was present.
    Text,
}

impl EmailBodyKind {
    /// Return the stable lowercase name of this choice.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Html => "html",
            Self::Text => "text",
        }
    }
}

/// The parsed, in-memory representation of a saved message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmailDocument {
    subject: Option<String>,
    from: Option<String>,
    to: Vec<String>,
    date: Option<String>,
    body_html: Option<String>,
    body_text: Option<String>,
    attachments: Vec<String>,
}

impl EmailDocument {
    /// The `Subject` header, when present.
    #[must_use]
    pub fn subject(&self) -> Option<&str> {
        self.subject.as_deref()
    }

    /// The `From` header, when present.
    #[must_use]
    pub fn from(&self) -> Option<&str> {
        self.from.as_deref()
    }

    /// The addresses listed in the `To` header.
    #[must_use]
    pub fn to(&self) -> &[String] {
        &self.to
    }

    /// The `Date` header, when present.
    #[must_use]
    pub fn date(&self) -> Option<&str> {
        self.date.as_deref()
    }

    /// The extracted HTML body, when present.
    #[must_use]
    pub fn body_html(&self) -> Option<&str> {
        self.body_html.as_deref()
    }

    /// The extracted plain-text body, when present.
    #[must_use]
    pub fn body_text(&self) -> Option<&str> {
        self.body_text.as_deref()
    }

    /// Attachment file names, in message order, with no attachment content.
    #[must_use]
    pub fn attachments(&self) -> &[String] {
        &self.attachments
    }
}

/// Options for rendering a parsed message to PDF.
#[derive(Debug, Clone)]
pub struct EmailOptions {
    limits: EmailLimits,
    create: CreateOptions,
    html: HtmlOptions,
    include_headers: bool,
}

impl EmailOptions {
    /// Default limits, page geometry, HTML options, and header inclusion.
    #[must_use]
    pub fn new() -> Self {
        Self {
            limits: EmailLimits::new(),
            create: CreateOptions::new(),
            html: HtmlOptions::new(),
            include_headers: true,
        }
    }

    /// Replace the parsing bounds.
    #[must_use]
    pub const fn with_limits(mut self, limits: EmailLimits) -> Self {
        self.limits = limits;
        self
    }

    /// Replace the output page geometry.
    #[must_use]
    pub fn with_create_options(mut self, create: CreateOptions) -> Self {
        self.create = create;
        self
    }

    /// Replace the HTML-resolution options used for an HTML body.
    #[must_use]
    pub fn with_html_options(mut self, html: HtmlOptions) -> Self {
        self.html = html;
        self
    }

    /// Omit the Subject/From/To/Date header block from the PDF.
    #[must_use]
    pub const fn without_headers(mut self) -> Self {
        self.include_headers = false;
        self
    }

    /// Return the parsing bounds.
    #[must_use]
    pub const fn limits(&self) -> &EmailLimits {
        &self.limits
    }

    /// Return the page-creation options.
    #[must_use]
    pub const fn create_options(&self) -> &CreateOptions {
        &self.create
    }

    /// Return the HTML options.
    #[must_use]
    pub const fn html_options(&self) -> &HtmlOptions {
        &self.html
    }

    /// Return whether the header block is included in the PDF.
    #[must_use]
    pub const fn includes_headers(&self) -> bool {
        self.include_headers
    }
}

impl Default for EmailOptions {
    fn default() -> Self {
        Self::new()
    }
}

/// The deterministic result of rendering one saved message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmailPdf {
    pdf: Vec<u8>,
    attachments: Vec<String>,
    body_kind: EmailBodyKind,
    body_bytes: usize,
}

impl EmailPdf {
    /// The rendered PDF byte stream.
    #[must_use]
    pub fn pdf(&self) -> &[u8] {
        &self.pdf
    }

    /// The attachment names listed by the message, in order.
    #[must_use]
    pub fn attachments(&self) -> &[String] {
        &self.attachments
    }

    /// Which body representation was rendered.
    #[must_use]
    pub const fn body_kind(&self) -> EmailBodyKind {
        self.body_kind
    }

    /// The number of UTF-8 body bytes rendered (excluding the header block).
    #[must_use]
    pub const fn body_bytes(&self) -> usize {
        self.body_bytes
    }

    /// Consume the result and return the PDF bytes.
    #[must_use]
    pub fn into_pdf(self) -> Vec<u8> {
        self.pdf
    }
}

#[derive(Default)]
struct Counters {
    parts: usize,
    attachments: usize,
}

fn record_part(counters: &mut Counters, limits: &EmailLimits) -> Result<(), EmailError> {
    counters.parts += 1;
    if counters.parts > limits.max_parts {
        return Err(EmailError::TooManyParts {
            limit: limits.max_parts,
        });
    }
    Ok(())
}

fn attachment_name(headers: &str, content_type: &str) -> Option<String> {
    let disposition = header_value(headers, "content-disposition").unwrap_or_default();
    let is_attachment = disposition
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .eq_ignore_ascii_case("attachment");
    let filename =
        param_value(&disposition, "filename").or_else(|| param_value(content_type, "name"))?;
    if !is_attachment && param_value(&disposition, "filename").is_none() {
        return None;
    }
    let trimmed = filename.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_owned())
    }
}

fn decode_body(headers: &str, body: &str, limits: &EmailLimits) -> Result<String, EmailError> {
    let encoding = header_value(headers, "content-transfer-encoding");
    let decoded =
        decode_transfer(encoding.as_deref(), body, limits.max_bytes).map_err(|message| {
            if message.contains("unsupported Content-Transfer-Encoding") {
                EmailError::Malformed(message)
            } else {
                EmailError::TooLarge {
                    bytes: limits.max_bytes.saturating_add(1),
                    limit: limits.max_bytes,
                }
            }
        })?;
    Ok(std::str::from_utf8(strip_bom(&decoded))?.to_owned())
}

fn parse_entity(
    headers: &str,
    body: &str,
    limits: &EmailLimits,
    depth: usize,
    counters: &mut Counters,
    document: &mut EmailDocument,
) -> Result<(), EmailError> {
    record_part(counters, limits)?;
    let raw_type = header_value(headers, "content-type").unwrap_or_default();
    let content_type = content_essence(Some(&raw_type));

    if let Some(name) = attachment_name(headers, &raw_type)
        && !document
            .attachments
            .iter()
            .any(|existing| existing == &name)
    {
        counters.attachments += 1;
        if counters.attachments > limits.max_attachments {
            return Err(EmailError::TooManyAttachments {
                limit: limits.max_attachments,
            });
        }
        document.attachments.push(name);
    }

    if content_type.starts_with("multipart/") {
        if depth >= limits.max_depth {
            return Err(EmailError::TooDeep {
                limit: limits.max_depth,
            });
        }
        let boundary = param_value(&raw_type, "boundary")
            .ok_or_else(|| EmailError::Malformed(format!("'{content_type}' has no boundary")))?;
        let parts =
            split_multipart(body, &boundary, limits.max_parts).map_err(EmailError::Malformed)?;
        for part in parts {
            let (part_headers, part_body) = split_headers(part);
            parse_entity(
                part_headers,
                part_body,
                limits,
                depth + 1,
                counters,
                document,
            )?;
        }
        return Ok(());
    }

    if content_type == "text/html" {
        if document.body_html.is_none() {
            document.body_html = Some(decode_body(headers, body, limits)?);
        }
    } else if content_type == "text/plain" && document.body_text.is_none() {
        document.body_text = Some(decode_body(headers, body, limits)?);
    }
    Ok(())
}

/// Parse a saved `.eml` message with the default bounds.
///
/// # Errors
///
/// See [`parse_eml_with_limits`].
pub fn parse_eml(input: &str) -> Result<EmailDocument, EmailError> {
    parse_eml_with_limits(input, &EmailLimits::new())
}

/// Parse a saved `.eml` message with explicit bounds.
///
/// The top-level headers provide Subject, From, To, and Date; MIME entities are
/// walked recursively. The first HTML and first plain-text bodies found are
/// retained. Attachment names are collected but their bytes are never decoded.
///
/// # Errors
///
/// Returns [`EmailError::TooLarge`] over the byte budget,
/// [`EmailError::TooManyParts`] or [`EmailError::TooDeep`] over the structural
/// budgets, [`EmailError::Malformed`] for a missing multipart boundary or an
/// unsupported transfer encoding, and [`EmailError::NotUtf8`] for an invalid
/// decoded body.
pub fn parse_eml_with_limits(
    input: &str,
    limits: &EmailLimits,
) -> Result<EmailDocument, EmailError> {
    if input.len() > limits.max_bytes {
        return Err(EmailError::TooLarge {
            bytes: input.len(),
            limit: limits.max_bytes,
        });
    }
    let (headers, body) = split_headers(input);
    let mut document = EmailDocument {
        subject: header_value(headers, "subject"),
        from: header_value(headers, "from"),
        to: header_value(headers, "to")
            .map(|value| {
                value
                    .split(',')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default(),
        date: header_value(headers, "date"),
        body_html: None,
        body_text: None,
        attachments: Vec::new(),
    };
    let mut counters = Counters::default();
    parse_entity(headers, body, limits, 0, &mut counters, &mut document)?;
    Ok(document)
}

fn escape_html(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            other => out.push(other),
        }
    }
    out
}

fn header_rows(document: &EmailDocument) -> Vec<(&'static str, String)> {
    let mut rows = Vec::new();
    if let Some(value) = document.subject() {
        rows.push(("Subject", value.to_owned()));
    }
    if let Some(value) = document.from() {
        rows.push(("From", value.to_owned()));
    }
    if !document.to().is_empty() {
        rows.push(("To", document.to().join(", ")));
    }
    if let Some(value) = document.date() {
        rows.push(("Date", value.to_owned()));
    }
    rows
}

/// Render a parsed message to PDF bytes using the given options.
///
/// HTML bodies are preferred over plain-text bodies. When headers are included,
/// they are emitted as a small, escaped block ahead of the body through the
/// same importer, so no header value can inject markup.
///
/// # Errors
///
/// Returns [`EmailError::Html`] when the chosen body cannot be rendered (for
/// example when both body and headers are empty).
pub fn render_email_pdf(
    document: &EmailDocument,
    options: &EmailOptions,
) -> Result<EmailPdf, EmailError> {
    let rows = if options.include_headers {
        header_rows(document)
    } else {
        Vec::new()
    };
    let (pdf, body_kind, body_bytes) = if let Some(html) = document.body_html() {
        let mut markup = String::new();
        for (label, value) in &rows {
            markup.push_str("<p><b>");
            markup.push_str(label);
            markup.push_str(":</b> ");
            markup.push_str(&escape_html(value));
            markup.push_str("</p>");
        }
        markup.push_str(html);
        let pdf = create_from_html_with_options(&markup, &options.html, &options.create)?;
        (pdf, EmailBodyKind::Html, html.len())
    } else {
        let text = document.body_text().unwrap_or("");
        let mut combined = String::new();
        for (label, value) in &rows {
            combined.push_str(label);
            combined.push_str(": ");
            combined.push_str(value);
            combined.push('\n');
        }
        if !rows.is_empty() {
            combined.push('\n');
        }
        combined.push_str(text);
        let pdf = create_from_text(&combined, &options.create)?;
        (pdf, EmailBodyKind::Text, text.len())
    };
    Ok(EmailPdf {
        pdf,
        attachments: document.attachments.clone(),
        body_kind,
        body_bytes,
    })
}

/// Parse a saved `.eml` message and render it to PDF in one step.
///
/// # Errors
///
/// Returns the errors of [`parse_eml_with_limits`] and [`render_email_pdf`].
pub fn create_pdf_from_eml(input: &str, options: &EmailOptions) -> Result<EmailPdf, EmailError> {
    let document = parse_eml_with_limits(input, options.limits())?;
    render_email_pdf(&document, options)
}

/// Read a saved `.eml` file and render it to PDF.
///
/// The file is read with a bounded reader; an over-budget file produces
/// [`EmailError::TooLarge`] rather than being loaded in full.
///
/// # Errors
///
/// Returns the errors of [`create_pdf_from_eml`] plus
/// [`EmailError::ReadFailed`] when the file cannot be read.
pub fn create_pdf_from_eml_file(
    path: &Path,
    options: &EmailOptions,
) -> Result<EmailPdf, EmailError> {
    let bytes = match read_bounded(path, options.limits().max_bytes()) {
        Ok(bytes) => bytes,
        Err(BoundedReadError::TooLarge { bytes, limit }) => {
            return Err(EmailError::TooLarge { bytes, limit });
        }
        Err(BoundedReadError::Io(message)) => return Err(EmailError::ReadFailed(message)),
    };
    let text = std::str::from_utf8(strip_bom(&bytes))?;
    create_pdf_from_eml(text, options)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    const TEXT_MAIL: &str = concat!(
        "Subject: Meeting notes\r\n",
        "From: Ada <ada@example.invalid>\r\n",
        "To: team@example.invalid\r\n",
        "Date: Tue, 19 Nov 2026 10:00:00 +0000\r\n",
        "Content-Type: text/plain; charset=utf-8\r\n",
        "\r\n",
        "Line one.\r\nLine two.\r\n",
    );

    #[test]
    fn plain_text_message_is_parsed_and_rendered() {
        let result =
            create_pdf_from_eml(TEXT_MAIL, &EmailOptions::new()).expect("render text mail");
        assert_eq!(result.body_kind(), EmailBodyKind::Text);
        assert_eq!(result.body_bytes(), "Line one.\nLine two.\n".len());
        assert!(result.pdf().starts_with(b"%PDF-1.4"));
    }

    #[test]
    fn folded_headers_are_joined_and_addresses_split() {
        let message = concat!(
            "Subject: Weekly\r\n report\r\n",
            "From: a@example.invalid\r\n",
            "To: one@example.invalid, two@example.invalid\r\n",
            "Content-Type: text/plain\r\n\r\nbody\r\n",
        );
        let parsed = parse_eml(message).expect("parse folded mail");
        assert_eq!(parsed.subject(), Some("Weekly report"));
        assert_eq!(parsed.to().len(), 2);
    }

    #[test]
    fn multipart_alternative_prefers_the_html_body() {
        let message = concat!(
            "Subject: Alternatives\r\n",
            "Content-Type: multipart/alternative; boundary=alt\r\n",
            "\r\n",
            "--alt\r\nContent-Type: text/plain\r\n\r\nplain body\r\n",
            "--alt\r\nContent-Type: text/html\r\n\r\n<h2>Rich body</h2>\r\n",
            "--alt--\r\n",
        );
        let result = create_pdf_from_eml(message, &EmailOptions::new()).expect("render html mail");
        assert_eq!(result.body_kind(), EmailBodyKind::Html);
        assert_eq!(result.body_bytes(), "<h2>Rich body</h2>\n".len());
        let parsed = parse_eml(message).expect("parse alternatives");
        assert_eq!(parsed.body_text(), Some("plain body\n"));
    }

    #[test]
    fn attachment_names_are_listed_but_content_is_not_embedded() {
        let message = concat!(
            "Subject: With attachment\r\n",
            "Content-Type: multipart/mixed; boundary=mix\r\n\r\n",
            "--mix\r\nContent-Type: text/plain\r\n\r\nsee attached\r\n",
            "--mix\r\nContent-Type: application/octet-stream; name=\"secret.bin\"\r\n",
            "Content-Disposition: attachment; filename=\"secret.bin\"\r\n",
            "Content-Transfer-Encoding: base64\r\n\r\nU0VDUkVU\r\n",
            "--mix--\r\n",
        );
        let result = create_pdf_from_eml(message, &EmailOptions::new()).expect("render mail");
        assert_eq!(result.attachments(), ["secret.bin"]);
        assert_eq!(result.body_kind(), EmailBodyKind::Text);
    }

    #[test]
    fn malformed_message_without_a_boundary_is_a_typed_error() {
        let message = "Content-Type: multipart/mixed\r\n\r\n--b--\r\n";
        let error = parse_eml(message).expect_err("missing boundary must fail");
        assert!(matches!(error, EmailError::Malformed(_)));
        assert!(error.to_string().contains("boundary"));
    }

    #[test]
    fn unsupported_transfer_encoding_is_a_typed_error() {
        let message =
            "Content-Type: text/plain\r\nContent-Transfer-Encoding: binhex\r\n\r\nbody\r\n";
        let error = parse_eml(message).expect_err("unsupported encoding must fail");
        assert!(matches!(error, EmailError::Malformed(_)));
        assert!(error.to_string().contains("binhex"));
    }

    #[test]
    fn oversized_message_is_rejected_before_parsing() {
        let message = "x".repeat(65);
        let error = parse_eml_with_limits(&message, &EmailLimits::new().with_max_bytes(64))
            .expect_err("over budget message must fail");
        assert!(matches!(
            error,
            EmailError::TooLarge {
                bytes: 65,
                limit: 64
            }
        ));
    }

    #[test]
    fn part_budget_is_enforced() {
        let message = concat!(
            "Content-Type: multipart/mixed; boundary=b\r\n\r\n",
            "--b\r\nContent-Type: text/plain\r\n\r\none\r\n",
            "--b\r\nContent-Type: text/plain\r\n\r\ntwo\r\n",
            "--b--\r\n",
        );
        let limits = EmailLimits::new().with_max_parts(2);
        assert!(matches!(
            parse_eml_with_limits(message, &limits),
            Err(EmailError::TooManyParts { limit: 2 })
        ));
    }

    #[test]
    fn message_file_is_read_with_a_bound() {
        use std::fs;
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "pdfxi-t0107-email-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).expect("create temp dir");
        let path: PathBuf = dir.join("message.eml");
        fs::write(&path, TEXT_MAIL).expect("write eml");
        let options = EmailOptions::new().with_limits(EmailLimits::new().with_max_bytes(16));
        assert!(matches!(
            create_pdf_from_eml_file(&path, &options),
            Err(EmailError::TooLarge { limit: 16, .. })
        ));
        let _ = fs::remove_dir_all(&dir);
    }
}
