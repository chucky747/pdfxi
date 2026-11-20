//! PDF Portfolio browsing model.
//!
//! This module implements the *model* behind a PDF Portfolio browser. It is
//! dependency-light: no `egui`, renderer, or PDF backend dependency. A caller
//! parses a portfolio's embedded-file collection into a [`PortfolioSummary`],
//! and [`PortfolioModel::from_summary`] validates it into a deterministic,
//! nested [`PortfolioTree`]. Per-entry problems (traversal paths, malformed
//! fields, duplicate full paths) do not abort the build; they are collected in
//! a typed [`PortfolioReport`] and the offending entries are skipped.
//!
//! # Path safety
//!
//! A [`PortfolioPath`] is an *internal* portfolio path: a slash-separated list
//! of components rooted at the collection. Every component is validated to be
//! traversal-free (no `.`/`..`, no empty components, no absolute or drive
//! prefixes, no backslashes, no control characters, no `:` alternate-stream
//! markers). This keeps hostile collection data from escaping the tree.
//!
//! # Ordering and determinism
//!
//! Folders and files are sorted case-insensitively (then by raw name for stable
//! ties), with folders before files in each directory. No hash or locale
//! ordering is used, so the same summary always produces the same tree and
//! rows. [`PortfolioTree::rows`] exposes the tree as a flat pre-order list with
//! depth, ready for a host tree view.
//!
//! # Host boundary
//!
//! This module holds only embedded-file *metadata*; it never extracts stream
//! bytes and performs no filesystem or network I/O.

use std::collections::BTreeMap;

/// Maximum number of entries accepted in one portfolio summary.
pub const MAX_PORTFOLIO_ENTRIES: usize = 8192;
/// Maximum number of characters in an entry name (one path component).
pub const MAX_PORTFOLIO_NAME_LEN: usize = 512;
/// Maximum number of characters in an entry description.
pub const MAX_PORTFOLIO_DESCRIPTION_LEN: usize = 1024;
/// Maximum number of characters in an entry MIME type or extension token.
pub const MAX_PORTFOLIO_MIME_LEN: usize = 128;
/// Maximum number of characters in a whole folder path.
pub const MAX_PORTFOLIO_PATH_LEN: usize = 2048;
/// Maximum number of components in a folder path or full file path.
pub const MAX_PORTFOLIO_PATH_DEPTH: usize = 64;

/// Typed error returned when a portfolio path or component is invalid.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PortfolioPathError {
    /// A path contained an empty component (for example `a//b` or a leading `/`).
    #[error("portfolio path has an empty component")]
    EmptyComponent,
    /// A path contained a `.` component.
    #[error("portfolio path has a '.' component")]
    DotComponent,
    /// A path contained a `..` parent-traversal component.
    #[error("portfolio path has a parent-traversal component")]
    Traversal,
    /// A component looked like an absolute path or a drive/stream prefix.
    #[error("portfolio path has an absolute or drive prefix")]
    AbsoluteOrDrive,
    /// A component contained a forbidden character (backslash or control char).
    #[error("portfolio path has an invalid character")]
    InvalidCharacter,
    /// A single component exceeded [`MAX_PORTFOLIO_NAME_LEN`].
    #[error("portfolio path component is too long")]
    ComponentTooLong,
    /// The path had more than [`MAX_PORTFOLIO_PATH_DEPTH`] components.
    #[error("portfolio path is too deep")]
    TooDeep,
    /// The path exceeded [`MAX_PORTFOLIO_PATH_LEN`] characters.
    #[error("portfolio path is too long")]
    TooLong,
}

/// A validated, traversal-free internal portfolio path.
///
/// The root path has zero components. All other paths are an ordered list of
/// single-component names joined by `/` when displayed.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PortfolioPath {
    components: Vec<String>,
}

impl std::fmt::Display for PortfolioPath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.components.join("/"))
    }
}

impl PortfolioPath {
    /// The root path (zero components).
    pub fn root() -> Self {
        Self {
            components: Vec::new(),
        }
    }

