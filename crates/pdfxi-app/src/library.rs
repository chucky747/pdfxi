//! Local PDF library, persisted recents, and stars.
//!
//! This module is window-free: it reads the library folder, sums storage, and
//! persists recents/stars in `library.toml` next to `settings.toml`. The shell
//! draws it, but every rule (dedupe, cap, missing detection, atomic save) is
//! testable without a window.

use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// File name used inside the per-user configuration directory.
pub const LIBRARY_FILE_NAME: &str = "library.toml";

/// Maximum number of recent documents kept.
pub const MAX_RECENT: usize = 20;

/// One recently opened document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecentEntry {
    /// Absolute path of the document.
    pub path: PathBuf,
    /// Display title captured when it was opened.
    pub title: String,
    /// RFC 3339 timestamp of the last open.
    pub last_opened: String,
    /// Page that was current when the document was last used.
    pub last_page: usize,
}

impl RecentEntry {
    /// Whether the document file no longer exists.
    #[must_use]
    pub fn is_missing(&self) -> bool {
        !self.path.is_file()
    }
}

/// Persisted library data.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct LibraryData {
    /// Recent documents, newest first.
    pub recent: Vec<RecentEntry>,
    /// Starred document paths.
    pub starred: BTreeSet<PathBuf>,
}

/// One PDF found in the library folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LibraryEntry {
    /// Absolute path.
    pub path: PathBuf,
    /// File stem used as the display title.
    pub title: String,
    /// Last modification time, when the platform reports one.
    pub modified: Option<SystemTime>,
}

