//! Persisted viewer settings and launch-flag parsing.
//!
//! The shell keeps two user preferences: which theme to use and which window
//! control style the custom title bar draws. They are stored as TOML in the
//! per-user configuration directory — `%APPDATA%\PDFXI\settings.toml` on
//! Windows, `$XDG_CONFIG_HOME/PDFXI/settings.toml` (or
//! `~/.config/PDFXI/settings.toml`) on Linux, and
//! `~/Library/Application Support/PDFXI/settings.toml` on macOS.
//!
//! Loading never fails: a missing, unreadable, or corrupt file yields
//! [`Settings::default`], and unknown keys are ignored. Saving is atomic: the
//! text is written to a temporary file in the same directory and then renamed
//! over the destination, so a crash cannot leave a half-written file behind.
//!
//! This module has no GUI or platform dependency. The path is resolved from the
//! environment at runtime (never hard-coded), and every load/save helper takes
//! the path as a parameter so tests can use a temporary directory. It also
//! holds the pure command-line parser: `--dark`, `--light`, and
//! `--controls=windows|macos` override the saved settings for one run without
//! being written back.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// File name used inside the per-user configuration directory.
pub const SETTINGS_FILE_NAME: &str = "settings.toml";

/// Directory name (under the platform configuration root) owned by PDFXI.
pub const SETTINGS_DIR_NAME: &str = "PDFXI";

/// Which theme the viewer should use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemePreference {
    /// Force the light palette.
    Light,
    /// Force the dark palette.
    Dark,
    /// Follow the operating system's light/dark preference.
    #[default]
    System,
}

impl ThemePreference {
    /// Human-readable name shown in the settings menu.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Light => "Light",
            Self::Dark => "Dark",
            Self::System => "Follow system",
        }
    }

    /// Parse a user-facing identifier, ignoring ASCII case.
    ///
    /// Accepts `light`, `dark`, and `system`; anything else returns `None`.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value.to_ascii_lowercase().as_str() {
            "light" => Some(Self::Light),
            "dark" => Some(Self::Dark),
            "system" => Some(Self::System),
            _ => None,
        }
    }
}

/// Which window controls the custom title bar draws.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WindowControlStyle {
    /// Windows caption buttons: minimize, maximize/restore, close at the right.
    Windows,
    /// macOS traffic lights: close, minimize, zoom at the left.
    MacOs,
}

impl Default for WindowControlStyle {
    /// Windows on Windows, macOS everywhere else.
    fn default() -> Self {
        if cfg!(windows) {
            Self::Windows
        } else {
            Self::MacOs
        }
    }
}

impl WindowControlStyle {
    /// Human-readable name shown in the settings menu.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Windows => "Windows",
            Self::MacOs => "macOS",
        }
    }

    /// Parse a user-facing identifier, ignoring ASCII case.
    ///
    /// Accepts `windows` and `macos`; anything else returns `None`.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value.to_ascii_lowercase().as_str() {
            "windows" => Some(Self::Windows),
            "macos" | "mac" => Some(Self::MacOs),
            _ => None,
        }
    }
}

/// The complete persisted settings document.
///
/// Missing fields use their defaults, so an older or hand-edited file still
/// loads. Unknown keys are ignored by the TOML deserializer. Paths are stored
/// as strings and expanded on load; relative paths are rejected.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Theme preference (`light`, `dark`, or `system`).
    pub theme: ThemePreference,
    /// Window control style (`windows` or `macos`).
    pub window_controls: WindowControlStyle,
    /// Local display name used for comment authors and signers.
    pub display_name: String,
    /// Local library folder; `None` uses the default Documents/PDFXI folder.
    pub library_root: Option<PathBuf>,
    /// Optional shared folder; when set, the Shared rail entry appears.
    pub shared_folder: Option<PathBuf>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: ThemePreference::default(),
            window_controls: WindowControlStyle::default(),
            display_name: default_display_name(),
            library_root: None,
            shared_folder: None,
        }
    }
}

impl Settings {
    /// Apply one run's launch flags on top of these settings.
    ///
    /// The receiver is left untouched, so a flag can never be written back to
    /// disk by accident: only an explicit settings change persists a value.
    #[must_use]
    pub fn with_overrides(&self, options: &LaunchOptions) -> Self {
        Self {
            theme: options.theme.unwrap_or(self.theme),
            window_controls: options.window_controls.unwrap_or(self.window_controls),
            ..self.clone()
        }
    }

    /// The configured library folder, or the local Documents/PDFXI default.
    #[must_use]
    pub fn library_root_or_default(&self) -> Option<PathBuf> {
        self.library_root.clone().or_else(default_library_root)
    }
}

