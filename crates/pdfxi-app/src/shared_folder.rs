//! Local "shared folder" source model (ADR 0008 replacement for network/SharePoint integration).
//!
//! ADR 0008 replaces retired cloud/host integrations with local, file-based
//! equivalents. This module is the local stand-in for a SharePoint-style shared
//! document library: a [`SharedFolderSource`] describes an existing local root
//! path and a [`SharedFolderListingPlan`] tells the host exactly what to
//! enumerate. The module performs **no** filesystem access, no credential
//! handling, and no network I/O; validation is purely on caller-supplied path
//! strings so hostile input cannot escape the declared root via traversal or
//! network prefixes.
//!
//! # Determinism
//!
//! A listing plan records a normalized root, a recursion flag, a bounded entry
//! cap, and a sorted/deduplicated extension filter. [`SharedFolderListingPlan::serialize`]
//! is stable for a given source, so the plan can be persisted or compared.

/// Maximum number of characters in a shared-folder root path.
pub const MAX_SHARED_FOLDER_ROOT_LEN: usize = 4096;
/// Maximum number of extension filters on a shared folder.
pub const MAX_SHARED_FOLDER_EXTENSIONS: usize = 64;
/// Default maximum number of entries the host should enumerate.
pub const DEFAULT_SHARED_FOLDER_MAX_ENTRIES: usize = 8192;

/// Typed error for a shared-folder source.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SharedFolderError {
    /// The root path was empty.
    #[error("shared folder root must not be empty")]
    EmptyRoot,
    /// The root path exceeded [`MAX_SHARED_FOLDER_ROOT_LEN`].
    #[error("shared folder root is too long")]
    RootTooLong,
    /// The root contained control characters or an embedded null byte.
    #[error("shared folder root contains an invalid character")]
    InvalidCharacter,
    /// The root looked like a UNC share, URI, or other network path.
    #[error("network or URI paths are not allowed for a local shared folder")]
    NetworkPathRejected,
    /// The root or a child path contained a parent-traversal component.
    #[error("shared folder path contains a parent-traversal component")]
    TraversalPath,
    /// The root was otherwise malformed (for example a doubled separator).
    #[error("invalid shared folder root")]
    InvalidRoot,
    /// A child path was invalid.
    #[error("invalid shared folder child path")]
    InvalidChild,
}

/// A validated relative path inside a [`SharedFolderSource`].
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct SharedFolderPath {
    relative: String,
}

impl SharedFolderPath {
    /// The normalized slash-separated relative path.
    pub fn as_str(&self) -> &str {
        &self.relative
    }
}

/// Validate a root path and return it unchanged (validation only, no I/O).
fn validate_root(root: &str) -> Result<(), SharedFolderError> {
    if root.is_empty() {
        return Err(SharedFolderError::EmptyRoot);
    }
    if root.chars().count() > MAX_SHARED_FOLDER_ROOT_LEN {
        return Err(SharedFolderError::RootTooLong);
    }
    if root.chars().any(char::is_control) {
        return Err(SharedFolderError::InvalidCharacter);
    }
    if root.starts_with("\\\\") || root.starts_with("//") || root.contains("://") {
        return Err(SharedFolderError::NetworkPathRejected);
    }
    if let Some(colon) = root.find(':') {
        let prefix = &root[..colon];
        // A single ASCII letter before ':' is a Windows drive prefix and is
        // allowed; any other scheme-style prefix is a URI and is rejected.
        if prefix.len() != 1 || !prefix.chars().all(|c| c.is_ascii_alphabetic()) {
            return Err(SharedFolderError::NetworkPathRejected);
        }
    }
    let parts: Vec<&str> = root.split(['/', '\\']).collect();
    for (index, component) in parts.iter().enumerate() {
        if *component == ".." {
            return Err(SharedFolderError::TraversalPath);
        }
        if component.is_empty() {
            // A single leading separator denotes an absolute POSIX (or current
            // drive) path and a trailing separator denotes a directory; only
            // empty components in the middle (doubled separators) are invalid.
            let leading = index == 0 && (root.starts_with('/') || root.starts_with('\\'));
            let trailing = index + 1 == parts.len();
            if !leading && !trailing {
                return Err(SharedFolderError::InvalidRoot);
            }
        }
    }
    Ok(())
}

/// A local shared folder source with traversal-free path validation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SharedFolderSource {
    root: String,
    recursive: bool,
    max_entries: usize,
    extensions: Vec<String>,
}

