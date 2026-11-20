//! Droplet and hot-folder batch-preflight planning (feature PRINT-008).
//!
//! This module models the *data* behind a droplet / watched-folder batch
//! preflight job. It deliberately performs **no filesystem I/O**: it never
//! watches a directory, opens a file, or checks whether a path exists. Instead
//! it validates path strings lexically and emits a [`WatchedPath`] newtype that
//! only a trusted *host boundary* may resolve into a real directory handle.
//! The host is responsible for access checks, symlink resolution and actually
//! observing the folder; this crate stays pure and deterministic.
//!
//! A [`FolderWatchConfig`] describes which folder to watch, where outputs go,
//! which [`PreflightProfile`] to apply, a file filter and a bounded poll
//! interval. The host feeds back per-file facts as [`FilePreflightInput`]
//! records; [`BatchPlan::build`] turns them into a deterministically ordered
//! [`FileOutcomePlan`] list and a [`BatchSummary`] with an FNV-1a digest so two
//! runs over the same facts are byte-identical.

/// Maximum length, in bytes, of a validated path string.
pub const MAX_PATH_BYTES: usize = 4096;
/// Maximum length, in bytes, of a config, profile or droplet name.
pub const MAX_NAME_BYTES: usize = 128;
/// Maximum length, in bytes, of a logical (relative) file name.
pub const MAX_LOGICAL_NAME_BYTES: usize = 1024;
/// Maximum number of files accepted in a single batch plan.
pub const MAX_BATCH_FILES: usize = 10_000;
/// Minimum accepted poll interval, in milliseconds.
pub const MIN_POLL_INTERVAL_MS: u32 = 100;
/// Maximum accepted poll interval, in milliseconds.
pub const MAX_POLL_INTERVAL_MS: u32 = 3_600_000;
/// Default poll interval, in milliseconds.
pub const DEFAULT_POLL_INTERVAL_MS: u32 = 2_000;

/// Structured errors produced while validating watch configs and batch plans.
#[derive(Debug, thiserror::Error, PartialEq)]
pub enum DropletError {
    /// A path string was empty.
    #[error("path must not be empty")]
    EmptyPath,
    /// A path string exceeded the byte-length cap.
    #[error("path length {len} exceeds the {limit}-byte limit")]
    PathTooLong {
        /// Actual byte length.
        len: usize,
        /// Enforced limit.
        limit: usize,
    },
    /// A path contained a NUL byte.
    #[error("path must not contain a NUL byte")]
    PathHasNul,
    /// A path contained a control character.
    #[error("path must not contain control byte 0x{byte:02X}")]
    PathHasControl {
        /// The rejected control byte.
        byte: u8,
    },
    /// A folder path was not absolute/rooted.
    #[error("folder path must be absolute (drive/UNC/POSIX root): {0}")]
    RelativePath(String),
    /// A path contained a `..` traversal component.
    #[error("path must not contain a '..' component: {0}")]
    TraversalComponent(String),
    /// A config, profile or droplet name was empty.
    #[error("name must not be empty")]
    EmptyName,
    /// A config, profile or droplet name exceeded the byte-length cap.
    #[error("name length {len} exceeds the {limit}-byte limit")]
    NameTooLong {
        /// Actual byte length.
        len: usize,
        /// Enforced limit.
        limit: usize,
    },
    /// A profile token was not a recognised built-in.
    #[error("unknown preflight profile: {0}")]
    UnknownProfile(String),
    /// A logical file name was empty.
    #[error("logical file name must not be empty")]
    EmptyLogicalName,
    /// A logical file name was not a relative identifier.
    #[error("logical file name must be relative: {0}")]
    LogicalNameNotRelative(String),
    /// A logical file name exceeded the byte-length cap.
    #[error("logical file name length {len} exceeds the {limit}-byte limit")]
    LogicalNameTooLong {
        /// Actual byte length.
        len: usize,
        /// Enforced limit.
        limit: usize,
    },
    /// Two inputs shared the same logical name.
    #[error("duplicate logical file name: {0}")]
    DuplicateLogicalName(String),
    /// The batch had more files than the configured cap.
    #[error("batch file count {count} exceeds the {limit}-file limit")]
    TooManyFiles {
        /// Actual file count.
        count: usize,
        /// Enforced limit.
        limit: usize,
    },
    /// The file filter pattern was empty.
    #[error("file pattern must not be empty")]
    EmptyFilePattern,
    /// The poll interval was outside the accepted range.
    #[error("poll interval {value} ms must be in {min}..={max} ms")]
    InvalidPollInterval {
        /// The rejected value.
        value: u32,
        /// Minimum accepted interval.
        min: u32,
        /// Maximum accepted interval.
        max: u32,
    },
}

