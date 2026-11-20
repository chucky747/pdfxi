//! Local "open in browser" handoff for exported HTML (PLAT-004).
//!
//! This module models the handoff of a locally exported file to a browser.
//! It performs **no network access and no process spawning**: it parses a
//! validated [`HandoffRequest`] (local path or `file:` URL, with an optional
//! MIME type), resolves a local-only path with traversal rejection, and
//! produces a deterministic open/download [`HandoffPlan`] with a stable summary.
//! It also retains the original HTML-only [`BrowserHandoff`] URL builder. ADR
//! 0008 requires local, file-based equivalents; this is the browser-integration
//! equivalent. No browser is launched and no extension is installed.
//!
//! The central safety rule is [`validate_rooted_path`]: only rooted (absolute)
//! paths with no parent traversal are accepted, so a hostile drop name cannot
//! escape the export directory via `..` or a drive/UNC trick.

use thiserror::Error;

/// Maximum accepted byte length of an exported HTML path.
pub const MAX_HANDOFF_PATH_BYTES: usize = 4_096;
/// Maximum accepted byte length of a URL fragment.
pub const MAX_FRAGMENT_BYTES: usize = 256;

/// A validated local HTML handoff request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowserHandoff {
    path: String,
    fragment: Option<String>,
}

impl BrowserHandoff {
    /// Create a handoff for a rooted local HTML path.
    ///
    /// # Errors
    ///
    /// Returns a typed [`BrowserHandoffError`] if the path is empty, oversized,
    /// not rooted, contains a traversal component or control character, or does
    /// not end in `.html`/`.htm`.
    pub fn new(path: &str) -> Result<Self, BrowserHandoffError> {
        validate_rooted_path(path)?;
        Ok(Self {
            path: path.to_owned(),
            fragment: None,
        })
    }

    /// Return a copy that appends a validated URL fragment.
    ///
    /// # Errors
    ///
    /// Returns [`BrowserHandoffError::InvalidFragment`] or
    /// [`BrowserHandoffError::FragmentTooLong`] for a rejected fragment.
    pub fn with_fragment(mut self, fragment: &str) -> Result<Self, BrowserHandoffError> {
        validate_fragment(fragment)?;
        self.fragment = Some(fragment.to_owned());
        Ok(self)
    }

    /// Return the validated local path.
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Return the validated fragment, if any.
    pub fn fragment(&self) -> Option<&str> {
        self.fragment.as_deref()
    }

    /// Resolve a deterministic [`BrowserHandoffPlan`].
    pub fn resolve_plan(&self) -> BrowserHandoffPlan {
        let file_url = build_file_url(&self.path, self.fragment.as_deref());
        let mut summary = format!("scheme=file; path={}", self.path);
        if let Some(fragment) = self.fragment.as_deref() {
            summary.push_str(&format!("; fragment={fragment}"));
        } else {
            summary.push_str("; fragment=");
        }
        BrowserHandoffPlan { file_url, summary }
    }
}

/// The deterministic output of a validated handoff.
///
/// The plan carries only strings; the host decides which browser to launch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowserHandoffPlan {
    file_url: String,
    summary: String,
}

impl BrowserHandoffPlan {
    /// Return the built `file:` URL.
    pub fn file_url(&self) -> &str {
        &self.file_url
    }

    /// Return the byte-for-byte deterministic summary string.
    pub fn summary(&self) -> &str {
        &self.summary
    }
}

/// A small allowlist of MIME types a handoff request may declare.
///
/// Declaring a type is optional; when present it decides the open/download
/// outcome deterministically (see [`resolve_request`]). Unknown types are
/// rejected rather than passed to a browser.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandoffMime {
    /// HTML, which a browser can display directly.
    TextHtml,
    /// Plain text, handled as a download.
    TextPlain,
    /// PDF, handled as a download by this local contract.
    ApplicationPdf,
    /// An opaque local file, handled as a download.
    OctetStream,
}

