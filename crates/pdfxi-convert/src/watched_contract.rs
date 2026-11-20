//! Library-level watched-folder adapter contract for CREATE-014 (ADR 0008).
//!
//! The OS-bound watcher lives in the automation lane; this module deliberately
//! stays at the deterministic, file-system-free library layer. A caller
//! supplies a validated [`WatchedConfig`] and the files it has already
//! observed; [`build_watched_plan`] validates each candidate path, applies the
//! extension and size budgets, and returns a sorted
//! [`WatchedPlan`] with a stable summary. No background thread, OS watcher,
//! symlink resolution, or filesystem mutation happens here.

use std::path::{Component, Path};

/// Maximum number of configured extensions.
pub const WATCHED_MAX_EXTENSIONS: usize = 64;

/// Maximum length of a relative candidate path, in bytes.
pub const WATCHED_MAX_NAME_BYTES: usize = 1_024;

/// Maximum configured file count.
pub const WATCHED_MAX_FILES: usize = 100_000;

/// A typed failure while validating watched-folder configuration.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WatchedConfigError {
    /// The watch root was empty.
    #[error("watched-folder root must not be empty")]
    EmptyRoot,
    /// The watch root was not absolute.
    #[error("watched-folder root must be absolute")]
    RelativeRoot,
    /// The watch root contained a `..` component.
    #[error("watched-folder root must not contain traversal components")]
    Traversal,
    /// The watch root contained a NUL byte.
    #[error("watched-folder root must not contain a NUL byte")]
    NulByte,
    /// The watch root contained a `.` component.
    #[error("watched-folder root must not contain a current-directory component")]
    CurrentDir,
    /// No extensions were configured.
    #[error("watched-folder config needs at least one extension")]
    NoExtensions,
    /// Too many extensions were configured.
    #[error("watched-folder config exceeds {limit} extensions")]
    TooManyExtensions {
        /// Enforced extension budget.
        limit: usize,
    },
    /// An extension was malformed or duplicated.
    #[error("invalid watched-folder extension '{value}'")]
    InvalidExtension {
        /// The offending extension text.
        value: String,
    },
    /// A numeric budget was zero.
    #[error("watched-folder {field} limit must be positive")]
    ZeroLimit {
        /// Name of the offending field.
        field: &'static str,
    },
    /// A numeric budget was too large.
    #[error("watched-folder {field} limit exceeds {limit}")]
    LimitTooLarge {
        /// Name of the offending field.
        field: &'static str,
        /// Enforced upper bound.
        limit: u64,
    },
}

/// Validated-input description of a watched folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatchedConfig {
    root: String,
    extensions: Vec<String>,
    max_files: usize,
    max_bytes: u64,
    recursive: bool,
}

impl WatchedConfig {
    /// Create a configuration from explicit fields.
    #[must_use]
    pub fn new(
        root: impl Into<String>,
        extensions: Vec<String>,
        max_files: usize,
        max_bytes: u64,
        recursive: bool,
    ) -> Self {
        Self {
            root: root.into(),
            extensions,
            max_files,
            max_bytes,
            recursive,
        }
    }

    /// Create a single-extension configuration with generous budgets.
    #[must_use]
    pub fn simple(root: impl Into<String>, extension: &str) -> Self {
        Self::new(
            root,
            vec![extension.to_owned()],
            WATCHED_MAX_FILES,
            u64::MAX,
            false,
        )
    }

    /// Return the configured absolute root.
    #[must_use]
    pub fn root(&self) -> &str {
        &self.root
    }

    /// Return the configured extensions in caller-supplied order.
    #[must_use]
    pub fn extensions(&self) -> &[String] {
        &self.extensions
    }

    /// Return the configured file-count budget.
    #[must_use]
    pub const fn max_files(&self) -> usize {
        self.max_files
    }

    /// Return the configured per-file byte budget.
    #[must_use]
    pub const fn max_bytes(&self) -> u64 {
        self.max_bytes
    }

    /// Return whether descendants are included.
    #[must_use]
    pub const fn recursive(&self) -> bool {
        self.recursive
    }