/// Return `true` when `value` has a Windows drive, UNC, or POSIX root.
fn is_rooted(value: &str) -> bool {
    let bytes = value.as_bytes();
    match bytes.first() {
        Some(b'/') | Some(b'\\') => true,
        Some(drive) if drive.is_ascii_alphabetic() => {
            bytes.len() >= 3 && bytes[1] == b':' && (bytes[2] == b'\\' || bytes[2] == b'/')
        }
        _ => false,
    }
}

/// Validate a lexical path string without touching the filesystem.
fn validate_path(
    value: &str,
    empty: DropletError,
    too_long: fn(usize, usize) -> DropletError,
) -> Result<(), DropletError> {
    if value.is_empty() {
        return Err(empty);
    }
    let len = value.len();
    if len > MAX_PATH_BYTES {
        return Err(too_long(len, MAX_PATH_BYTES));
    }
    for &byte in value.as_bytes() {
        if byte == 0 {
            return Err(DropletError::PathHasNul);
        }
        if byte < 0x20 {
            return Err(DropletError::PathHasControl { byte });
        }
    }
    if !is_rooted(value) {
        return Err(DropletError::RelativePath(value.to_owned()));
    }
    if value.split(['/', '\\']).any(|component| component == "..") {
        return Err(DropletError::TraversalComponent(value.to_owned()));
    }
    Ok(())
}

/// A lexically validated path that a host boundary may resolve into a real
/// folder. Construction never checks existence, permissions, or symlinks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WatchedPath {
    value: String,
}

impl WatchedPath {
    /// Validate and wrap a rooted folder path string.
    ///
    /// # Errors
    ///
    /// Returns a typed [`DropletError`] for an empty/oversized path, a NUL or
    /// control byte, a relative path, or a `..` traversal component.
    pub fn new(value: &str) -> Result<Self, DropletError> {
        validate_path(value, DropletError::EmptyPath, |len, limit| {
            DropletError::PathTooLong { len, limit }
        })?;
        Ok(Self {
            value: value.to_owned(),
        })
    }

    /// Return the validated path string.
    pub fn as_str(&self) -> &str {
        &self.value
    }
}

impl std::fmt::Display for WatchedPath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.value)
    }
}

/// A preflight profile selected for a droplet or watched folder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PreflightProfile {
    /// No preflight; files are passed through.
    None,
    /// PDF/A-1b conformance.
    PdfA1b,
    /// PDF/A-2b conformance.
    PdfA2b,
    /// PDF/X-1a conformance.
    PdfX1a,
    /// PDF/X-4 conformance.
    PdfX4,
    /// A host-defined named profile (resolved by the host boundary).
    Custom(String),
}

impl PreflightProfile {
    /// Parse a built-in profile from a case-insensitive token.
    ///
    /// Accepts `none`/`off`/`passthrough`, `pdfa-1b`/`pdfa1b`,
    /// `pdfa-2b`/`pdfa2b`, `pdfx-1a`/`pdfx1a`, and `pdfx-4`/`pdfx4`.
    /// Use [`Self::custom`] for host-defined profiles.
    ///
    /// # Errors
    ///
    /// Returns [`DropletError::UnknownProfile`] for an unrecognised token.
    pub fn parse(value: &str) -> Result<Self, DropletError> {
        match value.trim().to_ascii_lowercase().as_str() {
            "none" | "off" | "passthrough" => Ok(Self::None),
            "pdfa-1b" | "pdfa1b" | "pdf/a-1b" => Ok(Self::PdfA1b),
            "pdfa-2b" | "pdfa2b" | "pdf/a-2b" => Ok(Self::PdfA2b),
            "pdfx-1a" | "pdfx1a" | "pdf/x-1a" => Ok(Self::PdfX1a),
            "pdfx-4" | "pdfx4" | "pdf/x-4" => Ok(Self::PdfX4),
            other => Err(DropletError::UnknownProfile(other.to_owned())),
        }
    }