    /// Parse a slash-separated folder path, rejecting traversal and absolute
    /// prefixes. An empty string parses as the root path.
    pub fn parse(input: &str) -> Result<Self, PortfolioPathError> {
        if input.is_empty() {
            return Ok(Self::root());
        }
        if input.chars().count() > MAX_PORTFOLIO_PATH_LEN {
            return Err(PortfolioPathError::TooLong);
        }
        let mut components = Vec::new();
        for part in input.split('/') {
            Self::validate_component(part)?;
            components.push(part.to_owned());
        }
        if components.len() > MAX_PORTFOLIO_PATH_DEPTH {
            return Err(PortfolioPathError::TooDeep);
        }
        Ok(Self { components })
    }

    /// Validate a single path component (also used to validate entry names).
    pub fn validate_component(component: &str) -> Result<(), PortfolioPathError> {
        if component.is_empty() {
            return Err(PortfolioPathError::EmptyComponent);
        }
        if component == "." {
            return Err(PortfolioPathError::DotComponent);
        }
        if component == ".." {
            return Err(PortfolioPathError::Traversal);
        }
        if component.chars().count() > MAX_PORTFOLIO_NAME_LEN {
            return Err(PortfolioPathError::ComponentTooLong);
        }
        if component.contains(':') {
            return Err(PortfolioPathError::AbsoluteOrDrive);
        }
        if component.chars().any(|c| c.is_control() || c == '\\') {
            return Err(PortfolioPathError::InvalidCharacter);
        }
        Ok(())
    }

    /// Append a validated name component, producing a full file path.
    pub fn join(&self, name: &str) -> Result<Self, PortfolioPathError> {
        Self::validate_component(name)?;
        let mut components = self.components.clone();
        components.push(name.to_owned());
        if components.len() > MAX_PORTFOLIO_PATH_DEPTH {
            return Err(PortfolioPathError::TooDeep);
        }
        Ok(Self { components })
    }

    /// The parent path, or `None` for the root.
    pub fn parent(&self) -> Option<Self> {
        if self.components.is_empty() {
            None
        } else {
            Some(Self {
                components: self.components[..self.components.len() - 1].to_vec(),
            })
        }
    }

    /// The ordered components of this path.
    pub fn components(&self) -> &[String] {
        &self.components
    }

    /// The final component, or `None` for the root.
    pub fn name(&self) -> Option<&str> {
        self.components.last().map(String::as_str)
    }

    /// Whether this is the root path.
    pub fn is_root(&self) -> bool {
        self.components.is_empty()
    }

    /// The slash-joined display form; the root renders as an empty string.
    pub fn as_str(&self) -> String {
        self.components.join("/")
    }

    /// Append an already-validated component without re-validation (private
    /// helper used while rebuilding a tree).
    fn child_unchecked(&self, name: &str) -> Self {
        let mut components = self.components.clone();
        components.push(name.to_owned());
        Self { components }
    }
}

/// One caller-supplied embedded-file declaration in a portfolio.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PortfolioEntry {
    name: String,
    description: String,
    size: u64,
    mime: String,
    modified: Option<u64>,
    folder: String,
}

impl PortfolioEntry {
    /// Describe one embedded file.
    ///
    /// `folder` is a slash-separated internal folder path (empty for the root);
    /// `mime` may be a MIME type or an extension token. `modified` is an
    /// optional Unix timestamp in seconds. No bytes are read or stored.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        size: u64,
        mime: impl Into<String>,
        modified: Option<u64>,
        folder: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            size,
            mime: mime.into(),
            modified,
            folder: folder.into(),
        }
    }

    /// The file name (leaf component).
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The human-readable description, which may be empty.
    pub fn description(&self) -> &str {
        &self.description
    }

    /// The uncompressed size in bytes.
    pub fn size(&self) -> u64 {
        self.size
    }

    /// The MIME type or extension token.
    pub fn mime(&self) -> &str {
        &self.mime
    }

    /// The optional modification time as a Unix timestamp in seconds.
    pub fn modified(&self) -> Option<u64> {
        self.modified
    }

    /// The raw caller-supplied internal folder path.
    pub fn folder(&self) -> &str {
        &self.folder
    }
}