impl HandoffMime {
    /// Return the canonical lowercase MIME token.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::TextHtml => "text/html",
            Self::TextPlain => "text/plain",
            Self::ApplicationPdf => "application/pdf",
            Self::OctetStream => "application/octet-stream",
        }
    }

    /// Parse a MIME token (case-insensitive) against the allowlist.
    ///
    /// # Errors
    ///
    /// Returns [`BrowserHandoffError::UnsupportedMime`] for a type outside the
    /// documented allowlist.
    pub fn parse(token: &str) -> Result<Self, BrowserHandoffError> {
        match token.trim().to_ascii_lowercase().as_str() {
            "text/html" | "text/html; charset=utf-8" => Ok(Self::TextHtml),
            "text/plain" => Ok(Self::TextPlain),
            "application/pdf" => Ok(Self::ApplicationPdf),
            "application/octet-stream" => Ok(Self::OctetStream),
            other => Err(BrowserHandoffError::UnsupportedMime(other.to_owned())),
        }
    }

    /// Return whether this type is displayed by the browser rather than saved.
    pub const fn is_displayable(self) -> bool {
        matches!(self, Self::TextHtml)
    }
}

/// The origin of a handoff request: a local path or a local `file:` URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HandoffSource {
    /// A caller-supplied rooted local path.
    Path(String),
    /// A caller-supplied `file:` URL that will be decoded to a local path.
    FileUrl(String),
}

/// A validated request to hand a local file to a browser.
///
/// Construct one with [`HandoffRequest::from_path`], [`HandoffRequest::from_file_url`]
/// or [`HandoffRequest::parse`], then call [`resolve_request`] for a
/// deterministic [`HandoffPlan`]. No network fetch or process spawn occurs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HandoffRequest {
    source: HandoffSource,
    mime: Option<HandoffMime>,
}

impl HandoffRequest {
    /// Create a request from a rooted local file path.
    ///
    /// # Errors
    ///
    /// Returns a typed [`BrowserHandoffError`] when the path is empty,
    /// oversized, not rooted, contains traversal or a control character.
    pub fn from_path(path: &str) -> Result<Self, BrowserHandoffError> {
        validate_local_file(path)?;
        Ok(Self {
            source: HandoffSource::Path(path.to_owned()),
            mime: None,
        })
    }

    /// Create a request from a `file:` URL.
    ///
    /// Only the `file` scheme is accepted; the URL is percent-decoded into a
    /// rooted local path and checked for traversal before the request is built.
    ///
    /// # Errors
    ///
    /// Returns [`BrowserHandoffError::UnsupportedScheme`] for a non-`file`
    /// scheme and other typed errors for a malformed or unsafe URL.
    pub fn from_file_url(url: &str) -> Result<Self, BrowserHandoffError> {
        let path = decode_file_url(url)?;
        Ok(Self {
            source: HandoffSource::FileUrl(path),
            mime: None,
        })
    }

    /// Parse a path or URL, detecting a scheme when one is present.
    ///
    /// A leading `file:` is accepted; any other scheme (for example `https:`)
    /// is rejected. A bare rooted path is treated as [`HandoffSource::Path`].
    ///
    /// # Errors
    ///
    /// Returns a typed [`BrowserHandoffError`] for an unsupported scheme or an
    /// invalid path/URL.
    pub fn parse(input: &str) -> Result<Self, BrowserHandoffError> {
        match split_scheme(input) {
            Some((scheme, _)) if scheme.eq_ignore_ascii_case("file") => Self::from_file_url(input),
            Some((scheme, _)) => Err(BrowserHandoffError::UnsupportedScheme(scheme.to_owned())),
            None => Self::from_path(input),
        }
    }

    /// Attach a validated MIME type to this request.
    ///
    /// # Errors
    ///
    /// Returns [`BrowserHandoffError::UnsupportedMime`] for an unknown type.
    pub fn with_mime(mut self, mime: &str) -> Result<Self, BrowserHandoffError> {
        self.mime = Some(HandoffMime::parse(mime)?);
        Ok(self)
    }

    /// Return the validated source of this request.
    pub fn source(&self) -> &HandoffSource {
        &self.source
    }

