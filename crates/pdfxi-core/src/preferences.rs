//! Deterministic preferences-file model (PLAT-006).
//!
//! This module models a versioned preferences file: schema version, documented
//! defaults, strict validation, and a byte-for-byte deterministic serialize /
//! parse round-trip. On top of the base set, [`DeploymentPreferences`] models
//! deployment customization — named feature toggles, allowed local paths and
//! an update channel — with deterministic merge/override semantics and its own
//! stable serialization. The path is supplied by the caller and is only
//! *validated and stored*; this module never writes to it, never reads from it,
//! and never touches the filesystem. A host owns all I/O, managed policy and
//! MSI/GPO deployment.
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

/// The release channel a deployment follows for updates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateChannel {
    /// Only stable releases.
    Stable,
    /// Stable releases plus beta builds.
    Beta,
    /// The newest nightly builds.
    Nightly,
    /// Automatic updates are disabled.
    Off,
}

impl UpdateChannel {
    /// Return the stable lowercase token used in the deployment document.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Stable => "stable",
            Self::Beta => "beta",
            Self::Nightly => "nightly",
            Self::Off => "off",
        }
    }

    /// Parse an update-channel token (case-insensitive).
    ///
    /// # Errors
    ///
    /// Returns [`PreferencesError::InvalidUpdateChannel`] for an unknown token.
    pub fn parse(token: &str) -> Result<Self, PreferencesError> {
        match token.trim().to_ascii_lowercase().as_str() {
            "stable" => Ok(Self::Stable),
            "beta" => Ok(Self::Beta),
            "nightly" => Ok(Self::Nightly),
            "off" => Ok(Self::Off),
            other => Err(PreferencesError::InvalidUpdateChannel(other.to_owned())),
        }
    }
}

/// Maximum accepted byte length of a feature-toggle name.
pub const MAX_TOGGLE_NAME_BYTES: usize = 64;

/// A validated deployment-customization set layered over base preferences.
///
/// It models the library-level part of deployment customization: named feature
/// toggles, a list of allowed local paths and an update channel, with
/// deterministic merge/override semantics and a byte-stable serialization. No
/// policy, registry, MSI transform or file I/O is performed — a host applies the
/// values through [`Self::serialize`] output in its own deployment tool.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeploymentPreferences {
    base: Preferences,
    toggles: std::collections::BTreeMap<String, bool>,
    allowed_paths: Vec<String>,
    channel: UpdateChannel,
}

impl DeploymentPreferences {
    /// Construct and validate a deployment-preferences set.
    ///
    /// Toggle names and allowed paths are validated; allowed paths are sorted
    /// and de-duplicated so construction order cannot affect the result.
    ///
    /// # Errors
    ///
    /// Returns a typed [`PreferencesError`] for an invalid toggle name, an
    /// unsafe allowed path or a control character.
    pub fn new(
        base: Preferences,
        toggles: std::collections::BTreeMap<String, bool>,
        allowed_paths: Vec<String>,
        channel: UpdateChannel,
    ) -> Result<Self, PreferencesError> {
        for name in toggles.keys() {
            validate_toggle_name(name)?;
        }
        for path in &allowed_paths {
            validate_allowed_path(path)?;
        }
        let mut allowed_paths = allowed_paths;
        allowed_paths.sort();
        allowed_paths.dedup();
        Ok(Self {
            base,
            toggles,
            allowed_paths,
            channel,
        })
    }

    /// Return the underlying base preferences.
    pub const fn base(&self) -> &Preferences {
        &self.base
    }

    /// Return the named feature toggles, ordered by name.
    pub fn toggles(&self) -> &std::collections::BTreeMap<String, bool> {
        &self.toggles
    }

    /// Return the sorted, de-duplicated allowed paths.
    pub fn allowed_paths(&self) -> &[String] {
        &self.allowed_paths
    }

    /// Return the configured update channel.
    pub const fn update_channel(&self) -> UpdateChannel {
        self.channel
    }

    /// Return whether a named feature toggle is enabled (missing means `false`).
    pub fn is_enabled(&self, name: &str) -> bool {
        self.toggles.get(name).copied().unwrap_or(false)
    }

    /// Merge this set with an overlay using deterministic override semantics.
    ///
    /// The overlay's base preferences and update channel win outright; toggles
    /// are unioned with overlay values taking precedence for shared names, and
    /// allowed paths are unioned, sorted and de-duplicated. The result never
    /// depends on input ordering and cannot fail because both inputs are valid.
    #[must_use = "the merged deployment preferences carry the override result"]
    pub fn merge(&self, overlay: &Self) -> Self {
        let mut toggles = self.toggles.clone();
        for (name, enabled) in &overlay.toggles {
            toggles.insert(name.clone(), *enabled);
        }
        let mut allowed_paths = self.allowed_paths.clone();
        allowed_paths.extend(overlay.allowed_paths.iter().cloned());
        allowed_paths.sort();
        allowed_paths.dedup();
        Self {
            base: overlay.base.clone(),
            toggles,
            allowed_paths,
            channel: overlay.channel,
        }
    }