impl SharedFolderSource {
    /// Create a source from a caller-supplied local root path.
    ///
    /// Absolute local paths (for example `C:\\Shared` or `/srv/shared`) and
    /// relative paths are both accepted. UNC shares, URIs, `..` traversal,
    /// doubled separators, and control characters are rejected with typed
    /// errors. No filesystem access is performed.
    pub fn new(root: impl Into<String>) -> Result<Self, SharedFolderError> {
        let root = root.into();
        validate_root(&root)?;
        Ok(Self {
            root,
            recursive: true,
            max_entries: DEFAULT_SHARED_FOLDER_MAX_ENTRIES,
            extensions: Vec::new(),
        })
    }

    /// The validated root path.
    pub fn root(&self) -> &str {
        &self.root
    }

    /// Whether the host should enumerate recursively.
    pub fn recursive(&self) -> bool {
        self.recursive
    }

    /// Set recursive enumeration.
    pub fn with_recursive(mut self, recursive: bool) -> Self {
        self.recursive = recursive;
        self
    }

    /// The bounded entry cap the host should honor.
    pub fn max_entries(&self) -> usize {
        self.max_entries
    }

    /// Set the entry cap (zero is kept as a legal "enumerate nothing" value).
    pub fn with_max_entries(mut self, max_entries: usize) -> Self {
        self.max_entries = max_entries;
        self
    }

    /// The sorted, deduplicated extension filter (empty means no filter).
    pub fn extensions(&self) -> &[String] {
        &self.extensions
    }

    /// Add an extension filter (without a leading dot, case-insensitive).
    ///
    /// Filters are normalized to lowercase, deduplicated, sorted, and capped
    /// at [`MAX_SHARED_FOLDER_EXTENSIONS`]; excess entries are ignored so the
    /// plan stays bounded and deterministic.
    pub fn with_extension(mut self, extension: &str) -> Self {
        let normalized = extension.trim().trim_start_matches('.').to_lowercase();
        if normalized.is_empty() || self.extensions.contains(&normalized) {
            return self;
        }
        if self.extensions.len() >= MAX_SHARED_FOLDER_EXTENSIONS {
            return self;
        }
        self.extensions.push(normalized);
        self.extensions.sort();
        self
    }

    /// Validate a caller-supplied relative child path against this source.
    ///
    /// The child must be relative and traversal-free; separators are
    /// normalized to `/`. Absolute paths, drive prefixes, `.`/`..` components,
    /// empty components, and control characters are rejected.
    pub fn validate_child(&self, child: &str) -> Result<SharedFolderPath, SharedFolderError> {
        if child.is_empty() {
            return Err(SharedFolderError::InvalidChild);
        }
        if child.chars().any(char::is_control) {
            return Err(SharedFolderError::InvalidChild);
        }
        if child.starts_with('/') || child.starts_with('\\') {
            return Err(SharedFolderError::InvalidChild);
        }
        if let Some(colon) = child.find(':') {
            let prefix = &child[..colon];
            if prefix.len() == 1 && prefix.chars().all(|c| c.is_ascii_alphabetic()) {
                return Err(SharedFolderError::InvalidChild);
            }
            let _ = prefix;
            return Err(SharedFolderError::InvalidChild);
        }
        let mut normalized = Vec::new();
        for component in child.split(['/', '\\']) {
            if component == ".." {
                return Err(SharedFolderError::TraversalPath);
            }
            if component.is_empty() || component == "." {
                return Err(SharedFolderError::InvalidChild);
            }
            normalized.push(component);
        }
        Ok(SharedFolderPath {
            relative: normalized.join("/"),
        })
    }

    /// Produce the deterministic enumeration plan for the host.
    pub fn listing_plan(&self) -> SharedFolderListingPlan {
        SharedFolderListingPlan {
            root: self.root.clone(),
            recursive: self.recursive,
            max_entries: self.max_entries,
            extensions: self.extensions.clone(),
        }
    }
}

/// A deterministic plan describing what the host should enumerate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SharedFolderListingPlan {
    root: String,
    recursive: bool,
    max_entries: usize,
    extensions: Vec<String>,
}

impl SharedFolderListingPlan {
    /// The local root the host should enumerate.
    pub fn root(&self) -> &str {
        &self.root
    }

    /// Whether enumeration is recursive.
    pub fn recursive(&self) -> bool {
        self.recursive
    }

    /// The maximum number of entries the host should return.
    pub fn max_entries(&self) -> usize {
        self.max_entries
    }

    /// The sorted extension filter.
    pub fn extensions(&self) -> &[String] {
        &self.extensions
    }

    /// Render the plan as a stable, escaped string for persistence or logs.
    pub fn serialize(&self) -> String {
        let extensions = self.extensions.join(",");
        format!(
            "ROOT|{}\nRECURSIVE|{}\nMAX|{}\nEXT|{}",
            escape_field(&self.root),
            if self.recursive { "1" } else { "0" },
            self.max_entries,
            extensions,
        )
    }
}

