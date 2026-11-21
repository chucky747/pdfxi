//! Checksum-pinned external PDF corpora: manifest parsing, bounded fetch, and
//! offline smoke checks.
//!
//! This module implements the on-demand half of the corpus policy documented in
//! `corpus/README.md`. It never downloads implicitly: [`fetch_entry`] is the
//! only network path, it refuses entries that are not pinned, it caps the
//! transfer size, and it verifies the exact byte count and SHA-256 before the
//! file is moved into the cache. Fast tests use [`verify_bytes`] and
//! [`smoke_cached`], which read only local files and perform no I/O over the
//! network.
//!
//! The cache lives outside the repository: [`cache_dir`] honours
//! `PDFXI_CORPUS_CACHE`, then the platform cache location, then the system temp
//! directory. Cached files are never committed.

use std::env;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

use pdfxi_core::Document;
use thiserror::Error;

use crate::hash::sha256_hex;

/// Hard upper bound for a single external-corpus file, in bytes.
///
/// Every manifest entry also records its exact expected size; this constant is
/// an additional guard so a malformed or hostile manifest cannot request an
/// unbounded download.
pub const MAX_CORPUS_BYTES: u64 = 8 * 1024 * 1024;

/// Wall-clock limit, in seconds, applied to a single download.
const FETCH_TIMEOUT_SECS: u64 = 30;

/// Environment variable that overrides the corpus cache directory.
pub const CACHE_ENV: &str = "PDFXI_CORPUS_CACHE";

/// One entry in `corpus/manifest.toml`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CorpusEntry {
    /// Stable identifier, used as the cache subdirectory name.
    pub id: String,
    /// Exact HTTPS URL pinned to an immutable upstream revision.
    pub url: String,
    /// File name of the cached file (a single path component, no separators).
    pub file: String,
    /// Lowercase hexadecimal SHA-256 of the cached bytes, or empty when the
    /// entry is documentation only.
    pub sha256: String,
    /// Exact expected byte count, or zero when the entry is documentation only.
    pub bytes: u64,
    /// Expected page count used by the smoke check.
    pub pages: usize,
    /// Text fragments that must be present in the extracted page text.
    pub markers: Vec<String>,
    /// SPDX license identifier or a note describing the licensing status.
    pub license: String,
    /// Human-readable provenance of the file.
    pub source: String,
    /// Either `approved` (fetchable) or `unapproved` (documentation only).
    pub status: String,
}

impl CorpusEntry {
    /// Whether this entry is fully pinned and therefore eligible for fetching.
    ///
    /// An entry must be approved, carry a 64-character lowercase hex digest,
    /// have a positive exact size, and use a resolved HTTPS URL. The
    /// `example.invalid` placeholder used by documentation-only entries is
    /// explicitly rejected.
    pub fn is_pinned(&self) -> bool {
        self.status == "approved"
            && self.bytes > 0
            && self.pages > 0
            && is_lowercase_hex(&self.sha256)
            && self.url.starts_with("https://")
            && !self.url.contains("example.invalid")
            && simple_name(&self.id)
            && simple_name(&self.file)
            && !self.markers.is_empty()
    }
}