/// Caller-supplied embedded-file collection.
#[derive(Clone, Debug, Default)]
pub struct PortfolioSummary {
    entries: Vec<PortfolioEntry>,
}

impl PortfolioSummary {
    /// Create an empty summary.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add one entry declaration.
    pub fn with_entry(mut self, entry: PortfolioEntry) -> Self {
        self.entries.push(entry);
        self
    }

    /// The declared entries, in caller-supplied order.
    pub fn entries(&self) -> &[PortfolioEntry] {
        &self.entries
    }
}

/// Why a portfolio entry could not be represented in the tree.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum UnsupportedReason {
    /// The name was empty or not a valid single path component.
    #[error("invalid or empty name")]
    InvalidName,
    /// The description exceeded [`MAX_PORTFOLIO_DESCRIPTION_LEN`].
    #[error("description too long")]
    DescriptionTooLong,
    /// The MIME/extension token was over-long or contained control characters.
    #[error("invalid MIME type or extension")]
    InvalidMime,
    /// The folder path contained a `.` or `..` traversal component.
    #[error("folder path contains traversal")]
    TraversalPath,
    /// The folder path was otherwise malformed.
    #[error("invalid folder path")]
    InvalidFolder,
    /// The folder path or a component exceeded its length limit.
    #[error("folder path too long")]
    PathTooLong,
    /// The folder path exceeded [`MAX_PORTFOLIO_PATH_DEPTH`].
    #[error("folder path too deep")]
    PathTooDeep,
}

/// One entry that was skipped because it was unsupported or malformed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnsupportedEntry {
    index: usize,
    name: String,
    reason: UnsupportedReason,
}

impl UnsupportedEntry {
    /// The zero-based index of the entry in the caller-supplied summary.
    pub fn index(&self) -> usize {
        self.index
    }

    /// The (possibly invalid) name that was declared.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Why the entry was skipped.
    pub fn reason(&self) -> &UnsupportedReason {
        &self.reason
    }
}

/// One entry that was skipped because its full path duplicated an earlier one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DuplicateEntry {
    index: usize,
    name: String,
    path: String,
    first_index: usize,
}

impl DuplicateEntry {
    /// The zero-based index of the duplicate entry.
    pub fn index(&self) -> usize {
        self.index
    }

    /// The declared name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The full path that collided.
    pub fn path(&self) -> &str {
        &self.path
    }

    /// The index of the entry that won the collision (the first one).
    pub fn first_index(&self) -> usize {
        self.first_index
    }
}

/// A typed report of entries that were skipped while building a model.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PortfolioReport {
    unsupported: Vec<UnsupportedEntry>,
    duplicates: Vec<DuplicateEntry>,
}

impl PortfolioReport {
    /// Unsupported/malformed entries, in caller-supplied index order.
    pub fn unsupported(&self) -> &[UnsupportedEntry] {
        &self.unsupported
    }

    /// Duplicate entries, in caller-supplied index order.
    pub fn duplicates(&self) -> &[DuplicateEntry] {
        &self.duplicates
    }

    /// Whether no entry was skipped.
    pub fn is_empty(&self) -> bool {
        self.unsupported.is_empty() && self.duplicates.is_empty()
    }

    /// Total number of reported issues.
    pub fn issue_count(&self) -> usize {
        self.unsupported.len() + self.duplicates.len()
    }
}

/// A global validation error that prevents building any model.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PortfolioError {
    /// The summary declared more than [`MAX_PORTFOLIO_ENTRIES`] entries.
    #[error("too many portfolio entries ({0}, maximum {1})")]
    TooManyEntries(usize, usize),
}

/// A file node in a [`PortfolioTree`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PortfolioFile {
    path: PortfolioPath,
    entry: PortfolioEntry,
}