/// Error returned when library data cannot be written.
#[derive(Debug, thiserror::Error)]
pub enum LibraryError {
    /// The configuration directory could not be created.
    #[error("could not create the library directory: {0}")]
    CreateDir(#[source] io::Error),
    /// The temporary file could not be written.
    #[error("could not write the library file: {0}")]
    Write(#[source] io::Error),
    /// The temporary file could not replace the destination.
    #[error("could not replace the library file: {0}")]
    Rename(#[source] io::Error),
    /// The data could not be serialized.
    #[error("could not serialize library data: {0}")]
    Serialize(#[source] toml::ser::Error),
}

/// The in-memory library with its optional persistence path.
#[derive(Debug, Clone)]
pub struct Library {
    data: LibraryData,
    path: Option<PathBuf>,
}

impl Default for Library {
    /// An empty library with no persistence path.
    fn default() -> Self {
        Self::new(None)
    }
}

impl Library {
    /// Load the library from `path`, or start empty without persistence.
    ///
    /// A missing or corrupt file yields empty recents and stars.
    #[must_use]
    pub fn new(path: Option<PathBuf>) -> Self {
        let data = path.as_deref().map(load_data).unwrap_or_default();
        Self { data, path }
    }

    /// The default per-user `library.toml` path.
    #[must_use]
    pub fn default_path() -> Option<PathBuf> {
        crate::settings::default_path().map(|path| path.with_file_name(LIBRARY_FILE_NAME))
    }

    /// Borrow the persisted data.
    #[must_use]
    pub fn data(&self) -> &LibraryData {
        &self.data
    }

    /// Recent documents, newest first.
    #[must_use]
    pub fn recent(&self) -> &[RecentEntry] {
        &self.data.recent
    }

    /// Starred paths.
    #[must_use]
    pub fn starred(&self) -> &BTreeSet<PathBuf> {
        &self.data.starred
    }

    /// Whether `path` is starred.
    #[must_use]
    pub fn is_starred(&self, path: &Path) -> bool {
        self.data.starred.contains(&normalize_key(path))
    }

    /// Record a document open, moving it to the front and capping the list.
    pub fn record_open(&mut self, path: &Path, title: &str, page: usize) {
        let key = normalize_key(path);
        self.data.recent.retain(|entry| entry.path != key);
        self.data.recent.insert(
            0,
            RecentEntry {
                path: key,
                title: title.to_owned(),
                last_opened: now_rfc3339(),
                last_page: page,
            },
        );
        self.data.recent.truncate(MAX_RECENT);
        let _ = self.save();
    }

    /// Toggle a star and return the new state.
    pub fn toggle_star(&mut self, path: &Path) -> bool {
        let key = normalize_key(path);
        let starred = if self.data.starred.remove(&key) {
            false
        } else {
            self.data.starred.insert(key);
            true
        };
        let _ = self.save();
        starred
    }

    /// Remove a recent entry by path.
    pub fn remove_recent(&mut self, path: &Path) {
        let key = normalize_key(path);
        self.data.recent.retain(|entry| entry.path != key);
        let _ = self.save();
    }

    /// Update the stored page for an already recent document.
    pub fn set_last_page(&mut self, path: &Path, page: usize) {
        let key = normalize_key(path);
        if let Some(entry) = self.data.recent.iter_mut().find(|entry| entry.path == key) {
            entry.last_page = page;
            entry.last_opened = now_rfc3339();
            let _ = self.save();
        }
    }

    /// Whether the library has a persistence path.
    #[must_use]
    pub fn is_persistent(&self) -> bool {
        self.path.is_some()
    }

    /// Write the current data atomically.
    pub fn save(&self) -> Result<(), LibraryError> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            fs::create_dir_all(parent).map_err(LibraryError::CreateDir)?;
        }
        let text = toml::to_string(&self.data).map_err(LibraryError::Serialize)?;
        let temporary = path.with_extension("toml.tmp");
        fs::write(&temporary, text).map_err(LibraryError::Write)?;
        if let Err(error) = fs::rename(&temporary, path) {
            let _ = fs::remove_file(&temporary);
            return Err(LibraryError::Rename(error));
        }
        Ok(())
    }
}

/// Load library data, falling back to empty on any error.
fn load_data(path: &Path) -> LibraryData {
    match fs::read_to_string(path) {
        Ok(text) => toml::from_str(&text).unwrap_or_default(),
        Err(_) => LibraryData::default(),
    }
}

/// Normalize a path for comparison without requiring the file to exist.
fn normalize_key(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// List the `*.pdf` files directly inside `root`, newest first.
///
/// The listing is non-recursive and skips subdirectories.
pub fn list_pdfs(root: &Path) -> io::Result<Vec<LibraryEntry>> {
    let mut entries = Vec::new();
    for entry in fs::read_dir(root)? {
        let Ok(entry) = entry else {
            continue;
        };
        let path = entry.path();
        if !is_pdf_path(&path) {
            continue;
        }
        let modified = entry.metadata().ok().and_then(|meta| meta.modified().ok());
        let title = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("Untitled")
            .to_owned();
        entries.push(LibraryEntry {
            path,
            title,
            modified,
        });
    }
    entries.sort_by(|left, right| {
        right
            .modified
            .cmp(&left.modified)
            .then_with(|| left.title.cmp(&right.title))
    });
    Ok(entries)
}

/// Whether a path carries a `.pdf` extension, ignoring ASCII case.
#[must_use]
pub fn is_pdf_path(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("pdf"))
}

/// Sum file sizes under `root`, bounded in depth and skipping symlinks.
#[must_use]
pub fn storage_used(root: &Path) -> u64 {
    fn walk(directory: &Path, depth: usize, total: &mut u64) {
        if depth > 16 {
            return;
        }
        let Ok(entries) = fs::read_dir(directory) else {
            return;
        };
        for entry in entries.flatten() {
            let Ok(metadata) = fs::symlink_metadata(entry.path()) else {
                continue;
            };
            if metadata.file_type().is_symlink() {
                continue;
            }
            if metadata.is_dir() {
                walk(&entry.path(), depth + 1, total);
            } else if metadata.is_file() {
                *total = total.saturating_add(metadata.len());
            }
        }
    }
    let mut total = 0;
    walk(root, 0, &mut total);
    total
}

/// Format a byte count as MB or GB for the storage meter.
#[must_use]
pub fn format_bytes(bytes: u64) -> String {
    const MB: u64 = 1024 * 1024;
    const GB: u64 = 1024 * MB;
    if bytes >= GB {
        format!("{:.1} GB", bytes as f64 / GB as f64)
    } else {
        format!("{:.1} MB", bytes as f64 / MB as f64)
    }
}

/// Avatar initials: first letters of up to two words, fallback `P`.
#[must_use]
pub fn initials(name: &str) -> String {
    let mut initials: String = name
        .split_whitespace()
        .filter_map(|word| word.chars().next())
        .take(2)
        .flat_map(char::to_uppercase)
        .collect();
    if initials.is_empty() {
        initials.push('P');
    }
    initials
}

/// Current UTC time as an RFC 3339 timestamp.
fn now_rfc3339() -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    rfc3339_from_unix(seconds)
}

/// Convert Unix seconds to an RFC 3339 UTC timestamp.
fn rfc3339_from_unix(seconds: u64) -> String {
    let days = (seconds / 86_400) as i64;
    let day_seconds = seconds % 86_400;
    let (year, month, day) = civil_from_days(days);
    let hour = day_seconds / 3_600;
    let minute = (day_seconds % 3_600) / 60;
    let second = day_seconds % 60;
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

/// Convert days since 1970-01-01 to a civil date (Howard Hinnant's algorithm).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { year + 1 } else { year };
    (year, month as u32, day as u32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    fn temp_dir(tag: &str) -> PathBuf {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "pdfxi-library-{}-{tag}-{unique}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create the temporary library directory");
        dir
    }

    #[test]
    fn recents_dedupe_move_to_front_and_cap() {
        let mut library = Library::new(None);
        for index in 0..25 {
            library.record_open(
                Path::new(&format!("doc-{index}.pdf")),
                &format!("Doc {index}"),
                index,
            );
        }
        assert_eq!(library.recent().len(), MAX_RECENT);
        assert_eq!(library.recent()[0].title, "Doc 24");

        library.record_open(Path::new("doc-10.pdf"), "Doc 10 again", 3);
        assert_eq!(library.recent().len(), MAX_RECENT);
        assert_eq!(library.recent()[0].title, "Doc 10 again");
        assert_eq!(
            library
                .recent()
                .iter()
                .filter(|entry| entry.path == Path::new("doc-10.pdf"))
                .count(),
            1,
            "the opened document appears exactly once"
        );
    }

    #[test]
    fn missing_recent_entry_is_marked() {
        let mut library = Library::new(None);
        let missing = Path::new("definitely-not-there.pdf");
        library.record_open(missing, "Missing", 0);
        assert!(library.recent()[0].is_missing());
    }

    #[test]
    fn stars_round_trip_through_the_file() {
        let dir = temp_dir("stars");
        let path = dir.join("library.toml");
        let document = dir.join("starred.pdf");
        fs::write(&document, b"pdf").expect("write a placeholder document");

        let mut library = Library::new(Some(path.clone()));
        assert!(library.toggle_star(&document));
        assert!(library.is_starred(&document));
        let loaded = Library::new(Some(path.clone()));
        assert!(loaded.is_starred(&document), "stars persist");

        let mut loaded = loaded;
        assert!(!loaded.toggle_star(&document));
        let reloaded = Library::new(Some(path));
        assert!(!reloaded.is_starred(&document), "unstar persists");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn corrupt_library_falls_back_to_empty() {
        let dir = temp_dir("corrupt");
        let path = dir.join("library.toml");
        fs::write(&path, b"\x00not = [valid").expect("write a corrupt library");
        let library = Library::new(Some(path));
        assert!(library.recent().is_empty());
        assert!(library.starred().is_empty());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn storage_sum_adds_file_sizes() {
        let dir = temp_dir("storage");
        fs::write(dir.join("a.pdf"), vec![0u8; 100]).expect("write a");
        fs::write(dir.join("b.pdf"), vec![0u8; 200]).expect("write b");
        fs::write(dir.join("notes.txt"), vec![0u8; 50]).expect("write notes");
        assert_eq!(storage_used(&dir), 350);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn rfc3339_timestamp_is_well_formed() {
        assert_eq!(rfc3339_from_unix(0), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339_from_unix(86_400), "1970-01-02T00:00:00Z");
    }
}