/// The neutral default display name.
///
/// Comments and signatures must not leak the platform account name by default,
/// so a fresh install starts as `Reader`; the name stays editable in the
/// identity popover. Existing settings that already contain a name keep it.
#[must_use]
pub fn default_display_name() -> String {
    "Reader".to_owned()
}

/// The per-user home directory, or `None` when the environment exposes none.
#[must_use]
pub fn home_dir() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .filter(|value| !value.is_empty())
                .map(PathBuf::from)
        })
}

/// Default local library folder: `Documents/PDFXI` under the home directory.
#[must_use]
pub fn default_library_root() -> Option<PathBuf> {
    home_dir().map(|home| home.join("Documents").join("PDFXI"))
}

/// Expand `~`, `%VAR%` and `$VAR` in a stored path and require it to be absolute.
///
/// Relative paths and paths whose home directory cannot be resolved are
/// rejected, so a hand-edited settings file cannot point the library elsewhere
/// by accident.
#[must_use]
pub fn expand_path(raw: &str) -> Option<PathBuf> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let replaced = if trimmed == "~" {
        home_dir()?.to_string_lossy().into_owned()
    } else if let Some(rest) = trimmed
        .strip_prefix("~/")
        .or_else(|| trimmed.strip_prefix("~\\"))
    {
        let mut base = home_dir()?.to_string_lossy().into_owned();
        base.push(std::path::MAIN_SEPARATOR);
        base.push_str(rest);
        base
    } else {
        trimmed.to_owned()
    };
    let path = PathBuf::from(expand_env_vars(&replaced));
    path.is_absolute().then_some(path)
}

/// Expand `%NAME%`, `$NAME` and `${NAME}` references in `input`.
fn expand_env_vars(input: &str) -> String {
    let chars: Vec<char> = input.chars().collect();
    let mut output = String::with_capacity(input.len());
    let mut index = 0;
    while index < chars.len() {
        let current = chars[index];
        if current == '%'
            && let Some(end) = chars[index + 1..].iter().position(|ch| *ch == '%')
        {
            let name: String = chars[index + 1..index + 1 + end].iter().collect();
            if let Some(value) = std::env::var_os(&name) {
                output.push_str(&value.to_string_lossy());
                index += end + 2;
                continue;
            }
        } else if current == '$' && index + 1 < chars.len() {
            if chars[index + 1] == '{'
                && let Some(end) = chars[index + 2..].iter().position(|ch| *ch == '}')
            {
                let name: String = chars[index + 2..index + 2 + end].iter().collect();
                if let Some(value) = std::env::var_os(&name) {
                    output.push_str(&value.to_string_lossy());
                    index += end + 3;
                    continue;
                }
            } else {
                let mut end = index + 1;
                while end < chars.len() && (chars[end].is_ascii_alphanumeric() || chars[end] == '_')
                {
                    end += 1;
                }
                if end > index + 1 {
                    let name: String = chars[index + 1..end].iter().collect();
                    if let Some(value) = std::env::var_os(&name) {
                        output.push_str(&value.to_string_lossy());
                        index = end;
                        continue;
                    }
                }
            }
        }
        output.push(current);
        index += 1;
    }
    output
}

/// Resolve a path read from the settings file.
fn resolve_stored_path(path: &Path) -> Option<PathBuf> {
    expand_path(path.to_string_lossy().as_ref())
}

/// Settings overrides parsed from the command line.///
/// Every field is optional: `None` means "use the saved setting".
/// A rail/panel selected by a launch flag for captures and deep links.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaunchPanel {
    /// Home side panel.
    Home,
    /// Library folder.
    Files,
    /// Recent documents.
    Recent,
    /// Starred documents.
    Starred,
    /// Page thumbnails.
    Pages,
    /// Shared folder.
    Shared,
    /// Comments panel.
    Comments,
    /// AI assistant panel.
    AiAssistant,
}

impl LaunchPanel {
    /// Parse a user-facing rail name, ignoring ASCII case.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value.to_ascii_lowercase().as_str() {
            "home" => Some(Self::Home),
            "files" => Some(Self::Files),
            "recent" => Some(Self::Recent),
            "starred" => Some(Self::Starred),
            "pages" => Some(Self::Pages),
            "shared" => Some(Self::Shared),
            "comments" => Some(Self::Comments),
            "ai" | "assistant" => Some(Self::AiAssistant),
            _ => None,
        }
    }
}

/// Settings overrides parsed from the command line.
///
/// Every field is optional: `None` means "use the saved setting".
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LaunchOptions {
    /// Theme requested with `--light` or `--dark`.
    pub theme: Option<ThemePreference>,
    /// Window controls requested with `--controls=windows|macos`.
    pub window_controls: Option<WindowControlStyle>,
    /// Rail requested with `--rail=...` or `--panel=...`.
    pub panel: Option<LaunchPanel>,
    /// Info title requested with `--set-title=...`.
    ///
    /// Used to prove the session edit path headlessly and to capture the dirty
    /// tab state; it performs the same `session.edit` call the UI would.
    pub title: Option<String>,
}