    /// Validate the configuration without touching the file system.
    ///
    /// # Errors
    ///
    /// Returns the typed [`WatchedConfigError`] variants for an empty,
    /// relative, traversal-bearing, or NUL-containing root; for missing,
    /// duplicated, malformed, or over-budget extensions; and for zero or
    /// oversized numeric limits.
    pub fn validate(&self) -> Result<(), WatchedConfigError> {
        if self.root.is_empty() {
            return Err(WatchedConfigError::EmptyRoot);
        }
        if self.root.contains('\0') {
            return Err(WatchedConfigError::NulByte);
        }
        if !Path::new(&self.root).is_absolute() {
            return Err(WatchedConfigError::RelativeRoot);
        }
        for component in Path::new(&self.root).components() {
            match component {
                Component::ParentDir => return Err(WatchedConfigError::Traversal),
                Component::CurDir => return Err(WatchedConfigError::CurrentDir),
                Component::Normal(_) | Component::RootDir | Component::Prefix(_) => {}
            }
        }

        if self.extensions.is_empty() {
            return Err(WatchedConfigError::NoExtensions);
        }
        if self.extensions.len() > WATCHED_MAX_EXTENSIONS {
            return Err(WatchedConfigError::TooManyExtensions {
                limit: WATCHED_MAX_EXTENSIONS,
            });
        }
        let mut seen: Vec<String> = Vec::with_capacity(self.extensions.len());
        for extension in &self.extensions {
            let lower = extension.to_ascii_lowercase();
            if !is_safe_extension(extension) || seen.contains(&lower) {
                return Err(WatchedConfigError::InvalidExtension {
                    value: extension.clone(),
                });
            }
            seen.push(lower);
        }

        if self.max_files == 0 {
            return Err(WatchedConfigError::ZeroLimit { field: "max_files" });
        }
        if u64::try_from(self.max_files).is_err() {
            return Err(WatchedConfigError::LimitTooLarge {
                field: "max_files",
                limit: u64::MAX,
            });
        }
        if self.max_bytes == 0 {
            return Err(WatchedConfigError::ZeroLimit { field: "max_bytes" });
        }
        Ok(())
    }
}

/// Return whether an extension is a short ASCII alphanumeric token.
fn is_safe_extension(value: &str) -> bool {
    !value.is_empty() && value.len() <= 16 && value.bytes().all(|byte| byte.is_ascii_alphanumeric())
}

/// Validate a caller-supplied relative path.
fn validate_relative_path(path: &str) -> Result<(), &'static str> {
    if path.is_empty() {
        return Err("empty path");
    }
    if path.len() > WATCHED_MAX_NAME_BYTES {
        return Err("path too long");
    }
    if path.contains('\0') {
        return Err("nul byte in path");
    }
    if path.contains('\\') {
        return Err("backslash separator");
    }
    let candidate = Path::new(path);
    if candidate.is_absolute() {
        return Err("absolute candidate path");
    }
    for component in candidate.components() {
        match component {
            Component::Normal(_) => {}
            Component::ParentDir => return Err("parent traversal"),
            Component::CurDir | Component::RootDir | Component::Prefix(_) => {
                return Err("non-normal path component");
            }
        }
    }
    Ok(())
}

/// One caller-observed file awaiting a deterministic plan decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatchedCandidate {
    relative_path: String,
    bytes: u64,
}

impl WatchedCandidate {
    /// Describe one observed file by its relative path and size.
    #[must_use]
    pub fn new(relative_path: impl Into<String>, bytes: u64) -> Self {
        Self {
            relative_path: relative_path.into(),
            bytes,
        }
    }

    /// Return the normalized relative path.
    #[must_use]
    pub fn relative_path(&self) -> &str {
        &self.relative_path
    }

    /// Return the observed file size in bytes.
    #[must_use]
    pub const fn bytes(&self) -> u64 {
        self.bytes
    }
}

/// The deterministic decision recorded for one candidate file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WatchedDisposition {
    /// The candidate will be handed to a converter.
    Accepted,
    /// The candidate was rejected with a stable reason.
    Rejected {
        /// Stable machine-readable reason.
        reason: &'static str,
    },
}

impl WatchedDisposition {
    /// Return a stable label suitable for a deterministic summary.
    #[must_use]
    pub const fn label(&self) -> &'static str {
        match self {
            Self::Accepted => "accepted",
            Self::Rejected { reason } => reason,
        }
    }
}

/// One planned watched-folder file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedWatchedFile {
    relative_path: String,
    disposition: WatchedDisposition,
}

impl PlannedWatchedFile {
    /// Return the normalized relative path.
    #[must_use]
    pub fn relative_path(&self) -> &str {
        &self.relative_path
    }

    /// Return the recorded disposition.
    #[must_use]
    pub fn disposition(&self) -> &WatchedDisposition {
        &self.disposition
    }
}

/// A deterministic, host-consumable watched-folder plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatchedPlan {
    root: String,
    files: Vec<PlannedWatchedFile>,
    accepted: usize,
    rejected: usize,
    summary: String,
}

impl WatchedPlan {
    /// Configured absolute root.
    #[must_use]
    pub fn root(&self) -> &str {
        &self.root
    }

    /// Planned files sorted by relative path.
    #[must_use]
    pub fn files(&self) -> &[PlannedWatchedFile] {
        &self.files
    }

    /// Number of accepted candidates.
    #[must_use]
    pub const fn accepted(&self) -> usize {
        self.accepted
    }

