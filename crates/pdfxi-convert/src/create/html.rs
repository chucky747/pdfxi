//! Create a PDF from a bounded, dependency-free subset of HTML.
//!
//! This importer is deliberately small and total for hostile input. It never
//! opens a network connection, evaluates JavaScript, or follows a path outside
//! the caller-supplied base directory. Everything is parsed with a bounded
//! tokenizer and rendered through the same pagination engine used by
//! [`super::create_from_text`] and [`super::create_from_markdown`].
//!
//! # Supported elements
//!
//! - Headings `h1`–`h6`, paragraphs `p`, generic containers (`div`, `section`,
//!   `article`, `body`, `html`, …);
//! - inline `strong`/`b`, `em`/`i`, `code`, `a` (text only — the link target is
//!   validated but never fetched), `br`, and `span`;
//! - unordered and ordered lists (`ul`, `ol`, `li`);
//! - tables (`table`, `tr`, `th`, `td`) rendered one row per line with simple
//!   cell borders;
//! - `blockquote`, `pre`, `hr`, and local `img` references to PNG or JPEG files;
//! - `style` elements and inline `style` attributes for the CSS subset below.
//!
//! Text is entity-decoded (`&amp;`, `&lt;`, `&gt;`, `&quot;`, `&apos;`,
//! `&nbsp;`, common punctuation, and numeric decimal/hex references). Unknown
//! entities are emitted literally, which is deterministic recovery rather than
//! an error.
//!
//! # Supported CSS
//!
//! `style` elements accept simple tag and class selectors; both `style`
//! elements and `style` attributes accept these declarations:
//!
//! - `font-size` (`px`, `pt`, `em`, `%`, bare numbers);
//! - `font-weight` (`bold`, `normal`, numeric weights);
//! - `font-style` (`italic`, `oblique`, `normal`);
//! - `font-family` (only the presence of `mono`/`courier` selects Courier);
//! - `color` (`#rgb`, `#rrggbb`, `rgb(...)`, and a small named palette);
//! - `text-align` (`left`, `center`, `right`; `justify` falls back to `left`);
//! - `margin`, `margin-left`, `margin-top`, `margin-bottom` (CSS `px` is
//!   converted to points at `1px = 0.75pt`; `margin-right` is ignored);
//! - `border` shorthand plus `border-width`/`border-color`/`border-style`
//!   (only solid rectangular edges are drawn; style keywords are accepted
//!   and ignored deterministically);
//! - `text-decoration` (`underline` draws a rule under the span).
//!
//! Every unrecognised declaration or selector is ignored deterministically.
//!
//! # Security and bounds
//!
//! Active content (`script`, `iframe`, `object`, `embed`, `applet`, event-handler
//! attributes, and `javascript:` URLs) is rejected with
//! [`CreateError::HtmlScriptRejected`]. Remote and opaque URLs (`http:`,
//! `https:`, protocol-relative `//`, `file:`, `data:`, `ftp:`, `mailto:`) are
//! rejected with [`CreateError::HtmlRemoteResource`]. Absolute paths, `..`
//! components, backslashes, encoded traversal, and symlinks that escape the
//! base directory are rejected with [`CreateError::HtmlPathTraversal`]. Input
//! size, node count, nesting depth, attribute count, CSS rule count, and CSS
//! declaration count are all bounded with typed errors.
//!
//! No external crate is used: image bytes are embedded through the existing
//! PNG/JPEG path in [`super::create`].

use super::{
    CreateError, CreateImageFormat, CreateOptions, DEFAULT_DPI, FlowItem, FontKind, ImagePlacement,
    ImageSource, LineBorder, PlacedItem, PlacedLine, Span, StyledLine, TextAlign, encode_text,
    layout_items, page_object, parse_jpeg, parse_png, pdf_escape, text_width,
};

use std::path::{Path, PathBuf};

/// Default upper bound on HTML input bytes (4 MiB). Larger documents return
/// [`CreateError::HtmlTooLarge`] before any parsing work is done.
pub const HTML_MAX_INPUT_BYTES: usize = 4 * 1024 * 1024;

const MAX_NODES: usize = 8192;
const MAX_DEPTH: usize = 64;
const MAX_ATTRS: usize = 64;
const MAX_CSS_RULES: usize = 256;
const MAX_DECLARATIONS: usize = 256;
const MAX_ENTITIES: usize = 8192;

// ---------------------------------------------------------------------------
// Public options
// ---------------------------------------------------------------------------

/// Options specific to HTML import.
///
/// These are independent of the page geometry in [`CreateOptions`]: the latter
/// controls the output page, while this type controls how untrusted input is
/// resolved and bounded.
#[derive(Debug, Clone)]
pub struct HtmlOptions {
    base_dir: PathBuf,
    base_dir_set: bool,
    max_bytes: usize,
    css_enabled: bool,
}

impl HtmlOptions {
    /// Use the current directory as the image base, the documented 4 MiB input
    /// limit, and honour `style` blocks and attributes.
    #[must_use]
    pub fn new() -> Self {
        Self {
            base_dir: PathBuf::from("."),
            base_dir_set: false,
            max_bytes: HTML_MAX_INPUT_BYTES,
            css_enabled: true,
        }
    }

    /// Set the directory that relative `img` paths are resolved against.
    ///
    /// The path is canonicalised before use and must contain every resolved
    /// image; symlinks that point outside it are rejected.
    #[must_use]
    pub fn with_base_dir(mut self, base: impl Into<PathBuf>) -> Self {
        self.base_dir = base.into();
        self.base_dir_set = true;
        self
    }

    /// Replace the maximum accepted input size in bytes.
    #[must_use]
    pub fn with_max_bytes(mut self, max_bytes: usize) -> Self {
        self.max_bytes = max_bytes;
        self
    }

    /// Ignore `style` blocks and `style` attributes.
    #[must_use]
    pub fn without_css(mut self) -> Self {
        self.css_enabled = false;
        self
    }

    /// Return the configured image base directory.
    #[must_use]
    pub fn base_dir(&self) -> &Path {
        &self.base_dir
    }

    /// Return the configured input byte limit.
    #[must_use]
    pub const fn max_bytes(&self) -> usize {
        self.max_bytes
    }

    /// Return `true` when CSS declarations are honoured.
    #[must_use]
    pub const fn css_enabled(&self) -> bool {
        self.css_enabled
    }
}

impl Default for HtmlOptions {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Public entry points
// ---------------------------------------------------------------------------

/// Create a PDF from HTML using [`HtmlOptions::default`].
///
/// Relative image paths resolve against the current directory. Call
/// [`create_from_html_with_options`] to supply an explicit base directory.
///
/// # Errors
///
/// Returns a typed [`CreateError`] for empty input, invalid page options,
/// active content, remote URLs, path traversal, oversized input, malformed
/// local images, and I/O failures.
pub fn create_from_html(html: &str, options: &CreateOptions) -> Result<Vec<u8>, CreateError> {
    create_from_html_with_options(html, &HtmlOptions::new(), options)
}

/// Create a PDF from HTML with explicit image-resolution and size limits.
///
/// # Errors
///
/// See [`create_from_html`]. Additionally returns
/// [`CreateError::HtmlTooLarge`] when `html.len()` exceeds
/// [`HtmlOptions::max_bytes`].
pub fn create_from_html_with_options(
    html: &str,
    html_options: &HtmlOptions,
    options: &CreateOptions,
) -> Result<Vec<u8>, CreateError> {
    options.validate()?;
    if html.len() > html_options.max_bytes {
        return Err(CreateError::HtmlTooLarge {
            bytes: html.len(),
            limit: html_options.max_bytes,
        });
    }
    render_html(html, html_options, options)
}

/// Read an HTML file and create a PDF, resolving images next to that file.
///
/// # Errors
///
/// Returns [`CreateError::Io`] when the file cannot be read or is not UTF-8;
/// otherwise the same errors as [`create_from_html`].
pub fn create_pdf_from_html_file(
    path: &Path,
    options: &CreateOptions,
) -> Result<Vec<u8>, CreateError> {
    create_pdf_from_html_file_with_options(path, &HtmlOptions::new(), options)
}

/// Read an HTML file with explicit options.
///
/// Unless [`HtmlOptions::with_base_dir`] was called, images resolve against
/// the HTML file's parent directory.
///
/// # Errors
///
/// See [`create_pdf_from_html_file`].
pub fn create_pdf_from_html_file_with_options(
    path: &Path,
    html_options: &HtmlOptions,
    options: &CreateOptions,
) -> Result<Vec<u8>, CreateError> {
    let bytes = std::fs::read(path)?;
    let html = std::str::from_utf8(&bytes)
        .map_err(|error| CreateError::InvalidOption(format!("HTML input is not UTF-8: {error}")))?;
    let effective = if html_options.base_dir_set {
        html_options.clone()
    } else {
        let base = path.parent().unwrap_or_else(|| Path::new("."));
        html_options.clone().with_base_dir(base)
    };
    create_from_html_with_options(html, &effective, options)
}

// ---------------------------------------------------------------------------
// HTML tokenizer
// ---------------------------------------------------------------------------

/// A token produced by the bounded HTML scanner.
#[derive(Debug)]
enum Token {
    /// An opening tag with decoded attributes.
    Start {
        name: String,
        attrs: Vec<(String, String)>,
        empty: bool,
    },
    /// A closing tag.
    End(String),
    /// An entity-decoded run of character data.
    Text(String),
    /// Verbatim text from a `style` element.
    Raw(String),
    /// A comment, doctype, or processing instruction that is ignored.
    Ignore,
}

/// Named character references supported by the importer.
const ENTITIES: &[(&str, char)] = &[
    ("amp", '&'),
    ("lt", '<'),
    ("gt", '>'),
    ("quot", '"'),
    ("apos", '\''),
    ("nbsp", '\u{A0}'),
    ("copy", '\u{A9}'),
    ("reg", '\u{AE}'),
    ("trade", '\u{2122}'),
    ("hellip", '\u{2026}'),
    ("mdash", '\u{2014}'),
    ("ndash", '\u{2013}'),
    ("lsquo", '\u{2018}'),
    ("rsquo", '\u{2019}'),
    ("ldquo", '\u{201C}'),
    ("rdquo", '\u{201D}'),
    ("bull", '\u{2022}'),
    ("euro", '\u{20AC}'),
    ("middot", '\u{B7}'),
    ("sect", '\u{A7}'),
    ("para", '\u{B6}'),
    ("deg", '\u{B0}'),
    ("plusmn", '\u{B1}'),
    ("times", '\u{D7}'),
    ("divide", '\u{F7}'),
    ("laquo", '\u{AB}'),
    ("raquo", '\u{BB}'),
];

/// Decode character references without indexing inside a multi-byte code
/// point. Unknown entities are preserved verbatim.
fn decode_entities(text: &str, budget: &mut usize) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        let Some(semi) = rest.find(';') else {
            out.push_str(rest);
            return out;
        };
        if semi > 16 {
            out.push('&');
            rest = &rest[1..];
            continue;
        }
        let entity = &rest[1..semi];
        let decoded = if let Some(hex) = entity
            .strip_prefix("#x")
            .or_else(|| entity.strip_prefix("#X"))
        {
            u32::from_str_radix(hex, 16).ok().and_then(char::from_u32)
        } else if let Some(decimal) = entity.strip_prefix('#') {
            decimal.parse::<u32>().ok().and_then(char::from_u32)
        } else {
            ENTITIES
                .iter()
                .find(|(name, _)| *name == entity)
                .map(|(_, character)| *character)
        };
        if *budget > 0
            && let Some(character) = decoded
        {
            out.push(character);
            *budget -= 1;
            rest = &rest[semi + 1..];
            continue;
        }
        out.push_str(&rest[..=semi]);
        rest = &rest[semi + 1..];
    }
    out.push_str(rest);
    out
}