    /// Serialize into a deterministic, sorted `key=value` document.
    ///
    /// Base keys carry a `base.` prefix, toggles `toggle.<name>`, allowed paths
    /// `allowed_path.<index>` and the channel the `channel` key. All lines are
    /// sorted lexicographically and the document ends with a newline.
    #[must_use = "the serialized document carries the deployment preferences"]
    pub fn serialize(&self) -> String {
        let mut lines: Vec<String> = vec![format!("channel={}", self.channel.as_str())];
        for line in serialize_preferences(&self.base).lines() {
            lines.push(format!("base.{line}"));
        }
        for (name, enabled) in &self.toggles {
            lines.push(format!("toggle.{name}={enabled}"));
        }
        for (index, path) in self.allowed_paths.iter().enumerate() {
            lines.push(format!("allowed_path.{index}={path}"));
        }
        lines.sort();
        let mut out = String::new();
        for line in lines {
            out.push_str(&line);
            out.push('\n');
        }
        out
    }

    /// Parse a document produced by [`Self::serialize`].
    ///
    /// Every base key is required exactly once (validated by
    /// [`parse_preferences`]), as is the `channel` key. Unknown or duplicate
    /// keys, malformed lines and invalid values are rejected.
    ///
    /// # Errors
    ///
    /// Returns a typed [`PreferencesError`] describing the first violation.
    pub fn parse(text: &str) -> Result<Self, PreferencesError> {
        let mut base_lines: Vec<String> = Vec::new();
        let mut toggles = std::collections::BTreeMap::new();
        let mut allowed_paths: Vec<String> = Vec::new();
        let mut channel: Option<UpdateChannel> = None;
        let mut seen: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        for line in text.lines() {
            if line.is_empty() {
                continue;
            }
            let (key, value) = line
                .split_once('=')
                .ok_or_else(|| PreferencesError::MalformedLine(line.to_owned()))?;
            if !seen.insert(key.to_owned()) {
                return Err(PreferencesError::DuplicateKey(key.to_owned()));
            }
            if key == "channel" {
                channel = Some(UpdateChannel::parse(value)?);
            } else if let Some(name) = key.strip_prefix("toggle.") {
                let enabled = match value {
                    "true" => true,
                    "false" => false,
                    other => return Err(PreferencesError::InvalidBoolean(other.to_owned())),
                };
                toggles.insert(name.to_owned(), enabled);
            } else if key.starts_with("allowed_path.") {
                allowed_paths.push(value.to_owned());
            } else if let Some(base_key) = key.strip_prefix("base.") {
                base_lines.push(format!("{base_key}={value}"));
            } else {
                return Err(PreferencesError::UnknownKey(key.to_owned()));
            }
        }
        let channel = channel.ok_or(PreferencesError::MissingKey("channel"))?;
        let base = parse_preferences(&base_lines.join("\n"))?;
        Self::new(base, toggles, allowed_paths, channel)
    }
}

impl Default for DeploymentPreferences {
    fn default() -> Self {
        Self {
            base: Preferences::default(),
            toggles: std::collections::BTreeMap::new(),
            allowed_paths: Vec::new(),
            channel: UpdateChannel::Stable,
        }
    }
}

/// Validate a feature-toggle name: bounded ASCII letters/digits, `_` or `-`.
fn validate_toggle_name(name: &str) -> Result<(), PreferencesError> {
    if name.is_empty()
        || name.len() > MAX_TOGGLE_NAME_BYTES
        || !name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        return Err(PreferencesError::InvalidToggleName(name.to_owned()));
    }
    Ok(())
}