    /// Number of rejected candidates.
    #[must_use]
    pub const fn rejected(&self) -> usize {
        self.rejected
    }

    /// Stable, byte-for-byte deterministic summary of the plan.
    #[must_use]
    pub fn summary(&self) -> &str {
        &self.summary
    }
}

/// A typed failure while planning a watched-folder poll.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WatchedPlanError {
    /// The configuration failed validation.
    #[error("invalid watched-folder config: {0}")]
    Config(#[from] WatchedConfigError),
    /// The caller supplied too many candidates.
    #[error("watched folder has {count} candidates (limit {limit})")]
    TooManyFiles {
        /// Number of candidates supplied.
        count: usize,
        /// Enforced budget.
        limit: usize,
    },
    /// Two candidates resolved to the same case-insensitive path.
    #[error("duplicate watched candidate path '{path}'")]
    DuplicatePath {
        /// The duplicated relative path.
        path: String,
    },
}

/// Extract the lowercase extension of a normalized relative path.
fn extension_of(path: &str) -> &str {
    let name = path.rsplit('/').next().unwrap_or(path);
    match name.rfind('.') {
        Some(index) => &name[index + 1..],
        None => "",
    }
}

/// Build a deterministic watched-folder plan from already-observed files.
///
/// The configuration is validated first. Each candidate path is checked for
/// traversal, absolute components, and unsupported characters; its extension
/// and size are checked against the configuration. Every candidate produces a
/// typed disposition, so a hostile file cannot abort the whole poll. The final
/// list is sorted by relative path and summarized deterministically.
///
/// # Errors
///
/// Returns [`WatchedPlanError`] for an invalid configuration, an over-budget
/// candidate count, or duplicate candidate paths.
pub fn build_watched_plan(
    config: &WatchedConfig,
    candidates: &[WatchedCandidate],
) -> Result<WatchedPlan, WatchedPlanError> {
    config.validate()?;
    if candidates.len() > config.max_files() {
        return Err(WatchedPlanError::TooManyFiles {
            count: candidates.len(),
            limit: config.max_files(),
        });
    }

    let mut seen: Vec<String> = Vec::with_capacity(candidates.len());
    let mut files: Vec<PlannedWatchedFile> = Vec::with_capacity(candidates.len());
    let mut accepted = 0usize;
    let mut rejected = 0usize;
    for candidate in candidates {
        let key = candidate.relative_path().to_ascii_lowercase();
        if seen.contains(&key) {
            return Err(WatchedPlanError::DuplicatePath {
                path: candidate.relative_path().to_owned(),
            });
        }
        seen.push(key);

        let reason = match validate_relative_path(candidate.relative_path()) {
            Err(reason) => Some(reason),
            Ok(()) => {
                let extension = extension_of(candidate.relative_path()).to_ascii_lowercase();
                if !config
                    .extensions()
                    .iter()
                    .any(|allowed| allowed.to_ascii_lowercase() == extension)
                {
                    Some("unsupported extension")
                } else if candidate.bytes() > config.max_bytes() {
                    Some("file too large")
                } else {
                    None
                }
            }
        };
        let disposition = match reason {
            Some(reason) => {
                rejected = rejected.saturating_add(1);
                WatchedDisposition::Rejected { reason }
            }
            None => {
                accepted = accepted.saturating_add(1);
                WatchedDisposition::Accepted
            }
        };
        files.push(PlannedWatchedFile {
            relative_path: candidate.relative_path().to_owned(),
            disposition,
        });
    }
    files.sort_by(|left, right| left.relative_path().cmp(right.relative_path()));

    let fragments: Vec<String> = files
        .iter()
        .map(|file| format!("{}:{}", file.relative_path(), file.disposition().label()))
        .collect();
    let summary = format!(
        "root={} recursive={} accepted={} rejected={} files=[{}]",
        config.root(),
        config.recursive(),
        accepted,
        rejected,
        fragments.join(", ")
    );

    Ok(WatchedPlan {
        root: config.root().to_owned(),
        files,
        accepted,
        rejected,
        summary,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn absolute_root() -> String {
        if cfg!(windows) {
            "C:\\pdfxi\\inbox".to_owned()
        } else {
            "/pdfxi/inbox".to_owned()
        }
    }

    #[test]
    fn watched_config_validation_rejects_relative_and_traversal_roots() {
        assert_eq!(
            WatchedConfig::simple("relative/inbox", "pdf").validate(),
            Err(WatchedConfigError::RelativeRoot)
        );
        assert_eq!(
            WatchedConfig::simple("", "pdf").validate(),
            Err(WatchedConfigError::EmptyRoot)
        );
        assert_eq!(
            WatchedConfig::simple(format!("{}\0x", absolute_root()), "pdf").validate(),
            Err(WatchedConfigError::NulByte)
        );
        let traversal = format!(
            "{}{}",
            absolute_root(),
            if cfg!(windows) {
                "\\..\\secret"
            } else {
                "/../secret"
            }
        );
        assert_eq!(
            WatchedConfig::simple(traversal, "pdf").validate(),
            Err(WatchedConfigError::Traversal)
        );
        assert!(
            WatchedConfig::simple(absolute_root(), "pdf")
                .validate()
                .is_ok()
        );
    }

    #[test]
    fn watched_config_validation_rejects_unsafe_extensions_and_zero_limits() {
        assert_eq!(
            WatchedConfig::simple(absolute_root(), "").validate(),
            Err(WatchedConfigError::InvalidExtension {
                value: String::new()
            })
        );
        assert_eq!(
            WatchedConfig::simple(absolute_root(), "p;df").validate(),
            Err(WatchedConfigError::InvalidExtension {
                value: "p;df".to_owned()
            })
        );
        let duplicate = WatchedConfig::new(
            absolute_root(),
            vec!["pdf".to_owned(), "PDF".to_owned()],
            10,
            100,
            false,
        );
        assert_eq!(
            duplicate.validate(),
            Err(WatchedConfigError::InvalidExtension {
                value: "PDF".to_owned()
            })
        );
        let zero_files = WatchedConfig::new(absolute_root(), vec!["pdf".to_owned()], 0, 100, false);
        assert_eq!(
            zero_files.validate(),
            Err(WatchedConfigError::ZeroLimit { field: "max_files" })
        );
        let many = vec!["pdf".to_owned(); WATCHED_MAX_EXTENSIONS + 1];
        assert_eq!(
            WatchedConfig::new(absolute_root(), many, 10, 100, false).validate(),
            Err(WatchedConfigError::TooManyExtensions {
                limit: WATCHED_MAX_EXTENSIONS
            })
        );
    }

    #[test]
    fn watched_plan_sorts_candidates_and_records_per_file_outcomes() {
        let config = WatchedConfig::new(
            absolute_root(),
            vec!["pdf".to_owned(), "txt".to_owned()],
            10,
            100,
            false,
        );
        let candidates = vec![
            WatchedCandidate::new("b.txt", 10),
            WatchedCandidate::new("z.exe", 10),
            WatchedCandidate::new("../escape.pdf", 10),
            WatchedCandidate::new("big.pdf", 101),
            WatchedCandidate::new("a.pdf", 10),
        ];
        let plan = build_watched_plan(&config, &candidates);
        assert!(plan.is_ok());
        let plan = plan.expect("plan");
        assert_eq!(plan.accepted(), 2);
        assert_eq!(plan.rejected(), 3);
        let paths: Vec<&str> = plan
            .files()
            .iter()
            .map(PlannedWatchedFile::relative_path)
            .collect();
        assert_eq!(
            paths,
            vec!["../escape.pdf", "a.pdf", "b.txt", "big.pdf", "z.exe"]
        );
        assert_eq!(plan.files()[0].disposition().label(), "parent traversal");
        assert_eq!(plan.files()[3].disposition().label(), "file too large");
        assert_eq!(
            plan.files()[4].disposition().label(),
            "unsupported extension"
        );
    }

    #[test]
    fn watched_plan_rejects_duplicate_paths_and_over_budget_inputs() {
        let config = WatchedConfig::new(absolute_root(), vec!["pdf".to_owned()], 1, 100, false);
        let over = vec![
            WatchedCandidate::new("a.pdf", 1),
            WatchedCandidate::new("b.pdf", 1),
        ];
        assert_eq!(
            build_watched_plan(&config, &over),
            Err(WatchedPlanError::TooManyFiles { count: 2, limit: 1 })
        );
        let generous = WatchedConfig::new(absolute_root(), vec!["pdf".to_owned()], 10, 100, false);
        let duplicates = vec![
            WatchedCandidate::new("a.pdf", 1),
            WatchedCandidate::new("A.PDF", 1),
        ];
        assert!(matches!(
            build_watched_plan(&generous, &duplicates),
            Err(WatchedPlanError::DuplicatePath { .. })
        ));
    }

    #[test]
    fn watched_plan_summary_is_byte_for_byte_deterministic() {
        let config = WatchedConfig::new(absolute_root(), vec!["pdf".to_owned()], 10, 100, true);
        let candidates = vec![
            WatchedCandidate::new("b.pdf", 2),
            WatchedCandidate::new("a.pdf", 1),
        ];
        let first = build_watched_plan(&config, &candidates);
        let second = build_watched_plan(&config, &candidates);
        assert_eq!(first, second);
        let first = first.expect("first plan");
        let second = second.expect("second plan");
        assert_eq!(first.summary(), second.summary());
        assert!(first.summary().contains("accepted=2"));
        assert!(first.summary().contains("recursive=true"));
    }
}