/// Find the first occurrence of `needle` at or after `start`.
fn find_bytes(haystack: &[u8], start: usize, needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || start > haystack.len() {
        return None;
    }
    haystack
        .get(start..)?
        .windows(needle.len())
        .position(|window| window == needle)
        .map(|offset| start + offset)
}

/// Remove whitespace and embedded control characters for URL scheme checks.
fn compact_lower(value: &str) -> String {
    value
        .chars()
        .filter(|character| !character.is_ascii_whitespace() && *character != '\0')
        .flat_map(char::to_lowercase)
        .collect()
}

/// Reject active-content constructs before any layout work happens.
fn validate_element(name: &str, attrs: &[(String, String)]) -> Result<(), CreateError> {
    let local = name.rsplit(':').next().unwrap_or(name);
    if local.contains("script")
        || matches!(
            local,
            "script" | "iframe" | "object" | "embed" | "applet" | "frame" | "frameset"
        )
    {
        return Err(CreateError::HtmlScriptRejected(format!("<{name}>")));
    }
    if matches!(local, "link" | "meta" | "base") {
        return Err(CreateError::HtmlRemoteResource(format!("<{name}>")));
    }
    for (key, value) in attrs {
        let lowered = key.to_ascii_lowercase();
        if lowered.starts_with("on") && lowered.len() > 2 {
            return Err(CreateError::HtmlScriptRejected(format!(
                "attribute '{key}'"
            )));
        }
        let url = compact_lower(value);
        if url.contains("javascript:") || url.contains("vbscript:") {
            return Err(CreateError::HtmlScriptRejected(format!(
                "URL in attribute '{key}'"
            )));
        }
        if matches!(
            lowered.as_str(),
            "href" | "src" | "action" | "poster" | "background" | "cite" | "data"
        ) {
            validate_url(value)?;
        }
    }
    Ok(())
}

/// Validate a URL-valued attribute. All remote and opaque schemes, absolute
/// paths, and traversal sequences are rejected; relative local paths are kept
/// for the image resolver.
fn validate_url(value: &str) -> Result<(), CreateError> {
    let trimmed = value.trim();
    let compact = compact_lower(trimmed);
    if compact.starts_with("http:")
        || compact.starts_with("https:")
        || compact.starts_with("file:")
        || compact.starts_with("data:")
        || compact.starts_with("ftp:")
        || compact.starts_with("mailto:")
        || compact.starts_with("//")
    {
        return Err(CreateError::HtmlRemoteResource(trimmed.to_owned()));
    }
    let bytes = trimmed.as_bytes();
    let drive_prefix =
        bytes.first().is_some_and(u8::is_ascii_alphabetic) && bytes.get(1) == Some(&b':');
    if compact.starts_with('/')
        || compact.starts_with('\\')
        || compact.contains("..")
        || compact.contains("%2e")
        || compact.contains("%5c")
        || drive_prefix
        || bytes.contains(&b'\0')
    {
        return Err(CreateError::HtmlPathTraversal(trimmed.to_owned()));
    }
    Ok(())
}

/// Scan HTML into tokens. Every loop advances `pos`, so the scanner is total
/// even for truncated tags, stray `<`, and unterminated comments.
fn tokenize(input: &str) -> Result<Vec<Token>, CreateError> {
    let bytes = input.as_bytes();
    let mut tokens: Vec<Token> = Vec::new();
    let mut pos = 0usize;
    let mut entity_budget = MAX_ENTITIES;

    while pos < bytes.len() {
        let text_end = bytes[pos..]
            .iter()
            .position(|&byte| byte == b'<')
            .map(|offset| pos + offset)
            .unwrap_or(bytes.len());
        if text_end > pos {
            let decoded = decode_entities(&input[pos..text_end], &mut entity_budget);
            tokens.push(Token::Text(decoded));
            pos = text_end;
        }
        if pos >= bytes.len() {
            break;
        }
        if bytes.get(pos + 1) == Some(&b'!') {
            let terminator: &[u8] = if input.get(pos..pos + 4) == Some("<!--") {
                b"-->"
            } else {
                b">"
            };
            pos = find_bytes(bytes, pos + 2, terminator)
                .map(|at| at + terminator.len())
                .unwrap_or(bytes.len());
            tokens.push(Token::Ignore);
            continue;
        }
        if bytes.get(pos + 1) == Some(&b'?') {
            pos = find_bytes(bytes, pos + 2, b"?>")
                .map(|at| at + 2)
                .unwrap_or(bytes.len());
            tokens.push(Token::Ignore);
            continue;
        }

        let is_end = bytes.get(pos + 1) == Some(&b'/');
        let name_start = pos + 1 + usize::from(is_end);
        let mut name_end = name_start;
        while let Some(&byte) = bytes.get(name_end) {
            if byte.is_ascii_whitespace() || matches!(byte, b'>' | b'/' | b'=') {
                break;
            }
            name_end += 1;
        }
        if name_end == name_start {
            tokens.push(Token::Text("<".to_owned()));
            pos += 1;
            continue;
        }
        let name = input
            .get(name_start..name_end)
            .unwrap_or("")
            .to_ascii_lowercase();

        if is_end {
            pos = find_bytes(bytes, name_end, b">")
                .map(|at| at + 1)
                .unwrap_or(bytes.len());
            tokens.push(Token::End(name));
            continue;
        }

        let mut attrs: Vec<(String, String)> = Vec::new();
        let mut cursor = name_end;
        let mut empty = false;
        let mut tag_end = bytes.len();
        loop {
            while bytes.get(cursor).is_some_and(u8::is_ascii_whitespace) {
                cursor += 1;
            }
            match bytes.get(cursor).copied() {
                None => break,
                Some(b'>') => {
                    tag_end = cursor + 1;
                    break;
                }
                Some(b'/') => {
                    empty = true;
                    cursor += 1;
                }
                Some(_) => {
                    let attr_start = cursor;
                    while let Some(&byte) = bytes.get(cursor) {
                        if byte.is_ascii_whitespace() || matches!(byte, b'=' | b'>' | b'/') {
                            break;
                        }
                        cursor += 1;
                    }
                    if cursor == attr_start {
                        cursor += 1;
                        continue;
                    }
                    let attr_name = input
                        .get(attr_start..cursor)
                        .unwrap_or("")
                        .to_ascii_lowercase();
                    while bytes.get(cursor).is_some_and(u8::is_ascii_whitespace) {
                        cursor += 1;
                    }
                    let mut value = String::new();
                    if bytes.get(cursor) == Some(&b'=') {
                        cursor += 1;
                        while bytes.get(cursor).is_some_and(u8::is_ascii_whitespace) {
                            cursor += 1;
                        }
                        match bytes.get(cursor).copied() {
                            Some(quote @ (b'"' | b'\'')) => {
                                cursor += 1;
                                let value_start = cursor;
                                while cursor < bytes.len() && bytes.get(cursor) != Some(&quote) {
                                    cursor += 1;
                                }
                                value = decode_entities(
                                    input.get(value_start..cursor).unwrap_or(""),
                                    &mut entity_budget,
                                );
                                if cursor < bytes.len() {
                                    cursor += 1;
                                }
                            }
                            _ => {
                                let value_start = cursor;
                                while cursor < bytes.len()
                                    && !bytes[cursor].is_ascii_whitespace()
                                    && bytes[cursor] != b'>'
                                {
                                    cursor += 1;
                                }
                                value = decode_entities(
                                    input.get(value_start..cursor).unwrap_or(""),
                                    &mut entity_budget,
                                );
                            }
                        }
                    }
                    if attrs.len() >= MAX_ATTRS {
                        return Err(CreateError::HtmlStructure(
                            "too many attributes on an element".to_owned(),
                        ));
                    }
                    attrs.push((attr_name, value));
                }
            }
        }
        pos = tag_end;
        if matches!(name.as_str(), "br" | "hr" | "img" | "wbr") {
            empty = true;
        }
        validate_element(&name, &attrs)?;
        tokens.push(Token::Start {
            name: name.clone(),
            attrs,
            empty,
        });

        if name == "style" {
            let closing = "</style";
            let tail = input.get(pos..).unwrap_or("");
            if let Some(relative) = tail.to_ascii_lowercase().find(closing) {
                let raw_end = pos + relative;
                tokens.push(Token::Raw(input.get(pos..raw_end).unwrap_or("").to_owned()));
                pos = raw_end;
            }
        }
    }
    Ok(tokens)
}

