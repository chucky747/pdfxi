//! Deterministic preferences-file model (PLAT-006).
//!
//! This module models a versioned preferences file: schema version, documented
//! defaults, strict validation, and a byte-for-byte deterministic serialize /
//! parse round-trip. The path is supplied by the caller and is only *validated
//! and stored*; this module never writes to it, never reads from it, and never
//! touches the filesystem. A host owns all I/O.
//!
//! The on-disk format is intentionally a small, sorted `key=value` text format
//! so it needs no external TOML dependency and so two hosts always produce the
//! same bytes for the same preferences.

use thiserror::Error;

/// The only schema version this model understands.
pub const SCHEMA_VERSION: u32 = 1;
/// Maximum accepted byte length of a caller-supplied preferences path.
pub const MAX_PREFERENCES_PATH_BYTES: usize = 4_096;
/// Maximum accepted byte length of a locale tag.
pub const MAX_LOCALE_BYTES: usize = 35;

/// The user-interface theme preference.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Theme {
    /// Follow the operating-system setting.
    System,
    /// Always use a light theme.
    Light,
    /// Always use a dark theme.
    Dark,
}

impl Theme {
    /// Return the stable lowercase token used in the file format.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Light => "light",
            Self::Dark => "dark",
        }
    }

    /// Parse a theme token (case-insensitive).
    ///
    /// # Errors
    ///
    /// Returns [`PreferencesError::InvalidTheme`] for an unknown token.
    pub fn parse(token: &str) -> Result<Self, PreferencesError> {
        match token.to_ascii_lowercase().as_str() {
            "system" => Ok(Self::System),
            "light" => Ok(Self::Light),
            "dark" => Ok(Self::Dark),
            other => Err(PreferencesError::InvalidTheme(other.to_owned())),
        }
    }
}

/// A complete, validated set of user preferences.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Preferences {
    version: u32,
    theme: Theme,
    locale: String,
    recent_count: u32,
    show_toolbar: bool,
    default_zoom_percent: u16,
    autosave_seconds: u32,
}

impl Preferences {
    /// Return the schema version of these preferences.
    pub const fn version(&self) -> u32 {
        self.version
    }

    /// Return the theme preference.
    pub const fn theme(&self) -> Theme {
        self.theme
    }

    /// Return the validated locale tag.
    pub fn locale(&self) -> &str {
        &self.locale
    }

    /// Return the maximum number of recent documents to keep.
    pub const fn recent_count(&self) -> u32 {
        self.recent_count
    }

    /// Return whether the toolbar is shown.
    pub const fn show_toolbar(&self) -> bool {
        self.show_toolbar
    }

    /// Return the default zoom percentage.
    pub const fn default_zoom_percent(&self) -> u16 {
        self.default_zoom_percent
    }

    /// Return the autosave interval in seconds.
    pub const fn autosave_seconds(&self) -> u32 {
        self.autosave_seconds
    }

    /// Construct and validate a preferences set.
    ///
    /// # Errors
    ///
    /// Returns a typed [`PreferencesError`] for an unsupported schema version,
    /// an invalid locale, or a value outside its documented range.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        version: u32,
        theme: Theme,
        locale: &str,
        recent_count: u32,
        show_toolbar: bool,
        default_zoom_percent: u16,
        autosave_seconds: u32,
    ) -> Result<Self, PreferencesError> {
        if version != SCHEMA_VERSION {
            return Err(PreferencesError::UnsupportedSchema(version));
        }
        validate_locale(locale)?;
        if recent_count > 100 {
            return Err(PreferencesError::OutOfRange("recent_count", recent_count));
        }
        if !(25..=400).contains(&default_zoom_percent) {
            return Err(PreferencesError::OutOfRange(
                "default_zoom_percent",
                u32::from(default_zoom_percent),
            ));
        }
        if autosave_seconds > 3_600 {
            return Err(PreferencesError::OutOfRange(
                "autosave_seconds",
                autosave_seconds,
            ));
        }
        Ok(Self {
            version,
            theme,
            locale: locale.to_owned(),
            recent_count,
            show_toolbar,
            default_zoom_percent,
            autosave_seconds,
        })
    }
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            version: SCHEMA_VERSION,
            theme: Theme::System,
            locale: "en-US".to_owned(),
            recent_count: 10,
            show_toolbar: true,
            default_zoom_percent: 100,
            autosave_seconds: 300,
        }
    }
}

/// A validated caller-supplied path paired with in-memory preferences.
///
/// Constructing this value validates the path; no file is created or read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreferencesFile {
    path: String,
    preferences: Preferences,
}

impl PreferencesFile {
    /// Create a preferences file model from a caller-supplied path.
    ///
    /// # Errors
    ///
    /// Returns [`PreferencesError::EmptyPath`], [`PreferencesError::PathTooLong`]
    /// or [`PreferencesError::ControlCharacter`] for a rejected path.
    pub fn new(path: &str, preferences: Preferences) -> Result<Self, PreferencesError> {
        validate_preferences_path(path)?;
        Ok(Self {
            path: path.to_owned(),
            preferences,
        })
    }