    /// Build a validated host-defined named profile.
    ///
    /// # Errors
    ///
    /// Returns [`DropletError::EmptyName`] or [`DropletError::NameTooLong`].
    pub fn custom(name: &str) -> Result<Self, DropletError> {
        validate_name(name)?;
        Ok(Self::Custom(name.to_owned()))
    }

    /// Return the canonical profile name.
    pub fn name(&self) -> &str {
        match self {
            Self::None => "none",
            Self::PdfA1b => "pdfa-1b",
            Self::PdfA2b => "pdfa-2b",
            Self::PdfX1a => "pdfx-1a",
            Self::PdfX4 => "pdfx-4",
            Self::Custom(name) => name,
        }
    }

    /// Return a stable three-bit encoding of the profile choice.
    pub fn bits(&self) -> u8 {
        match self {
            Self::None => 0,
            Self::PdfA1b => 1,
            Self::PdfA2b => 2,
            Self::PdfX1a => 3,
            Self::PdfX4 => 4,
            Self::Custom(_) => 5,
        }
    }
}

/// Validate a human-readable name string.
fn validate_name(name: &str) -> Result<(), DropletError> {
    if name.trim().is_empty() {
        return Err(DropletError::EmptyName);
    }
    let len = name.len();
    if len > MAX_NAME_BYTES {
        return Err(DropletError::NameTooLong {
            len,
            limit: MAX_NAME_BYTES,
        });
    }
    Ok(())
}

/// A validated watched-folder / droplet configuration.
///
/// No field is resolved against the real filesystem in this crate. The host
/// boundary consumes the validated [`WatchedPath`] values to register an
/// actual watch, honouring its own path and access-control policy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FolderWatchConfig {
    name: String,
    watch_folder: WatchedPath,
    output_folder: WatchedPath,
    profile: PreflightProfile,
    file_pattern: String,
    recursive: bool,
    poll_interval_ms: u32,
}

impl FolderWatchConfig {
    /// Create and validate a folder-watch configuration.
    ///
    /// # Errors
    ///
    /// Returns a typed [`DropletError`] for an invalid name, path, empty file
    /// pattern, or out-of-range poll interval.
    pub fn new(
        name: &str,
        watch_folder: &str,
        output_folder: &str,
        profile: PreflightProfile,
        file_pattern: &str,
        recursive: bool,
        poll_interval_ms: u32,
    ) -> Result<Self, DropletError> {
        validate_name(name)?;
        if file_pattern.trim().is_empty() {
            return Err(DropletError::EmptyFilePattern);
        }
        if !(MIN_POLL_INTERVAL_MS..=MAX_POLL_INTERVAL_MS).contains(&poll_interval_ms) {
            return Err(DropletError::InvalidPollInterval {
                value: poll_interval_ms,
                min: MIN_POLL_INTERVAL_MS,
                max: MAX_POLL_INTERVAL_MS,
            });
        }
        Ok(Self {
            name: name.to_owned(),
            watch_folder: WatchedPath::new(watch_folder)?,
            output_folder: WatchedPath::new(output_folder)?,
            profile,
            file_pattern: file_pattern.to_owned(),
            recursive,
            poll_interval_ms,
        })
    }

    /// Create a configuration with the default poll interval.
    ///
    /// # Errors
    ///
    /// Propagates [`DropletError`] from [`Self::new`].
    pub fn simple(
        name: &str,
        watch_folder: &str,
        output_folder: &str,
        profile: PreflightProfile,
        file_pattern: &str,
        recursive: bool,
    ) -> Result<Self, DropletError> {
        Self::new(
            name,
            watch_folder,
            output_folder,
            profile,
            file_pattern,
            recursive,
            DEFAULT_POLL_INTERVAL_MS,
        )
    }