impl PortfolioFile {
    /// The full path including the file name.
    pub fn path(&self) -> &PortfolioPath {
        &self.path
    }

    /// The validated entry metadata.
    pub fn entry(&self) -> &PortfolioEntry {
        &self.entry
    }
}

/// A folder node in a [`PortfolioTree`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PortfolioFolder {
    path: PortfolioPath,
    children: Vec<PortfolioNode>,
}

impl PortfolioFolder {
    /// The full folder path (root has zero components).
    pub fn path(&self) -> &PortfolioPath {
        &self.path
    }

    /// The sorted children (folders first, then files).
    pub fn children(&self) -> &[PortfolioNode] {
        &self.children
    }
}

/// One node in a portfolio folder tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PortfolioNode {
    /// A folder node.
    Folder(PortfolioFolder),
    /// A file node.
    File(PortfolioFile),
}

impl PortfolioNode {
    /// The full path of this node.
    pub fn path(&self) -> &PortfolioPath {
        match self {
            Self::Folder(folder) => folder.path(),
            Self::File(file) => file.path(),
        }
    }

    /// Whether this node is a folder.
    pub fn is_folder(&self) -> bool {
        matches!(self, Self::Folder(_))
    }

    /// Whether this node is a file.
    pub fn is_file(&self) -> bool {
        matches!(self, Self::File(_))
    }
}

/// What a flat [`PortfolioRow`] represents.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PortfolioRowKind {
    /// A folder row.
    Folder {
        /// The full folder path.
        path: PortfolioPath,
        /// The folder's display name (the root folder renders as `/`).
        name: String,
    },
    /// A file row carrying the embedded-file metadata.
    File {
        /// The full file path.
        path: PortfolioPath,
        /// The file name.
        name: String,
        /// The uncompressed size in bytes.
        size: u64,
        /// The MIME type or extension token.
        mime: String,
        /// The optional modification time.
        modified: Option<u64>,
        /// The description.
        description: String,
    },
}

/// A flat, pre-order row of a [`PortfolioTree`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PortfolioRow {
    depth: usize,
    kind: PortfolioRowKind,
}

impl PortfolioRow {
    /// The nesting depth (0 for children of the root).
    pub fn depth(&self) -> usize {
        self.depth
    }

    /// What this row represents.
    pub fn kind(&self) -> &PortfolioRowKind {
        &self.kind
    }
}

/// A deterministic, nested folder/file tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PortfolioTree {
    root: PortfolioFolder,
}

impl PortfolioTree {
    /// The root folder.
    pub fn root(&self) -> &PortfolioFolder {
        &self.root
    }

    /// The tree flattened into pre-order rows with depth.
    pub fn rows(&self) -> Vec<PortfolioRow> {
        let mut rows = Vec::new();
        push_rows(&self.root, 0, &mut rows);
        rows
    }

    /// Total number of file nodes in the tree.
    pub fn file_count(&self) -> usize {
        count_nodes(&self.root).0
    }

    /// Total number of folder nodes in the tree (excluding the root).
    pub fn folder_count(&self) -> usize {
        count_nodes(&self.root).1
    }
}

fn count_nodes(folder: &PortfolioFolder) -> (usize, usize) {
    let mut files = 0usize;
    let mut folders = 0usize;
    for child in &folder.children {
        match child {
            PortfolioNode::Folder(nested) => {
                folders += 1;
                let (f, d) = count_nodes(nested);
                files += f;
                folders += d;
            }
            PortfolioNode::File(_) => files += 1,
        }
    }
    (files, folders)
}

