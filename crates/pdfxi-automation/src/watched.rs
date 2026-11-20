//! Deterministic watched-folder planning and execution adapter.
//!
//! This module is the local, file-level equivalent for the watched-folder
//! half of CREATE-014 and AUTO-003. It deliberately contains no background
//! thread and no OS watcher: the host calls [`WatchedFolder::poll_once`]
//! whenever it wants a scan. Everything else is a pure, in-memory model:
//!
//! - [`WatchedFolderConfig`] validates a rooted, traversal-free local folder
//!   plus bounded extensions and file limits before any scan;
//! - [`WatchedFolder::plan`] produces a deterministic, sorted file list without
//!   reading file contents;
//! - [`WatchedFolder::poll_once`] converts each eligible file through a
//!   host-supplied [`FolderConverter`], records one [`FileOutcome`] per file,
//!   and marks only successful files in [`ProcessedMarkers`] so repeated polls
//!   are idempotent;
//! - symlinks are never followed, and every discovered path is canonicalised
//!   and checked to remain inside the configured root.
//!
//! Converted output bytes are returned in the outcome; this adapter does not
//! write files itself. Persisting them is the host's responsibility, which
//! keeps the model deterministic and free of path-escaping writes.

use std::collections::BTreeSet;
use std::fmt;
use std::fs;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

/// Default upper bound on the number of files a single poll may discover.
pub const WATCHED_MAX_FILES: usize = 1024;

/// Default upper bound on the bytes of one input file.
pub const WATCHED_MAX_FILE_BYTES: usize = 16 * 1024 * 1024;

/// Default upper bound on directory recursion depth.
pub const WATCHED_MAX_DEPTH: usize = 16;

/// Errors returned by watched-folder configuration and polling.
///
/// Each variant names the offending path or bound so a caller can show a
/// specific correction instead of a generic failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WatchedFolderError {
    /// The configured root was empty.
    EmptyRoot,
    /// The root was not an absolute local path.
    RelativeRoot {
        /// The rejected path.
        path: String,
    },
    /// The path contained a traversal or current-directory component.
    Traversal {
        /// The rejected path.
        path: String,
    },
    /// The path could not be represented as UTF-8 or contained a NUL byte.
    InvalidPath {
        /// The rejected path.
        path: String,
        /// Why it was rejected.
        reason: String,
    },
    /// An extension or output suffix was not a short ASCII token.
    InvalidExtension {
        /// The rejected value.
        value: String,
    },
    /// Discovery found more files than the configured bound.
    TooManyFiles {
        /// File budget that was exceeded.
        limit: usize,
    },
    /// A file was larger than the configured bound.
    TooLarge {
        /// Relative path of the file.
        path: String,
        /// Observed size (at least `limit + 1`).
        bytes: usize,
        /// Budget that was exceeded.
        limit: usize,
    },
    /// A filesystem operation failed.
    Io {
        /// Path involved in the failed operation.
        path: String,
        /// Human-readable cause.
        message: String,
    },
    /// Another configuration invariant was violated.
    InvalidConfig {
        /// Human-readable reason.
        reason: String,
    },
}

impl fmt::Display for WatchedFolderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyRoot => write!(f, "watched folder root must not be empty"),
            Self::RelativeRoot { path } => {
                write!(
                    f,
                    "watched folder root '{path}' must be an absolute local path"
                )
            }
            Self::Traversal { path } => {
                write!(
                    f,
                    "path '{path}' contains a '..' or '.' traversal component"
                )
            }
            Self::InvalidPath { path, reason } => {
                write!(f, "invalid path '{path}': {reason}")
            }
            Self::InvalidExtension { value } => write!(
                f,
                "invalid extension or suffix '{value}'; use 1-16 ASCII letters or digits"
            ),
            Self::TooManyFiles { limit } => {
                write!(
                    f,
                    "watched folder has more than {limit} files; raise max_files"
                )
            }
            Self::TooLarge { path, bytes, limit } => write!(
                f,
                "file '{path}' is too large: {bytes} bytes (limit {limit})"
            ),
            Self::Io { path, message } => {
                write!(f, "watched folder I/O error at '{path}': {message}")
            }
            Self::InvalidConfig { reason } => write!(f, "invalid watched folder config: {reason}"),
        }
    }
}

