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
/// Maximum number of characters in a revision token.
pub const MAX_REVISION_LEN: usize = 128;
/// Maximum number of items in a workspace sync descriptor.
pub const MAX_SYNC_ITEMS: usize = 4096;

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
    /// A child path was absolute or drive-prefixed and left the shared root.
    #[error("absolute child path escapes the shared folder root")]
    AbsoluteOutsideRoot,
    /// A child path's leaf name was unsafe on a local filesystem.
    #[error("unsafe shared folder item name: {0}")]
    UnsafeName(String),
    /// An item appeared more than once in a workspace descriptor.
    #[error("duplicate shared folder item: {0}")]
    DuplicateItem(String),
    /// A revision token was empty, over-long, or contained unsafe characters.
    #[error("invalid shared folder revision token")]
    InvalidRevision,
    /// Two descriptors were built over different roots and cannot be compared.
    #[error("shared folder descriptors have different roots")]
    RootMismatch,
    /// A workspace descriptor carried too many items.
    #[error("workspace descriptor item cap of {0} reached")]
    TooManyItems(usize),
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
            return Err(SharedFolderError::AbsoluteOutsideRoot);
        }
        if child.contains(':') {
            return Err(SharedFolderError::AbsoluteOutsideRoot);
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

// ---------------------------------------------------------------------------
// Workspace / sync descriptor model (local ADR 0008 equivalent)
// ---------------------------------------------------------------------------

use std::cmp::Ordering;

/// One tracked item in a local workspace descriptor.
///
/// Items are keyed by a validated relative path inside a
/// [`SharedFolderSource`] root and carry an opaque revision token plus a byte
/// size. No file bytes are read or stored.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct SyncItem {
    path: SharedFolderPath,
    revision: String,
    size: u64,
}

impl SyncItem {
    /// Validate and create a tracked item.
    ///
    /// The relative path must stay inside the source root, its leaf name must be
    /// safe on a local filesystem, and the revision token must be a bounded,
    /// non-empty ASCII token. All failures are typed [`SharedFolderError`]s.
    pub fn new(
        source: &SharedFolderSource,
        relative: &str,
        revision: impl Into<String>,
        size: u64,
    ) -> Result<Self, SharedFolderError> {
        let path = source.validate_child(relative)?;
        let leaf = path.as_str().rsplit('/').next().unwrap_or(path.as_str());
        validate_item_name(leaf)?;
        let revision = revision.into();
        if revision.is_empty()
            || revision.chars().count() > MAX_REVISION_LEN
            || !revision
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
        {
            return Err(SharedFolderError::InvalidRevision);
        }
        Ok(Self {
            path,
            revision,
            size,
        })
    }

    /// Validated relative path of the item.
    pub fn path(&self) -> &SharedFolderPath {
        &self.path
    }

    /// Opaque revision token for conflict detection.
    pub fn revision(&self) -> &str {
        &self.revision
    }

    /// Caller-reported byte size of the item.
    pub fn size(&self) -> u64 {
        self.size
    }
}

/// Reject leaf names that are unsafe on common local filesystems.
fn validate_item_name(name: &str) -> Result<(), SharedFolderError> {
    let unsafe_name = || SharedFolderError::UnsafeName(name.to_owned());
    if name.is_empty() || name == "." || name == ".." || name.trim() != name {
        return Err(unsafe_name());
    }
    if name
        .chars()
        .any(|ch| ch.is_control() || matches!(ch, '<' | '>' | ':' | '"' | '|' | '?' | '*'))
    {
        return Err(unsafe_name());
    }
    let upper = name.to_ascii_uppercase();
    let stem = upper.split('.').next().unwrap_or("");
    let prefix = stem.get(..3).unwrap_or("");
    let reserved = matches!(stem, "CON" | "PRN" | "AUX" | "NUL")
        || (matches!(prefix, "COM" | "LPT")
            && stem.len() == 4
            && matches!(stem.as_bytes().get(3), Some(b'1'..=b'9')));
    if reserved {
        return Err(unsafe_name());
    }
    Ok(())
}

/// A validated, caller-supplied local workspace descriptor.
///
/// The descriptor holds a share root plus a sorted, duplicate-free item list.
/// It performs no filesystem access: a host enumerates the root and supplies
/// the items. [`SyncPlan::build`] compares two descriptors deterministically.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceDescriptor {
    root: String,
    items: Vec<SyncItem>,
}

