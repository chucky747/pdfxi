//! Persisted shell preferences that are not document settings.
//!
//! The tools panel open/closed state survives a restart. It lives in a sibling
//! `shell.toml` next to `settings.toml` in the same per-user configuration
//! directory, so this task does not have to widen the settings module's
//! ownership. Paths are resolved from the environment at runtime through
//! [`crate::settings::default_path`]; nothing is hard-coded. Loading never
//! fails, and saving is atomic.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// File name used next to the main settings file.
pub const SHELL_PREFERENCES_FILE_NAME: &str = "shell.toml";

/// Shell preferences persisted between runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ShellPreferences {
    /// Whether the right tools panel is open.
    pub tools_open: bool,
}

impl Default for ShellPreferences {
    /// The tools panel starts open, as in the acceptance layout.
    fn default() -> Self {
        Self { tools_open: true }
    }
}

/// Error returned when shell preferences cannot be written.
#[derive(Debug, thiserror::Error)]
pub enum ShellPreferencesError {
    /// The configuration directory could not be created.
    #[error("could not create the shell preferences directory: {0}")]
    CreateDir(#[source] io::Error),
    /// The temporary file could not be written.
    #[error("could not write the shell preferences file: {0}")]
    Write(#[source] io::Error),
    /// The temporary file could not replace the destination.
    #[error("could not replace the shell preferences file: {0}")]
    Rename(#[source] io::Error),
    /// The preferences could not be serialized.
    #[error("could not serialize shell preferences: {0}")]
    Serialize(#[source] toml::ser::Error),
}

/// Per-user shell preferences path, or `None` without a config directory.
#[must_use]
pub fn default_path() -> Option<PathBuf> {
    crate::settings::default_path()
        .map(|settings| settings.with_file_name(SHELL_PREFERENCES_FILE_NAME))
}

/// Read shell preferences, falling back to defaults on any error.
#[must_use]
pub fn load(path: &Path) -> ShellPreferences {
    match fs::read_to_string(path) {
        Ok(text) => toml::from_str(&text).unwrap_or_default(),
        Err(_) => ShellPreferences::default(),
    }
}

/// Write shell preferences atomically, creating the parent directory.
pub fn save(path: &Path, preferences: &ShellPreferences) -> Result<(), ShellPreferencesError> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent).map_err(ShellPreferencesError::CreateDir)?;
    }
    let text = toml::to_string(preferences).map_err(ShellPreferencesError::Serialize)?;
    let temporary = temporary_path(path);
    fs::write(&temporary, text).map_err(ShellPreferencesError::Write)?;
    if let Err(error) = fs::rename(&temporary, path) {
        let _ = fs::remove_file(&temporary);
        return Err(ShellPreferencesError::Rename(error));
    }
    Ok(())
}

/// The sibling temporary path used for the atomic save.
fn temporary_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().map_or_else(
        || SHELL_PREFERENCES_FILE_NAME.to_owned(),
        |name| name.to_string_lossy().into_owned(),
    );
    name.push_str(".tmp");
    path.with_file_name(name)
}