    /// Return the configuration name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Return the validated watch folder.
    pub fn watch_folder(&self) -> &WatchedPath {
        &self.watch_folder
    }

    /// Return the validated output folder.
    pub fn output_folder(&self) -> &WatchedPath {
        &self.output_folder
    }

    /// Return the selected preflight profile.
    pub fn profile(&self) -> &PreflightProfile {
        &self.profile
    }

    /// Return the file filter pattern.
    pub fn file_pattern(&self) -> &str {
        &self.file_pattern
    }

    /// Return whether subfolders are watched recursively.
    pub const fn recursive(&self) -> bool {
        self.recursive
    }

    /// Return the poll interval in milliseconds.
    pub const fn poll_interval_ms(&self) -> u32 {
        self.poll_interval_ms
    }
}

/// The preflight verdict reported by the host for one file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PreflightResult {
    /// The file passed preflight.
    Pass,
    /// The file failed preflight.
    Fail,
    /// The file was skipped (for example, it did not match the filter).
    Skipped,
}

impl PreflightResult {
    /// Return the canonical token for this verdict.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Fail => "fail",
            Self::Skipped => "skipped",
        }
    }

    /// Return a stable two-bit encoding of this verdict.
    pub const fn bits(self) -> u8 {
        match self {
            Self::Pass => 0,
            Self::Fail => 1,
            Self::Skipped => 2,
        }
    }
}

/// One per-file fact supplied by the host after running a real preflight.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FilePreflightInput {
    logical_name: String,
    result: PreflightResult,
    findings: Vec<String>,
}

impl FilePreflightInput {
    /// Create a per-file input, validating the logical name lexically.
    ///
    /// # Errors
    ///
    /// Returns a typed [`DropletError`] for an empty, oversized, rooted,
    /// control-containing or traversal logical name.
    pub fn new(
        logical_name: &str,
        result: PreflightResult,
        findings: Vec<String>,
    ) -> Result<Self, DropletError> {
        validate_logical_name(logical_name)?;
        Ok(Self {
            logical_name: logical_name.to_owned(),
            result,
            findings,
        })
    }

    /// Return the logical (relative) file name.
    pub fn logical_name(&self) -> &str {
        &self.logical_name
    }

    /// Return the host-reported verdict.
    pub const fn result(&self) -> PreflightResult {
        self.result
    }

    /// Return the preflight findings/messages.
    pub fn findings(&self) -> &[String] {
        &self.findings
    }
}

/// Validate a logical file name: a bounded, relative identifier with no NUL or
/// control bytes and no traversal component.
fn validate_logical_name(value: &str) -> Result<(), DropletError> {
    if value.trim().is_empty() {
        return Err(DropletError::EmptyLogicalName);
    }
    let len = value.len();
    if len > MAX_LOGICAL_NAME_BYTES {
        return Err(DropletError::LogicalNameTooLong {
            len,
            limit: MAX_LOGICAL_NAME_BYTES,
        });
    }
    for &byte in value.as_bytes() {
        if byte == 0 || byte < 0x20 {
            return Err(DropletError::LogicalNameNotRelative(value.to_owned()));
        }
    }
    if is_rooted(value) {
        return Err(DropletError::LogicalNameNotRelative(value.to_owned()));
    }
    if value.split(['/', '\\']).any(|component| component == "..") {
        return Err(DropletError::TraversalComponent(value.to_owned()));
    }
    Ok(())
}

/// The deterministic, backend/host-consumable plan for one file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileOutcomePlan {
    /// The logical file name this outcome applies to.
    pub logical_name: String,
    /// The profile that will be / was applied.
    pub profile: PreflightProfile,
    /// The planned verdict.
    pub result: PreflightResult,
    /// Findings in deterministic sorted order.
    pub findings: Vec<String>,
}