impl LaunchOptions {
    /// Whether no override was requested.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.theme.is_none()
            && self.window_controls.is_none()
            && self.panel.is_none()
            && self.title.is_none()
    }
}

/// Parse viewer launch flags, ignoring everything else.
///
/// Recognised forms are `--dark`, `--light`, and `--controls=windows` or
/// `--controls=macos`. Unknown flags, bare `--controls`, and invalid values
/// are ignored without panicking. When a flag repeats, the last one wins.
/// Arguments that look like file paths are skipped so callers can pass the same
/// list to the document opener.
#[must_use]
pub fn parse_launch_arguments(arguments: &[String]) -> LaunchOptions {
    let mut options = LaunchOptions::default();
    for argument in arguments {
        match argument.as_str() {
            "--dark" => options.theme = Some(ThemePreference::Dark),
            "--light" => options.theme = Some(ThemePreference::Light),
            other => {
                if let Some(value) = other.strip_prefix("--controls=")
                    && let Some(style) = WindowControlStyle::parse(value)
                {
                    options.window_controls = Some(style);
                }
                if let Some(value) = other
                    .strip_prefix("--rail=")
                    .or_else(|| other.strip_prefix("--panel="))
                    && let Some(panel) = LaunchPanel::parse(value)
                {
                    options.panel = Some(panel);
                }
                if let Some(value) = other.strip_prefix("--set-title=") {
                    options.title = Some(value.to_owned());
                }
            }
        }
    }
    options
}

/// Error returned when settings cannot be written.
///
/// Loading has no error type because it always falls back to defaults.
#[derive(Debug, thiserror::Error)]
pub enum SettingsError {
    /// The configuration directory could not be created.
    #[error("could not create the settings directory: {0}")]
    CreateDir(#[source] io::Error),
    /// The temporary file could not be written.
    #[error("could not write the settings file: {0}")]
    Write(#[source] io::Error),
    /// The temporary file could not replace the destination.
    #[error("could not replace the settings file: {0}")]
    Rename(#[source] io::Error),
    /// The settings could not be serialized.
    #[error("could not serialize settings: {0}")]
    Serialize(#[source] toml::ser::Error),
}

/// The per-user settings path, or `None` when the platform exposes no
/// configuration directory.
///
/// Resolved from the environment on every call; nothing is cached and no path
/// is hard-coded. The directory itself is only created by [`save`].
#[must_use]
pub fn default_path() -> Option<PathBuf> {
    config_root().map(|root| root.join(SETTINGS_DIR_NAME).join(SETTINGS_FILE_NAME))
}

/// Read settings from `path`, falling back to defaults on any error.
///
/// A missing file, an unreadable file, a directory in place of the file, or
/// invalid TOML all yield [`Settings::default`]. Valid files with unknown keys
/// load normally; the unknown keys are ignored.
#[must_use]
pub fn load(path: &Path) -> Settings {
    match fs::read_to_string(path) {
        Ok(text) => {
            let mut settings: Settings = toml::from_str(&text).unwrap_or_default();
            settings.library_root = settings
                .library_root
                .as_deref()
                .and_then(resolve_stored_path);
            settings.shared_folder = settings
                .shared_folder
                .as_deref()
                .and_then(resolve_stored_path);
            settings
        }
        Err(_) => Settings::default(),
    }
}

/// Write `settings` to `path` atomically, creating the parent directory.
///
/// The text goes to a sibling temporary file first and is then renamed over
/// `path`, so readers never observe a partially written document. On Windows
/// `std::fs::rename` replaces an existing destination.
pub fn save(path: &Path, settings: &Settings) -> Result<(), SettingsError> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent).map_err(SettingsError::CreateDir)?;
    }
    let text = toml::to_string(settings).map_err(SettingsError::Serialize)?;
    let temporary = temporary_path(path);
    fs::write(&temporary, text).map_err(SettingsError::Write)?;
    if let Err(error) = fs::rename(&temporary, path) {
        // Best effort: never leave a stale temporary file behind.
        let _ = fs::remove_file(&temporary);
        return Err(SettingsError::Rename(error));
    }
    Ok(())
}

/// The sibling temporary path used for the atomic save.
fn temporary_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().map_or_else(
        || SETTINGS_FILE_NAME.to_owned(),
        |name| name.to_string_lossy().into_owned(),
    );
    name.push_str(".tmp");
    path.with_file_name(name)
}