impl std::error::Error for WatchedFolderError {}

/// Validate a caller-supplied local path without touching the file system.
fn validate_path(path: &Path) -> Result<String, WatchedFolderError> {
    let text = path
        .to_str()
        .ok_or_else(|| WatchedFolderError::InvalidPath {
            path: path.display().to_string(),
            reason: "path is not valid UTF-8".to_owned(),
        })?;
    if text.is_empty() {
        return Err(WatchedFolderError::EmptyRoot);
    }
    if text.contains('\0') {
        return Err(WatchedFolderError::InvalidPath {
            path: text.to_owned(),
            reason: "path contains a NUL byte".to_owned(),
        });
    }
    if path
        .components()
        .any(|part| matches!(part, Component::ParentDir | Component::CurDir))
    {
        return Err(WatchedFolderError::Traversal {
            path: text.to_owned(),
        });
    }
    if !path.is_absolute() {
        return Err(WatchedFolderError::RelativeRoot {
            path: text.to_owned(),
        });
    }
    Ok(text.to_owned())
}

fn validate_token(value: &str) -> Result<String, WatchedFolderError> {
    let lowered = value.trim().to_ascii_lowercase();
    if lowered.is_empty()
        || lowered.len() > 16
        || !lowered.bytes().all(|byte| byte.is_ascii_alphanumeric())
    {
        return Err(WatchedFolderError::InvalidExtension {
            value: value.to_owned(),
        });
    }
    Ok(lowered)
}

/// A validated, bounded configuration for one watched folder.
///
/// Paths must be absolute and traversal-free. Extensions and the output suffix
/// are normalised to lowercase ASCII tokens; the list is sorted and deduplicated
/// so two equivalent configurations produce identical plans.
#[derive(Debug, Clone)]
pub struct WatchedFolderConfig {
    root: PathBuf,
    extensions: Vec<String>,
    max_files: usize,
    max_file_bytes: usize,
    max_depth: usize,
    recursive: bool,
    output_suffix: String,
}

impl WatchedFolderConfig {
    /// Validate and create a configuration for `root` with `.txt` and `.html`
    /// as the default eligible extensions.
    ///
    /// # Errors
    ///
    /// Returns [`WatchedFolderError::EmptyRoot`],
    /// [`WatchedFolderError::RelativeRoot`], [`WatchedFolderError::Traversal`],
    /// or [`WatchedFolderError::InvalidPath`] when the root is unsafe.
    pub fn new(root: impl Into<PathBuf>) -> Result<Self, WatchedFolderError> {
        let root = root.into();
        validate_path(&root)?;
        let config = Self {
            root,
            extensions: vec!["html".to_owned(), "txt".to_owned()],
            max_files: WATCHED_MAX_FILES,
            max_file_bytes: WATCHED_MAX_FILE_BYTES,
            max_depth: WATCHED_MAX_DEPTH,
            recursive: true,
            output_suffix: "pdf".to_owned(),
        };
        config.validate()?;
        Ok(config)
    }