fn escape_field(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '|' => out.push_str("\\|"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            _ => out.push(ch),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_local_roots_are_accepted() {
        assert!(SharedFolderSource::new("C:\\Shared\\Docs").is_ok());
        assert!(SharedFolderSource::new("/srv/shared").is_ok());
        assert!(SharedFolderSource::new("shared-relative").is_ok());
        let source = SharedFolderSource::new("C:\\Shared").unwrap_or_else(|_| panic!("root"));
        assert_eq!(source.root(), "C:\\Shared");
        assert!(source.recursive());
        assert_eq!(source.max_entries(), DEFAULT_SHARED_FOLDER_MAX_ENTRIES);
    }

    #[test]
    fn traversal_and_network_roots_are_rejected() {
        assert_eq!(
            SharedFolderSource::new("../escape").unwrap_err(),
            SharedFolderError::TraversalPath
        );
        assert_eq!(
            SharedFolderSource::new("C:\\a\\..\\b").unwrap_err(),
            SharedFolderError::TraversalPath
        );
        assert_eq!(
            SharedFolderSource::new("\\\\server\\share").unwrap_err(),
            SharedFolderError::NetworkPathRejected
        );
        assert_eq!(
            SharedFolderSource::new("http://example.com/share").unwrap_err(),
            SharedFolderError::NetworkPathRejected
        );
        assert_eq!(
            SharedFolderSource::new("file:///srv/share").unwrap_err(),
            SharedFolderError::NetworkPathRejected
        );
    }

    #[test]
    fn malformed_roots_are_rejected() {
        assert_eq!(
            SharedFolderSource::new("").unwrap_err(),
            SharedFolderError::EmptyRoot
        );
        assert_eq!(
            SharedFolderSource::new("C:\\a\\\\b").unwrap_err(),
            SharedFolderError::InvalidRoot
        );
        assert_eq!(
            SharedFolderSource::new("C:\\a\0b").unwrap_err(),
            SharedFolderError::InvalidCharacter
        );
        let long = "a".repeat(MAX_SHARED_FOLDER_ROOT_LEN + 1);
        assert_eq!(
            SharedFolderSource::new(long).unwrap_err(),
            SharedFolderError::RootTooLong
        );
    }

    #[test]
    fn child_path_traversal_and_absolute_are_rejected() {
        let source = SharedFolderSource::new("C:\\Shared").unwrap_or_else(|_| panic!("root"));
        assert_eq!(
            source.validate_child("a/../../b").unwrap_err(),
            SharedFolderError::TraversalPath
        );
        assert!(matches!(
            source.validate_child("/abs"),
            Err(SharedFolderError::InvalidChild)
        ));
        assert!(matches!(
            source.validate_child("C:\\abs"),
            Err(SharedFolderError::InvalidChild)
        ));
        assert!(matches!(
            source.validate_child("a//b"),
            Err(SharedFolderError::InvalidChild)
        ));
        let child = source
            .validate_child("sub\\file.pdf")
            .unwrap_or_else(|_| panic!("child"));
        assert_eq!(child.as_str(), "sub/file.pdf");
    }

    #[test]
    fn listing_plan_is_deterministic_and_extensions_are_sorted() {
        let source = SharedFolderSource::new("C:\\Shared")
            .unwrap_or_else(|_| panic!("root"))
            .with_extension("PDF")
            .with_extension("txt")
            .with_extension(".pdf")
            .with_extension("")
            .with_recursive(false)
            .with_max_entries(10);
        let plan = source.listing_plan();
        assert_eq!(plan.extensions(), &["pdf".to_string(), "txt".to_string()]);
        assert!(!plan.recursive());
        assert_eq!(plan.max_entries(), 10);
        assert_eq!(plan, source.listing_plan());
        let serialized = plan.serialize();
        assert_eq!(serialized, source.listing_plan().serialize());
        assert_eq!(
            serialized,
            "ROOT|C:\\\\Shared\nRECURSIVE|0\nMAX|10\nEXT|pdf,txt"
        );
    }

    #[test]
    fn extension_cap_is_bounded() {
        let mut source = SharedFolderSource::new("C:\\Shared").unwrap_or_else(|_| panic!("root"));
        for i in 0..MAX_SHARED_FOLDER_EXTENSIONS + 10 {
            source = source.with_extension(&format!("ext{i:03}"));
        }
        assert_eq!(source.extensions().len(), MAX_SHARED_FOLDER_EXTENSIONS);
    }
}
