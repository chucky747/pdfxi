//! Bounded, dependency-free PostScript (DSC) ingestion for Distiller-style import.
//!
//! This module is the *read* counterpart of [`crate::postscript`]. It accepts the
//! bytes of a DSC-conformant PostScript or EPS file and produces a deterministic,
//! page-ordered vector/text document using the same [`PsContent`] model the
//! writer consumes. No PostScript interpreter, external converter, network call,
//! or new dependency is involved.
//!
//! # Honest scope
//!
//! A full PostScript interpreter is deliberately **not** implemented: PostScript
//! is a Turing-complete page-description language and a safe, complete
//! interpreter would require a large dependency and a sandbox this crate does
//! not own. Instead this module implements a documented, tested subset:
//!
//! 1. DSC header parsing for `%%BoundingBox`, `%%Pages`, `%%Page`, `%%Trailer`,
//!    and `%%EndComments`; EPS is detected from the `EPSF` header token or from a
//!    bounding box with no `%%Page` markers.
//! 2. A minimal content operator subset: `moveto`/`rmoveto`, `lineto`/`rlineto`,
//!    `newpath`, `closepath`, `rectfill`/`rectstroke`, `fill`/`stroke` (only for
//!    straight two-point lines and axis-aligned rectangles), `show` with escaped
//!    literal strings, and the standard-font `findfont`/`scalefont`/`setfont`
//!    sequence.
//! 3. Everything else is *not guessed*. Each unsupported operator, procedure,
//!    raster-image operator, or transform is recorded once per page in
//!    [`PsIngestReport::unsupported`] with a stable `kind`/`reason`, so callers get
//!    a typed, deterministic report rather than a silently incomplete import.
//!    [`ingest_postscript_strict`] turns any such finding into a typed
//!    [`PsIngestError::Unsupported`] error.
//!
//! Input is bounded before parsing ([`PS_INGEST_MAX_BYTES`],
//! [`PS_INGEST_MAX_LINES`], [`PS_INGEST_MAX_PAGES`],
//! [`PS_INGEST_MAX_TOKENS_PER_PAGE`]) and all ordering is by ascending `%%Page`
//! ordinal, so hostile or oversized input cannot drive unbounded allocation and
//! repeated runs produce byte-identical results.

use std::collections::HashSet;

use thiserror::Error;

use crate::postscript::{PsContent, PsLine, PsRect, PsText};

/// Maximum accepted input size in bytes (8 MiB), checked before any parsing.
pub const PS_INGEST_MAX_BYTES: usize = 8 * 1024 * 1024;
/// Maximum number of physical lines scanned from the input.
pub const PS_INGEST_MAX_LINES: usize = 200_000;
/// Maximum number of pages (from `%%Page` markers or the single EPS page).
pub const PS_INGEST_MAX_PAGES: usize = 10_000;
/// Maximum operand tokens scanned per page.
pub const PS_INGEST_MAX_TOKENS_PER_PAGE: usize = 200_000;
/// Maximum operand-stack depth before the stack is deterministically cleared.
pub const PS_INGEST_MAX_STACK: usize = 256;
/// Maximum number of distinct unsupported findings kept in the report.
pub const PS_INGEST_MAX_UNSUPPORTED: usize = 512;
/// Maximum characters captured from a single shown string.
pub const PS_INGEST_MAX_TEXT_LEN: usize = 8_192;
/// Font size used when a page never issues a `scalefont`.
pub const PS_INGEST_DEFAULT_FONT_SIZE: f32 = 12.0;

/// A typed failure while ingesting PostScript input.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum PsIngestError {
    /// The input buffer is empty.
    #[error("empty PostScript input")]
    EmptyInput,
    /// The input exceeds [`PS_INGEST_MAX_BYTES`].
    #[error("PostScript input exceeds {limit} bytes")]
    TooLarge {
        /// The enforced byte limit.
        limit: usize,
    },
    /// The input has too many physical lines.
    #[error("PostScript input exceeds {limit} lines")]
    TooManyLines {
        /// The enforced line limit.
        limit: usize,
    },
    /// The mandatory `%!PS-Adobe` DSC header is missing.
    #[error("missing %!PS-Adobe DSC header")]
    MissingHeader,
    /// The DSC header is not ASCII/UTF-8 where the specification requires it.
    #[error("DSC header is not valid UTF-8")]
    InvalidHeaderEncoding,
    /// A DSC comment carried an invalid value.
    #[error("invalid DSC {field}: {detail}")]
    InvalidDsc {
        /// The offending DSC keyword.
        field: &'static str,
        /// A stable explanation.
        detail: &'static str,
    },
    /// The document declares more pages than [`PS_INGEST_MAX_PAGES`].
    #[error("document declares more than {limit} pages")]
    TooManyPages {
        /// The enforced page limit.
        limit: usize,
    },
    /// A page scanned more tokens than [`PS_INGEST_MAX_TOKENS_PER_PAGE`].
    #[error("page {page} exceeds {limit} content tokens")]
    TooManyTokens {
        /// The offending page number.
        page: usize,
        /// The enforced token limit.
        limit: usize,
    },
    /// A construct outside the documented subset prevented ingestion.
    #[error("unsupported PostScript construct '{kind}': {detail}")]
    Unsupported {
        /// Stable machine-readable kind of the construct.
        kind: &'static str,
        /// Deterministic human-readable explanation.
        detail: String,
    },
}