/// Resolve the platform configuration root from the environment.
fn config_root() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        non_empty_env("APPDATA").map(PathBuf::from)
    }
    #[cfg(target_os = "macos")]
    {
        non_empty_env("HOME").map(|home| {
            PathBuf::from(home)
                .join("Library")
                .join("Application Support")
        })
    }
    #[cfg(all(not(windows), not(target_os = "macos")))]
    {
        non_empty_env("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| non_empty_env("HOME").map(|home| PathBuf::from(home).join(".config")))
    }
}

/// Read an environment variable, treating an empty value as unset.
fn non_empty_env(name: &str) -> Option<String> {
    std::env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(|value| value.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    fn temp_dir(tag: &str) -> PathBuf {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "pdfxi-settings-{}-{tag}-{unique}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create the temporary test directory");
        dir
    }

    #[test]
    fn a_saved_display_name_survives_and_missing_uses_reader() {
        let dir = temp_dir("display-name");
        let path = dir.join("settings.toml");
        fs::write(&path, "display_name = \"Ada\"\ntheme = \"dark\"\n").expect("write settings");
        assert_eq!(load(&path).display_name, "Ada");
        fs::write(&path, "theme = \"light\"\n").expect("write settings without a name");
        assert_eq!(load(&path).display_name, "Reader");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn settings_round_trip_through_a_temp_dir() {
        let dir = temp_dir("roundtrip");
        let path = dir.join("nested").join("settings.toml");
        let settings = Settings {
            theme: ThemePreference::Dark,
            window_controls: WindowControlStyle::MacOs,
            ..Settings::default()
        };
        save(&path, &settings).expect("save settings");
        assert!(path.is_file(), "save creates the directory and the file");
        assert_eq!(load(&path), settings);

        let light = Settings {
            theme: ThemePreference::Light,
            window_controls: WindowControlStyle::Windows,
            ..Settings::default()
        };
        save(&path, &light).expect("overwrite settings");
        assert_eq!(load(&path), light, "the second save replaces the file");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_corrupt_and_unreadable_files_fall_back_to_defaults() {
        let dir = temp_dir("fallback");

        let missing = dir.join("settings.toml");
        assert_eq!(load(&missing), Settings::default());
        assert_eq!(
            Settings::default().theme,
            ThemePreference::System,
            "first run follows the system theme"
        );

        let corrupt = dir.join("corrupt.toml");
        fs::write(&corrupt, b"\x00not = [valid").expect("write the corrupt file");
        assert_eq!(load(&corrupt), Settings::default());

        let unrelated = dir.join("wrong.toml");
        fs::write(&unrelated, "theme = 17\nwindow_controls = [true]").expect("write bad types");
        assert_eq!(load(&unrelated), Settings::default());

        let unusable = dir.join("a-directory");
        fs::create_dir_all(&unusable).expect("create a directory in place of the file");
        assert_eq!(load(&unusable), Settings::default());

        let extra = dir.join("extra.toml");
        fs::write(
            &extra,
            "theme = \"light\"\nwindow_controls = \"macos\"\nfuture_option = true\n",
        )
        .expect("write unknown-key settings");
        assert_eq!(
            load(&extra),
            Settings {
                theme: ThemePreference::Light,
                window_controls: WindowControlStyle::MacOs,
                ..Settings::default()
            },
            "unknown keys are ignored"
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn launch_flags_parse_and_override_saved_settings() {
        let saved = Settings {
            theme: ThemePreference::Dark,
            window_controls: WindowControlStyle::Windows,
            ..Settings::default()
        };

        let flags = parse_launch_arguments(&["--light".to_owned(), "--controls=macos".to_owned()]);
        let effective = saved.with_overrides(&flags);
        assert_eq!(effective.theme, ThemePreference::Light);
        assert_eq!(effective.window_controls, WindowControlStyle::MacOs);
        assert_eq!(
            saved.theme,
            ThemePreference::Dark,
            "flags must not mutate the saved settings"
        );
        assert_eq!(
            saved.window_controls,
            WindowControlStyle::Windows,
            "flags must not mutate the saved settings"
        );

        let bad = parse_launch_arguments(&[
            "--controls=beos".to_owned(),
            "--controls".to_owned(),
            "--controls=".to_owned(),
            "--unknown".to_owned(),
        ]);
        assert_eq!(bad.window_controls, None, "a bad value is ignored");
        assert_eq!(bad.theme, None);
        assert_eq!(saved.with_overrides(&bad), saved);

        let case = parse_launch_arguments(&["--controls=WINDOWS".to_owned()]);
        assert_eq!(
            case.window_controls,
            Some(WindowControlStyle::Windows),
            "values are case-insensitive"
        );

        let none = parse_launch_arguments(&["plan.pdf".to_owned()]);
        assert_eq!(none, LaunchOptions::default());
        assert!(none.is_empty());
        assert_eq!(saved.with_overrides(&none), saved);
    }
}