impl WorkspaceDescriptor {
    /// Create an empty descriptor for a validated source root.
    pub fn new(source: &SharedFolderSource) -> Self {
        Self {
            root: source.root().to_owned(),
            items: Vec::new(),
        }
    }

    /// The share root this descriptor describes.
    pub fn root(&self) -> &str {
        &self.root
    }

    /// Items in stable path order.
    pub fn items(&self) -> &[SyncItem] {
        &self.items
    }

    /// Insert an item, rejecting duplicates and enforcing the item cap.
    pub fn with_item(mut self, item: SyncItem) -> Result<Self, SharedFolderError> {
        if self.items.len() >= MAX_SYNC_ITEMS {
            return Err(SharedFolderError::TooManyItems(MAX_SYNC_ITEMS));
        }
        match self.items.binary_search(&item) {
            Ok(_) => Err(SharedFolderError::DuplicateItem(
                item.path().as_str().to_owned(),
            )),
            Err(position) => {
                self.items.insert(position, item);
                Ok(self)
            }
        }
    }
}

/// How two revisions of the same item relate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConflictKind {
    /// Both sides have the item but their revisions differ.
    RevisionMismatch,
    /// The item exists only in the local descriptor.
    LocalOnly,
    /// The item exists only in the remote descriptor.
    RemoteOnly,
}

/// One deterministic conflict between two workspace descriptors.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SyncConflict {
    /// Relative path of the conflicting item.
    pub path: String,
    /// Kind of conflict.
    pub kind: ConflictKind,
    /// Local revision, if the item exists locally.
    pub local_revision: Option<String>,
    /// Remote revision, if the item exists remotely.
    pub remote_revision: Option<String>,
}

/// A stable, ordered report of workspace conflicts.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ConflictReport {
    conflicts: Vec<SyncConflict>,
}

impl ConflictReport {
    /// Conflicts in ascending relative-path order.
    pub fn conflicts(&self) -> &[SyncConflict] {
        &self.conflicts
    }

    /// Number of conflicts.
    pub fn len(&self) -> usize {
        self.conflicts.len()
    }

    /// Whether no conflicts were found.
    pub fn is_empty(&self) -> bool {
        self.conflicts.is_empty()
    }

    /// Render the report as stable, escaped lines.
    pub fn serialize(&self) -> String {
        self.conflicts
            .iter()
            .map(|conflict| {
                format!(
                    "CONFLICT|{}|{}|{}|{}",
                    conflict_kind_label(conflict.kind),
                    escape_field(&conflict.path),
                    conflict.local_revision.as_deref().unwrap_or(""),
                    conflict.remote_revision.as_deref().unwrap_or(""),
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// Detect conflicts between two descriptors over the same root.
///
/// Matching revisions are not conflicts; differing revisions, local-only items,
/// and remote-only items are reported in ascending path order. Comparing
/// descriptors over different roots is a typed [`SharedFolderError::RootMismatch`].
pub fn detect_conflicts(
    local: &WorkspaceDescriptor,
    remote: &WorkspaceDescriptor,
) -> Result<ConflictReport, SharedFolderError> {
    require_same_root(local, remote)?;
    let mut conflicts = Vec::new();
    let (mut i, mut j) = (0usize, 0usize);
    while i < local.items.len() && j < remote.items.len() {
        let left = &local.items[i];
        let right = &remote.items[j];
        match left.path().as_str().cmp(right.path().as_str()) {
            Ordering::Equal => {
                if left.revision() != right.revision() {
                    conflicts.push(SyncConflict {
                        path: left.path().as_str().to_owned(),
                        kind: ConflictKind::RevisionMismatch,
                        local_revision: Some(left.revision().to_owned()),
                        remote_revision: Some(right.revision().to_owned()),
                    });
                }
                i += 1;
                j += 1;
            }
            Ordering::Less => {
                conflicts.push(SyncConflict {
                    path: left.path().as_str().to_owned(),
                    kind: ConflictKind::LocalOnly,
                    local_revision: Some(left.revision().to_owned()),
                    remote_revision: None,
                });
                i += 1;
            }
            Ordering::Greater => {
                conflicts.push(SyncConflict {
                    path: right.path().as_str().to_owned(),
                    kind: ConflictKind::RemoteOnly,
                    local_revision: None,
                    remote_revision: Some(right.revision().to_owned()),
                });
                j += 1;
            }
        }
    }
    for item in &local.items[i..] {
        conflicts.push(SyncConflict {
            path: item.path().as_str().to_owned(),
            kind: ConflictKind::LocalOnly,
            local_revision: Some(item.revision().to_owned()),
            remote_revision: None,
        });
    }
    for item in &remote.items[j..] {
        conflicts.push(SyncConflict {
            path: item.path().as_str().to_owned(),
            kind: ConflictKind::RemoteOnly,
            local_revision: None,
            remote_revision: Some(item.revision().to_owned()),
        });
    }
    Ok(ConflictReport { conflicts })
}

/// The action a host should take for one item in a sync plan.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SyncAction {
    /// Send the local item to the shared root.
    Upload,
    /// Fetch the remote item into the local workspace.
    Download,
    /// Both sides agree; no transfer is needed.
    Keep,
    /// Revisions disagree; a host policy must resolve the conflict.
    Resolve,
}

fn action_label(action: SyncAction) -> &'static str {
    match action {
        SyncAction::Upload => "UPLOAD",
        SyncAction::Download => "DOWNLOAD",
        SyncAction::Keep => "KEEP",
        SyncAction::Resolve => "RESOLVE",
    }
}

fn conflict_kind_label(kind: ConflictKind) -> &'static str {
    match kind {
        ConflictKind::RevisionMismatch => "REVISION",
        ConflictKind::LocalOnly => "LOCAL",
        ConflictKind::RemoteOnly => "REMOTE",
    }
}

/// One ordered step of a deterministic sync plan.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SyncStep {
    /// Relative path of the item.
    pub path: String,
    /// Action to take.
    pub action: SyncAction,
    /// Revision token carried by the step.
    pub revision: String,
}

/// A deterministic, host-applicable sync plan over two workspace descriptors.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SyncPlan {
    root: String,
    steps: Vec<SyncStep>,
}