    /// Return the declared MIME type, if any.
    pub const fn mime(&self) -> Option<HandoffMime> {
        self.mime
    }
}

/// Whether a local file should be opened in the browser or saved locally.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandoffDecision {
    /// Display the HTML file directly in the browser.
    Open,
    /// Offer the file as a local download/save instead of displaying it.
    Download,
}

impl HandoffDecision {
    /// Return the stable lowercase decision token.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Download => "download",
        }
    }
}

/// The deterministic output of resolving a [`HandoffRequest`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HandoffPlan {
    path: String,
    file_url: String,
    decision: HandoffDecision,
    mime: Option<HandoffMime>,
    summary: String,
}

impl HandoffPlan {
    /// Return the resolved, validated local path.
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Return the deterministic `file:` URL for the resolved path.
    pub fn file_url(&self) -> &str {
        &self.file_url
    }

    /// Return the open/download decision for the host.
    pub const fn decision(&self) -> HandoffDecision {
        self.decision
    }

    /// Return the declared MIME type, if any.
    pub const fn mime(&self) -> Option<HandoffMime> {
        self.mime
    }

    /// Return the byte-for-byte deterministic summary string.
    pub fn summary(&self) -> &str {
        &self.summary
    }
}

/// Resolve a validated [`HandoffRequest`] into a deterministic [`HandoffPlan`].
///
/// The decision rule is: when a MIME type is declared, [`HandoffMime::TextHtml`]
/// opens and every other allowed type downloads; when none is declared, an
/// `.html`/`.htm` path opens and every other extension downloads. The `file:`
/// URL is rebuilt from the resolved path with the module's internal file-URL
/// builder, so callers always see the same URL for the same local target.
///
/// # Errors
///
/// Returns a typed [`BrowserHandoffError`] if the stored source fails local
/// validation at resolution time.
pub fn resolve_request(request: &HandoffRequest) -> Result<HandoffPlan, BrowserHandoffError> {
    let path = match &request.source {
        HandoffSource::Path(path) => {
            validate_local_file(path)?;
            path.clone()
        }
        HandoffSource::FileUrl(path) => {
            validate_local_file(path)?;
            path.clone()
        }
    };
    let lowered = path.to_ascii_lowercase();
    let html_extension = lowered.ends_with(".html") || lowered.ends_with(".htm");
    let decision = match request.mime {
        Some(mime) => {
            if mime.is_displayable() {
                HandoffDecision::Open
            } else {
                HandoffDecision::Download
            }
        }
        None => {
            if html_extension {
                HandoffDecision::Open
            } else {
                HandoffDecision::Download
            }
        }
    };
    let file_url = build_file_url(&path, None);
    let mime_token = request.mime.map_or("-", HandoffMime::as_str);
    let summary = format!(
        "scheme=file; path={path}; decision={}; mime={mime_token}",
        decision.as_str()
    );
    Ok(HandoffPlan {
        path,
        file_url,
        decision,
        mime: request.mime,
        summary,
    })
}

/// Validate a caller-supplied path as a rooted, traversal-free local file.
///
/// This is the general form used by [`HandoffRequest`]: unlike
/// [`validate_rooted_path`] it accepts any file extension because a request may
/// legitimately resolve to a download target. Rooting, traversal, control
/// characters and the byte limit are still enforced.
///
/// # Errors
///
/// Returns a typed [`BrowserHandoffError`] describing the first violation.
pub fn validate_local_file(path: &str) -> Result<(), BrowserHandoffError> {
    if path.is_empty() {
        return Err(BrowserHandoffError::EmptyPath);
    }
    if path.len() > MAX_HANDOFF_PATH_BYTES {
        return Err(BrowserHandoffError::PathTooLong(path.len()));
    }
    if path.chars().any(|c| c.is_control()) {
        return Err(BrowserHandoffError::ControlCharacter);
    }
    if !is_rooted(path) {
        return Err(BrowserHandoffError::NotRooted(path.to_owned()));
    }
    for component in path.split(['/', '\\']) {
        if component == ".." || component == "." {
            return Err(BrowserHandoffError::Traversal(path.to_owned()));
        }
    }
    Ok(())
}