/// Validate an allowed path: rooted, bounded, free of controls and traversal.
fn validate_allowed_path(path: &str) -> Result<(), PreferencesError> {
    if path.is_empty() || path.len() > MAX_PREFERENCES_PATH_BYTES {
        return Err(PreferencesError::InvalidAllowedPath(path.to_owned()));
    }
    if path.chars().any(|c| c.is_control()) {
        return Err(PreferencesError::ControlCharacter);
    }
    for component in path.split(['/', '\\']) {
        if component == ".." || component == "." {
            return Err(PreferencesError::InvalidAllowedPath(path.to_owned()));
        }
    }
    let rooted = path.starts_with('/')
        || path.starts_with('\\')
        || (path.len() >= 2
            && path.as_bytes()[0].is_ascii_alphabetic()
            && path.as_bytes()[1] == b':');
    if !rooted {
        return Err(PreferencesError::InvalidAllowedPath(path.to_owned()));
    }
    Ok(())
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
    /// A feature-toggle name was empty or malformed.
    #[error("invalid deployment toggle name: {0}")]
    InvalidToggleName(String),
    /// An allowed path was empty, unrooted or contained traversal.
    #[error("invalid deployment allowed path: {0}")]
    InvalidAllowedPath(String),
    /// An update-channel token was not recognized.
    #[error("invalid update channel: {0}")]
    InvalidUpdateChannel(String),
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

    // ---- T-0181 PLAT-006 deployment customization ----

    fn toggles(values: &[(&str, bool)]) -> std::collections::BTreeMap<String, bool> {
        values
            .iter()
            .map(|(name, enabled)| ((*name).to_owned(), *enabled))
            .collect()
    }

    fn deployment() -> DeploymentPreferences {
        DeploymentPreferences::new(
            Preferences::default(),
            toggles(&[("dark_mode", true), ("beta_tools", false)]),
            vec![
                r"C:\Program Files\PDFXI".to_owned(),
                "/opt/pdfxi".to_owned(),
            ],
            UpdateChannel::Beta,
        )
        .expect("valid deployment preferences")
    }

    #[test]
    fn deployment_round_trip_is_lossless_and_deterministic() {
        let original = deployment();
        let text = original.serialize();
        assert_eq!(text, original.serialize());
        assert_eq!(text, deployment().serialize());
        let parsed = DeploymentPreferences::parse(&text).expect("round trip");
        assert_eq!(parsed, original);
        assert_eq!(parsed.serialize(), text);
        assert!(text.contains("channel=beta"));
        assert!(text.contains("toggle.dark_mode=true"));
        assert!(text.ends_with('\n'));
    }

    #[test]
    fn deployment_serialization_lines_are_sorted() {
        let text = deployment().serialize();
        let keys: Vec<&str> = text
            .lines()
            .filter_map(|line| line.split_once('=').map(|(key, _)| key))
            .collect();
        let mut sorted = keys.clone();
        sorted.sort_unstable();
        assert_eq!(keys, sorted);
    }

    #[test]
    fn deployment_merge_overrides_deterministically() {
        let base = deployment();
        let overlay = DeploymentPreferences::new(
            Preferences::default(),
            toggles(&[("beta_tools", true), ("new_feature", true)]),
            vec!["/opt/pdfxi".to_owned(), "/srv/data".to_owned()],
            UpdateChannel::Nightly,
        )
        .expect("overlay");
        let merged = base.merge(&overlay);
        assert_eq!(merged.update_channel(), UpdateChannel::Nightly);
        assert!(merged.is_enabled("dark_mode"));
        assert!(merged.is_enabled("beta_tools"));
        assert!(merged.is_enabled("new_feature"));
        assert_eq!(
            merged.allowed_paths(),
            &[
                "/opt/pdfxi".to_owned(),
                "/srv/data".to_owned(),
                r"C:\Program Files\PDFXI".to_owned(),
            ]
        );
        assert_eq!(merged, base.merge(&overlay));
        // The inputs are not mutated by the merge.
        assert!(!base.is_enabled("new_feature"));
        assert_eq!(base.update_channel(), UpdateChannel::Beta);
    }

    #[test]
    fn deployment_validates_toggles_paths_and_channels() {
        assert!(matches!(
            DeploymentPreferences::new(
                Preferences::default(),
                toggles(&[("bad name", true)]),
                vec![],
                UpdateChannel::Stable,
            ),
            Err(PreferencesError::InvalidToggleName(_))
        ));
        assert!(matches!(
            DeploymentPreferences::new(
                Preferences::default(),
                toggles(&[]),
                vec!["relative/path".to_owned()],
                UpdateChannel::Stable,
            ),
            Err(PreferencesError::InvalidAllowedPath(_))
        ));
        assert!(matches!(
            DeploymentPreferences::new(
                Preferences::default(),
                toggles(&[]),
                vec![r"C:\a\..\b".to_owned()],
                UpdateChannel::Stable,
            ),
            Err(PreferencesError::InvalidAllowedPath(_))
        ));
        assert!(matches!(
            UpdateChannel::parse("canary"),
            Err(PreferencesError::InvalidUpdateChannel(_))
        ));
        assert_eq!(
            UpdateChannel::parse("OFF").expect("channel"),
            UpdateChannel::Off
        );
    }

    #[test]
    fn deployment_parse_rejects_malformed_documents() {
        assert!(matches!(
            DeploymentPreferences::parse("channel=beta\n"),
            Err(PreferencesError::MissingKey(_))
        ));
        let mut text = deployment().serialize();
        text.push_str("channel=stable\n");
        assert!(matches!(
            DeploymentPreferences::parse(&text),
            Err(PreferencesError::DuplicateKey(_))
        ));
        let mut text = deployment().serialize();
        text.push_str("bogus=1\n");
        assert!(matches!(
            DeploymentPreferences::parse(&text),
            Err(PreferencesError::UnknownKey(_))
        ));
        let text = deployment()
            .serialize()
            .replace("channel=beta", "channel=canary");
        assert!(matches!(
            DeploymentPreferences::parse(&text),
            Err(PreferencesError::InvalidUpdateChannel(_))
        ));
    }
}