/// Whether the input was identified as Encapsulated PostScript or plain PostScript.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PostScriptKind {
    /// Encapsulated PostScript (EPSF) detected.
    Eps,
    /// Multi-page or header-only PostScript without an EPSF marker.
    Ps,
}

impl PostScriptKind {
    /// Returns a stable label for reports.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Eps => "EPS",
            Self::Ps => "PS",
        }
    }
}

/// An integer DSC `%%BoundingBox` in PostScript points.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PsBoundingBox {
    /// Lower-left x.
    pub llx: i64,
    /// Lower-left y.
    pub lly: i64,
    /// Upper-right x.
    pub urx: i64,
    /// Upper-right y.
    pub ury: i64,
}

/// One deterministic, typed finding of content that was not interpreted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PsUnsupportedFinding {
    /// One-based page number the finding belongs to.
    pub page: usize,
    /// Stable machine-readable kind (`operator`, `image`, `transform`, ...).
    pub kind: &'static str,
    /// The token or keyword that triggered the finding.
    pub token: String,
    /// Stable explanation of why it is outside the supported subset.
    pub reason: &'static str,
}

/// A report describing exactly what ingestion captured and what it skipped.
#[derive(Debug, Clone, PartialEq)]
pub struct PsIngestReport {
    /// Detected input family.
    pub kind: PostScriptKind,
    /// Number of pages emitted, in ascending ordinal order.
    pub page_count: usize,
    /// Value of `%%Pages`, when the DSC header supplied one.
    pub declared_pages: Option<usize>,
    /// Parsed `%%BoundingBox`, when supplied as four integers.
    pub bounding_box: Option<PsBoundingBox>,
    /// Distinct unsupported findings in page/encounter order.
    pub unsupported: Vec<PsUnsupportedFinding>,
}

impl PsIngestReport {
    /// Returns `true` when nothing outside the supported subset was encountered.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.unsupported.is_empty()
    }
}

/// One ingested page using the crate's existing vector/text content subset.
#[derive(Debug, Clone, PartialEq)]
pub struct IngestedPage {
    /// DSC page label as written after `%%Page:` (or `"1"` for an EPS body).
    pub label: String,
    /// One-based page ordinal used for deterministic ordering.
    pub ordinal: usize,
    /// Captured text, line, and rectangle content in encounter order.
    pub content: Vec<PsContent>,
}

/// A deterministic ingestion result: ordered pages plus an explicit report.
#[derive(Debug, Clone, PartialEq)]
pub struct IngestedDocument {
    /// Pages sorted by ascending ordinal.
    pub pages: Vec<IngestedPage>,
    /// What was captured and what was skipped.
    pub report: PsIngestReport,
}

impl IngestedDocument {
    /// Returns the page count.
    #[must_use]
    pub fn page_count(&self) -> usize {
        self.pages.len()
    }

    /// Counts the captured text runs across all pages.
    #[must_use]
    pub fn text_run_count(&self) -> usize {
        self.pages
            .iter()
            .flat_map(|page| page.content.iter())
            .filter(|item| matches!(item, PsContent::Text(_)))
            .count()
    }
}

/// Options controlling ingestion strictness.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PsIngestOptions {
    /// When `true`, any unsupported finding is promoted to
    /// [`PsIngestError::Unsupported`] instead of being only reported.
    pub strict: bool,
}

/// Ingests PostScript bytes with the default (reporting, non-strict) options.
///
/// # Errors
///
/// Returns a typed [`PsIngestError`] for empty, oversized, malformed, or
/// out-of-subset-bounds input. Unsupported *constructs* are reported in
/// [`PsIngestReport::unsupported`] rather than returned as errors; use
/// [`ingest_postscript_strict`] to promote them to errors.
pub fn ingest_postscript(input: &[u8]) -> Result<IngestedDocument, PsIngestError> {
    ingest_postscript_with_options(input, PsIngestOptions::default())
}

/// Convenience wrapper over a UTF-8 string slice.
///
/// # Errors
///
/// Propagates every [`PsIngestError`] from [`ingest_postscript`].
pub fn ingest_postscript_str(input: &str) -> Result<IngestedDocument, PsIngestError> {
    ingest_postscript(input.as_bytes())
}

/// Ingests PostScript bytes, failing on the first unsupported construct.
///
/// # Errors
///
/// Propagates every structural [`PsIngestError`] and, additionally, returns
/// [`PsIngestError::Unsupported`] when the report contains any finding. Findings
/// are deterministic, so the same input always selects the same error.
pub fn ingest_postscript_strict(input: &[u8]) -> Result<IngestedDocument, PsIngestError> {
    ingest_postscript_with_options(input, PsIngestOptions { strict: true })
}