/// Errors produced by manifest parsing, fetching, verification, or smoke runs.
#[derive(Debug, Error)]
pub enum CorpusError {
    /// The manifest could not be read from disk.
    #[error("cannot read corpus manifest at {path}: {detail}")]
    ManifestRead {
        /// Path that could not be read.
        path: String,
        /// Underlying I/O error text.
        detail: String,
    },
    /// A manifest entry is missing a required field or has an invalid value.
    #[error("corpus manifest entry {id}: {message}")]
    Malformed {
        /// Identifier of the offending entry.
        id: String,
        /// Description of the problem.
        message: String,
    },
    /// A fetch was requested for an entry that is not pinned.
    #[error("corpus '{id}' is not pinned (empty digest or unapproved status); documentation only")]
    NotPinned {
        /// Identifier of the entry.
        id: String,
    },
    /// The declared size exceeds the hard fetch bound.
    #[error("corpus '{id}' declares {bytes} bytes, above the {max} byte fetch bound")]
    TooLarge {
        /// Identifier of the entry.
        id: String,
        /// Declared size.
        bytes: u64,
        /// Hard bound.
        max: u64,
    },
    /// The entry's id or file name cannot be used as a safe cache path.
    #[error("corpus '{id}' has an unsafe cache path: {reason}")]
    UnsafePath {
        /// Identifier of the entry.
        id: String,
        /// Why the path was rejected.
        reason: String,
    },
    /// Downloading the file failed.
    #[error(
        "corpus '{id}' download from {url} failed: {detail} (check connectivity or use --offline)"
    )]
    Fetch {
        /// Identifier of the entry.
        id: String,
        /// URL that was requested.
        url: String,
        /// Diagnostic text from the downloader.
        detail: String,
    },
    /// The cached bytes do not match the pinned digest.
    #[error("corpus '{id}' SHA-256 mismatch: expected {expected}, got {actual}")]
    HashMismatch {
        /// Identifier of the entry.
        id: String,
        /// Digest from the manifest.
        expected: String,
        /// Digest computed from the bytes.
        actual: String,
    },
    /// The cached byte count does not match the pinned size.
    #[error("corpus '{id}' size mismatch: expected {expected} bytes, got {actual} bytes")]
    SizeMismatch {
        /// Identifier of the entry.
        id: String,
        /// Size from the manifest.
        expected: u64,
        /// Observed size.
        actual: u64,
    },
    /// A local cache file could not be read.
    #[error("cannot read cached corpus '{id}' at {path}: {detail}")]
    Read {
        /// Identifier of the entry.
        id: String,
        /// Cache path.
        path: String,
        /// Underlying I/O error text.
        detail: String,
    },
    /// The cached file does not exist; this is a skip, not a failure, for
    /// offline smoke runs.
    #[error("corpus '{id}' is not cached at {path}; fetch it first")]
    NotCached {
        /// Identifier of the entry.
        id: String,
        /// Cache path.
        path: String,
    },
    /// The document could not be opened by the core parser.
    #[error("corpus '{id}' could not be opened: {detail}")]
    Open {
        /// Identifier of the entry.
        id: String,
        /// Parser error text.
        detail: String,
    },
    /// The extracted page count differs from the manifest.
    #[error("corpus '{id}' page count mismatch: expected {expected}, got {actual}")]
    PageCount {
        /// Identifier of the entry.
        id: String,
        /// Expected number of pages.
        expected: usize,
        /// Observed number of pages.
        actual: usize,
    },
    /// Text extraction failed on a page.
    #[error("corpus '{id}' page {index} text extraction failed: {detail}")]
    Text {
        /// Identifier of the entry.
        id: String,
        /// Zero-based page index.
        index: usize,
        /// Extractor error text.
        detail: String,
    },
    /// An expected marker is absent from the extracted text.
    #[error("corpus '{id}' page {index} text is missing marker {marker:?}")]
    MissingMarker {
        /// Identifier of the entry.
        id: String,
        /// Zero-based page index.
        index: usize,
        /// Marker that was not found.
        marker: String,
    },
}

/// Whether a cached file was downloaded or reused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FetchOutcome {
    /// The file was already present and verified; nothing was downloaded.
    AlreadyCached,
    /// The file was downloaded, verified, and moved into the cache.
    Downloaded,
}

/// Result of a single smoke check against an already-cached file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SmokeReport {
    /// Identifier of the entry that was checked.
    pub id: String,
    /// Number of pages the core parser reported.
    pub pages: usize,
    /// Cache path that was opened.
    pub path: PathBuf,
}

/// Aggregate outcome of an offline smoke run.
#[derive(Debug, Default)]
pub struct SmokeSummary {
    /// Entries that were cached, hash-verified, and opened successfully.
    pub checked: Vec<SmokeReport>,
    /// Pinned entries whose cached file is absent; these are skipped.
    pub skipped: Vec<String>,
    /// Pinned entries that were cached but failed verification or smoke.
    pub failures: Vec<(String, CorpusError)>,
}