/// A deterministic aggregate over a [`BatchPlan`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BatchSummary {
    /// Total number of files in the batch.
    pub total_files: usize,
    /// Number of files that passed.
    pub passed: usize,
    /// Number of files that failed.
    pub failed: usize,
    /// Number of files that were skipped.
    pub skipped: usize,
    /// Whether every file passed (an empty batch is considered a pass).
    pub all_passed: bool,
    /// The profile applied to the whole batch.
    pub profile: PreflightProfile,
    /// The owning config name.
    pub config_name: String,
    /// FNV-1a digest over the canonical plan encoding.
    pub digest: u64,
}

/// A deterministic batch-preflight plan built entirely from in-memory facts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BatchPlan {
    /// The config name this plan belongs to.
    pub config_name: String,
    /// The validated watch folder (never resolved by this crate).
    pub watch_folder: WatchedPath,
    /// The profile applied to the whole batch.
    pub profile: PreflightProfile,
    /// Per-file outcome plans in deterministic logical-name order.
    pub files: Vec<FileOutcomePlan>,
    /// The aggregate summary.
    pub summary: BatchSummary,
}

impl BatchPlan {
    /// Build a deterministic batch plan from a config and per-file facts.
    ///
    /// Inputs are sorted by logical name, duplicate names are rejected, and the
    /// file count is bounded by [`MAX_BATCH_FILES`]. No filesystem call is made.
    ///
    /// # Errors
    ///
    /// Returns [`DropletError::TooManyFiles`] for an oversized batch.
    pub fn build(
        config: &FolderWatchConfig,
        inputs: &[FilePreflightInput],
    ) -> Result<Self, DropletError> {
        if inputs.len() > MAX_BATCH_FILES {
            return Err(DropletError::TooManyFiles {
                count: inputs.len(),
                limit: MAX_BATCH_FILES,
            });
        }

        let mut sorted: Vec<&FilePreflightInput> = inputs.iter().collect();
        sorted.sort_by(|a, b| a.logical_name.cmp(&b.logical_name));
        for pair in sorted.windows(2) {
            if pair[0].logical_name == pair[1].logical_name {
                return Err(DropletError::DuplicateLogicalName(
                    pair[0].logical_name.clone(),
                ));
            }
        }

        let mut files = Vec::with_capacity(sorted.len());
        let mut passed = 0usize;
        let mut failed = 0usize;
        let mut skipped = 0usize;
        let mut canonical = String::new();
        canonical.push_str(config.name());
        canonical.push('\u{1f}');
        canonical.push_str(config.profile().name());
        canonical.push('\u{1e}');

        for input in sorted {
            match input.result {
                PreflightResult::Pass => passed += 1,
                PreflightResult::Fail => failed += 1,
                PreflightResult::Skipped => skipped += 1,
            }
            let mut findings = input.findings.clone();
            findings.sort();
            canonical.push_str(&input.logical_name);
            canonical.push('\u{1f}');
            canonical.push_str(input.result.name());
            canonical.push('\u{1f}');
            canonical.push_str(&findings.join("\u{1d}"));
            canonical.push('\u{1e}');
            files.push(FileOutcomePlan {
                logical_name: input.logical_name.clone(),
                profile: config.profile().clone(),
                result: input.result,
                findings,
            });
        }

        let total_files = files.len();
        let summary = BatchSummary {
            total_files,
            passed,
            failed,
            skipped,
            all_passed: failed == 0 && skipped == 0,
            profile: config.profile().clone(),
            config_name: config.name().to_owned(),
            digest: fnv1a(canonical.as_bytes()),
        };

        Ok(Self {
            config_name: config.name().to_owned(),
            watch_folder: config.watch_folder().clone(),
            profile: config.profile().clone(),
            files,
            summary,
        })
    }
}