/// Split an input into a scheme name and the remainder when a scheme is present.
///
/// A single alphabetic character before `:` is treated as a Windows drive, not
/// a scheme, so `C:\dir\file` is a path.
fn split_scheme(input: &str) -> Option<(&str, &str)> {
    let colon = input.find(':')?;
    let scheme = &input[..colon];
    let is_scheme = scheme.len() > 1
        && scheme
            .bytes()
            .next()
            .is_some_and(|first| first.is_ascii_alphabetic())
        && scheme
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'-' | b'.'));
    if is_scheme {
        Some((scheme, &input[colon.saturating_add(1)..]))
    } else {
        None
    }
}

/// Percent-decode a `file:` URL into UTF-8 bytes with a typed error.
fn percent_decode(input: &str) -> Result<Vec<u8>, BrowserHandoffError> {
    let bytes = input.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0usize;
    while index < bytes.len() {
        if bytes[index] == b'%'
            && let Some(high) = bytes.get(index.saturating_add(1))
            && let Some(low) = bytes.get(index.saturating_add(2))
        {
            let hex = |byte: &u8| -> Option<u8> {
                match byte {
                    b'0'..=b'9' => Some(byte.saturating_sub(b'0')),
                    b'a'..=b'f' => Some(byte.saturating_sub(b'a').saturating_add(10)),
                    b'A'..=b'F' => Some(byte.saturating_sub(b'A').saturating_add(10)),
                    _ => None,
                }
            };
            match (hex(high), hex(low)) {
                (Some(high), Some(low)) => {
                    decoded.push(high.saturating_mul(16).saturating_add(low));
                    index = index.saturating_add(3);
                    continue;
                }
                _ => return Err(BrowserHandoffError::InvalidFileUrl(input.to_owned())),
            }
        }
        decoded.push(bytes[index]);
        index = index.saturating_add(1);
    }
    Ok(decoded)
}

/// Decode a `file:` URL into a rooted local path, rejecting unsafe inputs.
fn decode_file_url(url: &str) -> Result<String, BrowserHandoffError> {
    if url.len() > MAX_HANDOFF_PATH_BYTES {
        return Err(BrowserHandoffError::PathTooLong(url.len()));
    }
    let (scheme, rest) =
        split_scheme(url).ok_or_else(|| BrowserHandoffError::InvalidFileUrl(url.to_owned()))?;
    if !scheme.eq_ignore_ascii_case("file") {
        return Err(BrowserHandoffError::UnsupportedScheme(scheme.to_owned()));
    }
    let after_slashes = rest
        .strip_prefix("//")
        .ok_or_else(|| BrowserHandoffError::InvalidFileUrl(url.to_owned()))?;
    if after_slashes.is_empty() {
        return Err(BrowserHandoffError::InvalidFileUrl(url.to_owned()));
    }
    let decoded_bytes = percent_decode(after_slashes)?;
    let decoded = String::from_utf8(decoded_bytes)
        .map_err(|_| BrowserHandoffError::InvalidFileUrl(url.to_owned()))?;
    if decoded.chars().any(|c| c.is_control()) {
        return Err(BrowserHandoffError::ControlCharacter);
    }
    let path = if let Some((authority, tail)) = decoded.split_once('/') {
        if authority.is_empty() {
            // `file:///C:/dir` (drive) or `file:///tmp/dir` (Unix root).
            let body = tail.trim_start_matches('/');
            let mut chars = body.chars();
            let is_drive = chars.next().is_some_and(|c| c.is_ascii_alphabetic())
                && matches!(chars.next(), Some(':'));
            if is_drive {
                let rest = body
                    .get(2..)
                    .unwrap_or("")
                    .trim_start_matches('/')
                    .replace('/', "\\");
                format!("{}:\\{}", &body[..1], rest)
            } else {
                format!("/{body}")
            }
        } else {
            // `file://server/share/dir` is a UNC path.
            let tail = tail.trim_start_matches('/');
            let body = format!("{authority}/{tail}");
            format!(r"\\{}", body.replace('/', "\\"))
        }
    } else {
        // A bare authority has no file component.
        return Err(BrowserHandoffError::InvalidFileUrl(url.to_owned()));
    };
    validate_local_file(&path)?;
    Ok(path)
}