    /// Replace the eligible extensions, validating and normalising each one.
    ///
    /// # Errors
    ///
    /// Returns [`WatchedFolderError::InvalidExtension`] for an empty, long, or
    /// non-ASCII token.
    pub fn with_extensions<I, S>(mut self, extensions: I) -> Result<Self, WatchedFolderError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut normalised: Vec<String> = extensions
            .into_iter()
            .map(|value| validate_token(value.as_ref()))
            .collect::<Result<Vec<_>, _>>()?;
        normalised.sort();
        normalised.dedup();
        if normalised.is_empty() {
            return Err(WatchedFolderError::InvalidConfig {
                reason: "at least one extension is required".to_owned(),
            });
        }
        self.extensions = normalised;
        Ok(self)
    }

    /// Replace the file-count budget.
    #[must_use]
    pub const fn with_max_files(mut self, max_files: usize) -> Self {
        self.max_files = max_files;
        self
    }

    /// Replace the per-file byte budget.
    #[must_use]
    pub const fn with_max_file_bytes(mut self, max_file_bytes: usize) -> Self {
        self.max_file_bytes = max_file_bytes;
        self
    }

    /// Replace the directory recursion-depth budget.
    #[must_use]
    pub const fn with_max_depth(mut self, max_depth: usize) -> Self {
        self.max_depth = max_depth;
        self
    }

    /// Scan only the root directory, not its descendants.
    #[must_use]
    pub const fn non_recursive(mut self) -> Self {
        self.recursive = false;
        self
    }

    /// Replace the output suffix written into [`FileOutcome::output_name`].
    ///
    /// # Errors
    ///
    /// Returns [`WatchedFolderError::InvalidExtension`] for an unsafe suffix.
    pub fn with_output_suffix(mut self, suffix: &str) -> Result<Self, WatchedFolderError> {
        self.output_suffix = validate_token(suffix)?;
        Ok(self)
    }

    /// Re-validate every invariant. Called again before each scan so a config
    /// cannot become unsafe through mutation.
    ///
    /// # Errors
    ///
    /// Returns a typed error naming the first violated invariant.
    pub fn validate(&self) -> Result<(), WatchedFolderError> {
        validate_path(&self.root)?;
        if self.extensions.is_empty() {
            return Err(WatchedFolderError::InvalidConfig {
                reason: "at least one extension is required".to_owned(),
            });
        }
        for extension in &self.extensions {
            validate_token(extension)?;
        }
        validate_token(&self.output_suffix)?;
        if self.max_files == 0 || self.max_file_bytes == 0 || self.max_depth == 0 {
            return Err(WatchedFolderError::InvalidConfig {
                reason: "file, byte, and depth limits must be positive".to_owned(),
            });
        }
        Ok(())
    }

    /// The validated root directory.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The sorted, normalised eligible extensions.
    #[must_use]
    pub fn extensions(&self) -> &[String] {
        &self.extensions
    }

    /// The file-count budget.
    #[must_use]
    pub const fn max_files(&self) -> usize {
        self.max_files
    }

    /// The per-file byte budget.
    #[must_use]
    pub const fn max_file_bytes(&self) -> usize {
        self.max_file_bytes
    }

    /// The recursion-depth budget.
    #[must_use]
    pub const fn max_depth(&self) -> usize {
        self.max_depth
    }

    /// Whether descendants are scanned.
    #[must_use]
    pub const fn recursive(&self) -> bool {
        self.recursive
    }

    /// The output suffix used in outcome names.
    #[must_use]
    pub fn output_suffix(&self) -> &str {
        &self.output_suffix
    }
}

#[derive(Debug, Clone)]
struct DiscoveredFile {
    relative: String,
    absolute: PathBuf,
}

/// A sorted list of files a poll would consider, in deterministic order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderPlan {
    entries: Vec<String>,
}

impl FolderPlan {
    /// The discovered relative paths, sorted bytewise and using `/` separators.
    #[must_use]
    pub fn entries(&self) -> &[String] {
        &self.entries
    }

    /// The number of discovered files.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the plan is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// A stable one-path-per-line rendering suitable for golden tests.
    #[must_use]
    pub fn summary(&self) -> String {
        let mut out = String::new();
        for entry in &self.entries {
            out.push_str(entry);
            out.push('\n');
        }
        out
    }
}

fn join_relative(parent: &str, name: &str) -> String {
    if parent.is_empty() {
        name.to_owned()
    } else {
        format!("{parent}/{name}")
    }
}

fn bounded_io(path: &Path, error: std::io::Error) -> WatchedFolderError {
    WatchedFolderError::Io {
        path: path.display().to_string(),
        message: error.to_string(),
    }
}