/// Ingests PostScript bytes with explicit options.
///
/// # Errors
///
/// See [`ingest_postscript`] and [`ingest_postscript_strict`].
pub fn ingest_postscript_with_options(
    input: &[u8],
    options: PsIngestOptions,
) -> Result<IngestedDocument, PsIngestError> {
    if input.is_empty() {
        return Err(PsIngestError::EmptyInput);
    }
    if input.len() > PS_INGEST_MAX_BYTES {
        return Err(PsIngestError::TooLarge {
            limit: PS_INGEST_MAX_BYTES,
        });
    }

    let lines = line_ranges(input)?;
    let dsc = parse_dsc(input, &lines)?;

    if dsc.pages.len() > PS_INGEST_MAX_PAGES {
        return Err(PsIngestError::TooManyPages {
            limit: PS_INGEST_MAX_PAGES,
        });
    }
    if let Some(declared) = dsc.declared_pages
        && declared > PS_INGEST_MAX_PAGES
    {
        return Err(PsIngestError::TooManyPages {
            limit: PS_INGEST_MAX_PAGES,
        });
    }

    let mut report = PsIngestReport {
        kind: dsc.kind,
        page_count: dsc.pages.len(),
        declared_pages: dsc.declared_pages,
        bounding_box: dsc.bounding_box,
        unsupported: Vec::new(),
    };
    if let Some(declared) = dsc.declared_pages
        && declared != dsc.pages.len()
    {
        push_finding(
            &mut report.unsupported,
            1,
            "dsc-mismatch",
            "%%Pages",
            "declared %%Pages count differs from the number of parsed page markers",
        );
    }

    let mut pages = Vec::with_capacity(dsc.pages.len());
    let mut seen = HashSet::with_capacity(dsc.pages.len());
    let mut ordered: Vec<&RawPage> = dsc.pages.iter().collect();
    ordered.sort_by_key(|page| page.ordinal);
    for raw in ordered {
        if !seen.insert(raw.ordinal) {
            push_finding(
                &mut report.unsupported,
                raw.ordinal,
                "duplicate-page",
                &raw.label,
                "duplicate %%Page ordinal; only the first occurrence in document order is kept",
            );
            continue;
        }
        let mut interp = PageInterpreter::new(raw.ordinal);
        tokenize(&input[raw.start..raw.end], &mut interp)?;
        interp.finish();
        report.unsupported.extend(interp.findings);
        pages.push(IngestedPage {
            label: raw.label.clone(),
            ordinal: raw.ordinal,
            content: interp.content,
        });
    }

    report.page_count = pages.len();

    if options.strict
        && let Some(first) = report.unsupported.first()
    {
        return Err(PsIngestError::Unsupported {
            kind: first.kind,
            detail: format!("page {}: {}", first.page, first.reason),
        });
    }

    Ok(IngestedDocument { pages, report })
}

fn line_ranges(input: &[u8]) -> Result<Vec<(usize, usize)>, PsIngestError> {
    let mut ranges = Vec::new();
    let mut start = 0;
    for (index, byte) in input.iter().enumerate() {
        if *byte == b'\n' {
            if ranges.len() >= PS_INGEST_MAX_LINES {
                return Err(PsIngestError::TooManyLines {
                    limit: PS_INGEST_MAX_LINES,
                });
            }
            ranges.push((start, index));
            start = index + 1;
        }
    }
    if start < input.len() {
        if ranges.len() >= PS_INGEST_MAX_LINES {
            return Err(PsIngestError::TooManyLines {
                limit: PS_INGEST_MAX_LINES,
            });
        }
        ranges.push((start, input.len()));
    }
    Ok(ranges)
}

struct RawPage {
    label: String,
    ordinal: usize,
    start: usize,
    end: usize,
}

struct Dsc {
    kind: PostScriptKind,
    bounding_box: Option<PsBoundingBox>,
    declared_pages: Option<usize>,
    pages: Vec<RawPage>,
}

struct PageMark {
    label: String,
    ordinal: usize,
    line_start: usize,
    content_start: usize,
}

fn parse_dsc(input: &[u8], lines: &[(usize, usize)]) -> Result<Dsc, PsIngestError> {
    let mut header: Option<String> = None;
    for &(start, end) in lines {
        let line = trim_cr(
            std::str::from_utf8(&input[start..end])
                .map_err(|_| PsIngestError::InvalidHeaderEncoding)?,
        );
        if !line.trim().is_empty() {
            header = Some(line.to_owned());
            break;
        }
    }
    let header = header.ok_or(PsIngestError::MissingHeader)?;
    if !header.starts_with("%!PS-Adobe") {
        return Err(PsIngestError::MissingHeader);
    }

    let mut bounding_box = None;
    let mut declared_pages = None;
    let mut end_comments_end = 0usize;
    let mut trailer_start = None;
    let mut marks: Vec<PageMark> = Vec::new();

    for (index, &(start, end)) in lines.iter().enumerate() {
        let raw = match std::str::from_utf8(&input[start..end]) {
            Ok(text) => trim_cr(text),
            Err(_) => continue,
        };
        let line_end = if index + 1 < lines.len() {
            lines[index + 1].0
        } else {
            end
        };
        let content_start = line_end.min(input.len());

        if raw.starts_with("%%BoundingBox:") {
            bounding_box = parse_bounding_box(raw);
        } else if raw.starts_with("%%Pages:") {
            declared_pages = parse_pages(raw)?;
        } else if raw.starts_with("%%Page:") {
            let (label, ordinal) = parse_page_mark(raw)?;
            marks.push(PageMark {
                label,
                ordinal,
                line_start: start,
                content_start,
            });
        } else if raw.starts_with("%%EndComments") {
            end_comments_end = content_start;
        } else if raw.starts_with("%%Trailer") && trailer_start.is_none() {
            trailer_start = Some(start);
        } else if raw.starts_with("%%BeginData:")
            || raw.starts_with("%%BeginBinary:")
            || raw.starts_with("%%BeginData ")
        {
            return Err(PsIngestError::Unsupported {
                kind: "binary-data",
                detail:
                    "binary %%BeginData/%%BeginBinary sections cannot be skipped deterministically"
                        .to_owned(),
            });
        }
    }

    let pages = if marks.is_empty() {
        let body_end = trailer_start.unwrap_or(input.len());
        let body_start = if end_comments_end > 0 {
            end_comments_end
        } else {
            lines.first().map_or(0, |&(start, end)| end.max(start))
        };
        vec![RawPage {
            label: "1".to_owned(),
            ordinal: 1,
            start: body_start.min(body_end),
            end: body_end,
        }]
    } else {
        let mut built = Vec::with_capacity(marks.len());
        for (index, mark) in marks.iter().enumerate() {
            let end = if index + 1 < marks.len() {
                marks[index + 1].line_start
            } else {
                trailer_start.unwrap_or(input.len())
            };
            built.push(RawPage {
                label: mark.label.clone(),
                ordinal: mark.ordinal,
                start: mark.content_start,
                end: end.max(mark.content_start),
            });
        }
        built
    };

    let header_epsf = header.contains("EPSF");
    let has_bbox = bounding_box.is_some();
    let kind = if header_epsf || (has_bbox && marks.is_empty()) {
        PostScriptKind::Eps
    } else {
        PostScriptKind::Ps
    };

    Ok(Dsc {
        kind,
        bounding_box,
        declared_pages,
        pages,
    })
}