/// Typed errors produced by browser-handoff validation.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum BrowserHandoffError {
    /// The path was empty.
    #[error("HTML handoff path must not be empty")]
    EmptyPath,
    /// The path exceeded the documented byte limit.
    #[error("HTML handoff path is {0} bytes (limit 4096)")]
    PathTooLong(usize),
    /// The path was not absolute.
    #[error("HTML handoff path must be rooted: {0}")]
    NotRooted(String),
    /// The path contained a `.` or `..` component.
    #[error("HTML handoff path contains a traversal component: {0}")]
    Traversal(String),
    /// The path contained a NUL or other control character.
    #[error("HTML handoff path contains a control character")]
    ControlCharacter,
    /// The path did not name an `.html` or `.htm` file.
    #[error("HTML handoff path must end in .html or .htm: {0}")]
    InvalidExtension(String),
    /// The fragment contained a forbidden character.
    #[error("invalid HTML handoff fragment: {0}")]
    InvalidFragment(String),
    /// The fragment exceeded the documented byte limit.
    #[error("HTML handoff fragment is {0} bytes (limit 256)")]
    FragmentTooLong(usize),
    /// A URL used a scheme other than `file`.
    #[error("unsupported handoff URL scheme: {0}")]
    UnsupportedScheme(String),
    /// A `file:` URL was structurally malformed or not valid UTF-8.
    #[error("invalid local handoff URL: {0}")]
    InvalidFileUrl(String),
    /// A declared MIME type was outside the documented allowlist.
    #[error("unsupported handoff MIME type: {0}")]
    UnsupportedMime(String),
}

/// Validate a caller-supplied path as a rooted, traversal-free HTML file.
///
/// Accepted roots are a Unix `/` path, a Windows drive such as `C:\\dir\\file`,
/// and a UNC path such as `\\\\server\\share\\file`. Separators may be either
/// `/` or `\\`. Any `.` or `..` component, control character, or non-HTML
/// extension is rejected.
///
/// # Errors
///
/// Returns a typed [`BrowserHandoffError`] describing the first violation.
pub fn validate_rooted_path(path: &str) -> Result<(), BrowserHandoffError> {
    if path.is_empty() {
        return Err(BrowserHandoffError::EmptyPath);
    }
    if path.len() > MAX_HANDOFF_PATH_BYTES {
        return Err(BrowserHandoffError::PathTooLong(path.len()));
    }
    if path.chars().any(|c| c.is_control()) {
        return Err(BrowserHandoffError::ControlCharacter);
    }
    if !is_rooted(path) {
        return Err(BrowserHandoffError::NotRooted(path.to_owned()));
    }
    for component in path.split(['/', '\\']) {
        if component == ".." || component == "." {
            return Err(BrowserHandoffError::Traversal(path.to_owned()));
        }
    }
    let lowered = path.to_ascii_lowercase();
    if !(lowered.ends_with(".html") || lowered.ends_with(".htm")) {
        return Err(BrowserHandoffError::InvalidExtension(path.to_owned()));
    }
    Ok(())
}

/// Return whether a path has an absolute root in any supported style.
fn is_rooted(path: &str) -> bool {
    if path.starts_with('/') || path.starts_with('\\') {
        return true;
    }
    let mut chars = path.chars();
    let first = match chars.next() {
        Some(value) => value,
        None => return false,
    };
    if first.is_ascii_alphabetic() {
        return matches!(chars.next(), Some(':') | Some('/') | Some('\\'));
    }
    false
}

/// Validate a URL fragment as a bounded string of safe ASCII characters.
fn validate_fragment(fragment: &str) -> Result<(), BrowserHandoffError> {
    if fragment.len() > MAX_FRAGMENT_BYTES {
        return Err(BrowserHandoffError::FragmentTooLong(fragment.len()));
    }
    if fragment.is_empty() {
        return Ok(());
    }
    let safe = fragment
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'/' | b':'));
    if safe {
        Ok(())
    } else {
        Err(BrowserHandoffError::InvalidFragment(fragment.to_owned()))
    }
}