// ---------------------------------------------------------------------------
// Tree construction with deterministic recovery
// ---------------------------------------------------------------------------

/// A parsed node: an element with attributes, character data, or raw CSS.
#[derive(Debug)]
enum NodeKind {
    /// An element and its decoded attributes.
    Element {
        name: String,
        attrs: Vec<(String, String)>,
    },
    /// Character data.
    Text(String),
    /// Verbatim text from a `style` element.
    Raw(String),
}

/// A node in the recovered document tree.
#[derive(Debug)]
struct Node {
    kind: NodeKind,
    children: Vec<Node>,
}

impl Node {
    fn root() -> Self {
        Self {
            kind: NodeKind::Element {
                name: "#document".to_owned(),
                attrs: Vec::new(),
            },
            children: Vec::new(),
        }
    }
}

/// Assemble a tree from tokens. Unmatched closing tags are ignored and
/// mismatched tags close to the nearest matching ancestor, which gives total,
/// deterministic recovery for malformed input. Open elements are attached to
/// their parent only when they are closed (including at end of input), so no
/// node is left holding a discarded clone of its children.
fn build_tree(tokens: Vec<Token>) -> Result<Node, CreateError> {
    let mut stack: Vec<Node> = vec![Node::root()];
    let mut nodes = 1usize;

    for token in tokens {
        match token {
            Token::Ignore => {}
            Token::Text(text) => {
                if text.is_empty() {
                    continue;
                }
                if nodes >= MAX_NODES {
                    return Err(CreateError::HtmlStructure("too many nodes".to_owned()));
                }
                nodes += 1;
                if let Some(parent) = stack.last_mut() {
                    parent.children.push(Node {
                        kind: NodeKind::Text(text),
                        children: Vec::new(),
                    });
                }
            }
            Token::Raw(text) => {
                if nodes >= MAX_NODES {
                    return Err(CreateError::HtmlStructure("too many nodes".to_owned()));
                }
                nodes += 1;
                if let Some(parent) = stack.last_mut() {
                    parent.children.push(Node {
                        kind: NodeKind::Raw(text),
                        children: Vec::new(),
                    });
                }
            }
            Token::Start { name, attrs, empty } => {
                if nodes >= MAX_NODES {
                    return Err(CreateError::HtmlStructure("too many nodes".to_owned()));
                }
                nodes += 1;
                let node = Node {
                    kind: NodeKind::Element { name, attrs },
                    children: Vec::new(),
                };
                if empty {
                    if let Some(parent) = stack.last_mut() {
                        parent.children.push(node);
                    }
                } else {
                    if stack.len() >= MAX_DEPTH {
                        return Err(CreateError::HtmlStructure(
                            "nesting depth limit exceeded".to_owned(),
                        ));
                    }
                    stack.push(node);
                }
            }
            Token::End(name) => {
                if let Some(index) = stack.iter().rposition(|node| {
                    matches!(&node.kind, NodeKind::Element { name: candidate, .. } if candidate == &name)
                }) && index > 0
                {
                    let mut closed: Vec<Node> = stack.drain(index..).collect();
                    while let Some(child) = closed.pop() {
                        let parent = closed.last_mut().or_else(|| stack.last_mut());
                        if let Some(parent) = parent {
                            parent.children.push(child);
                        }
                    }
                }
            }
        }
    }
    while stack.len() > 1 {
        if let Some(node) = stack.pop()
            && let Some(parent) = stack.last_mut()
        {
            parent.children.push(node);
        }
    }
    stack
        .pop()
        .ok_or_else(|| CreateError::HtmlStructure("empty document tree".to_owned()))
}

// ---------------------------------------------------------------------------
// CSS subset
// ---------------------------------------------------------------------------

/// A parsed CSS rule with a simple selector and declarations.
#[derive(Debug)]
struct CssRule {
    selector: String,
    declarations: Vec<(String, String)>,
}

/// Split a declaration list into lower-cased `(property, value)` pairs.
/// Unknown properties are kept so [`apply_style`] can ignore them
/// deterministically.
fn parse_declarations(css: &str, out: &mut Vec<(String, String)>) -> Result<(), CreateError> {
    for part in css.split(';') {
        let Some((property, value)) = part.split_once(':') else {
            continue;
        };
        let property = property.trim().to_ascii_lowercase();
        let value = value.trim().to_ascii_lowercase();
        if property.is_empty()
            || value.is_empty()
            || !property
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        {
            continue;
        }
        if out.len() >= MAX_DECLARATIONS {
            return Err(CreateError::HtmlStructure(
                "too many CSS declarations".to_owned(),
            ));
        }
        out.push((property, value));
    }
    Ok(())
}

/// Strip CSS comments, including unterminated ones.
fn strip_css_comments(css: &str) -> String {
    let mut out = String::with_capacity(css.len());
    let mut rest = css;
    while let Some(at) = rest.find("/*") {
        out.push_str(&rest[..at]);
        rest = &rest[at + 2..];
        match rest.find("*/") {
            Some(end) => rest = &rest[end + 2..],
            None => return out,
        }
    }
    out.push_str(rest);
    out
}

/// Collect simple tag, class, and universal selectors from `style` elements.
fn collect_css(root: &Node) -> Result<Vec<CssRule>, CreateError> {
    let mut rules: Vec<CssRule> = Vec::new();
    collect_css_node(root, &mut rules)?;
    Ok(rules)
}

/// Recursive helper for [`collect_css`].
fn collect_css_node(node: &Node, rules: &mut Vec<CssRule>) -> Result<(), CreateError> {
    if let NodeKind::Raw(text) = &node.kind {
        let cleaned = strip_css_comments(text);
        for block in cleaned.split('}') {
            let Some((selector, body)) = block.split_once('{') else {
                continue;
            };
            for simple in selector.split(',') {
                let simple = simple.trim().to_ascii_lowercase();
                if simple.is_empty() {
                    continue;
                }
                let is_class = simple.starts_with('.');
                if simple.contains([' ', '>', '+', '~', ':', '[', '*'])
                    || (!is_class && simple.contains('.'))
                    || (is_class && simple[1..].contains('.'))
                {
                    continue;
                }
                if rules.len() >= MAX_CSS_RULES {
                    return Err(CreateError::HtmlStructure("too many CSS rules".to_owned()));
                }
                let mut declarations = Vec::new();
                parse_declarations(body, &mut declarations)?;
                if !declarations.is_empty() {
                    rules.push(CssRule {
                        selector: simple,
                        declarations,
                    });
                }
            }
        }
    }
    for child in &node.children {
        collect_css_node(child, rules)?;
    }
    Ok(())
}

/// Parse a CSS length. `px` uses the CSS absolute unit ratio of `0.75pt`.
fn parse_length(value: &str, base: f64) -> Option<f64> {
    let value = value.trim();
    let parsed = if let Some(number) = value.strip_suffix("px") {
        number.parse::<f64>().ok().map(|length| length * 0.75)
    } else if let Some(number) = value.strip_suffix("pt") {
        number.parse::<f64>().ok()
    } else if let Some(number) = value.strip_suffix("em") {
        number.parse::<f64>().ok().map(|length| length * base)
    } else if let Some(number) = value.strip_suffix('%') {
        number
            .parse::<f64>()
            .ok()
            .map(|length| length / 100.0 * base)
    } else {
        value.parse::<f64>().ok()
    };
    parsed.filter(|length| length.is_finite() && length.abs() <= 10_000.0)
}