    /// Return the validated caller-supplied path.
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Return the in-memory preferences.
    pub const fn preferences(&self) -> &Preferences {
        &self.preferences
    }
}

/// Typed errors produced by preferences validation and parsing.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum PreferencesError {
    /// The schema version was not understood.
    #[error("unsupported preferences schema version: {0}")]
    UnsupportedSchema(u32),
    /// The locale tag was empty or malformed.
    #[error("invalid locale tag: {0}")]
    InvalidLocale(String),
    /// A numeric value was outside its documented range.
    #[error("preferences value {0} is out of range: {1}")]
    OutOfRange(&'static str, u32),
    /// The theme token was not recognized.
    #[error("invalid theme preference: {0}")]
    InvalidTheme(String),
    /// The boolean token was not `true` or `false`.
    #[error("invalid boolean preference: {0}")]
    InvalidBoolean(String),
    /// A required key was absent.
    #[error("missing preferences key: {0}")]
    MissingKey(&'static str),
    /// An unknown key was present.
    #[error("unknown preferences key: {0}")]
    UnknownKey(String),
    /// A key appeared more than once.
    #[error("duplicate preferences key: {0}")]
    DuplicateKey(String),
    /// A line did not contain a single `=` separator.
    #[error("malformed preferences line: {0}")]
    MalformedLine(String),
    /// A numeric value could not be parsed.
    #[error("invalid numeric preference: {0}")]
    InvalidNumber(String),
    /// The caller-supplied path was empty.
    #[error("preferences path must not be empty")]
    EmptyPath,
    /// The caller-supplied path was too long.
    #[error("preferences path is {0} bytes (limit 4096)")]
    PathTooLong(usize),
    /// A control character was found in a value or path.
    #[error("preferences contain a control character")]
    ControlCharacter,
}

/// Validate a locale tag: 2..=35 ASCII letters/digits, `-` or `_`.
fn validate_locale(locale: &str) -> Result<(), PreferencesError> {
    if locale.len() < 2 || locale.len() > MAX_LOCALE_BYTES {
        return Err(PreferencesError::InvalidLocale(locale.to_owned()));
    }
    if !locale
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err(PreferencesError::InvalidLocale(locale.to_owned()));
    }
    Ok(())
}

/// Validate a caller-supplied preferences path (non-empty, bounded, no controls).
fn validate_preferences_path(path: &str) -> Result<(), PreferencesError> {
    if path.is_empty() {
        return Err(PreferencesError::EmptyPath);
    }
    if path.len() > MAX_PREFERENCES_PATH_BYTES {
        return Err(PreferencesError::PathTooLong(path.len()));
    }
    if path.chars().any(|c| c.is_control()) {
        return Err(PreferencesError::ControlCharacter);
    }
    Ok(())
}

/// Serialize preferences into a deterministic, sorted `key=value` document.
///
/// Keys are emitted in ascending lexicographic order, each on its own line,
/// with a trailing newline. The same preferences always produce the same bytes.
pub fn serialize_preferences(preferences: &Preferences) -> String {
    let mut fields = [
        ("autosave_seconds", preferences.autosave_seconds.to_string()),
        (
            "default_zoom_percent",
            preferences.default_zoom_percent.to_string(),
        ),
        ("locale", preferences.locale.clone()),
        ("recent_count", preferences.recent_count.to_string()),
        ("show_toolbar", preferences.show_toolbar.to_string()),
        ("theme", preferences.theme.as_str().to_owned()),
        ("version", preferences.version.to_string()),
    ];
    fields.sort_by(|left, right| left.0.cmp(right.0));
    let mut out = String::new();
    for (key, value) in fields {
        out.push_str(key);
        out.push('=');
        out.push_str(&value);
        out.push('\n');
    }
    out
}

/// Parse a preferences document produced by [`serialize_preferences`].
///
/// Every key is required exactly once. The schema version must equal
/// [`SCHEMA_VERSION`], and all values are validated through [`Preferences::new`].
///
/// # Errors
///
/// Returns a typed [`PreferencesError`] for a malformed line, unknown or
/// duplicate key, missing key, bad value, or unsupported schema.
pub fn parse_preferences(text: &str) -> Result<Preferences, PreferencesError> {
    const KEYS: [&str; 7] = [
        "autosave_seconds",
        "default_zoom_percent",
        "locale",
        "recent_count",
        "show_toolbar",
        "theme",
        "version",
    ];
    let mut values: std::collections::BTreeMap<&str, &str> = std::collections::BTreeMap::new();
    for line in text.lines() {
        if line.is_empty() {
            continue;
        }
        let (key, value) = line
            .split_once('=')
            .ok_or_else(|| PreferencesError::MalformedLine(line.to_owned()))?;
        if !KEYS.contains(&key) {
            return Err(PreferencesError::UnknownKey(key.to_owned()));
        }
        if values.insert(key, value).is_some() {
            return Err(PreferencesError::DuplicateKey(key.to_owned()));
        }
    }
    let get = |key: &'static str| {
        values
            .get(key)
            .copied()
            .ok_or(PreferencesError::MissingKey(key))
    };
    let version = parse_u32(get("version")?)?;
    let theme = Theme::parse(get("theme")?)?;
    let locale = get("locale")?.to_owned();
    let recent_count = parse_u32(get("recent_count")?)?;
    let default_zoom = parse_u16(get("default_zoom_percent")?)?;
    let autosave = parse_u32(get("autosave_seconds")?)?;
    let show_toolbar = match get("show_toolbar")? {
        "true" => true,
        "false" => false,
        other => return Err(PreferencesError::InvalidBoolean(other.to_owned())),
    };
    Preferences::new(
        version,
        theme,
        &locale,
        recent_count,
        show_toolbar,
        default_zoom,
        autosave,
    )
}