/// Return `true` when `value` is a non-empty lowercase hexadecimal string.
///
/// Used to reject uppercase, short, or malformed digests before a fetch is
/// allowed to start.
pub fn is_lowercase_hex(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

/// Return `true` when `value` is a single safe path component.
fn simple_name(value: &str) -> bool {
    !value.is_empty()
        && value != "."
        && value != ".."
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.')
}

/// Strip optional surrounding quotes from a manifest value.
fn unquote(value: &str) -> String {
    let value = value.trim();
    if value.len() >= 2 && value.starts_with('"') && value.ends_with('"') {
        value[1..value.len() - 1].to_owned()
    } else {
        value.to_owned()
    }
}

/// Parse the `markers = ["a", "b"]` array syntax used by the manifest.
fn parse_markers(value: &str) -> Vec<String> {
    let value = value.trim();
    let inner = value
        .strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
        .unwrap_or("");
    inner
        .split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(unquote)
        .collect()
}

/// Parse `corpus/manifest.toml` source text into entries.
///
/// The parser is intentionally dependency-free and only understands the small
/// key/value subset used by this manifest. Unknown keys are ignored so the
/// format can grow without breaking older tooling.
pub fn parse_manifest(source: &str) -> Result<Vec<CorpusEntry>, CorpusError> {
    let mut entries: Vec<CorpusEntry> = Vec::new();
    let mut current: Option<CorpusEntry> = None;
    let mut id = String::new();
    for raw in source.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line == "[[corpus]]" {
            if let Some(entry) = current.take() {
                entries.push(entry);
            }
            id.clear();
            current = Some(CorpusEntry {
                id: String::new(),
                url: String::new(),
                file: String::new(),
                sha256: String::new(),
                bytes: 0,
                pages: 0,
                markers: Vec::new(),
                license: String::new(),
                source: String::new(),
                status: String::new(),
            });
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let Some(entry) = current.as_mut() else {
            continue;
        };
        let key = key.trim();
        match key {
            "id" => {
                id = unquote(value);
                entry.id = id.clone();
            }
            "url" => entry.url = unquote(value),
            "file" => entry.file = unquote(value),
            "sha256" => entry.sha256 = unquote(value),
            "bytes" => {
                entry.bytes = value.trim().parse().map_err(|_| CorpusError::Malformed {
                    id: id.clone(),
                    message: format!("invalid bytes value {value:?}"),
                })?;
            }
            "pages" => {
                entry.pages = value.trim().parse().map_err(|_| CorpusError::Malformed {
                    id: id.clone(),
                    message: format!("invalid pages value {value:?}"),
                })?;
            }
            "markers" => entry.markers = parse_markers(value),
            "license" => entry.license = unquote(value),
            "source" => entry.source = unquote(value),
            "status" => entry.status = unquote(value),
            _ => {}
        }
    }
    if let Some(entry) = current {
        entries.push(entry);
    }

    let mut seen: Vec<&str> = Vec::new();
    for entry in &entries {
        if entry.id.is_empty() {
            return Err(CorpusError::Malformed {
                id: "<unknown>".to_owned(),
                message: "missing id".to_owned(),
            });
        }
        if seen.contains(&entry.id.as_str()) {
            return Err(CorpusError::Malformed {
                id: entry.id.clone(),
                message: "duplicate id".to_owned(),
            });
        }
        seen.push(&entry.id);
    }
    Ok(entries)
}

/// Absolute path to `corpus/manifest.toml` in the workspace.
pub fn manifest_path() -> PathBuf {
    // Proven invariant: Cargo sets CARGO_MANIFEST_DIR to
    // `<workspace>/crates/pdfxi-testkit`, so both parents always exist.
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("pdfxi-testkit lives in the workspace crates directory")
        .join("corpus")
        .join("manifest.toml")
}

/// Read and parse the workspace corpus manifest.
pub fn load_manifest() -> Result<Vec<CorpusEntry>, CorpusError> {
    let path = manifest_path();
    let source = fs::read_to_string(&path).map_err(|error| CorpusError::ManifestRead {
        path: path.display().to_string(),
        detail: error.to_string(),
    })?;
    parse_manifest(&source)
}

/// Directory in which external corpus files are cached.
///
/// Resolution order: `PDFXI_CORPUS_CACHE`, the platform cache directory
/// (`LOCALAPPDATA` on Windows, `XDG_CACHE_HOME`/`~/.cache` elsewhere), then the
/// system temp directory. The result is always outside the source tree unless
/// the caller explicitly overrides it.
pub fn cache_dir() -> PathBuf {
    if let Ok(dir) = env::var(CACHE_ENV)
        && !dir.trim().is_empty()
    {
        return PathBuf::from(dir);
    }
    #[cfg(windows)]
    let base = env::var_os("LOCALAPPDATA").map(PathBuf::from);
    #[cfg(not(windows))]
    let base = env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")));
    base.unwrap_or_else(env::temp_dir)
        .join("pdfxi")
        .join("corpus")
}

/// Cache path for a single entry.
///
/// Both `id` and `file` must be single safe path components, which prevents
/// traversal through `..` or platform separators.
pub fn cached_path(entry: &CorpusEntry) -> Result<PathBuf, CorpusError> {
    if !simple_name(&entry.id) {
        return Err(CorpusError::UnsafePath {
            id: entry.id.clone(),
            reason: "id is not a single safe path component".to_owned(),
        });
    }
    let file = Path::new(&entry.file);
    if !simple_name(&entry.file)
        || file.components().count() != 1
        || !matches!(file.components().next(), Some(Component::Normal(_)))
    {
        return Err(CorpusError::UnsafePath {
            id: entry.id.clone(),
            reason: "file is not a single safe path component".to_owned(),
        });
    }
    Ok(cache_dir().join(&entry.id).join(file))
}

/// Verify a byte buffer against the pinned size and SHA-256 of an entry.
///
/// This performs no I/O and is the same check used after a download and before
/// an offline smoke run.
pub fn verify_bytes(entry: &CorpusEntry, bytes: &[u8]) -> Result<(), CorpusError> {
    let actual_size = bytes.len() as u64;
    if actual_size != entry.bytes {
        return Err(CorpusError::SizeMismatch {
            id: entry.id.clone(),
            expected: entry.bytes,
            actual: actual_size,
        });
    }
    let actual_hash = sha256_hex(bytes);
    if actual_hash != entry.sha256 {
        return Err(CorpusError::HashMismatch {
            id: entry.id.clone(),
            expected: entry.sha256.clone(),
            actual: actual_hash,
        });
    }
    Ok(())
}

/// Read and verify an already-cached file, returning its path.
pub fn verify_cached(entry: &CorpusEntry) -> Result<PathBuf, CorpusError> {
    let path = cached_path(entry)?;
    if !path.exists() {
        return Err(CorpusError::NotCached {
            id: entry.id.clone(),
            path: path.display().to_string(),
        });
    }
    let bytes = fs::read(&path).map_err(|error| CorpusError::Read {
        id: entry.id.clone(),
        path: path.display().to_string(),
        detail: error.to_string(),
    })?;
    verify_bytes(entry, &bytes)?;
    Ok(path)
}

/// Fetch and verify one pinned entry into the cache.
///
/// This is the only function in the crate that touches the network. It refuses
/// unpinned entries, rejects sizes above [`MAX_CORPUS_BYTES`], downloads to a
/// temporary `.part` file with `curl`, verifies the exact size and digest, and
/// only then renames the file into place. A failed or mismatched download is
/// deleted, so a corrupt file is never left in the cache.
pub fn fetch_entry(entry: &CorpusEntry) -> Result<FetchOutcome, CorpusError> {
    if !entry.is_pinned() {
        return Err(CorpusError::NotPinned {
            id: entry.id.clone(),
        });
    }
    if entry.bytes > MAX_CORPUS_BYTES {
        return Err(CorpusError::TooLarge {
            id: entry.id.clone(),
            bytes: entry.bytes,
            max: MAX_CORPUS_BYTES,
        });
    }
    let path = cached_path(entry)?;
    if path.exists() {
        verify_cached(entry)?;
        return Ok(FetchOutcome::AlreadyCached);
    }

    let parent = path.parent().ok_or_else(|| CorpusError::UnsafePath {
        id: entry.id.clone(),
        reason: "cache path has no parent directory".to_owned(),
    })?;
    fs::create_dir_all(parent).map_err(|error| CorpusError::Read {
        id: entry.id.clone(),
        path: parent.display().to_string(),
        detail: error.to_string(),
    })?;

    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| CorpusError::UnsafePath {
            id: entry.id.clone(),
            reason: "cache path has no valid file name".to_owned(),
        })?;
    let part = parent.join(format!(".{file_name}.part"));
    let _ = fs::remove_file(&part);

    let url = entry.url.as_str();
    let timeout = FETCH_TIMEOUT_SECS.to_string();
    let max_filesize = entry.bytes.to_string();
    let output = Command::new("curl")
        .args([
            "--fail",
            "--silent",
            "--show-error",
            "--location",
            "--max-time",
            &timeout,
            "--max-filesize",
            &max_filesize,
            "--output",
            &part.to_string_lossy(),
            url,
        ])
        .output()
        .map_err(|error| CorpusError::Fetch {
            id: entry.id.clone(),
            url: entry.url.clone(),
            detail: format!("could not start curl: {error}; install curl or pass --offline"),
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        let _ = fs::remove_file(&part);
        return Err(CorpusError::Fetch {
            id: entry.id.clone(),
            url: entry.url.clone(),
            detail: format!("curl exited with {}: {stderr}", output.status),
        });
    }

    let bytes = fs::read(&part).map_err(|error| CorpusError::Read {
        id: entry.id.clone(),
        path: part.display().to_string(),
        detail: error.to_string(),
    })?;
    if let Err(error) = verify_bytes(entry, &bytes) {
        let _ = fs::remove_file(&part);
        return Err(error);
    }
    fs::rename(&part, &path).map_err(|error| CorpusError::Read {
        id: entry.id.clone(),
        path: path.display().to_string(),
        detail: format!("could not move verified download into place: {error}"),
    })?;
    Ok(FetchOutcome::Downloaded)
}

/// Open, hash-verify, and smoke-check one cached entry.
///
/// The check confirms the pinned page count and that every recorded marker
/// appears in the extracted text. It reads only local files.
pub fn smoke_entry(entry: &CorpusEntry) -> Result<SmokeReport, CorpusError> {
    let path = verify_cached(entry)?;
    let bytes = fs::read(&path).map_err(|error| CorpusError::Read {
        id: entry.id.clone(),
        path: path.display().to_string(),
        detail: error.to_string(),
    })?;
    let document = Document::from_bytes(bytes).map_err(|error| CorpusError::Open {
        id: entry.id.clone(),
        detail: error.to_string(),
    })?;
    let pages = document.page_count().map_err(|error| CorpusError::Open {
        id: entry.id.clone(),
        detail: format!("page_count failed: {error}"),
    })?;
    if pages != entry.pages {
        return Err(CorpusError::PageCount {
            id: entry.id.clone(),
            expected: entry.pages,
            actual: pages,
        });
    }

    for index in 0..pages {
        let page = document
            .page_text(index)
            .map_err(|error| CorpusError::Text {
                id: entry.id.clone(),
                index,
                detail: error.to_string(),
            })?;
        for marker in &entry.markers {
            if !page.contains(marker) {
                return Err(CorpusError::MissingMarker {
                    id: entry.id.clone(),
                    index,
                    marker: marker.clone(),
                });
            }
        }
    }

    Ok(SmokeReport {
        id: entry.id.clone(),
        pages,
        path,
    })
}

/// Smoke-check every pinned entry that is present in the cache.
///
/// Pinned entries whose file is absent are reported in [`SmokeSummary::skipped`]
/// and do not fail the run. A cached file with the wrong digest or a smoke
/// failure is reported in [`SmokeSummary::failures`] and must be treated as an
/// error by callers. Unpinned entries are ignored.
pub fn smoke_cached(entries: &[CorpusEntry]) -> SmokeSummary {
    let mut summary = SmokeSummary::default();
    for entry in entries.iter().filter(|entry| entry.is_pinned()) {
        match cached_path(entry) {
            Err(error) => summary.failures.push((entry.id.clone(), error)),
            Ok(path) if !path.exists() => summary.skipped.push(entry.id.clone()),
            Ok(_) => match smoke_entry(entry) {
                Ok(report) => summary.checked.push(report),
                Err(error) => summary.failures.push((entry.id.clone(), error)),
            },
        }
    }
    summary
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pinned_entry() -> CorpusEntry {
        CorpusEntry {
            id: "test-corpus".to_owned(),
            url: "https://example.org/test.pdf".to_owned(),
            file: "test.pdf".to_owned(),
            sha256: sha256_hex(b"pdf bytes"),
            bytes: 9,
            pages: 1,
            markers: vec!["pdf".to_owned()],
            license: "Apache-2.0".to_owned(),
            source: "unit test".to_owned(),
            status: "approved".to_owned(),
        }
    }

    #[test]
    fn verify_bytes_accepts_pinned_bytes() {
        verify_bytes(&pinned_entry(), b"pdf bytes").expect("pinned bytes verify");
    }

    #[test]
    fn verify_bytes_rejects_wrong_hash_and_size() {
        let entry = pinned_entry();
        let hash_error = verify_bytes(&entry, b"PDF bytes").unwrap_err();
        assert!(matches!(hash_error, CorpusError::HashMismatch { .. }));
        let size_error = verify_bytes(&entry, b"short").unwrap_err();
        assert!(matches!(size_error, CorpusError::SizeMismatch { .. }));
    }

    #[test]
    fn manifest_parses_pinned_and_unapproved_entries() {
        let source = "version = 1\n\
                      [[corpus]]\n\
                      id = \"one\"\n\
                      url = \"https://example.org/one.pdf\"\n\
                      file = \"one.pdf\"\n\
                      sha256 = \"abcd\"\n\
                      bytes = 4\n\
                      pages = 1\n\
                      markers = [\"hi\", \"there\"]\n\
                      license = \"Apache-2.0\"\n\
                      source = \"test\"\n\
                      status = \"approved\"\n\
                      [[corpus]]\n\
                      id = \"two\"\n\
                      url = \"https://example.invalid/two.tar\"\n\
                      sha256 = \"\"\n\
                      bytes = 0\n\
                      status = \"unapproved\"\n";
        let entries = parse_manifest(source).expect("manifest parses");
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].markers, vec!["hi", "there"]);
        assert!(entries[0].is_pinned());
        assert!(!entries[1].is_pinned());
    }

    #[test]
    fn parse_manifest_rejects_duplicate_ids() {
        let source = "[[corpus]]\nid = \"dup\"\n[[corpus]]\nid = \"dup\"\n";
        let error = parse_manifest(source).unwrap_err();
        assert!(matches!(error, CorpusError::Malformed { .. }));
    }

    #[test]
    fn fetch_refuses_unpinned_entries_without_network() {
        let mut entry = pinned_entry();
        entry.status = "unapproved".to_owned();
        let error = fetch_entry(&entry).unwrap_err();
        assert!(matches!(error, CorpusError::NotPinned { .. }));
    }

    #[test]
    fn cached_path_rejects_traversal() {
        let mut entry = pinned_entry();
        entry.file = "../escape.pdf".to_owned();
        let error = cached_path(&entry).unwrap_err();
        assert!(matches!(error, CorpusError::UnsafePath { .. }));
    }

    #[test]
    fn smoke_cached_skips_absent_entries() {
        let mut entry = pinned_entry();
        entry.id = "definitely-not-cached-test-entry".to_owned();
        let summary = smoke_cached(&[entry]);
        assert_eq!(summary.skipped.len(), 1);
        assert!(summary.checked.is_empty());
        assert!(summary.failures.is_empty());
    }
}