fn trim_cr(text: &str) -> &str {
    text.strip_suffix('\r').unwrap_or(text)
}

fn parse_bounding_box(line: &str) -> Option<PsBoundingBox> {
    let mut parts = line["%%BoundingBox:".len()..].split_whitespace();
    let llx = parts.next()?.parse::<i64>().ok()?;
    let lly = parts.next()?.parse::<i64>().ok()?;
    let urx = parts.next()?.parse::<i64>().ok()?;
    let ury = parts.next()?.parse::<i64>().ok()?;
    Some(PsBoundingBox { llx, lly, urx, ury })
}

fn parse_pages(line: &str) -> Result<Option<usize>, PsIngestError> {
    let value = line["%%Pages:".len()..].trim();
    if value.starts_with('(') {
        return Ok(None);
    }
    value
        .parse::<usize>()
        .map(Some)
        .map_err(|_| PsIngestError::InvalidDsc {
            field: "%%Pages",
            detail: "expected a non-negative integer or (atend)",
        })
}

fn parse_page_mark(line: &str) -> Result<(String, usize), PsIngestError> {
    let mut parts = line["%%Page:".len()..].split_whitespace();
    let label = parts.next().ok_or(PsIngestError::InvalidDsc {
        field: "%%Page",
        detail: "missing page label",
    })?;
    let ordinal = parts.next().ok_or(PsIngestError::InvalidDsc {
        field: "%%Page",
        detail: "missing page ordinal",
    })?;
    if ordinal.starts_with('(') {
        return Err(PsIngestError::InvalidDsc {
            field: "%%Page",
            detail: "(atend) page markers are not supported",
        });
    }
    let ordinal = ordinal
        .parse::<usize>()
        .map_err(|_| PsIngestError::InvalidDsc {
            field: "%%Page",
            detail: "page ordinal is not a non-negative integer",
        })?;
    Ok((label.to_owned(), ordinal))
}

fn push_finding(
    findings: &mut Vec<PsUnsupportedFinding>,
    page: usize,
    kind: &'static str,
    token: &str,
    reason: &'static str,
) {
    if findings.len() >= PS_INGEST_MAX_UNSUPPORTED {
        return;
    }
    if findings
        .iter()
        .any(|item| item.page == page && item.kind == kind && item.token == token)
    {
        return;
    }
    findings.push(PsUnsupportedFinding {
        page,
        kind,
        token: token.to_owned(),
        reason,
    });
}

