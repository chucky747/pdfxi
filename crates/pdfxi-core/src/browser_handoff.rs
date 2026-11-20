//! Local "open in browser" handoff for exported HTML (PLAT-004).
//!
//! This module models the handoff of a locally exported HTML file to a browser.
//! It performs **no network access and no process spawning**: it validates the
//! caller-supplied path, builds a deterministic `file:` URL and a stable plan
//! summary, and leaves the actual launch to the host. ADR 0008 requires local,
//! file-based equivalents; this is the browser-integration equivalent.
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
}