fn push_rows(folder: &PortfolioFolder, depth: usize, out: &mut Vec<PortfolioRow>) {
    for child in &folder.children {
        match child {
            PortfolioNode::Folder(nested) => {
                let name = match nested.path().name() {
                    Some(name) => name.to_owned(),
                    None => "/".to_owned(),
                };
                out.push(PortfolioRow {
                    depth,
                    kind: PortfolioRowKind::Folder {
                        path: nested.path().clone(),
                        name,
                    },
                });
                push_rows(nested, depth + 1, out);
            }
            PortfolioNode::File(file) => {
                let entry = file.entry();
                out.push(PortfolioRow {
                    depth,
                    kind: PortfolioRowKind::File {
                        path: file.path().clone(),
                        name: entry.name().to_owned(),
                        size: entry.size(),
                        mime: entry.mime().to_owned(),
                        modified: entry.modified(),
                        description: entry.description().to_owned(),
                    },
                });
            }
        }
    }
}

fn cmp_names(a: &str, b: &str) -> std::cmp::Ordering {
    (a.to_lowercase(), a).cmp(&(b.to_lowercase(), b))
}

#[derive(Default)]
struct FolderBuilder {
    folders: BTreeMap<String, FolderBuilder>,
    files: BTreeMap<String, PortfolioFile>,
}

impl FolderBuilder {
    fn insert(&mut self, path: &PortfolioPath, file: PortfolioFile) {
        let components = path.components();
        if components.is_empty() {
            return;
        }
        let mut current = self;
        for component in &components[..components.len() - 1] {
            current = current.folders.entry(component.clone()).or_default();
        }
        let name = &components[components.len() - 1];
        current.files.insert(name.clone(), file);
    }

    fn into_folder(self, path: PortfolioPath) -> PortfolioFolder {
        let mut folders: Vec<(String, FolderBuilder)> = self.folders.into_iter().collect();
        folders.sort_by(|a, b| cmp_names(&a.0, &b.0));
        let mut files: Vec<(String, PortfolioFile)> = self.files.into_iter().collect();
        files.sort_by(|a, b| cmp_names(&a.0, &b.0));

        let mut children = Vec::with_capacity(folders.len() + files.len());
        for (name, builder) in folders {
            let child_path = path.child_unchecked(&name);
            children.push(PortfolioNode::Folder(builder.into_folder(child_path)));
        }
        for (_, file) in files {
            children.push(PortfolioNode::File(file));
        }
        PortfolioFolder { path, children }
    }
}

fn validate_entry(entry: &PortfolioEntry) -> Result<PortfolioPath, UnsupportedReason> {
    if entry.name().is_empty() || PortfolioPath::validate_component(entry.name()).is_err() {
        return Err(UnsupportedReason::InvalidName);
    }
    if entry.description().chars().count() > MAX_PORTFOLIO_DESCRIPTION_LEN {
        return Err(UnsupportedReason::DescriptionTooLong);
    }
    if entry.mime().chars().count() > MAX_PORTFOLIO_MIME_LEN
        || entry.mime().chars().any(char::is_control)
    {
        return Err(UnsupportedReason::InvalidMime);
    }
    let folder = match PortfolioPath::parse(entry.folder()) {
        Ok(path) => path,
        Err(PortfolioPathError::Traversal | PortfolioPathError::DotComponent) => {
            return Err(UnsupportedReason::TraversalPath);
        }
        Err(PortfolioPathError::TooDeep) => return Err(UnsupportedReason::PathTooDeep),
        Err(PortfolioPathError::TooLong | PortfolioPathError::ComponentTooLong) => {
            return Err(UnsupportedReason::PathTooLong);
        }
        Err(_) => return Err(UnsupportedReason::InvalidFolder),
    };
    match folder.join(entry.name()) {
        Ok(path) => Ok(path),
        Err(PortfolioPathError::Traversal | PortfolioPathError::DotComponent) => {
            Err(UnsupportedReason::TraversalPath)
        }
        Err(PortfolioPathError::TooDeep) => Err(UnsupportedReason::PathTooDeep),
        Err(PortfolioPathError::TooLong) => Err(UnsupportedReason::PathTooLong),
        Err(_) => Err(UnsupportedReason::InvalidFolder),
    }
}

/// A validated portfolio model with its deterministic tree and typed report.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PortfolioModel {
    tree: PortfolioTree,
    report: PortfolioReport,
    accepted: usize,
}