fn reason_for(kind: &'static str) -> &'static str {
    match kind {
        "operator" => "operator is outside the supported text/line/rectangle/fill subset",
        "image" => "raster image operators are not interpreted by this importer",
        "transform" => "non-identity coordinate transforms are not modeled",
        "curve" => "Bezier paths are not approximated",
        "complex-path" => "only straight two-point lines and axis-aligned rectangles are captured",
        "procedure" => "executable procedures are not evaluated",
        "font-metrics" => "font metrics beyond a point size are not modeled",
        "array" => "array operands are not interpreted",
        "hex-string" => "hexadecimal strings are not decoded",
        "unterminated-string" => "unterminated literal strings are not recovered",
        "text-too-long" => "shown string exceeds the configured character limit",
        "stack-underflow" => "operator did not receive the expected operands",
        "invalid-number" => "numeric operand is not finite",
        _ => "construct is outside the supported text/line/rectangle/fill subset",
    }
}

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Num(f32),
    Str(String),
    Name(String),
    Word(String),
    Unsupported(&'static str),
    OpenBrace,
    CloseBrace,
}

struct PageInterpreter {
    page: usize,
    tokens: usize,
    stack: Vec<Tok>,
    content: Vec<PsContent>,
    findings: Vec<PsUnsupportedFinding>,
    seen: HashSet<(String, String)>,
    x: f32,
    y: f32,
    width: f32,
    font_size: f32,
    path: Vec<(f32, f32)>,
    brace_depth: usize,
}

impl PageInterpreter {
    fn new(page: usize) -> Self {
        Self {
            page,
            tokens: 0,
            stack: Vec::new(),
            content: Vec::new(),
            findings: Vec::new(),
            seen: HashSet::new(),
            x: 0.0,
            y: 0.0,
            width: 1.0,
            font_size: PS_INGEST_DEFAULT_FONT_SIZE,
            path: Vec::new(),
            brace_depth: 0,
        }
    }

    fn note(&mut self, kind: &'static str, token: impl Into<String>) {
        let token = token.into();
        if self.findings.len() >= PS_INGEST_MAX_UNSUPPORTED
            || !self.seen.insert((kind.to_owned(), token.clone()))
        {
            return;
        }
        let reason = reason_for(kind);
        self.findings.push(PsUnsupportedFinding {
            page: self.page,
            kind,
            token,
            reason,
        });
    }

    fn push(&mut self, token: Tok) {
        if self.stack.len() >= PS_INGEST_MAX_STACK {
            self.stack.clear();
            self.note("stack-underflow", "operand stack overflow");
        }
        self.stack.push(token);
    }

    fn pop_num(&mut self, operator: &str) -> Option<f32> {
        match self.stack.pop() {
            Some(Tok::Num(value)) if value.is_finite() => Some(value),
            Some(Tok::Num(_)) => {
                self.note("invalid-number", operator);
                None
            }
            _ => {
                self.note("stack-underflow", operator);
                None
            }
        }
    }

    fn consume(&mut self, token: Tok) -> Result<(), PsIngestError> {
        self.tokens += 1;
        if self.tokens > PS_INGEST_MAX_TOKENS_PER_PAGE {
            return Err(PsIngestError::TooManyTokens {
                page: self.page,
                limit: PS_INGEST_MAX_TOKENS_PER_PAGE,
            });
        }

        if self.brace_depth > 0 {
            match token {
                Tok::OpenBrace => self.brace_depth += 1,
                Tok::CloseBrace => self.brace_depth = self.brace_depth.saturating_sub(1),
                _ => {}
            }
            return Ok(());
        }
        match token {
            Tok::OpenBrace => {
                self.brace_depth = 1;
                self.note("procedure", "{ ... }");
                return Ok(());
            }
            Tok::CloseBrace => return Ok(()),
            Tok::Unsupported(kind) => {
                self.note(kind, kind);
                return Ok(());
            }
            other => self.handle(other),
        }
        Ok(())
    }

    fn handle(&mut self, token: Tok) {
        match token {
            Tok::Num(value) => self.push(Tok::Num(value)),
            Tok::Str(value) => self.push(Tok::Str(value)),
            Tok::Name(value) => self.push(Tok::Name(value)),
            Tok::Word(word) => self.apply_operator(&word),
            Tok::Unsupported(kind) => self.note(kind, kind),
            Tok::OpenBrace | Tok::CloseBrace => {}
        }
    }

    fn apply_operator(&mut self, op: &str) {
        match op {
            "moveto" => {
                let (Some(y), Some(x)) = (self.pop_num(op), self.pop_num(op)) else {
                    return;
                };
                self.set_point(x, y);
            }
            "rmoveto" => {
                let (Some(dy), Some(dx)) = (self.pop_num(op), self.pop_num(op)) else {
                    return;
                };
                self.set_point(self.x + dx, self.y + dy);
            }
            "lineto" | "rlineto" => {
                let (Some(second), Some(first)) = (self.pop_num(op), self.pop_num(op)) else {
                    return;
                };
                if op == "rlineto" {
                    self.extend_path(self.x + first, self.y + second);
                } else {
                    self.extend_path(first, second);
                }
            }
            "newpath" => self.path.clear(),
            "closepath" => {}
            "rectfill" => self.rect(true, op),
            "rectstroke" => self.rect(false, op),
            "fill" | "eofill" => self.finish_path(true, op),
            "stroke" => self.finish_path(false, op),
            "show" | "ashow" => self.show(op),
            "findfont" => {
                let _ = self.stack.pop();
            }
            "scalefont" => {
                let Some(size) = self.pop_num(op) else {
                    return;
                };
                let _ = self.stack.pop();
                if size.is_finite() && size > 0.0 {
                    self.font_size = size;
                } else {
                    self.note("font-metrics", op);
                }
                self.push(Tok::Name("scaled-font".to_owned()));
            }
            "setfont" => {
                let _ = self.stack.pop();
            }
            "setlinewidth" => {
                let Some(width) = self.pop_num(op) else {
                    return;
                };
                if width.is_finite() && width > 0.0 {
                    self.width = width;
                } else {
                    self.note("invalid-number", op);
                }
            }
            "gsave" | "grestore" | "showpage" | "setgray" | "setrgbcolor" | "setcmykcolor"
            | "def" => {
                self.stack.clear();
                self.path.clear();
            }
            "image" | "imagemask" | "colorimage" | "shfill" | "shadingfill" => {
                self.note("image", op);
                self.stack.clear();
            }
            "translate" | "rotate" | "scale" | "concat" | "setmatrix" => {
                self.note("transform", op);
                self.stack.clear();
            }
            "curveto" | "rcurveto" | "arcto" => {
                self.note("curve", op);
                self.stack.clear();
                self.path.clear();
            }
            "charpath" | "stringwidth" => {
                self.note("font-metrics", op);
                self.stack.clear();
            }
            _ => self.note("operator", op),
        }
    }

    fn set_point(&mut self, x: f32, y: f32) {
        self.x = x;
        self.y = y;
        self.path.clear();
        self.path.push((x, y));
    }

    fn extend_path(&mut self, x: f32, y: f32) {
        self.x = x;
        self.y = y;
        self.path.push((x, y));
    }

    fn rect(&mut self, fill: bool, op: &str) {
        let (Some(h), Some(w), Some(y), Some(x)) = (
            self.pop_num(op),
            self.pop_num(op),
            self.pop_num(op),
            self.pop_num(op),
        ) else {
            return;
        };
        if w > 0.0 && h > 0.0 {
            let mut rect = PsRect::new(x, y, w, h);
            if fill {
                rect = rect.filled();
            }
            self.content.push(PsContent::Rect(rect));
        } else {
            self.note("complex-path", op);
        }
    }

    fn finish_path(&mut self, fill: bool, op: &str) {
        if self.path.len() == 2 {
            let ((x1, y1), (x2, y2)) = (self.path[0], self.path[1]);
            self.content
                .push(PsContent::Line(PsLine::new(x1, y1, x2, y2, self.width)));
        } else if let Some((x, y, w, h)) = path_rect(&self.path)
            && w > 0.0
            && h > 0.0
        {
            let mut rect = PsRect::new(x, y, w, h);
            if fill {
                rect = rect.filled();
            }
            self.content.push(PsContent::Rect(rect));
        } else if !self.path.is_empty() {
            self.note("complex-path", op);
        }
        self.path.clear();
    }

    fn show(&mut self, op: &str) {
        let Some(text) = self.stack.pop() else {
            self.note("stack-underflow", op);
            return;
        };
        let Tok::Str(text) = text else {
            self.note("stack-underflow", op);
            return;
        };
        if text.is_empty() {
            self.x += self.font_size * 0.5;
            return;
        }
        let advance = (text.chars().count() as f32 * self.font_size * 0.5).min(10_000.0);
        self.content.push(PsContent::Text(PsText::new(
            text,
            self.x,
            self.y,
            self.font_size,
        )));
        self.x += advance;
    }

    fn finish(&mut self) {
        self.findings
            .sort_by(|a, b| a.kind.cmp(b.kind).then(a.token.cmp(&b.token)));
    }
}

const RECT_EPSILON: f32 = 1e-3;

fn path_rect(points: &[(f32, f32)]) -> Option<(f32, f32, f32, f32)> {
    if points.len() < 4 {
        return None;
    }
    let min_x = points.iter().map(|p| p.0).fold(f32::INFINITY, f32::min);
    let max_x = points.iter().map(|p| p.0).fold(f32::NEG_INFINITY, f32::max);
    let min_y = points.iter().map(|p| p.1).fold(f32::INFINITY, f32::min);
    let max_y = points.iter().map(|p| p.1).fold(f32::NEG_INFINITY, f32::max);
    if !min_x.is_finite() || !max_x.is_finite() || !min_y.is_finite() || !max_y.is_finite() {
        return None;
    }
    for &(x, y) in points {
        let on_x = (x - min_x).abs() <= RECT_EPSILON || (x - max_x).abs() <= RECT_EPSILON;
        let on_y = (y - min_y).abs() <= RECT_EPSILON || (y - max_y).abs() <= RECT_EPSILON;
        if !on_x || !on_y {
            return None;
        }
    }
    Some((min_x, min_y, max_x - min_x, max_y - min_y))
}

fn tokenize(region: &[u8], interp: &mut PageInterpreter) -> Result<(), PsIngestError> {
    let mut index = 0;
    while index < region.len() {
        let byte = region[index];
        match byte {
            b'%' => {
                while index < region.len() && region[index] != b'\n' {
                    index += 1;
                }
            }
            b' ' | b'\t' | b'\r' | b'\n' | 0x0c => index += 1,
            b'(' => {
                let token = scan_literal_string(region, &mut index);
                interp.consume(token)?;
            }
            b')' => {
                index += 1;
                interp.consume(Tok::Unsupported("unterminated-string"))?;
            }
            b'<' => {
                skip_angle_string(region, &mut index);
                interp.consume(Tok::Unsupported("hex-string"))?;
            }
            b'[' | b']' => {
                index += 1;
                interp.consume(Tok::Unsupported("array"))?;
            }
            b'{' => {
                index += 1;
                interp.consume(Tok::OpenBrace)?;
            }
            b'}' => {
                index += 1;
                interp.consume(Tok::CloseBrace)?;
            }
            b'/' => {
                index += 1;
                let start = index;
                while index < region.len() && !is_delimiter(region[index]) {
                    index += 1;
                }
                let name = String::from_utf8_lossy(&region[start..index]).into_owned();
                interp.consume(Tok::Name(name))?;
            }
            _ => {
                let start = index;
                while index < region.len() && !is_delimiter(region[index]) {
                    index += 1;
                }
                let word = String::from_utf8_lossy(&region[start..index]).into_owned();
                if let Ok(number) = word.parse::<f32>()
                    && number.is_finite()
                {
                    interp.consume(Tok::Num(number))?;
                } else {
                    interp.consume(Tok::Word(word))?;
                }
            }
        }
    }
    Ok(())
}

fn is_delimiter(byte: u8) -> bool {
    matches!(
        byte,
        b' ' | b'\t'
            | b'\r'
            | b'\n'
            | 0x0c
            | b'('
            | b')'
            | b'<'
            | b'>'
            | b'['
            | b']'
            | b'{'
            | b'}'
            | b'/'
            | b'%'
    )
}

fn scan_literal_string(region: &[u8], index: &mut usize) -> Tok {
    let mut out = String::new();
    let mut depth = 1usize;
    *index += 1;
    while *index < region.len() {
        let byte = region[*index];
        *index += 1;
        match byte {
            b'(' => {
                depth += 1;
                if out.chars().count() < PS_INGEST_MAX_TEXT_LEN {
                    out.push('(');
                }
            }
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return Tok::Str(out);
                }
                if out.chars().count() < PS_INGEST_MAX_TEXT_LEN {
                    out.push(')');
                }
            }
            b'\\' => scan_escape(region, index, &mut out),
            _ => {
                if out.chars().count() < PS_INGEST_MAX_TEXT_LEN {
                    out.push(char::from(byte));
                }
            }
        }
    }
    Tok::Unsupported("unterminated-string")
}