fn walk_dir(
    dir: &Path,
    canonical_root: &Path,
    relative: &str,
    depth: usize,
    config: &WatchedFolderConfig,
    found: &mut Vec<DiscoveredFile>,
    seen: &mut usize,
) -> Result<(), WatchedFolderError> {
    let mut entries: Vec<fs::DirEntry> = fs::read_dir(dir)
        .map_err(|error| bounded_io(dir, error))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| bounded_io(dir, error))?;
    entries.sort_by_key(fs::DirEntry::file_name);

    for entry in entries {
        *seen += 1;
        if *seen > config.max_files {
            return Err(WatchedFolderError::TooManyFiles {
                limit: config.max_files,
            });
        }
        let file_type = entry
            .file_type()
            .map_err(|error| bounded_io(&entry.path(), error))?;
        // Symlinks are skipped outright: following one could escape the root.
        if file_type.is_symlink() {
            continue;
        }
        let name = entry.file_name();
        let name = name
            .to_str()
            .ok_or_else(|| WatchedFolderError::InvalidPath {
                path: entry.path().display().to_string(),
                reason: "file name is not valid UTF-8".to_owned(),
            })?;
        let child_relative = join_relative(relative, name);
        let child = entry.path();
        let canonical = fs::canonicalize(&child).map_err(|error| bounded_io(&child, error))?;
        if !canonical.starts_with(canonical_root) {
            return Err(WatchedFolderError::Traversal {
                path: child_relative,
            });
        }

        if file_type.is_dir() {
            if config.recursive && depth < config.max_depth {
                walk_dir(
                    &child,
                    canonical_root,
                    &child_relative,
                    depth + 1,
                    config,
                    found,
                    seen,
                )?;
            }
        } else if file_type.is_file() {
            found.push(DiscoveredFile {
                relative: child_relative,
                absolute: child,
            });
        }
    }
    Ok(())
}

fn discover(config: &WatchedFolderConfig) -> Result<Vec<DiscoveredFile>, WatchedFolderError> {
    config.validate()?;
    let canonical_root =
        fs::canonicalize(config.root()).map_err(|error| bounded_io(config.root(), error))?;
    if !canonical_root.is_dir() {
        return Err(WatchedFolderError::InvalidConfig {
            reason: "configured root is not a directory".to_owned(),
        });
    }
    let mut found = Vec::new();
    let mut seen = 0usize;
    walk_dir(
        config.root(),
        &canonical_root,
        "",
        0,
        config,
        &mut found,
        &mut seen,
    )?;
    found.sort_by(|left, right| left.relative.cmp(&right.relative));
    Ok(found)
}

/// The in-memory set of relative paths that were converted successfully.
///
/// Markers survive across polls of one [`WatchedFolder`] but are not persisted
/// to disk; a fresh host process starts with an empty set. Only successful
/// conversions are marked, so a failed file is retried on the next poll.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProcessedMarkers {
    paths: BTreeSet<String>,
}

impl ProcessedMarkers {
    /// Create an empty marker set.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Mark a relative path as processed. Marking twice is idempotent.
    pub fn mark(&mut self, relative: impl Into<String>) {
        self.paths.insert(relative.into());
    }

    /// Return `true` when the relative path has been processed.
    #[must_use]
    pub fn contains(&self, relative: &str) -> bool {
        self.paths.contains(relative)
    }

    /// The number of marked paths.
    #[must_use]
    pub fn len(&self) -> usize {
        self.paths.len()
    }

    /// Whether no paths are marked.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.paths.is_empty()
    }

    /// The marked paths in stable sorted order.
    pub fn paths(&self) -> impl ExactSizeIterator<Item = &str> {
        self.paths.iter().map(String::as_str)
    }
}

/// The terminal state of one file during a poll.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FileOutcomeState {
    /// The file was converted and marked processed.
    Converted,
    /// The file was already marked processed and was not re-read.
    Skipped,
    /// The file's extension was not eligible.
    Unsupported,
    /// Reading or converting the file failed; it stays eligible for retry.
    Failed,
}

impl FileOutcomeState {
    /// Return the stable lowercase name of this state.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Converted => "converted",
            Self::Skipped => "skipped",
            Self::Unsupported => "unsupported",
            Self::Failed => "failed",
        }
    }
}

impl fmt::Display for FileOutcomeState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The per-file result recorded by a poll.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileOutcome {
    relative: String,
    state: FileOutcomeState,
    message: Option<String>,
    output: Option<Vec<u8>>,
    output_name: Option<String>,
}