impl PortfolioModel {
    /// Validate `summary` into a deterministic tree.
    ///
    /// A global error is returned only when the summary is structurally invalid
    /// (too many entries). Per-entry problems are recorded in the returned
    /// report and the offending entry is skipped; the first entry wins any
    /// full-path collision.
    pub fn from_summary(summary: &PortfolioSummary) -> Result<Self, PortfolioError> {
        let entries = summary.entries();
        if entries.len() > MAX_PORTFOLIO_ENTRIES {
            return Err(PortfolioError::TooManyEntries(
                entries.len(),
                MAX_PORTFOLIO_ENTRIES,
            ));
        }

        let mut accepted: BTreeMap<PortfolioPath, PortfolioFile> = BTreeMap::new();
        let mut first_index: BTreeMap<String, usize> = BTreeMap::new();
        let mut unsupported = Vec::new();
        let mut duplicates = Vec::new();

        for (index, entry) in entries.iter().enumerate() {
            let full_path = match validate_entry(entry) {
                Ok(path) => path,
                Err(reason) => {
                    unsupported.push(UnsupportedEntry {
                        index,
                        name: entry.name().to_owned(),
                        reason,
                    });
                    continue;
                }
            };
            let key = full_path.as_str();
            if let Some(&first) = first_index.get(&key) {
                duplicates.push(DuplicateEntry {
                    index,
                    name: entry.name().to_owned(),
                    path: key,
                    first_index: first,
                });
                continue;
            }
            first_index.insert(key, index);
            accepted.insert(
                full_path.clone(),
                PortfolioFile {
                    path: full_path,
                    entry: entry.clone(),
                },
            );
        }

        let mut builder = FolderBuilder::default();
        for (path, file) in accepted {
            builder.insert(&path, file);
        }
        let tree = PortfolioTree {
            root: builder.into_folder(PortfolioPath::root()),
        };
        let report = PortfolioReport {
            unsupported,
            duplicates,
        };
        Ok(Self {
            tree,
            report,
            accepted: first_index.len(),
        })
    }

    /// The deterministic folder/file tree.
    pub fn tree(&self) -> &PortfolioTree {
        &self.tree
    }

    /// The typed report of skipped entries.
    pub fn report(&self) -> &PortfolioReport {
        &self.report
    }