/// Parse one of the supported colour syntaxes.
fn parse_color(value: &str) -> Option<[f64; 3]> {
    let value = value.trim();
    if let Some(hex) = value.strip_prefix('#') {
        let channel = |text: &str| -> Option<f64> {
            let number = u8::from_str_radix(text, 16).ok()?;
            Some(f64::from(number) / 255.0)
        };
        return match hex.len() {
            3 => {
                let expand = |at: usize| -> Option<String> {
                    let digit = hex.get(at..at + 1)?;
                    Some(format!("{digit}{digit}"))
                };
                Some([
                    channel(&expand(0)?)?,
                    channel(&expand(1)?)?,
                    channel(&expand(2)?)?,
                ])
            }
            6 => Some([
                channel(hex.get(0..2)?)?,
                channel(hex.get(2..4)?)?,
                channel(hex.get(4..6)?)?,
            ]),
            _ => None,
        };
    }
    if let Some(inner) = value
        .strip_prefix("rgb(")
        .and_then(|text| text.strip_suffix(')'))
    {
        let parts: Vec<&str> = inner.split(',').map(str::trim).collect();
        if parts.len() == 3 {
            let mut channels = [0.0; 3];
            for (slot, part) in channels.iter_mut().zip(parts) {
                let number = if let Some(percent) = part.strip_suffix('%') {
                    percent.parse::<f64>().ok().map(|value| value * 2.55)
                } else {
                    part.parse::<f64>().ok()
                }?;
                *slot = (number.clamp(0.0, 255.0).round() as u8) as f64 / 255.0;
            }
            return Some(channels);
        }
        return None;
    }
    match value {
        "black" => Some([0.0, 0.0, 0.0]),
        "white" => Some([1.0, 1.0, 1.0]),
        "red" => Some([1.0, 0.0, 0.0]),
        "green" => Some([0.0, 0.5, 0.0]),
        "blue" => Some([0.0, 0.0, 1.0]),
        "gray" | "grey" => Some([0.5, 0.5, 0.5]),
        "silver" => Some([0.75, 0.75, 0.75]),
        "yellow" => Some([1.0, 1.0, 0.0]),
        "cyan" | "aqua" => Some([0.0, 1.0, 1.0]),
        "magenta" | "fuchsia" => Some([1.0, 0.0, 1.0]),
        "orange" => Some([1.0, 0.65, 0.0]),
        "purple" => Some([0.5, 0.0, 0.5]),
        _ => None,
    }
}

/// Parse the supported `border` shorthand. Unknown style keywords are ignored
/// deterministically and every accepted border is drawn as solid edges.
fn parse_border(value: &str) -> Option<LineBorder> {
    let value = value.trim();
    if value == "none" || value == "hidden" || value == "0" {
        return None;
    }
    let mut width = 1.0;
    let mut color = [0.0; 3];
    for token in value.split_whitespace() {
        if token == "none" || token == "hidden" {
            return None;
        }
        if let Some(length) = parse_length(token, 1.0) {
            if length > 0.0 {
                width = length.clamp(0.25, 12.0);
            }
        } else if let Some(parsed) = parse_color(token) {
            color = parsed;
        }
    }
    Some(LineBorder {
        width,
        color,
        sides: LineBorder::BOX,
    })
}

/// The resolved style attached to a line or inline run.
#[derive(Debug, Clone, Copy)]
struct Style {
    size: f64,
    bold: bool,
    italic: bool,
    mono: bool,
    color: Option<[f64; 3]>,
    align: TextAlign,
    indent: f64,
    space_before: f64,
    space_after: f64,
    border: Option<LineBorder>,
    underline: bool,
}

impl Style {
    fn base(font_size: f64) -> Self {
        Self {
            size: font_size,
            bold: false,
            italic: false,
            mono: false,
            color: None,
            align: TextAlign::Left,
            indent: 0.0,
            space_before: 0.0,
            space_after: 0.0,
            border: None,
            underline: false,
        }
    }

    fn font(self) -> FontKind {
        if self.mono {
            FontKind::Mono
        } else {
            match (self.bold, self.italic) {
                (true, true) => FontKind::BoldItalic,
                (true, false) => FontKind::Bold,
                (false, true) => FontKind::Italic,
                (false, false) => FontKind::Regular,
            }
        }
    }
}