impl FileOutcome {
    /// The file's path relative to the configured root, using `/` separators.
    #[must_use]
    pub fn relative(&self) -> &str {
        &self.relative
    }

    /// The terminal outcome state.
    #[must_use]
    pub const fn state(&self) -> FileOutcomeState {
        self.state
    }

    /// An optional explanation, present for unsupported and failed files.
    #[must_use]
    pub fn message(&self) -> Option<&str> {
        self.message.as_deref()
    }

    /// Converted bytes, present only for [`FileOutcomeState::Converted`].
    #[must_use]
    pub fn output(&self) -> Option<&[u8]> {
        self.output.as_deref()
    }

    /// A deterministic output name with the configured suffix.
    #[must_use]
    pub fn output_name(&self) -> Option<&str> {
        self.output_name.as_deref()
    }

    /// A stable one-line rendering: `<state>\t<relative-path>`.
    #[must_use]
    pub fn summary(&self) -> String {
        format!("{}\t{}", self.state.as_str(), self.relative)
    }
}

/// A deterministic report for one [`WatchedFolder::poll_once`] call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PollReport {
    entries: Vec<FileOutcome>,
}

impl PollReport {
    /// All per-file outcomes in discovery order.
    #[must_use]
    pub fn entries(&self) -> &[FileOutcome] {
        &self.entries
    }

    /// Count outcomes in a given state.
    #[must_use]
    pub fn count(&self, state: FileOutcomeState) -> usize {
        self.entries
            .iter()
            .filter(|entry| entry.state == state)
            .count()
    }

    /// One summary line per file, in discovery order.
    #[must_use]
    pub fn summary(&self) -> String {
        let mut out = String::new();
        for entry in &self.entries {
            out.push_str(&entry.summary());
            out.push('\n');
        }
        out
    }
}

/// A host-supplied conversion step for one watched file.
///
/// Implementations receive the relative path and raw input bytes and return
/// the converted bytes. Returning `Err` records a [`FileOutcomeState::Failed`]
/// outcome without aborting the rest of the poll.
pub trait FolderConverter {
    /// Convert `bytes` read from `relative_path` into output bytes.
    ///
    /// # Errors
    ///
    /// Return a human-readable reason when the file cannot be converted.
    fn convert(&self, relative_path: &str, bytes: &[u8]) -> Result<Vec<u8>, String>;
}

/// A [`FolderConverter`] that returns its input unchanged.
///
/// Useful when the host has already produced PDF bytes and only needs the
/// deterministic planning, outcome, and marker behaviour of this adapter.
#[derive(Debug, Clone, Copy, Default)]
pub struct PassthroughConverter;

impl FolderConverter for PassthroughConverter {
    fn convert(&self, _relative_path: &str, bytes: &[u8]) -> Result<Vec<u8>, String> {
        Ok(bytes.to_vec())
    }
}

fn output_name_for(relative: &str, suffix: &str) -> String {
    let path = Path::new(relative);
    let stem = path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or(relative);
    match path.parent().and_then(|value| value.to_str()) {
        Some(parent) if !parent.is_empty() => format!("{parent}/{stem}.{suffix}"),
        _ => format!("{stem}.{suffix}"),
    }
}

fn read_limited(path: &Path, limit: usize) -> Result<Vec<u8>, WatchedFolderError> {
    let file = fs::File::open(path).map_err(|error| bounded_io(path, error))?;
    let mut buffer = Vec::new();
    let allowance = u64::try_from(limit).unwrap_or(u64::MAX).saturating_add(1);
    let read = file
        .take(allowance)
        .read_to_end(&mut buffer)
        .map_err(|error| bounded_io(path, error))?;
    if read > limit {
        return Err(WatchedFolderError::TooLarge {
            path: path.display().to_string(),
            bytes: read,
            limit,
        });
    }
    Ok(buffer)
}

/// A watched folder with in-memory processing markers.
///
/// Construct one with a validated [`WatchedFolderConfig`], then let the host
/// call [`poll_once`](Self::poll_once) on its own schedule. No thread or watcher
/// is started by this type.
#[derive(Debug)]
pub struct WatchedFolder {
    config: WatchedFolderConfig,
    markers: ProcessedMarkers,
}