    /// Number of accepted entries in the tree.
    pub fn accepted(&self) -> usize {
        self.accepted
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_portfolio_produces_a_root_only_tree() {
        let model = PortfolioModel::from_summary(&PortfolioSummary::new());
        assert!(model.is_ok());
        let model = model.unwrap_or_else(|_| panic!("empty summary must build"));
        assert_eq!(model.accepted(), 0);
        assert!(model.report().is_empty());
        assert!(model.tree().root().children().is_empty());
        assert_eq!(model.tree().rows().len(), 0);
        assert_eq!(model.tree().file_count(), 0);
        assert_eq!(model.tree().folder_count(), 0);
    }

    #[test]
    fn tree_construction_nests_folders() {
        let summary = PortfolioSummary::new()
            .with_entry(PortfolioEntry::new(
                "a.pdf",
                "A",
                10,
                "application/pdf",
                Some(100),
                "Contracts/2024",
            ))
            .with_entry(PortfolioEntry::new(
                "b.txt",
                "B",
                20,
                "text/plain",
                None,
                "Contracts",
            ))
            .with_entry(PortfolioEntry::new("root.pdf", "", 5, "pdf", None, ""));
        let model = PortfolioModel::from_summary(&summary).unwrap_or_else(|_| panic!("build"));
        assert_eq!(model.accepted(), 3);
        assert_eq!(model.tree().file_count(), 3);
        assert_eq!(model.tree().folder_count(), 2);

        let rows = model.tree().rows();
        let depths: Vec<(usize, String)> = rows
            .iter()
            .map(|row| match row.kind() {
                PortfolioRowKind::Folder { name, .. } => (row.depth(), format!("D:{name}")),
                PortfolioRowKind::File { name, .. } => (row.depth(), format!("F:{name}")),
            })
            .collect();
        assert_eq!(
            depths,
            vec![
                (0, "D:Contracts".to_string()),
                (1, "D:2024".to_string()),
                (2, "F:a.pdf".to_string()),
                (1, "F:b.txt".to_string()),
                (0, "F:root.pdf".to_string()),
            ]
        );
    }

    #[test]
    fn sorting_is_folders_first_and_case_insensitive() {
        let summary = PortfolioSummary::new()
            .with_entry(PortfolioEntry::new("zeta.txt", "", 1, "txt", None, ""))
            .with_entry(PortfolioEntry::new("Alpha.txt", "", 1, "txt", None, ""))
            .with_entry(PortfolioEntry::new("in.txt", "", 1, "txt", None, "beta"))
            .with_entry(PortfolioEntry::new("in.txt", "", 1, "txt", None, "Alpha"));
        let model = PortfolioModel::from_summary(&summary).unwrap_or_else(|_| panic!("build"));
        let root_names: Vec<String> = model
            .tree()
            .root()
            .children()
            .iter()
            .map(|node| match node {
                PortfolioNode::Folder(folder) => match folder.path().name() {
                    Some(name) => name.to_owned(),
                    None => "/".to_owned(),
                },
                PortfolioNode::File(file) => file.entry().name().to_owned(),
            })
            .collect();
        assert_eq!(
            root_names,
            vec![
                "Alpha".to_string(),
                "beta".to_string(),
                "Alpha.txt".to_string(),
                "zeta.txt".to_string(),
            ]
        );
    }

    #[test]
    fn traversal_folder_is_reported_and_skipped() {
        let summary = PortfolioSummary::new()
            .with_entry(PortfolioEntry::new("ok.txt", "", 1, "txt", None, "safe"))
            .with_entry(PortfolioEntry::new(
                "bad.txt",
                "",
                1,
                "txt",
                None,
                "../escape",
            ))
            .with_entry(PortfolioEntry::new("dot.txt", "", 1, "txt", None, "./a"));
        let model = PortfolioModel::from_summary(&summary).unwrap_or_else(|_| panic!("build"));
        assert_eq!(model.accepted(), 1);
        assert_eq!(model.tree().file_count(), 1);
        let unsupported = model.report().unsupported();
        assert_eq!(unsupported.len(), 2);
        assert_eq!(unsupported[0].reason(), &UnsupportedReason::TraversalPath);
        assert_eq!(unsupported[1].reason(), &UnsupportedReason::TraversalPath);
    }

    #[test]
    fn duplicate_full_paths_are_reported_and_first_wins() {
        let summary = PortfolioSummary::new()
            .with_entry(PortfolioEntry::new(
                "same.txt", "first", 1, "txt", None, "d",
            ))
            .with_entry(PortfolioEntry::new(
                "same.txt", "second", 2, "txt", None, "d",
            ));
        let model = PortfolioModel::from_summary(&summary).unwrap_or_else(|_| panic!("build"));
        assert_eq!(model.accepted(), 1);
        assert_eq!(model.tree().file_count(), 1);
        let duplicates = model.report().duplicates();
        assert_eq!(duplicates.len(), 1);
        assert_eq!(duplicates[0].index(), 1);
        assert_eq!(duplicates[0].first_index(), 0);
        assert_eq!(duplicates[0].path(), "d/same.txt");
        let file = match &model.tree().root().children()[0] {
            PortfolioNode::Folder(f) => match &f.children()[0] {
                PortfolioNode::File(file) => file,
                PortfolioNode::Folder(_) => panic!("expected file"),
            },
            PortfolioNode::File(_) => panic!("expected folder"),
        };
        assert_eq!(file.entry().description(), "first");
    }

    #[test]
    fn malformed_entries_are_reported_and_skipped() {
        let summary = PortfolioSummary::new()
            .with_entry(PortfolioEntry::new("", "", 1, "txt", None, ""))
            .with_entry(PortfolioEntry::new(
                "long-desc.txt",
                "x".repeat(2000),
                1,
                "txt",
                None,
                "",
            ))
            .with_entry(PortfolioEntry::new("bad-mime.txt", "", 1, "a\nb", None, ""))
            .with_entry(PortfolioEntry::new("good.txt", "ok", 1, "txt", None, ""));
        let model = PortfolioModel::from_summary(&summary).unwrap_or_else(|_| panic!("build"));
        assert_eq!(model.accepted(), 1);
        let unsupported = model.report().unsupported();
        assert_eq!(unsupported.len(), 3);
        assert_eq!(unsupported[0].reason(), &UnsupportedReason::InvalidName);
        assert_eq!(
            unsupported[1].reason(),
            &UnsupportedReason::DescriptionTooLong
        );
        assert_eq!(unsupported[2].reason(), &UnsupportedReason::InvalidMime);
    }

    #[test]
    fn build_is_deterministic_regardless_of_input_order() {
        let first = PortfolioSummary::new()
            .with_entry(PortfolioEntry::new("b.txt", "b", 2, "txt", None, "d"))
            .with_entry(PortfolioEntry::new("a.txt", "a", 1, "txt", None, "d"))
            .with_entry(PortfolioEntry::new("z.txt", "z", 3, "txt", None, ""));
        let second = PortfolioSummary::new()
            .with_entry(PortfolioEntry::new("z.txt", "z", 3, "txt", None, ""))
            .with_entry(PortfolioEntry::new("a.txt", "a", 1, "txt", None, "d"))
            .with_entry(PortfolioEntry::new("b.txt", "b", 2, "txt", None, "d"));
        let a = PortfolioModel::from_summary(&first).unwrap_or_else(|_| panic!("build a"));
        let b = PortfolioModel::from_summary(&second).unwrap_or_else(|_| panic!("build b"));
        assert_eq!(a.tree(), b.tree());
        assert_eq!(a.tree().rows(), b.tree().rows());
    }

    #[test]
    fn path_validation_rejects_absolute_drive_backslash_and_dotdot() {
        assert!(PortfolioPath::parse("a/b").is_ok());
        assert_eq!(
            PortfolioPath::parse("/a").unwrap_err(),
            PortfolioPathError::EmptyComponent
        );
        assert_eq!(
            PortfolioPath::parse("a//b").unwrap_err(),
            PortfolioPathError::EmptyComponent
        );
        assert_eq!(
            PortfolioPath::parse("a/../b").unwrap_err(),
            PortfolioPathError::Traversal
        );
        assert_eq!(
            PortfolioPath::parse("a\\b").unwrap_err(),
            PortfolioPathError::InvalidCharacter
        );
        assert_eq!(
            PortfolioPath::parse("C:/a").unwrap_err(),
            PortfolioPathError::AbsoluteOrDrive
        );
        assert_eq!(
            PortfolioPath::validate_component(".").unwrap_err(),
            PortfolioPathError::DotComponent
        );
        let root = PortfolioPath::root();
        assert!(root.is_root());
        assert!(root.parent().is_none());
        assert_eq!(
            root.join("file.txt")
                .unwrap_or_else(|_| panic!("join"))
                .as_str(),
            "file.txt"
        );
    }

    #[test]
    fn too_many_entries_is_a_global_typed_error() {
        let mut summary = PortfolioSummary::new();
        for _ in 0..MAX_PORTFOLIO_ENTRIES + 1 {
            summary = summary.with_entry(PortfolioEntry::new("x", "", 0, "txt", None, ""));
        }
        match PortfolioModel::from_summary(&summary) {
            Err(PortfolioError::TooManyEntries(count, max)) => {
                assert_eq!(count, MAX_PORTFOLIO_ENTRIES + 1);
                assert_eq!(max, MAX_PORTFOLIO_ENTRIES);
            }
            _ => panic!("expected TooManyEntries"),
        }
    }
}