impl SyncPlan {
    /// Build a plan with one step per item, in ascending path order.
    pub fn build(
        local: &WorkspaceDescriptor,
        remote: &WorkspaceDescriptor,
    ) -> Result<Self, SharedFolderError> {
        require_same_root(local, remote)?;
        let mut steps = Vec::with_capacity(local.items.len() + remote.items.len());
        let (mut i, mut j) = (0usize, 0usize);
        while i < local.items.len() && j < remote.items.len() {
            let left = &local.items[i];
            let right = &remote.items[j];
            match left.path().as_str().cmp(right.path().as_str()) {
                Ordering::Equal => {
                    steps.push(SyncStep {
                        path: left.path().as_str().to_owned(),
                        action: if left.revision() == right.revision() {
                            SyncAction::Keep
                        } else {
                            SyncAction::Resolve
                        },
                        revision: left.revision().to_owned(),
                    });
                    i += 1;
                    j += 1;
                }
                Ordering::Less => {
                    steps.push(SyncStep {
                        path: left.path().as_str().to_owned(),
                        action: SyncAction::Upload,
                        revision: left.revision().to_owned(),
                    });
                    i += 1;
                }
                Ordering::Greater => {
                    steps.push(SyncStep {
                        path: right.path().as_str().to_owned(),
                        action: SyncAction::Download,
                        revision: right.revision().to_owned(),
                    });
                    j += 1;
                }
            }
        }
        for item in &local.items[i..] {
            steps.push(SyncStep {
                path: item.path().as_str().to_owned(),
                action: SyncAction::Upload,
                revision: item.revision().to_owned(),
            });
        }
        for item in &remote.items[j..] {
            steps.push(SyncStep {
                path: item.path().as_str().to_owned(),
                action: SyncAction::Download,
                revision: item.revision().to_owned(),
            });
        }
        Ok(Self {
            root: local.root.clone(),
            steps,
        })
    }

    /// Share root the plan applies to.
    pub fn root(&self) -> &str {
        &self.root
    }

    /// Ordered steps the host should apply.
    pub fn steps(&self) -> &[SyncStep] {
        &self.steps
    }

    /// Render the plan as a stable, escaped descriptor.
    pub fn serialize(&self) -> String {
        let mut lines = vec![format!("SYNC|{}", escape_field(&self.root))];
        lines.extend(self.steps.iter().map(|step| {
            format!(
                "STEP|{}|{}|{}",
                action_label(step.action),
                escape_field(&step.path),
                escape_field(&step.revision)
            )
        }));
        lines.join("\n")
    }
}