/// Apply the supported declarations to a style. Unknown declarations are
/// skipped, which is the documented deterministic recovery behaviour.
fn apply_style(style: &mut Style, declarations: &[(String, String)]) {
    for (property, value) in declarations {
        match property.as_str() {
            "font-size" => {
                if let Some(length) = parse_length(value, style.size) {
                    style.size = length.clamp(4.0, 144.0);
                }
            }
            "font-weight" => {
                style.bold = matches!(
                    value.as_str(),
                    "bold" | "bolder" | "600" | "700" | "800" | "900"
                );
            }
            "font-style" => style.italic = matches!(value.as_str(), "italic" | "oblique"),
            "font-family" => style.mono = value.contains("mono") || value.contains("courier"),
            "color" => {
                if let Some(color) = parse_color(value) {
                    style.color = Some(color);
                }
            }
            "text-align" => {
                style.align = match value.as_str() {
                    "center" => TextAlign::Center,
                    "right" => TextAlign::Right,
                    _ => TextAlign::Left,
                };
            }
            "margin-left" => {
                if let Some(length) = parse_length(value, style.size) {
                    style.indent += length.max(0.0);
                }
            }
            "margin-top" => {
                if let Some(length) = parse_length(value, style.size) {
                    style.space_before += length.max(0.0);
                }
            }
            "margin-bottom" => {
                if let Some(length) = parse_length(value, style.size) {
                    style.space_after += length.max(0.0);
                }
            }
            "margin" => {
                let values: Vec<&str> = value.split_whitespace().collect();
                if !values.is_empty() && values.len() <= 4 {
                    let lengths: Vec<f64> = values
                        .iter()
                        .filter_map(|text| parse_length(text, style.size))
                        .collect();
                    if lengths.len() == values.len() {
                        // top, right, bottom, left — right is intentionally
                        // ignored because the layout engine uses a fixed box.
                        if let Some(top) = lengths.first() {
                            style.space_before += top.max(0.0);
                        }
                        if lengths.len() >= 3 {
                            style.space_after += lengths[2].max(0.0);
                        }
                        if let Some(left) = lengths.last() {
                            style.indent += left.max(0.0);
                        }
                    }
                }
            }
            "border" => style.border = parse_border(value),
            "border-width" => {
                if let Some(mut border) = style.border {
                    if let Some(length) = parse_length(value, 1.0) {
                        border.width = length.clamp(0.25, 12.0);
                    }
                    style.border = Some(border);
                } else if let Some(length) = parse_length(value, 1.0) {
                    style.border = Some(LineBorder {
                        width: length.clamp(0.25, 12.0),
                        color: [0.0, 0.0, 0.0],
                        sides: LineBorder::BOX,
                    });
                }
            }
            "border-color" => {
                if let Some(color) = parse_color(value) {
                    let width = style.border.map_or(1.0, |border| border.width);
                    style.border = Some(LineBorder {
                        width,
                        color,
                        sides: LineBorder::BOX,
                    });
                }
            }
            "border-style" => {
                if value == "none" || value == "hidden" {
                    style.border = None;
                }
            }
            "text-decoration" | "text-decoration-line" => {
                style.underline = value.contains("underline");
            }
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------
// Flow rendering
// ---------------------------------------------------------------------------

/// A piece of inline content with the style that applies to it.
#[derive(Debug)]
enum InlineAtom {
    /// Entity-decoded text. Embedded newlines force a visual line break.
    Text { text: String, style: Style },
    /// A local image reference resolved when the line is emitted.
    Image { src: String, style: Style },
}

/// The renderer owns the collected flow items and resolved image sources.
struct Renderer<'a> {
    options: &'a CreateOptions,
    base: &'a Path,
    css_enabled: bool,
    rules: Vec<CssRule>,
    items: Vec<FlowItem>,
    images: Vec<ImageSource>,
}

/// Return `true` when an element participates in inline layout rather than
/// starting a new block.
fn is_inline_element(name: &str) -> bool {
    !matches!(
        name,
        "html"
            | "body"
            | "div"
            | "section"
            | "article"
            | "main"
            | "header"
            | "footer"
            | "nav"
            | "aside"
            | "figure"
            | "center"
            | "p"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "blockquote"
            | "pre"
            | "ul"
            | "ol"
            | "li"
            | "table"
            | "tr"
            | "td"
            | "th"
            | "hr"
            | "style"
    )
}

/// Apply the semantic defaults of an element to the inherited style.
fn apply_tag_defaults(style: &mut Style, name: &str) {
    match name {
        "h1" => {
            style.size *= 2.0;
            style.bold = true;
            style.space_after += 6.0;
        }
        "h2" => {
            style.size *= 1.6;
            style.bold = true;
            style.space_after += 5.0;
        }
        "h3" => {
            style.size *= 1.35;
            style.bold = true;
            style.space_after += 4.0;
        }
        "h4" => {
            style.size *= 1.15;
            style.bold = true;
        }
        "h5" | "h6" => style.bold = true,
        "strong" | "b" => style.bold = true,
        "em" | "i" | "cite" => style.italic = true,
        "code" | "kbd" | "samp" | "tt" => {
            style.mono = true;
            style.size *= 0.9;
        }
        "a" => style.underline = true,
        "blockquote" => {
            style.italic = true;
            style.indent += 24.0;
            style.border = Some(LineBorder {
                width: 2.0,
                color: [0.5, 0.5, 0.5],
                sides: LineBorder::LEFT,
            });
        }
        "pre" => {
            style.mono = true;
            style.size *= 0.9;
        }
        "li" => style.indent += 18.0,
        "th" => style.bold = true,
        _ => {}
    }
}

impl<'a> Renderer<'a> {
    fn new(
        options: &'a CreateOptions,
        base: &'a Path,
        css_enabled: bool,
        rules: Vec<CssRule>,
    ) -> Self {
        Self {
            options,
            base,
            css_enabled,
            rules,
            items: Vec::new(),
            images: Vec::new(),
        }
    }

    /// Compute the style of an element from inherited values, semantic
    /// defaults, matching CSS rules, and inline declarations (last wins).
    fn style_for(&self, name: &str, attrs: &[(String, String)], inherited: Style) -> Style {
        let mut style = inherited;
        apply_tag_defaults(&mut style, name);
        if self.css_enabled {
            let classes: Vec<&str> = attrs
                .iter()
                .find(|(key, _)| key == "class")
                .map(|(_, value)| value.split_whitespace().collect())
                .unwrap_or_default();
            for rule in &self.rules {
                let matches = rule.selector == "*"
                    || rule.selector == name
                    || classes.iter().any(|class| {
                        rule.selector
                            .strip_prefix('.')
                            .is_some_and(|selector| selector == *class)
                    });
                if matches {
                    apply_style(&mut style, &rule.declarations);
                }
            }
            if let Some((_, declarations)) = attrs.iter().find(|(key, _)| key == "style") {
                let mut parsed = Vec::new();
                if parse_declarations(declarations, &mut parsed).is_ok() {
                    apply_style(&mut style, &parsed);
                }
            }
        }
        style
    }

    /// Render all children of a container, grouping consecutive inline nodes
    /// into a single line.
    fn render_children(&mut self, children: &[Node], style: Style) -> Result<(), CreateError> {
        let mut pending: Vec<InlineAtom> = Vec::new();
        for child in children {
            match &child.kind {
                NodeKind::Text(text) => {
                    pending.push(InlineAtom::Text {
                        text: text.clone(),
                        style,
                    });
                }
                NodeKind::Raw(_) => {}
                NodeKind::Element { name, attrs } if is_inline_element(name) => {
                    let child_style = self.style_for(name, attrs, style);
                    self.collect_inline(child, child_style, &mut pending)?;
                }
                NodeKind::Element { .. } => {
                    if !pending.is_empty() {
                        self.emit_inline(&pending, style, true)?;
                        pending.clear();
                    }
                    self.render_node(child, style)?;
                }
            }
        }
        if !pending.is_empty() {
            self.emit_inline(&pending, style, true)?;
        }
        Ok(())
    }

    /// Collect text and image atoms from an inline element's descendants.
    fn collect_inline(
        &mut self,
        node: &Node,
        style: Style,
        atoms: &mut Vec<InlineAtom>,
    ) -> Result<(), CreateError> {
        match &node.kind {
            NodeKind::Text(text) => atoms.push(InlineAtom::Text {
                text: text.clone(),
                style,
            }),
            NodeKind::Raw(_) => {}
            NodeKind::Element { name, attrs } => match name.as_str() {
                "br" | "wbr" => atoms.push(InlineAtom::Text {
                    text: "\n".to_owned(),
                    style,
                }),
                "img" => {
                    let src = attrs
                        .iter()
                        .find(|(key, _)| key == "src")
                        .map(|(_, value)| value.clone())
                        .unwrap_or_default();
                    atoms.push(InlineAtom::Image { src, style });
                }
                "style" => {}
                _ => {
                    let child_style = self.style_for(name, attrs, style);
                    for child in &node.children {
                        self.collect_inline(child, child_style, atoms)?;
                    }
                }
            },
        }
        Ok(())
    }

    /// Render one block node and its descendants.
    fn render_node(&mut self, node: &Node, inherited: Style) -> Result<(), CreateError> {
        match &node.kind {
            NodeKind::Text(text) => {
                let atoms = vec![InlineAtom::Text {
                    text: text.clone(),
                    style: inherited,
                }];
                self.emit_inline(&atoms, inherited, true)
            }
            NodeKind::Raw(_) => Ok(()),
            NodeKind::Element { name, attrs } => {
                let style = self.style_for(name, attrs, inherited);
                match name.as_str() {
                    "html" | "body" | "div" | "section" | "article" | "main" | "header"
                    | "footer" | "nav" | "aside" | "figure" | "center" | "form" => {
                        self.render_children(&node.children, style)
                    }
                    "ul" | "ol" => {
                        let ordered = name == "ol";
                        let mut index = 0usize;
                        for child in &node.children {
                            if !matches!(&child.kind, NodeKind::Element { name, .. } if name == "li")
                            {
                                continue;
                            }
                            index += 1;
                            let mut atoms = Vec::new();
                            let prefix = if ordered {
                                format!("{index}. ")
                            } else {
                                "\u{2022} ".to_owned()
                            };
                            atoms.push(InlineAtom::Text {
                                text: prefix,
                                style,
                            });
                            self.collect_inline(child, style, &mut atoms)?;
                            self.emit_inline(&atoms, style, true)?;
                        }
                        Ok(())
                    }
                    "pre" => {
                        let mut atoms = Vec::new();
                        for child in &node.children {
                            self.collect_inline(child, style, &mut atoms)?;
                        }
                        self.emit_pre(&atoms, style)
                    }
                    "table" => self.render_table(node, style),
                    "hr" => {
                        let rule_style = Style {
                            border: Some(LineBorder {
                                width: 1.0,
                                color: [0.5, 0.5, 0.5],
                                sides: LineBorder::BOTTOM,
                            }),
                            ..style
                        };
                        self.push_line(
                            b" ".to_vec(),
                            vec![Span {
                                start: 0,
                                len: 1,
                                x: 0.0,
                                font: rule_style.font(),
                                color: None,
                                underline: false,
                            }],
                            rule_style,
                            0.0,
                        );
                        Ok(())
                    }
                    "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "p" | "blockquote" | "li" | "td"
                    | "th" | "address" | "figcaption" | "tr" => {
                        let mut atoms = Vec::new();
                        for child in &node.children {
                            self.collect_inline(child, style, &mut atoms)?;
                        }
                        self.emit_inline(&atoms, style, true)
                    }
                    "img" => {
                        let src = attrs
                            .iter()
                            .find(|(key, _)| key == "src")
                            .map(|(_, value)| value.as_str())
                            .unwrap_or("");
                        self.add_image(src, style)
                    }
                    "br" | "style" => Ok(()),
                    _ => self.render_children(&node.children, style),
                }
            }
        }
    }

    /// Render one table row per line with a rectangular border around the row.
    fn render_table(&mut self, table: &Node, style: Style) -> Result<(), CreateError> {
        for child in &table.children {
            let NodeKind::Element { name, .. } = &child.kind else {
                continue;
            };
            if name != "tr" {
                continue;
            }
            let mut cell_texts: Vec<String> = Vec::new();
            let mut cell_style = style;
            for cell in &child.children {
                let NodeKind::Element {
                    name: cell_name,
                    attrs,
                } = &cell.kind
                else {
                    continue;
                };
                if cell_name != "td" && cell_name != "th" {
                    continue;
                }
                cell_style = self.style_for(cell_name, attrs, style);
                let mut atoms = Vec::new();
                for grandchild in &cell.children {
                    self.collect_inline(grandchild, cell_style, &mut atoms)?;
                }
                let mut text = String::new();
                for atom in atoms {
                    if let InlineAtom::Text { text: value, .. } = atom {
                        if !text.is_empty() {
                            text.push(' ');
                        }
                        text.push_str(&value);
                    }
                }
                cell_texts.push(text);
            }
            if cell_texts.is_empty() {
                continue;
            }
            let joined = cell_texts.join("  |  ");
            let row_style = Style {
                border: Some(LineBorder {
                    width: 0.75,
                    color: [0.0, 0.0, 0.0],
                    sides: LineBorder::BOX,
                }),
                ..cell_style
            };
            let bytes = encode_text(&joined);
            let span = Span {
                start: 0,
                len: bytes.len(),
                x: 0.0,
                font: row_style.font(),
                color: row_style.color,
                underline: false,
            };
            self.push_line(bytes, vec![span], row_style, 3.0);
        }
        Ok(())
    }

    /// Emit verbatim monospace lines for a `pre` element.
    fn emit_pre(&mut self, atoms: &[InlineAtom], style: Style) -> Result<(), CreateError> {
        let mut text = String::new();
        for atom in atoms {
            if let InlineAtom::Text { text: value, .. } = atom {
                text.push_str(value);
            }
        }
        let pre_style = Style {
            mono: true,
            indent: style.indent + 18.0,
            ..style
        };
        for source_line in text.split('\n') {
            let bytes = encode_text(source_line);
            let span = Span {
                start: 0,
                len: bytes.len(),
                x: 0.0,
                font: FontKind::Mono,
                color: pre_style.color,
                underline: false,
            };
            self.push_line(bytes, vec![span], pre_style, 0.0);
        }
        Ok(())
    }

    /// Emit inline atoms as wrapped lines, placing images as block items.
    fn emit_inline(
        &mut self,
        atoms: &[InlineAtom],
        block_style: Style,
        wrap: bool,
    ) -> Result<(), CreateError> {
        if block_style.space_before > 0.0 {
            let blank_size =
                (block_style.space_before / self.options.line_height()).clamp(1.0, 144.0);
            let blank = StyledLine::regular(Vec::new(), blank_size);
            self.items.push(FlowItem::Text(blank));
        }
        let content_width = self.options.page_size().width
            - self.options.margins().left
            - self.options.margins().right;
        let available = (content_width - block_style.indent).max(1.0);
        let limit = if wrap { available } else { 10_000.0 };

        let mut state = LineState::new(block_style);

        for atom in atoms {
            match atom {
                InlineAtom::Image { src, style } => {
                    if state.has_content {
                        flush_line(&mut self.items, &mut state, block_style, false);
                    }
                    self.add_image(src, *style)?;
                }
                InlineAtom::Text { text, style } => {
                    for segment in text.split('\n') {
                        // Only ASCII separators are word breaks: `&nbsp;`
                        // (U+00A0) is a real glyph and must survive encoding.
                        for word in segment.split([' ', '\t', '\r']) {
                            if word.is_empty() {
                                continue;
                            }
                            let font = style.font();
                            let encoded = encode_text(word);
                            let word_width = text_width(&encoded, style.size, font.is_mono());
                            let space_width =
                                text_width(b" ", style.size, state.style.font().is_mono());
                            if state.has_content && state.width + space_width + word_width > limit {
                                flush_line(&mut self.items, &mut state, block_style, false);
                            }
                            let before_separator = state.bytes.len();
                            if state.has_content {
                                state.bytes.push(b' ');
                                state.width += space_width;
                            } else {
                                state.style = *style;
                            }
                            // Merge consecutive words that share a style so
                            // ordinary prose is shown as one PDF string. The
                            // separator space becomes part of the span.
                            let span_end =
                                before_separator + usize::from(state.has_content) + encoded.len();
                            let merge = state.spans.last().is_some_and(|span| {
                                span.font == font
                                    && span.color == style.color
                                    && span.underline == style.underline
                                    && span.start + span.len == before_separator
                            });
                            if merge {
                                if let Some(last) = state.spans.last_mut() {
                                    last.len = span_end - last.start;
                                }
                            } else {
                                // Include the separator space in this span so
                                // text extractors keep the word boundary even
                                // though the next run has a different style.
                                let leading_space = usize::from(state.has_content);
                                state.spans.push(Span {
                                    start: before_separator,
                                    len: leading_space + encoded.len(),
                                    x: state.width
                                        - if leading_space == 1 { space_width } else { 0.0 },
                                    font,
                                    color: style.color,
                                    underline: style.underline,
                                });
                            }
                            state.bytes.extend_from_slice(&encoded);
                            state.width += word_width;
                            state.has_content = true;
                        }
                    }
                }
            }
        }
        if state.has_content {
            flush_line(&mut self.items, &mut state, block_style, true);
        }
        Ok(())
    }

    /// Push a single fully styled line (used by tables, rules, and `pre`).
    fn push_line(&mut self, bytes: Vec<u8>, spans: Vec<Span>, style: Style, space_after: f64) {
        let mut line = StyledLine::regular(bytes, style.size);
        line.font = style.font();
        line.indent = style.indent;
        line.wrap = false;
        line.align = style.align;
        line.border = style.border;
        line.space_after = space_after;
        line.spans = spans;
        self.items.push(FlowItem::Text(line));
    }

    /// Resolve, validate, and measure a local image reference.
    fn add_image(&mut self, src: &str, style: Style) -> Result<(), CreateError> {
        validate_url(src)?;
        let relative = Path::new(src.trim());
        let candidate = self.base.join(relative);
        let base_canon = std::fs::canonicalize(self.base)?;
        let candidate_canon = std::fs::canonicalize(&candidate)?;
        if !candidate_canon.starts_with(&base_canon) {
            return Err(CreateError::HtmlPathTraversal(src.to_owned()));
        }
        let bytes = std::fs::read(&candidate_canon)?;
        let extension = candidate_canon
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or("");
        let format = CreateImageFormat::from_extension(extension).ok_or_else(|| {
            CreateError::UnsupportedImageFormat {
                extension: extension.to_owned(),
            }
        })?;
        let source = match format {
            CreateImageFormat::Png => parse_png(&bytes)?,
            CreateImageFormat::Jpeg => parse_jpeg(&bytes)?,
        };
        let dpi = source
            .dpi
            .filter(|value| value.is_finite() && *value > 0.0)
            .unwrap_or(DEFAULT_DPI);
        let native_width = f64::from(source.width) / dpi * 72.0;
        let native_height = f64::from(source.height) / dpi * 72.0;
        let content_width = self.options.page_size().width
            - self.options.margins().left
            - self.options.margins().right;
        let available = (content_width - style.indent).max(1.0);
        let scale = (available / native_width).min(1.0);
        let image_id = self.images.len();
        self.images.push(source);
        self.items.push(FlowItem::Image(ImagePlacement {
            image_id,
            width: native_width * scale,
            height: native_height * scale,
            indent: style.indent,
            align: style.align,
            space_after: style.space_after,
        }));
        Ok(())
    }
}

/// Mutable state for the visual line currently being gathered from inline
/// runs. Bundling it keeps the line-flushing helper small and total.
#[derive(Debug)]
struct LineState {
    bytes: Vec<u8>,
    spans: Vec<Span>,
    width: f64,
    style: Style,
    has_content: bool,
}

impl LineState {
    fn new(style: Style) -> Self {
        Self {
            bytes: Vec::new(),
            spans: Vec::new(),
            width: 0.0,
            style,
            has_content: false,
        }
    }
}

/// Move the current run into a finished line. `final_line` controls whether
/// the block's border and trailing spacing are attached to it.
fn flush_line(
    items: &mut Vec<FlowItem>,
    state: &mut LineState,
    block_style: Style,
    final_line: bool,
) {
    if state.has_content {
        let mut line = StyledLine::regular(std::mem::take(&mut state.bytes), state.style.size);
        line.font = state.style.font();
        line.indent = state.style.indent;
        line.wrap = false;
        line.align = state.style.align;
        line.border = if final_line { block_style.border } else { None };
        line.space_after = if final_line {
            block_style.space_after
        } else {
            0.0
        };
        line.spans = std::mem::take(&mut state.spans);
        items.push(FlowItem::Text(line));
    }
    state.has_content = false;
    state.width = 0.0;
    state.style = block_style;
}

/// Parse and render HTML into a complete PDF byte stream.
fn render_html(
    html: &str,
    html_options: &HtmlOptions,
    options: &CreateOptions,
) -> Result<Vec<u8>, CreateError> {
    let tokens = tokenize(html)?;
    let tree = build_tree(tokens)?;
    let rules = if html_options.css_enabled {
        collect_css(&tree)?
    } else {
        Vec::new()
    };
    let mut renderer = Renderer::new(
        options,
        html_options.base_dir(),
        html_options.css_enabled,
        rules,
    );
    let base_style = Style::base(options.font_size());
    renderer.render_children(&tree.children, base_style)?;
    if renderer.items.is_empty() {
        return Err(CreateError::EmptyDocument);
    }
    Ok(build_html_pdf(&renderer.items, &renderer.images, options))
}

// ---------------------------------------------------------------------------
// PDF assembly (reuses the create module's deterministic writer primitives)
// ---------------------------------------------------------------------------

/// The five base-14 resources used by HTML pages: regular, bold, mono,
/// oblique, and bold-oblique Helvetica variants.
type HtmlCommon = (u32, u32, u32, u32, u32, u32, u32);

/// Reserve the catalog, page tree, and five font objects.
fn reserve_html_common(writer: &mut super::PdfWriter) -> HtmlCommon {
    (
        writer.reserve(),
        writer.reserve(),
        writer.reserve(),
        writer.reserve(),
        writer.reserve(),
        writer.reserve(),
        writer.reserve(),
    )
}

/// Write the shared catalog, page tree, and five base-14 font objects.
fn write_html_common(writer: &mut super::PdfWriter, ids: HtmlCommon, page_ids: &[u32]) {
    let (catalog, pages, f1, f2, f3, f4, f5) = ids;
    writer.set(
        catalog,
        format!("<< /Type /Catalog /Pages {pages} 0 R >>").into_bytes(),
    );
    let kids = page_ids
        .iter()
        .map(|id| format!("{id} 0 R"))
        .collect::<Vec<_>>()
        .join(" ");
    writer.set(
        pages,
        format!(
            "<< /Type /Pages /Kids [ {kids} ] /Count {} >>",
            page_ids.len()
        )
        .into_bytes(),
    );
    writer.set(
        f1,
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>"
            .to_vec(),
    );
    writer.set(
        f2,
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica-Bold /Encoding /WinAnsiEncoding >>"
            .to_vec(),
    );
    writer.set(
        f3,
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Courier /Encoding /WinAnsiEncoding >>".to_vec(),
    );
    writer.set(
        f4,
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica-Oblique /Encoding /WinAnsiEncoding >>"
            .to_vec(),
    );
    writer.set(
        f5,
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica-BoldOblique /Encoding /WinAnsiEncoding >>"
            .to_vec(),
    );
}

/// Resource dictionary fragment naming all five fonts.
fn html_font_resources(ids: HtmlCommon) -> String {
    let (_, _, f1, f2, f3, f4, f5) = ids;
    format!("/Font << /F1 {f1} 0 R /F2 {f2} 0 R /F3 {f3} 0 R /F4 {f4} 0 R /F5 {f5} 0 R >>")
}

/// Append a filled rectangle to a content stream.
fn fill_rect(out: &mut Vec<u8>, color: [f64; 3], x: f64, y: f64, width: f64, height: f64) {
    if width <= 0.0 || height <= 0.0 {
        return;
    }
    out.extend_from_slice(
        format!(
            "q {:.3} {:.3} {:.3} rg {:.2} {:.2} {:.2} {:.2} re f Q\n",
            color[0], color[1], color[2], x, y, width, height
        )
        .as_bytes(),
    );
}

/// Render one page of placed items into a content stream and collect the image
/// sources in `/ImN` order.
fn render_html_page(
    page: &[PlacedItem],
    images: &[ImageSource],
) -> Result<(Vec<u8>, Vec<ImageSource>), CreateError> {
    let mut decorations = Vec::new();
    let mut text = Vec::new();
    text.extend_from_slice(b"BT\n");
    let mut page_images: Vec<ImageSource> = Vec::new();

    for item in page {
        match item {
            PlacedItem::Image {
                image_id,
                x,
                y,
                width,
                height,
            } => {
                let index = page_images.len();
                let source = images.get(*image_id).ok_or_else(|| {
                    CreateError::HtmlStructure("image index out of range".to_owned())
                })?;
                page_images.push(source.clone());
                text.extend_from_slice(
                    format!("q {width:.2} 0 0 {height:.2} {x:.2} {y:.2} cm /Im{index} Do Q\n")
                        .as_bytes(),
                );
            }
            PlacedItem::Text(line) => {
                render_borders(&mut decorations, line);
                render_line(&mut text, line);
            }
        }
    }
    text.extend_from_slice(b"ET\n");
    decorations.extend_from_slice(&text);
    Ok((decorations, page_images))
}

/// Draw a line box's border edges and underlines as filled rectangles.
fn render_borders(out: &mut Vec<u8>, line: &PlacedLine) {
    if line.bytes.is_empty() {
        return;
    }
    let width = text_width(&line.bytes, line.size, line.font.is_mono());
    let left = line.x - 2.0;
    let right = line.x + width + 2.0;
    let bottom = line.y - line.size - 2.0;
    let top = line.y + 2.0;
    if let Some(border) = line.border {
        let color = border.color;
        if border.sides & LineBorder::TOP != 0 {
            fill_rect(
                out,
                color,
                left,
                top - border.width,
                right - left,
                border.width,
            );
        }
        if border.sides & LineBorder::BOTTOM != 0 {
            fill_rect(out, color, left, bottom, right - left, border.width);
        }
        if border.sides & LineBorder::LEFT != 0 {
            fill_rect(out, color, left, bottom, border.width, top - bottom);
        }
        if border.sides & LineBorder::RIGHT != 0 {
            fill_rect(
                out,
                color,
                right - border.width,
                bottom,
                border.width,
                top - bottom,
            );
        }
    }
    for span in &line.spans {
        if span.underline {
            let color = span.color.unwrap_or([0.0, 0.0, 0.0]);
            let end = span.start.saturating_add(span.len).min(line.bytes.len());
            let span_width = text_width(
                line.bytes.get(span.start..end).unwrap_or(&[]),
                line.size,
                span.font.is_mono(),
            );
            fill_rect(out, color, line.x + span.x, line.y - 1.5, span_width, 0.75);
        }
    }
}

/// Emit the text operators for one placed line.
fn render_line(out: &mut Vec<u8>, line: &PlacedLine) {
    if line.bytes.is_empty() {
        return;
    }
    let default_color = line.color.unwrap_or([0.0, 0.0, 0.0]);
    if line.spans.is_empty() {
        out.extend_from_slice(
            format!(
                "{:.3} {:.3} {:.3} rg /{} {:.2} Tf 1 0 0 1 {:.2} {:.2} Tm\n",
                default_color[0],
                default_color[1],
                default_color[2],
                line.font.resource(),
                line.size,
                line.x,
                line.y
            )
            .as_bytes(),
        );
        out.push(b'(');
        out.extend_from_slice(&pdf_escape(&line.bytes));
        out.extend_from_slice(b"\\n) Tj\n");
        return;
    }
    let last = line.spans.len().saturating_sub(1);
    for (index, span) in line.spans.iter().enumerate() {
        let end = span.start.saturating_add(span.len).min(line.bytes.len());
        let slice = line.bytes.get(span.start..end).unwrap_or(&[]);
        if slice.is_empty() {
            continue;
        }
        let color = span.color.unwrap_or(default_color);
        out.extend_from_slice(
            format!(
                "{:.3} {:.3} {:.3} rg /{} {:.2} Tf 1 0 0 1 {:.2} {:.2} Tm\n",
                color[0],
                color[1],
                color[2],
                span.font.resource(),
                line.size,
                line.x + span.x,
                line.y
            )
            .as_bytes(),
        );
        out.push(b'(');
        out.extend_from_slice(&pdf_escape(slice));
        if index == last {
            out.extend_from_slice(b"\\n) Tj\n");
        } else {
            out.extend_from_slice(b") Tj\n");
        }
    }
}

/// Assemble all pages into a deterministic classic-xref PDF.
fn build_html_pdf(items: &[FlowItem], images: &[ImageSource], options: &CreateOptions) -> Vec<u8> {
    let pages = layout_items(items, options);
    let mut writer = super::PdfWriter::new();
    let common = reserve_html_common(&mut writer);
    let (catalog, pages_id, _, _, _, _, _) = common;

    let mut planned = Vec::with_capacity(pages.len());
    let mut page_ids = Vec::with_capacity(pages.len());
    for page in &pages {
        let page_id = writer.reserve();
        page_ids.push(page_id);
        let content_id = writer.reserve();
        let (content, page_images) = render_html_page(page, images).unwrap_or_default();
        let image_ids: Vec<u32> = (0..page_images.len()).map(|_| writer.reserve()).collect();
        planned.push((page_id, content_id, content, page_images, image_ids));
    }
    write_html_common(&mut writer, common, &page_ids);

    for (page_id, content_id, content, page_images, image_ids) in planned {
        let mut resources = html_font_resources(common);
        if !image_ids.is_empty() {
            let xobjects = image_ids
                .iter()
                .enumerate()
                .map(|(index, id)| format!("/Im{} {} 0 R", index, id))
                .collect::<Vec<_>>()
                .join(" ");
            resources.push_str(&format!(" /XObject << {xobjects} >>"));
        }
        writer.set(
            page_id,
            page_object(pages_id, options, &resources, content_id),
        );
        writer.set(content_id, super::stream_object(&content));
        for (index, source) in page_images.iter().enumerate() {
            if let Some(&id) = image_ids.get(index) {
                let mut body = format!("{}\nstream\n", source.stream_dictionary).into_bytes();
                body.extend_from_slice(&source.data);
                body.extend_from_slice(b"\nendstream");
                writer.set(id, body);
            }
        }
    }
    writer.finish(catalog)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::create::{PageSize, create_from_text};
    use std::fs;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    static TEMP_COUNTER: AtomicUsize = AtomicUsize::new(0);

    fn options() -> CreateOptions {
        CreateOptions::new().with_page_size(PageSize::A4)
    }

    fn as_text(pdf: &[u8]) -> String {
        String::from_utf8_lossy(pdf).into_owned()
    }

    /// Minimal valid non-interlaced RGB PNG.
    fn sample_png() -> Vec<u8> {
        fn crc(data: &[u8]) -> u32 {
            let mut value = 0xFFFF_FFFFu32;
            for &byte in data {
                value ^= u32::from(byte);
                for _ in 0..8 {
                    let mask = 0u32.wrapping_sub(value & 1);
                    value = (value >> 1) ^ (0xEDB8_8320 & mask);
                }
            }
            value ^ 0xFFFF_FFFF
        }
        fn adler(data: &[u8]) -> u32 {
            let (mut a, mut b) = (1u32, 0u32);
            for &byte in data {
                a = (a + u32::from(byte)) % 65_521;
                b = (b + a) % 65_521;
            }
            (b << 16) | a
        }
        fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
            out.extend_from_slice(&(data.len() as u32).to_be_bytes());
            out.extend_from_slice(kind);
            out.extend_from_slice(data);
            let mut checked = Vec::from(*kind);
            checked.extend_from_slice(data);
            out.extend_from_slice(&crc(&checked).to_be_bytes());
        }
        let mut ihdr = Vec::from(&4u32.to_be_bytes()[..]);
        ihdr.extend_from_slice(&4u32.to_be_bytes());
        ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
        let raw = vec![0u8; 4 * 3 + 1];
        let mut z = vec![0x78, 0x01, 0x01];
        z.extend_from_slice(&(raw.len() as u16).to_le_bytes());
        z.extend_from_slice(&(!(raw.len() as u16)).to_le_bytes());
        z.extend_from_slice(&raw);
        z.extend_from_slice(&adler(&raw).to_be_bytes());
        let mut out = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
        chunk(&mut out, b"IHDR", &ihdr);
        chunk(&mut out, b"IDAT", &z);
        chunk(&mut out, b"IEND", &[]);
        out
    }

    /// Small JPEG-shaped buffer with a JFIF marker and SOF frame.
    fn sample_jpeg() -> Vec<u8> {
        let mut out = vec![0xFF, 0xD8];
        let mut app0 = Vec::from(&b"JFIF\0"[..]);
        app0.extend_from_slice(&[1, 1]);
        app0.extend_from_slice(&72u16.to_be_bytes());
        app0.extend_from_slice(&72u16.to_be_bytes());
        app0.extend_from_slice(&[0, 0]);
        out.push(0xFF);
        out.push(0xE0);
        out.extend_from_slice(&((app0.len() + 2) as u16).to_be_bytes());
        out.extend_from_slice(&app0);
        let mut sof = vec![8];
        sof.extend_from_slice(&8u16.to_be_bytes());
        sof.extend_from_slice(&12u16.to_be_bytes());
        sof.extend_from_slice(&[3, 1, 0x11, 0, 2, 0x11, 1, 0x11, 1]);
        out.push(0xFF);
        out.push(0xC0);
        out.extend_from_slice(&((sof.len() + 2) as u16).to_be_bytes());
        out.extend_from_slice(&sof);
        out.extend_from_slice(&[0xFF, 0xDA, 0x00, 0x02, 0xFF, 0xD9]);
        out
    }

    /// Create a temporary directory with a unique name for image tests.
    fn temp_dir() -> std::path::PathBuf {
        let unique = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        let path = std::env::temp_dir().join(format!(
            "pdfxi-html-{}-{nanos}-{unique}",
            std::process::id()
        ));
        fs::create_dir_all(&path).expect("create temp dir");
        path
    }

    #[test]
    fn headings_paragraphs_and_entities_render() {
        let html = "<h1>Tom &amp; Jerry</h1><p>&lt;tag&gt; &nbsp; &#169; &#x2022;.</p>";
        let pdf = create_from_html(html, &options()).expect("headings and entities");
        let text = as_text(&pdf);
        assert!(text.contains("Tom & Jerry"), "named amp entity: {text}");
        assert!(text.contains("<tag>"), "named lt/gt entities");
        assert!(text.contains("\\240"), "nbsp maps to Latin-1 0xA0");
        assert!(text.contains("\\251"), "copyright maps to 0xA9");
        assert!(text.contains("\\225"), "bullet maps to 0x95");
        assert!(text.contains("/F2"), "heading is bold");
    }

    #[test]
    fn unordered_and_ordered_lists_render_items() {
        let html = "<ul><li>first</li><li>second</li></ul><ol><li>alpha</li><li>beta</li></ol>";
        let pdf = create_from_html(html, &options()).expect("lists");
        let text = as_text(&pdf);
        assert!(text.contains("\\225 first"), "unordered bullet");
        assert!(text.contains("\\225 second"), "second unordered item");
        assert!(
            text.contains("1. alpha") && text.contains("2. beta"),
            "ordered items"
        );
    }

    #[test]
    fn table_rows_and_cells_render_with_borders() {
        let html =
            "<table><tr><th>Name</th><th>Qty</th></tr><tr><td>Widget</td><td>3</td></tr></table>";
        let pdf = create_from_html(html, &options()).expect("table");
        let text = as_text(&pdf);
        assert!(text.contains("Name  |  Qty"), "header row cells");
        assert!(text.contains("Widget  |  3"), "body row cells");
        assert!(text.contains(" re f"), "border rectangles are drawn");
    }

    #[test]
    fn inline_css_styles_apply_fonts_colors_and_alignment() {
        let html = "<p style=\"font-size:20px;font-weight:bold;color:#ff0000\">Big</p>\
                    <p style=\"font-style:italic;text-align:center\">Nice</p>\
                    <p><em>Soft</em> <code>x=1</code></p>";
        let pdf = create_from_html(html, &options()).expect("styles");
        let text = as_text(&pdf);
        assert!(text.contains("/F2 15.00 Tf"), "20px = 15pt bold text");
        assert!(text.contains("1.000 0.000 0.000 rg"), "red fill colour");
        assert!(text.contains("/F4"), "italic Helvetica-Oblique resource");
        assert!(text.contains("/F3"), "Courier for code");
        assert!(
            text.contains("/Helvetica-Oblique"),
            "oblique font object present"
        );
    }

    #[test]
    fn local_png_and_jpeg_images_are_embedded() {
        let dir = temp_dir();
        fs::write(dir.join("a.png"), sample_png()).expect("write png");
        fs::write(dir.join("b.jpg"), sample_jpeg()).expect("write jpeg");
        let html = "<p>Before</p><img src=\"a.png\"><img src=\"b.jpg\"><p>After</p>";
        let html_options = HtmlOptions::new().with_base_dir(&dir);
        let pdf = create_from_html_with_options(html, &html_options, &options()).expect("images");
        let text = as_text(&pdf);
        assert!(text.contains("/Subtype /Image"), "image XObjects present");
        assert!(
            text.contains("/Filter /FlateDecode"),
            "PNG uses FlateDecode"
        );
        assert!(text.contains("/Filter /DCTDecode"), "JPEG uses DCTDecode");
        assert!(
            text.contains("Before") && text.contains("After"),
            "text around images"
        );
        assert_eq!(text.matches("/Subtype /Image").count(), 2);
        assert!(
            text.contains("/Im0 Do") && text.contains("/Im1 Do"),
            "image draw operators"
        );
        assert!(
            text.contains("/XObject << /Im0 ") && text.contains("/Im1 "),
            "XObject resource names must match the /ImN draw operators"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn scripts_event_handlers_and_javascript_urls_are_rejected() {
        for html in [
            "<p>ok</p><script>alert(1)</script>",
            "<p onclick=\"bad()\">unsafe</p>",
            "<a href=\"javascript:alert(1)\">click</a>",
            "<iframe src=\"x.html\"></iframe>",
        ] {
            let error = create_from_html(html, &options()).expect_err("active content rejected");
            assert!(
                matches!(error, CreateError::HtmlScriptRejected(_)),
                "expected script rejection for {html}: {error}"
            );
        }
    }

    #[test]
    fn remote_urls_and_file_schemes_are_rejected() {
        for html in [
            "<a href=\"https://example.org/x\">remote</a>",
            "<img src=\"//cdn.example.org/a.png\">",
            "<img src=\"file:///etc/passwd\">",
            "<a href=\"data:text/html,x\">data</a>",
        ] {
            let error = create_from_html(html, &options()).expect_err("remote rejected");
            assert!(
                matches!(error, CreateError::HtmlRemoteResource(_)),
                "expected remote rejection for {html}: {error}"
            );
        }
    }

    #[test]
    fn path_traversal_is_rejected() {
        let dir = temp_dir();
        let html_options = HtmlOptions::new().with_base_dir(&dir);
        for html in [
            "<img src=\"../secret.png\">",
            "<img src=\"/etc/passwd\">",
            "<img src=\"C:\\secret.png\">",
            "<img src=\"%2e%2e/secret.png\">",
        ] {
            let error = create_from_html_with_options(html, &html_options, &options())
                .expect_err("traversal rejected");
            assert!(
                matches!(error, CreateError::HtmlPathTraversal(_)),
                "expected traversal rejection for {html}: {error}"
            );
        }
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn malformed_html_recovers_deterministically() {
        let html = "<p>unclosed <strong>bold <em>and italic</p><p>next</p></div>\
                    stray & <bogus>text</bogus> &#xZZ; done";
        let pdf = create_from_html(html, &options()).expect("malformed recovery");
        let again = create_from_html(html, &options()).expect("malformed recovery again");
        assert_eq!(pdf, again, "recovery is deterministic");
        let text = as_text(&pdf);
        assert!(text.contains("unclosed") && text.contains("next"));
        assert!(
            text.contains("done"),
            "unknown entity is preserved literally"
        );
        assert!(pdf.starts_with(b"%PDF-1.4") && pdf.ends_with(b"%%EOF\n"));
    }

    #[test]
    fn deterministic_bytes_match_repeated_calls() {
        let html = "<h1>Deterministic</h1><p>A <strong>bold</strong> word.</p>";
        let first = create_from_html(html, &options()).expect("first");
        let second = create_from_html(html, &options()).expect("second");
        assert_eq!(first, second);
        assert_eq!(first, create_from_html(html, &options()).expect("third"));
    }

    #[test]
    fn css_rules_apply_and_unknown_declarations_are_ignored() {
        let html = "<style>p { font-size: 16px; color: rgb(0,128,0); bogus: 1px; } \
                    .accent { font-weight: bold; }</style>\
                    <p>Plain</p><p class=\"accent\">Accent</p>";
        let pdf = create_from_html(html, &options()).expect("css rules");
        let text = as_text(&pdf);
        assert!(text.contains("/F2 12.00 Tf"), "16px = 12pt, bold class");
        assert!(text.contains("0.000 0.502 0.000 rg"), "rgb(0,128,0) fill");
        assert!(!text.contains("bogus"), "unknown declaration ignored");
    }

    #[test]
    fn blockquote_pre_and_code_render_monospace_and_rules() {
        let html = "<blockquote>quoted</blockquote><pre>let x = 1;\nlet y = 2;</pre>\
                    <p>A <code>z</code> value.</p>";
        let pdf = create_from_html(html, &options()).expect("blockquote/pre/code");
        let text = as_text(&pdf);
        assert!(text.contains("quoted") && text.contains("let x = 1;"));
        assert!(text.contains("/F3"), "monospace Courier resource used");
        assert!(text.contains(" re f"), "blockquote left rule drawn");
    }

    #[test]
    fn oversized_and_empty_input_return_typed_errors() {
        let limited = HtmlOptions::new().with_max_bytes(8);
        let error = create_from_html_with_options("<p>too long</p>", &limited, &options())
            .expect_err("oversized");
        assert!(matches!(error, CreateError::HtmlTooLarge { .. }));
        let error = create_from_html("<p> </p><div></div>", &options()).expect_err("empty");
        assert!(matches!(error, CreateError::EmptyDocument));
    }

    #[test]
    fn link_target_is_never_fetched_but_text_is_rendered() {
        let html = "<a href=\"missing-local-file.html\">Local link</a>";
        let pdf = create_from_html(html, &options()).expect("link text");
        let text = as_text(&pdf);
        assert!(text.contains("Local link"));
        assert!(text.contains(" rg"), "text colour operator present");
    }

    #[test]
    fn text_and_html_paths_keep_separate_font_tables() {
        let text_pdf = create_from_text("plain", &options()).expect("text path");
        let html_pdf = create_from_html("<p>plain</p>", &options()).expect("html path");
        let text = as_text(&text_pdf);
        assert!(
            !text.contains("Helvetica-Oblique"),
            "text path keeps three fonts"
        );
        let html = as_text(&html_pdf);
        assert!(
            html.contains("Helvetica-Oblique"),
            "html path declares five fonts"
        );
        assert!(html.contains("/Count 1"));
    }
}