/// Compute a 64-bit FNV-1a hash, used as a deterministic plan digest.
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for &byte in bytes {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> FolderWatchConfig {
        FolderWatchConfig::simple(
            "Nightly",
            r"C:\watched\in",
            r"C:\watched\out",
            PreflightProfile::PdfA2b,
            "*.pdf",
            true,
        )
        .expect("valid config")
    }

    #[test]
    fn watched_path_accepts_rooted_paths() {
        assert_eq!(
            WatchedPath::new(r"C:\watched\in").expect("drive").as_str(),
            r"C:\watched\in"
        );
        assert!(WatchedPath::new(r"\\server\share\in").is_ok());
        assert!(WatchedPath::new("/var/spool/in").is_ok());
    }

    #[test]
    fn watched_path_rejects_unsafe_inputs() {
        assert_eq!(WatchedPath::new(""), Err(DropletError::EmptyPath));
        assert!(matches!(
            WatchedPath::new("relative/folder"),
            Err(DropletError::RelativePath(_))
        ));
        assert!(matches!(
            WatchedPath::new(r"C:\watched\..\secret"),
            Err(DropletError::TraversalComponent(_))
        ));
        assert!(matches!(
            WatchedPath::new("C:\\bad\0path"),
            Err(DropletError::PathHasNul)
        ));
        assert!(matches!(
            WatchedPath::new("C:\\bad\npath"),
            Err(DropletError::PathHasControl { .. })
        ));
        assert_eq!(
            WatchedPath::new(&"x".repeat(MAX_PATH_BYTES + 1)),
            Err(DropletError::PathTooLong {
                len: MAX_PATH_BYTES + 1,
                limit: MAX_PATH_BYTES
            })
        );
    }

    #[test]
    fn preflight_profiles_parse_and_custom() {
        assert_eq!(
            PreflightProfile::parse("PDF/A-2b"),
            Ok(PreflightProfile::PdfA2b)
        );
        assert_eq!(PreflightProfile::parse("off"), Ok(PreflightProfile::None));
        assert_eq!(
            PreflightProfile::parse("pdfx1a"),
            Ok(PreflightProfile::PdfX1a)
        );
        assert!(matches!(
            PreflightProfile::parse("mystery"),
            Err(DropletError::UnknownProfile(_))
        ));
        assert_eq!(
            PreflightProfile::custom("House Standard")
                .expect("custom")
                .name(),
            "House Standard"
        );
        assert_eq!(PreflightProfile::custom("  "), Err(DropletError::EmptyName));
        assert_eq!(PreflightProfile::None.bits(), 0);
        assert_eq!(PreflightProfile::Custom("x".to_owned()).bits(), 5);
    }

    #[test]
    fn folder_watch_config_validation() {
        let cfg = config();
        assert_eq!(cfg.name(), "Nightly");
        assert_eq!(cfg.file_pattern(), "*.pdf");
        assert!(cfg.recursive());
        assert_eq!(cfg.poll_interval_ms(), DEFAULT_POLL_INTERVAL_MS);
        assert_eq!(cfg.watch_folder().as_str(), r"C:\watched\in");
        assert_eq!(cfg.profile(), &PreflightProfile::PdfA2b);

        assert!(
            FolderWatchConfig::simple(
                "",
                "C:\\in",
                "C:\\out",
                PreflightProfile::None,
                "*.pdf",
                false
            )
            .is_err()
        );
        assert!(matches!(
            FolderWatchConfig::new(
                "x",
                "relative",
                "C:\\out",
                PreflightProfile::None,
                "*.pdf",
                false,
                1000
            ),
            Err(DropletError::RelativePath(_))
        ));
        assert_eq!(
            FolderWatchConfig::new(
                "x",
                "C:\\in",
                "C:\\out",
                PreflightProfile::None,
                "  ",
                false,
                1000
            ),
            Err(DropletError::EmptyFilePattern)
        );
        assert!(matches!(
            FolderWatchConfig::new(
                "x",
                "C:\\in",
                "C:\\out",
                PreflightProfile::None,
                "*",
                false,
                99
            ),
            Err(DropletError::InvalidPollInterval { .. })
        ));
    }

    #[test]
    fn batch_plan_is_sorted_deterministic_and_summarised() {
        let cfg = config();
        let inputs = vec![
            FilePreflightInput::new(
                "z.pdf",
                PreflightResult::Fail,
                vec!["missing font".to_owned(), "bad box".to_owned()],
            )
            .expect("z"),
            FilePreflightInput::new("a.pdf", PreflightResult::Pass, vec![]).expect("a"),
            FilePreflightInput::new(
                "m.pdf",
                PreflightResult::Skipped,
                vec!["no match".to_owned()],
            )
            .expect("m"),
        ];
        let a = BatchPlan::build(&cfg, &inputs).expect("plan a");
        let b = BatchPlan::build(&cfg, &inputs).expect("plan b");
        assert_eq!(a, b);
        assert_eq!(
            a.files
                .iter()
                .map(|f| f.logical_name.as_str())
                .collect::<Vec<_>>(),
            vec!["a.pdf", "m.pdf", "z.pdf"]
        );
        // Findings are sorted within a file.
        assert_eq!(
            a.files[2].findings,
            vec!["bad box".to_owned(), "missing font".to_owned()]
        );
        assert_eq!(a.summary.total_files, 3);
        assert_eq!(a.summary.passed, 1);
        assert_eq!(a.summary.failed, 1);
        assert_eq!(a.summary.skipped, 1);
        assert!(!a.summary.all_passed);
        assert_eq!(a.summary.config_name, "Nightly");
        assert_eq!(a.summary.profile, PreflightProfile::PdfA2b);
        assert_eq!(a.summary.digest, b.summary.digest);
        assert_eq!(a.watch_folder, *cfg.watch_folder());
    }

    #[test]
    fn empty_batch_is_a_deterministic_pass() {
        let cfg = config();
        let a = BatchPlan::build(&cfg, &[]).expect("empty a");
        let b = BatchPlan::build(&cfg, &[]).expect("empty b");
        assert_eq!(a, b);
        assert!(a.files.is_empty());
        assert_eq!(a.summary.total_files, 0);
        assert!(a.summary.all_passed);
        assert_eq!(a.summary.digest, b.summary.digest);
    }

    #[test]
    fn invalid_and_duplicate_logical_names_are_rejected() {
        assert_eq!(
            FilePreflightInput::new("  ", PreflightResult::Pass, vec![]),
            Err(DropletError::EmptyLogicalName)
        );
        assert!(matches!(
            FilePreflightInput::new("C:\\absolute.pdf", PreflightResult::Pass, vec![]),
            Err(DropletError::LogicalNameNotRelative(_))
        ));
        assert!(matches!(
            FilePreflightInput::new("../escape.pdf", PreflightResult::Pass, vec![]),
            Err(DropletError::TraversalComponent(_))
        ));
        assert_eq!(
            FilePreflightInput::new(
                &"x".repeat(MAX_LOGICAL_NAME_BYTES + 1),
                PreflightResult::Pass,
                vec![]
            ),
            Err(DropletError::LogicalNameTooLong {
                len: MAX_LOGICAL_NAME_BYTES + 1,
                limit: MAX_LOGICAL_NAME_BYTES
            })
        );

        let dup = vec![
            FilePreflightInput::new("a.pdf", PreflightResult::Pass, vec![]).expect("a"),
            FilePreflightInput::new("a.pdf", PreflightResult::Pass, vec![]).expect("a again"),
        ];
        assert_eq!(
            BatchPlan::build(&config(), &dup),
            Err(DropletError::DuplicateLogicalName("a.pdf".to_owned()))
        );
    }

    #[test]
    fn oversized_batch_is_rejected_without_allocating() {
        // Build only two distinct names but claim a large count by repeating
        // unique logical names up to the cap plus one.
        let inputs: Vec<FilePreflightInput> = (0..=MAX_BATCH_FILES)
            .map(|i| {
                FilePreflightInput::new(&format!("f{i:05}.pdf"), PreflightResult::Pass, vec![])
                    .expect("named input")
            })
            .collect();
        assert_eq!(
            BatchPlan::build(&config(), &inputs),
            Err(DropletError::TooManyFiles {
                count: MAX_BATCH_FILES + 1,
                limit: MAX_BATCH_FILES
            })
        );
    }
}