/// Build a `file:` URL from a rooted path in a deterministic way.
fn build_file_url(path: &str, fragment: Option<&str>) -> String {
    let converted = path.replace('\\', "/");
    let is_unc = path.starts_with("\\\\") || converted.starts_with("//");
    let mut url = if is_unc {
        format!("file://{}", converted.trim_start_matches('/'))
    } else if converted.starts_with('/') {
        format!("file://{converted}")
    } else {
        // The only remaining validated root is a Windows drive, so the path
        // begins with `C:/` (or another drive letter) and needs `file:///`.
        format!("file:///{converted}")
    };
    if let Some(fragment) = fragment {
        url.push('#');
        url.push_str(fragment);
    }
    url
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rooted_windows_path_builds_a_deterministic_file_url() {
        let handoff = BrowserHandoff::new(r"C:\exports\report.html").expect("valid path");
        let plan = handoff.resolve_plan();
        assert_eq!(plan.file_url(), "file:///C:/exports/report.html");
        assert_eq!(plan, handoff.resolve_plan());
        assert_eq!(plan.summary(), handoff.resolve_plan().summary());
        assert!(plan.summary().contains("path=C:\\exports\\report.html"));
    }

    #[test]
    fn unix_and_unc_roots_are_accepted() {
        let unix = BrowserHandoff::new("/tmp/report.htm").expect("unix path");
        assert_eq!(unix.resolve_plan().file_url(), "file:///tmp/report.htm");
        let unc = BrowserHandoff::new(r"\\server\share\report.html").expect("unc path");
        assert_eq!(
            unc.resolve_plan().file_url(),
            "file://server/share/report.html"
        );
    }

    #[test]
    fn browser_handoff_rejects_relative_paths() {
        let err = BrowserHandoff::new("exports/report.html").expect_err("relative");
        assert!(matches!(err, BrowserHandoffError::NotRooted(_)));
        assert!(err.to_string().contains("must be rooted"));
    }

    #[test]
    fn browser_handoff_rejects_parent_traversal() {
        let err = BrowserHandoff::new(r"C:\exports\..\secret.html").expect_err("traversal");
        assert!(matches!(err, BrowserHandoffError::Traversal(_)));
        let err = BrowserHandoff::new(r"C:\exports\.\report.html").expect_err("dot component");
        assert!(matches!(err, BrowserHandoffError::Traversal(_)));
    }

    #[test]
    fn browser_handoff_rejects_bad_extension_and_control_chars() {
        assert!(matches!(
            BrowserHandoff::new(r"C:\exports\report.pdf"),
            Err(BrowserHandoffError::InvalidExtension(_))
        ));
        assert!(matches!(
            BrowserHandoff::new("C:\\exports\\report.html\0"),
            Err(BrowserHandoffError::ControlCharacter)
        ));
    }

    #[test]
    fn fragments_are_validated_and_included() {
        let handoff = BrowserHandoff::new(r"C:\exports\report.html")
            .expect("path")
            .with_fragment("page-2")
            .expect("fragment");
        assert_eq!(handoff.fragment(), Some("page-2"));
        assert_eq!(
            handoff.resolve_plan().file_url(),
            "file:///C:/exports/report.html#page-2"
        );
        assert!(matches!(
            BrowserHandoff::new(r"C:\exports\report.html")
                .expect("path")
                .with_fragment("bad frag"),
            Err(BrowserHandoffError::InvalidFragment(_))
        ));
    }

    #[test]
    fn browser_handoff_typed_errors_are_distinct() {
        let a = BrowserHandoffError::EmptyPath;
        let b = BrowserHandoffError::PathTooLong(9000);
        assert_ne!(a, b);
        assert!(b.to_string().contains("9000"));
        assert!(
            BrowserHandoffError::ControlCharacter
                .to_string()
                .contains("control character")
        );
    }

    // ---- T-0181 PLAT-004 request parsing, resolution and decision plan ----

    #[test]
    fn request_from_path_builds_a_deterministic_open_plan() {
        let request = HandoffRequest::from_path(r"C:\exports\report.html").expect("request");
        assert_eq!(
            request.source(),
            &HandoffSource::Path(r"C:\exports\report.html".to_owned())
        );
        let plan = resolve_request(&request).expect("plan");
        assert_eq!(plan, resolve_request(&request).expect("plan again"));
        assert_eq!(plan.decision(), HandoffDecision::Open);
        assert_eq!(plan.file_url(), "file:///C:/exports/report.html");
        assert_eq!(plan.path(), r"C:\exports\report.html");
        assert!(plan.summary().contains("decision=open"));
        assert!(plan.summary().ends_with("mime=-"));
    }

    #[test]
    fn non_html_path_defaults_to_download() {
        let request = HandoffRequest::from_path(r"C:\exports\report.pdf").expect("request");
        let plan = resolve_request(&request).expect("plan");
        assert_eq!(plan.decision(), HandoffDecision::Download);
        assert_eq!(plan.decision().as_str(), "download");
        assert!(plan.summary().contains("decision=download"));
    }

    #[test]
    fn request_parses_file_urls_and_rejects_other_schemes() {
        let url = HandoffRequest::parse("file:///C:/exports/report.html").expect("file url");
        let plan = resolve_request(&url).expect("plan");
        assert_eq!(plan.path(), r"C:\exports\report.html");
        assert_eq!(plan.decision(), HandoffDecision::Open);

        let unc = HandoffRequest::parse("file://server/share/report.html").expect("unc url");
        assert_eq!(
            resolve_request(&unc).expect("plan").path(),
            r"\\server\share\report.html"
        );

        let unix = HandoffRequest::parse("file:///tmp/report.htm").expect("unix url");
        assert_eq!(
            resolve_request(&unix).expect("plan").path(),
            "/tmp/report.htm"
        );

        assert!(matches!(
            HandoffRequest::parse("https://example.com/report.html"),
            Err(BrowserHandoffError::UnsupportedScheme(_))
        ));
    }

    #[test]
    fn percent_encoded_traversal_is_rejected() {
        // `%2e%2e` decodes to `..`, which the local-path validator rejects.
        assert!(matches!(
            HandoffRequest::from_file_url("file:///C:/exports/%2e%2e/secret.html"),
            Err(BrowserHandoffError::Traversal(_))
        ));
        assert!(matches!(
            HandoffRequest::from_file_url("file:///C:/bad%00name.html"),
            Err(BrowserHandoffError::ControlCharacter)
        ));
        assert!(matches!(
            HandoffRequest::from_file_url("file:///C:/bad%ZZname.html"),
            Err(BrowserHandoffError::InvalidFileUrl(_))
        ));
    }

    #[test]
    fn mime_allowlist_controls_the_decision_deterministically() {
        let html = HandoffRequest::from_path(r"C:\exports\report.bin")
            .expect("request")
            .with_mime("TEXT/HTML")
            .expect("mime");
        assert_eq!(html.mime(), Some(HandoffMime::TextHtml));
        assert_eq!(html.mime().map_or("-", HandoffMime::as_str), "text/html");
        assert_eq!(
            resolve_request(&html).expect("plan").decision(),
            HandoffDecision::Open
        );

        let pdf = HandoffRequest::from_path(r"C:\exports\report.html")
            .expect("request")
            .with_mime("application/pdf")
            .expect("mime");
        assert_eq!(
            resolve_request(&pdf).expect("plan").decision(),
            HandoffDecision::Download
        );
        assert!(HandoffMime::TextHtml.is_displayable());
        assert!(!HandoffMime::ApplicationPdf.is_displayable());
        assert!(matches!(
            HandoffRequest::from_path(r"C:\a.html")
                .expect("request")
                .with_mime("application/x-msdownload"),
            Err(BrowserHandoffError::UnsupportedMime(_))
        ));
    }
}