fn require_same_root(
    local: &WorkspaceDescriptor,
    remote: &WorkspaceDescriptor,
) -> Result<(), SharedFolderError> {
    if local.root == remote.root {
        Ok(())
    } else {
        Err(SharedFolderError::RootMismatch)
    }
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
            Err(SharedFolderError::AbsoluteOutsideRoot)
        ));
        assert!(matches!(
            source.validate_child("C:\\abs"),
            Err(SharedFolderError::AbsoluteOutsideRoot)
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

    fn source() -> SharedFolderSource {
        SharedFolderSource::new("C:\\Shared").unwrap_or_else(|_| panic!("root"))
    }

    #[test]
    fn sync_items_validate_paths_names_and_revisions() {
        let source = source();
        let item = SyncItem::new(&source, "sub\\file.pdf", "rev-1.2", 42)
            .unwrap_or_else(|_| panic!("item"));
        assert_eq!(item.path().as_str(), "sub/file.pdf");
        assert_eq!(item.revision(), "rev-1.2");
        assert_eq!(item.size(), 42);
        assert_eq!(
            SyncItem::new(&source, "/abs", "r1", 1),
            Err(SharedFolderError::AbsoluteOutsideRoot)
        );
        assert_eq!(
            SyncItem::new(&source, "a/../../b", "r1", 1),
            Err(SharedFolderError::TraversalPath)
        );
        assert_eq!(
            SyncItem::new(&source, "CON.txt", "r1", 1),
            Err(SharedFolderError::UnsafeName("CON.txt".to_owned()))
        );
        assert_eq!(
            SyncItem::new(&source, "bad|name", "r1", 1),
            Err(SharedFolderError::UnsafeName("bad|name".to_owned()))
        );
        assert_eq!(
            SyncItem::new(&source, "trailing ", "r1", 1),
            Err(SharedFolderError::UnsafeName("trailing ".to_owned()))
        );
        assert_eq!(
            SyncItem::new(&source, "file.txt", "", 1),
            Err(SharedFolderError::InvalidRevision)
        );
        let long_revision = "a".repeat(MAX_REVISION_LEN + 1);
        assert_eq!(
            SyncItem::new(&source, "file.txt", long_revision, 1),
            Err(SharedFolderError::InvalidRevision)
        );
    }

    #[test]
    fn workspace_descriptor_rejects_duplicates_and_enforces_cap() {
        let source = source();
        let item = SyncItem::new(&source, "b.txt", "r1", 1).unwrap_or_else(|_| panic!("item"));
        let mut descriptor = WorkspaceDescriptor::new(&source)
            .with_item(item.clone())
            .expect("add");
        assert_eq!(descriptor.root(), "C:\\Shared");
        assert_eq!(descriptor.items().len(), 1);
        assert_eq!(
            descriptor.clone().with_item(item),
            Err(SharedFolderError::DuplicateItem("b.txt".to_owned()))
        );
        let a = SyncItem::new(&source, "a.txt", "r1", 1).unwrap_or_else(|_| panic!("a"));
        descriptor = descriptor.with_item(a).expect("add a");
        assert_eq!(descriptor.items()[0].path().as_str(), "a.txt");
        assert_eq!(descriptor.items()[1].path().as_str(), "b.txt");
        let other = SharedFolderSource::new("C:\\Other").unwrap_or_else(|_| panic!("other"));
        let foreign = SyncItem::new(&other, "f.txt", "r1", 1).unwrap_or_else(|_| panic!("foreign"));
        assert!(WorkspaceDescriptor::new(&other).with_item(foreign).is_ok());
        let mut full = WorkspaceDescriptor::new(&source);
        for index in 0..MAX_SYNC_ITEMS {
            let item = SyncItem::new(&source, &format!("item{index:05}.txt"), "r1", 1)
                .unwrap_or_else(|_| panic!("capped item"));
            full = full.with_item(item).expect("fill descriptor");
        }
        let overflow =
            SyncItem::new(&source, "overflow.txt", "r1", 1).unwrap_or_else(|_| panic!("overflow"));
        assert_eq!(
            full.with_item(overflow),
            Err(SharedFolderError::TooManyItems(MAX_SYNC_ITEMS))
        );
    }

    #[test]
    fn conflicts_are_detected_by_revision_and_root() {
        let source = source();
        let make = |path: &str, revision: &str| {
            SyncItem::new(&source, path, revision, 1).unwrap_or_else(|_| panic!("sync item"))
        };
        let local = WorkspaceDescriptor::new(&source)
            .with_item(make("a.txt", "r1"))
            .expect("local a")
            .with_item(make("b.txt", "r2"))
            .expect("local b");
        let remote = WorkspaceDescriptor::new(&source)
            .with_item(make("a.txt", "r9"))
            .expect("remote a")
            .with_item(make("c.txt", "r3"))
            .expect("remote c");
        let report = detect_conflicts(&local, &remote).expect("conflicts");
        assert_eq!(report.len(), 3);
        assert_eq!(report.conflicts()[0].kind, ConflictKind::RevisionMismatch);
        assert_eq!(report.conflicts()[0].path, "a.txt");
        assert_eq!(report.conflicts()[1].kind, ConflictKind::LocalOnly);
        assert_eq!(report.conflicts()[2].kind, ConflictKind::RemoteOnly);
        assert_eq!(report, detect_conflicts(&local, &remote).expect("repeat"));
        let identical = detect_conflicts(&local, &local).expect("identical");
        assert!(identical.is_empty());
        assert_eq!(identical.serialize(), "");
        let other = SharedFolderSource::new("C:\\Other").unwrap_or_else(|_| panic!("other"));
        let mismatched = WorkspaceDescriptor::new(&other);
        assert_eq!(
            detect_conflicts(&local, &mismatched),
            Err(SharedFolderError::RootMismatch)
        );
    }

    #[test]
    fn sync_plan_is_deterministic_and_serializes_stably() {
        let source = source();
        let make = |path: &str, revision: &str| {
            SyncItem::new(&source, path, revision, 1).unwrap_or_else(|_| panic!("sync item"))
        };
        let local = WorkspaceDescriptor::new(&source)
            .with_item(make("keep.txt", "r1"))
            .expect("keep")
            .with_item(make("local.txt", "r2"))
            .expect("local")
            .with_item(make("conflict.txt", "r3"))
            .expect("conflict");
        let remote = WorkspaceDescriptor::new(&source)
            .with_item(make("keep.txt", "r1"))
            .expect("keep")
            .with_item(make("remote.txt", "r4"))
            .expect("remote")
            .with_item(make("conflict.txt", "r9"))
            .expect("conflict");
        let plan = SyncPlan::build(&local, &remote).expect("plan");
        let actions: Vec<(&str, SyncAction)> = plan
            .steps()
            .iter()
            .map(|step| (step.path.as_str(), step.action))
            .collect();
        assert_eq!(
            actions,
            vec![
                ("conflict.txt", SyncAction::Resolve),
                ("keep.txt", SyncAction::Keep),
                ("local.txt", SyncAction::Upload),
                ("remote.txt", SyncAction::Download),
            ]
        );
        assert_eq!(plan.root(), "C:\\Shared");
        assert_eq!(plan, SyncPlan::build(&local, &remote).expect("repeat"));
        let serialized = plan.serialize();
        assert_eq!(
            serialized,
            SyncPlan::build(&local, &remote)
                .expect("repeat")
                .serialize()
        );
        assert!(serialized.starts_with("SYNC|C:\\\\Shared\n"));
        assert!(serialized.contains("STEP|RESOLVE|conflict.txt|r3"));
        assert_eq!(plan.serialize().lines().count(), 5);
    }

    #[test]
    fn conflict_and_plan_reports_escape_delimiters() {
        let source = SharedFolderSource::new("relative\\root").unwrap_or_else(|_| panic!("root"));
        let local = WorkspaceDescriptor::new(&source)
            .with_item(SyncItem::new(&source, "a.txt", "r1", 1).expect("item"))
            .expect("local");
        let remote = WorkspaceDescriptor::new(&source)
            .with_item(SyncItem::new(&source, "a.txt", "r2", 1).expect("item"))
            .expect("remote");
        let report = detect_conflicts(&local, &remote).expect("report");
        assert!(
            report
                .serialize()
                .starts_with("CONFLICT|REVISION|a.txt|r1|r2")
        );
        let plan = SyncPlan::build(&local, &remote).expect("plan");
        assert!(plan.serialize().starts_with("SYNC|relative\\\\root\n"));
        assert!(plan.serialize().contains("STEP|RESOLVE|a.txt|r1"));
    }
}