/// Parse a `u32` value with a typed error.
fn parse_u32(value: &str) -> Result<u32, PreferencesError> {
    value
        .parse::<u32>()
        .map_err(|_| PreferencesError::InvalidNumber(value.to_owned()))
}

/// Parse a `u16` value with a typed error.
fn parse_u16(value: &str) -> Result<u16, PreferencesError> {
    value
        .parse::<u16>()
        .map_err(|_| PreferencesError::InvalidNumber(value.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_documented_and_valid() {
        let defaults = Preferences::default();
        assert_eq!(defaults.version(), SCHEMA_VERSION);
        assert_eq!(defaults.theme(), Theme::System);
        assert_eq!(defaults.locale(), "en-US");
        assert_eq!(defaults.recent_count(), 10);
        assert!(defaults.show_toolbar());
        assert_eq!(defaults.default_zoom_percent(), 100);
        assert_eq!(defaults.autosave_seconds(), 300);
    }

    #[test]
    fn preferences_round_trip_is_lossless() {
        let original = Preferences::new(SCHEMA_VERSION, Theme::Dark, "fr-FR", 25, false, 150, 600)
            .expect("valid preferences");
        let text = serialize_preferences(&original);
        let parsed = parse_preferences(&text).expect("round trip");
        assert_eq!(parsed, original);
        assert_eq!(serialize_preferences(&parsed), text);
    }

    #[test]
    fn serialization_ordering_is_deterministic() {
        let prefs = Preferences::default();
        let text = serialize_preferences(&prefs);
        assert_eq!(text, serialize_preferences(&prefs));
        let keys: Vec<&str> = text
            .lines()
            .map(|line| line.split('=').next().unwrap_or(""))
            .collect();
        let mut sorted = keys.clone();
        sorted.sort_unstable();
        assert_eq!(keys, sorted);
        assert!(text.ends_with('\n'));
    }

    #[test]
    fn invalid_schema_is_rejected() {
        let bad = Preferences::new(
            SCHEMA_VERSION + 1,
            Theme::Light,
            "en-US",
            10,
            true,
            100,
            300,
        );
        assert!(matches!(bad, Err(PreferencesError::UnsupportedSchema(2))));
        let mut text = serialize_preferences(&Preferences::default());
        text = text.replace("version=1", "version=99");
        assert!(matches!(
            parse_preferences(&text),
            Err(PreferencesError::UnsupportedSchema(99))
        ));
    }

    #[test]
    fn invalid_values_are_rejected() {
        assert!(matches!(
            Preferences::new(SCHEMA_VERSION, Theme::Dark, "x", 10, true, 100, 300),
            Err(PreferencesError::InvalidLocale(_))
        ));
        assert!(matches!(
            Preferences::new(SCHEMA_VERSION, Theme::Dark, "en-US", 0, true, 24, 300),
            Err(PreferencesError::OutOfRange("default_zoom_percent", 24))
        ));
        assert!(matches!(
            Theme::parse("neon"),
            Err(PreferencesError::InvalidTheme(_))
        ));
    }

    #[test]
    fn malformed_documents_are_rejected() {
        assert!(matches!(
            parse_preferences("not a key value"),
            Err(PreferencesError::MalformedLine(_))
        ));
        assert!(matches!(
            parse_preferences("bogus=1\n"),
            Err(PreferencesError::UnknownKey(_))
        ));
        let mut text = serialize_preferences(&Preferences::default());
        text.push_str("theme=light\n");
        assert!(matches!(
            parse_preferences(&text),
            Err(PreferencesError::DuplicateKey(_))
        ));
    }

    #[test]
    fn caller_supplied_path_is_validated_without_io() {
        let prefs = Preferences::default();
        let file =
            PreferencesFile::new(r"C:\config\pdfxi\prefs.txt", prefs.clone()).expect("valid path");
        assert_eq!(file.path(), r"C:\config\pdfxi\prefs.txt");
        assert_eq!(file.preferences(), &prefs);
        assert!(matches!(
            PreferencesFile::new("", prefs),
            Err(PreferencesError::EmptyPath)
        ));
    }

    #[test]
    fn preferences_typed_errors_are_distinct() {
        let a = PreferencesError::UnsupportedSchema(7);
        let b = PreferencesError::MissingKey("theme");
        assert_ne!(a, b);
        assert!(a.to_string().contains("7"));
        assert!(b.to_string().contains("theme"));
    }
}