fn scan_escape(region: &[u8], index: &mut usize, out: &mut String) {
    if *index >= region.len() {
        return;
    }
    let byte = region[*index];
    *index += 1;
    match byte {
        b'n' => out.push('\n'),
        b'r' => out.push('\r'),
        b't' => out.push('\t'),
        b'b' => out.push('\u{0008}'),
        b'f' => out.push('\u{000c}'),
        b'\n' => {}
        b'\r' => {
            if *index < region.len() && region[*index] == b'\n' {
                *index += 1;
            }
        }
        b'0'..=b'7' => {
            let mut value = (byte - b'0') as u32;
            let mut digits = 1;
            while digits < 3 && *index < region.len() && (b'0'..=b'7').contains(&region[*index]) {
                value = value * 8 + (region[*index] - b'0') as u32;
                *index += 1;
                digits += 1;
            }
            out.push(char::from(u8::try_from(value).unwrap_or(0xff)));
        }
        other => out.push(char::from(other)),
    }
}

fn skip_angle_string(region: &[u8], index: &mut usize) {
    *index += 1;
    while *index < region.len() && region[*index] != b'>' {
        *index += 1;
    }
    if *index < region.len() {
        *index += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ps(header: &str, body: &str) -> String {
        format!("%!PS-Adobe-3.0\n{header}\n%%EndComments\n{body}\n%%Trailer\n%%EOF\n")
    }

    #[test]
    fn parses_dsc_headers_boundingbox_pages_and_page_labels() {
        let input = ps(
            "%%BoundingBox: 10 20 612 792\n%%Pages: 2",
            "%%Page: one 1\n(First) show\n%%Page: two 2\n(Second) show\n",
        );
        let doc = ingest_postscript_str(&input).expect("ingest");
        assert_eq!(doc.report.kind, PostScriptKind::Ps);
        assert_eq!(doc.report.declared_pages, Some(2));
        assert_eq!(
            doc.report.bounding_box,
            Some(PsBoundingBox {
                llx: 10,
                lly: 20,
                urx: 612,
                ury: 792,
            })
        );
        assert_eq!(doc.page_count(), 2);
        assert_eq!(doc.pages[0].label, "one");
        assert_eq!(doc.pages[1].label, "two");
        assert!(doc.report.is_clean());
    }

    #[test]
    fn detects_eps_from_header_token_and_boundingbox() {
        let header_eps = "%!PS-Adobe-3.0 EPSF-3.0\n%%BoundingBox: 0 0 200 200\n%%EndComments\n(Hi) show\n%%EOF\n";
        let doc = ingest_postscript_str(header_eps).expect("ingest");
        assert_eq!(doc.report.kind, PostScriptKind::Eps);
        assert_eq!(doc.page_count(), 1);
        assert_eq!(doc.pages[0].label, "1");

        let bbox_no_pages = ps("%%BoundingBox: 0 0 100 100", "(Hi) show\n");
        let doc = ingest_postscript_str(&bbox_no_pages).expect("ingest");
        assert_eq!(doc.report.kind, PostScriptKind::Eps);
    }

    #[test]
    fn detects_ps_when_no_epsf_marker_and_page_markers_exist() {
        let input = ps("%%BoundingBox: 0 0 100 100", "%%Page: a 1\n(a) show\n");
        let doc = ingest_postscript_str(&input).expect("ingest");
        assert_eq!(doc.report.kind, PostScriptKind::Ps);
    }

    #[test]
    fn extracts_text_line_and_rect_operators() {
        let input = ps(
            "",
            "%%Page: p 1\n/Helvetica findfont 14 scalefont setfont\n\
             10 20 moveto (Hello) show\n5 6 moveto 40 6 lineto 2 setlinewidth stroke\n\
             1 2 30 40 rectfill\n",
        );
        let doc = ingest_postscript_str(&input).expect("ingest");
        let page = &doc.pages[0];
        assert_eq!(page.content.len(), 3);
        assert!(
            matches!(&page.content[0], PsContent::Text(text) if text.text == "Hello" && (text.x - 10.0).abs() < 0.01 && (text.y - 20.0).abs() < 0.01 && (text.font_size - 14.0).abs() < 0.01)
        );
        assert!(
            matches!(&page.content[1], PsContent::Line(line) if (line.x1 - 5.0).abs() < 0.01 && (line.x2 - 40.0).abs() < 0.01 && (line.width - 2.0).abs() < 0.01)
        );
        assert!(
            matches!(&page.content[2], PsContent::Rect(rect) if rect.fill && (rect.width - 30.0).abs() < 0.01)
        );
        assert_eq!(doc.text_run_count(), 1);
    }

    #[test]
    fn unsupported_operator_is_recorded_with_deterministic_reason() {
        let input = ps("", "%%Page: p 1\n72 72 translate\n1 2 3 4 crazyop\n");
        let doc = ingest_postscript_str(&input).expect("ingest");
        assert!(!doc.report.is_clean());
        let transform = doc
            .report
            .unsupported
            .iter()
            .find(|item| item.kind == "transform")
            .expect("transform finding");
        assert_eq!(transform.token, "translate");
        assert!(transform.reason.contains("not modeled"));
        let operator = doc
            .report
            .unsupported
            .iter()
            .find(|item| item.kind == "operator")
            .expect("operator finding");
        assert_eq!(operator.token, "crazyop");
        assert!(operator.reason.contains("outside the supported"));
    }

    #[test]
    fn strict_mode_returns_typed_unsupported_error() {
        let input = ps("", "%%Page: p 1\n100 100 8 8 8 8 image\n");
        let result = ingest_postscript_strict(input.as_bytes());
        assert!(matches!(
            result,
            Err(PsIngestError::Unsupported { kind: "image", .. })
        ));
        let lenient = ingest_postscript_str(&input).expect("lenient ingest");
        assert!(lenient.report.unsupported.iter().any(|f| f.kind == "image"));
    }

    #[test]
    fn oversized_input_is_rejected_before_parsing() {
        let mut input = vec![b' '; PS_INGEST_MAX_BYTES + 1];
        input[..10].copy_from_slice(b"%!PS-Adobe");
        assert!(matches!(
            ingest_postscript(&input),
            Err(PsIngestError::TooLarge { limit }) if limit == PS_INGEST_MAX_BYTES
        ));
        assert!(matches!(
            ingest_postscript(b""),
            Err(PsIngestError::EmptyInput)
        ));
    }

    #[test]
    fn too_many_tokens_is_a_typed_error() {
        let mut body = String::from("%%Page: p 1\n");
        for _ in 0..PS_INGEST_MAX_TOKENS_PER_PAGE + 1 {
            body.push_str("1 ");
        }
        let input = ps("", &body);
        assert!(matches!(
            ingest_postscript_str(&input),
            Err(PsIngestError::TooManyTokens { page: 1, .. })
        ));
    }

    #[test]
    fn pages_are_sorted_by_ordinal_deterministically() {
        let input = ps(
            "%%Pages: 3",
            "%%Page: c 3\n(C) show\n%%Page: a 1\n(A) show\n%%Page: b 2\n(B) show\n",
        );
        let doc = ingest_postscript_str(&input).expect("ingest");
        let labels: Vec<&str> = doc.pages.iter().map(|page| page.label.as_str()).collect();
        assert_eq!(labels, vec!["a", "b", "c"]);
        assert_eq!(doc.page_count(), 3);
    }

    #[test]
    fn ingestion_is_byte_for_byte_deterministic() {
        let input = ps(
            "%%BoundingBox: 0 0 200 200\n%%Pages: 2",
            "%%Page: b 2\n1 2 20 30 rectfill\n%%Page: a 1\n(A) show\n",
        );
        let a = ingest_postscript_str(&input).expect("ingest");
        let b = ingest_postscript_str(&input).expect("ingest");
        assert_eq!(a, b);
    }

    #[test]
    fn missing_header_and_bad_dsc_are_typed_errors() {
        assert!(matches!(
            ingest_postscript_str("not postscript\n"),
            Err(PsIngestError::MissingHeader)
        ));
        let bad = "%!PS-Adobe-3.0\n%%Pages: many\n%%EOF\n";
        assert!(matches!(
            ingest_postscript_str(bad),
            Err(PsIngestError::InvalidDsc {
                field: "%%Pages",
                ..
            })
        ));
    }

    #[test]
    fn binary_data_section_is_a_typed_unsupported_error() {
        let input = "%!PS-Adobe-3.0\n%%BeginData: 12 Binary Bytes\nraw\n%%EndData\n%%EOF\n";
        assert!(matches!(
            ingest_postscript_str(input),
            Err(PsIngestError::Unsupported {
                kind: "binary-data",
                ..
            })
        ));
    }

    #[test]
    fn escaped_literal_strings_are_decoded() {
        let input = ps("", "%%Page: p 1\n(a\\(b\\)c\\n) show\n");
        let doc = ingest_postscript_str(&input).expect("ingest");
        assert!(
            matches!(&doc.pages[0].content[0], PsContent::Text(text) if text.text == "a(b)c\n")
        );
    }
}