impl WatchedFolder {
    /// Validate `config` and create an unmarked folder.
    ///
    /// # Errors
    ///
    /// Returns the first [`WatchedFolderError`] reported by
    /// [`WatchedFolderConfig::validate`].
    pub fn new(config: WatchedFolderConfig) -> Result<Self, WatchedFolderError> {
        config.validate()?;
        Ok(Self {
            config,
            markers: ProcessedMarkers::new(),
        })
    }

    /// The validated configuration.
    #[must_use]
    pub const fn config(&self) -> &WatchedFolderConfig {
        &self.config
    }

    /// The current processing markers.
    #[must_use]
    pub const fn markers(&self) -> &ProcessedMarkers {
        &self.markers
    }

    /// Mutable access to the markers, for hosts that restore prior state.
    #[must_use]
    pub fn markers_mut(&mut self) -> &mut ProcessedMarkers {
        &mut self.markers
    }

    /// Build a deterministic, sorted plan without reading file contents.
    ///
    /// # Errors
    ///
    /// Returns [`WatchedFolderError::TooManyFiles`] when the folder exceeds the
    /// configured bound, [`WatchedFolderError::Traversal`] when a path escapes
    /// the root, and [`WatchedFolderError::Io`] for filesystem failures.
    pub fn plan(&self) -> Result<FolderPlan, WatchedFolderError> {
        let files = discover(&self.config)?;
        Ok(FolderPlan {
            entries: files.into_iter().map(|file| file.relative).collect(),
        })
    }

    /// Scan once, converting each eligible unprocessed file.
    ///
    /// Files are visited in the sorted order of [`Self::plan`]. Wrong-extension
    /// files produce [`FileOutcomeState::Unsupported`]; already-marked files
    /// produce [`FileOutcomeState::Skipped`] without being read. Read and
    /// conversion failures are isolated to that file's outcome and are not
    /// marked, so a later poll retries them. Only successful conversions are
    /// marked and carry output bytes.
    ///
    /// # Errors
    ///
    /// Discovery errors (bad root, too many files, traversal, I/O) are fatal
    /// for the poll and are returned as [`WatchedFolderError`].
    pub fn poll_once(
        &mut self,
        converter: &dyn FolderConverter,
    ) -> Result<PollReport, WatchedFolderError> {
        let files = discover(&self.config)?;
        let mut entries = Vec::with_capacity(files.len());
        for file in files {
            let extension = Path::new(&file.relative)
                .extension()
                .and_then(|value| value.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            if !self
                .config
                .extensions
                .iter()
                .any(|allowed| allowed == &extension)
            {
                entries.push(FileOutcome {
                    relative: file.relative,
                    state: FileOutcomeState::Unsupported,
                    message: Some(format!("extension '{extension}' is not eligible")),
                    output: None,
                    output_name: None,
                });
                continue;
            }
            if self.markers.contains(&file.relative) {
                entries.push(FileOutcome {
                    relative: file.relative,
                    state: FileOutcomeState::Skipped,
                    message: Some("already processed".to_owned()),
                    output: None,
                    output_name: None,
                });
                continue;
            }
            let bytes = match read_limited(&file.absolute, self.config.max_file_bytes) {
                Ok(bytes) => bytes,
                Err(error) => {
                    entries.push(FileOutcome {
                        relative: file.relative,
                        state: FileOutcomeState::Failed,
                        message: Some(error.to_string()),
                        output: None,
                        output_name: None,
                    });
                    continue;
                }
            };
            match converter.convert(&file.relative, &bytes) {
                Ok(output) => {
                    let name = output_name_for(&file.relative, &self.config.output_suffix);
                    self.markers.mark(file.relative.clone());
                    entries.push(FileOutcome {
                        relative: file.relative,
                        state: FileOutcomeState::Converted,
                        message: None,
                        output: Some(output),
                        output_name: Some(name),
                    });
                }
                Err(message) => entries.push(FileOutcome {
                    relative: file.relative,
                    state: FileOutcomeState::Failed,
                    message: Some(message),
                    output: None,
                    output_name: None,
                }),
            }
        }
        Ok(PollReport { entries })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_DIR: AtomicU64 = AtomicU64::new(0);

    struct Fixture {
        path: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "pdfxi-t0107-watched-{}-{}",
                std::process::id(),
                NEXT_DIR.fetch_add(1, Ordering::Relaxed)
            ));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).expect("create fixture dir");
            Self { path }
        }

        fn write(&self, name: &str, bytes: &[u8]) {
            let target = self.path.join(name);
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent).expect("create parent");
            }
            fs::write(target, bytes).expect("write fixture file");
        }

        fn config(&self) -> WatchedFolderConfig {
            WatchedFolderConfig::new(self.path.clone())
                .expect("valid fixture root")
                .with_extensions(["html", "txt", "md"])
                .expect("valid extensions")
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    #[derive(Debug, Default)]
    struct MapConverter {
        failing: Vec<String>,
    }

    impl MapConverter {
        fn fail(mut self, relative: &str) -> Self {
            self.failing.push(relative.to_owned());
            self
        }
    }

    impl FolderConverter for MapConverter {
        fn convert(&self, relative_path: &str, bytes: &[u8]) -> Result<Vec<u8>, String> {
            if self.failing.iter().any(|name| name == relative_path) {
                return Err(format!("cannot convert '{relative_path}'"));
            }
            let mut out = b"PDF:".to_vec();
            out.extend_from_slice(bytes);
            Ok(out)
        }
    }

    fn paths(report: &PollReport) -> Vec<(&str, FileOutcomeState)> {
        report
            .entries()
            .iter()
            .map(|entry| (entry.relative(), entry.state()))
            .collect()
    }

    #[test]
    fn config_rejects_relative_roots() {
        let error = WatchedFolderConfig::new("relative/folder").expect_err("relative root");
        assert!(matches!(error, WatchedFolderError::RelativeRoot { .. }));
        assert!(error.to_string().contains("absolute"));
    }

    #[test]
    fn config_rejects_traversal_and_nul() {
        let traversal =
            WatchedFolderConfig::new(Path::new(r"C:\base\..\escape")).expect_err("traversal root");
        assert!(matches!(traversal, WatchedFolderError::Traversal { .. }));
        let nul = WatchedFolderConfig::new(Path::new("C:\\base\\a\0b")).expect_err("nul root");
        assert!(matches!(nul, WatchedFolderError::InvalidPath { .. }));
    }

    #[test]
    fn config_rejects_unsafe_extensions_and_zero_limits() {
        let config = WatchedFolderConfig::new(std::env::temp_dir()).expect("temp root");
        assert!(matches!(
            config.clone().with_extensions([".."]),
            Err(WatchedFolderError::InvalidExtension { .. })
        ));
        assert!(config.clone().with_max_files(0).validate().is_err());
        assert!(config.with_output_suffix("a.b").is_err());
    }

    #[test]
    fn empty_folder_produces_an_empty_deterministic_report() {
        let fixture = Fixture::new();
        let mut folder = WatchedFolder::new(fixture.config()).expect("folder");
        let plan = folder.plan().expect("plan");
        assert!(plan.is_empty());
        assert_eq!(plan.summary(), "");
        let report = folder.poll_once(&PassthroughConverter).expect("poll");
        assert!(report.entries().is_empty());
        assert_eq!(report.summary(), "");
    }

    #[test]
    fn plan_is_sorted_and_poll_records_per_file_outcomes() {
        let fixture = Fixture::new();
        fixture.write("z.txt", b"z");
        fixture.write("a.html", b"a");
        fixture.write("nested/b.md", b"b");
        fixture.write("notes.bin", b"skip");
        let mut folder = WatchedFolder::new(fixture.config()).expect("folder");

        let plan = folder.plan().expect("plan");
        assert_eq!(
            plan.entries(),
            ["a.html", "nested/b.md", "notes.bin", "z.txt"]
        );
        assert_eq!(plan.summary(), "a.html\nnested/b.md\nnotes.bin\nz.txt\n");

        let report = folder.poll_once(&MapConverter::default()).expect("poll");
        assert_eq!(
            paths(&report),
            [
                ("a.html", FileOutcomeState::Converted),
                ("nested/b.md", FileOutcomeState::Converted),
                ("notes.bin", FileOutcomeState::Unsupported),
                ("z.txt", FileOutcomeState::Converted),
            ]
        );
        let first = &report.entries()[0];
        assert_eq!(first.output(), Some(&b"PDF:a"[..]));
        assert_eq!(first.output_name(), Some("a.pdf"));
        assert_eq!(first.summary(), "converted\ta.html");
        assert_eq!(report.count(FileOutcomeState::Converted), 3);
        assert_eq!(report.count(FileOutcomeState::Unsupported), 1);
        assert!(
            report.entries()[2]
                .message()
                .is_some_and(|message| message.contains("bin"))
        );
    }

    #[test]
    fn poll_is_idempotent_and_failed_files_are_retried() {
        let fixture = Fixture::new();
        fixture.write("good.txt", b"good");
        fixture.write("bad.html", b"bad");
        let mut folder = WatchedFolder::new(fixture.config()).expect("folder");
        let converter = MapConverter::default().fail("bad.html");

        let first = folder.poll_once(&converter).expect("first poll");
        assert_eq!(first.count(FileOutcomeState::Converted), 1);
        assert_eq!(first.count(FileOutcomeState::Failed), 1);
        assert_eq!(folder.markers().len(), 1);
        assert!(folder.markers().contains("good.txt"));
        assert!(!folder.markers().contains("bad.html"));

        let second = folder.poll_once(&converter).expect("second poll");
        assert_eq!(
            paths(&second),
            [
                ("bad.html", FileOutcomeState::Failed),
                ("good.txt", FileOutcomeState::Skipped),
            ]
        );
        assert_eq!(folder.markers().len(), 1);
        let converter = MapConverter::default();
        let third = folder.poll_once(&converter).expect("third poll");
        assert_eq!(third.count(FileOutcomeState::Converted), 1);
        assert_eq!(folder.markers().len(), 2);
    }

    #[test]
    fn non_recursive_mode_ignores_descendants() {
        let fixture = Fixture::new();
        fixture.write("top.txt", b"top");
        fixture.write("nested/deep.txt", b"deep");
        let config = fixture.config().non_recursive();
        let folder = WatchedFolder::new(config).expect("folder");
        assert_eq!(folder.plan().expect("plan").entries(), ["top.txt"]);
    }

    #[test]
    fn too_many_files_is_a_typed_error() {
        let fixture = Fixture::new();
        fixture.write("a.txt", b"a");
        fixture.write("b.txt", b"b");
        let folder = WatchedFolder::new(fixture.config().with_max_files(1)).expect("folder");
        assert!(matches!(
            folder.plan(),
            Err(WatchedFolderError::TooManyFiles { limit: 1 })
        ));
    }

    #[test]
    fn oversized_file_fails_only_its_outcome() {
        let fixture = Fixture::new();
        fixture.write("big.txt", b"0123456789");
        fixture.write("small.txt", b"ok");
        let config = fixture.config().with_max_file_bytes(4);
        let mut folder = WatchedFolder::new(config).expect("folder");
        let report = folder.poll_once(&PassthroughConverter).expect("poll");
        assert_eq!(
            paths(&report),
            [
                ("big.txt", FileOutcomeState::Failed),
                ("small.txt", FileOutcomeState::Converted),
            ]
        );
        assert!(
            report.entries()[0]
                .message()
                .is_some_and(|message| message.contains("too large"))
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_are_not_followed() {
        let fixture = Fixture::new();
        let outside = std::env::temp_dir().join(format!(
            "pdfxi-t0107-outside-{}-{}",
            std::process::id(),
            NEXT_DIR.fetch_add(1, Ordering::Relaxed)
        ));
        fs::write(&outside, b"secret").expect("write outside file");
        let link = fixture.path.join("link.txt");
        std::os::unix::fs::symlink(&outside, &link).expect("create symlink");
        let folder = WatchedFolder::new(fixture.config()).expect("folder");
        assert!(folder.plan().expect("plan").is_empty());
        let _ = fs::remove_file(&outside);
    }
}
