//! Named toolsets: JSON files that map aliases to saved actions.
//!
//! A toolset is a small, deterministic JSON document:
//!
//! ```json
//! {
//!   "version": 1,
//!   "name": "maintenance",
//!   "tools": {
//!     "daily": {
//!       "file": "actions.txt",
//!       "action": "nightly",
//!       "defaults": { "source": "in.pdf" }
//!     }
//!   }
//! }
//! ```
//!
//! Aliases are sorted when listed; each entry names an action file, an action
//! within that file, and optional parameter defaults applied to steps that do
//! not already define those keys. Validation rejects empty names, empty
//! toolsets, and aliases that shadow dangerous built-in operations
//! ([`DANGEROUS_ALIASES`]). The API layer additionally refuses to *run* an
//! alias whose target action contains a dangerous step kind
//! ([`DANGEROUS_KINDS`]), so a toolset cannot be used to launder an
//! unchecked operation behind a friendly name.

use std::collections::BTreeMap;
use std::fmt;

use crate::json::{JsonError, JsonValue};

/// Toolset document version accepted by this implementation.
pub const TOOLSET_VERSION: u64 = 1;

/// Aliases that may never be defined in a toolset.
///
/// A toolset is an allowlist of friendly names; allowing one of these would
/// let a file masquerade as a built-in shell, execution, or destructive
/// operation. Matching is case-insensitive with `-` and `_` treated alike.
pub const DANGEROUS_ALIASES: &[&str] = &[
    "exec",
    "execute",
    "shell",
    "cmd",
    "command",
    "run-command",
    "system",
    "powershell",
    "bash",
    "delete",
    "format",
    "security",
    "custom-command",
];

/// Step kinds a toolset alias is not allowed to reach.
///
/// `Security` changes protection settings and `CustomCommand` represents an
/// arbitrary command; neither may be invoked through a declarative alias.
pub const DANGEROUS_KINDS: &[crate::StepKind] =
    &[crate::StepKind::Security, crate::StepKind::CustomCommand];

/// Returns `true` when `alias` shadows a dangerous built-in operation.
pub fn is_dangerous_alias(alias: &str) -> bool {
    let normalized = normalize_alias(alias);
    DANGEROUS_ALIASES
        .iter()
        .any(|name| normalize_alias(name) == normalized)
}

fn normalize_alias(alias: &str) -> String {
    alias
        .chars()
        .map(|ch| if ch == '_' { '-' } else { ch })
        .flat_map(char::to_lowercase)
        .collect()
}

/// A single alias definition inside a [`Toolset`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tool {
    /// Path to the saved action file.
    file: String,
    /// Name of the action inside that file.
    action: String,
    /// Parameter defaults, sorted by key.
    defaults: BTreeMap<String, String>,
}

impl Tool {
    /// Creates a tool definition.
    pub fn new(
        file: impl Into<String>,
        action: impl Into<String>,
        defaults: BTreeMap<String, String>,
    ) -> Self {
        Self {
            file: file.into(),
            action: action.into(),
            defaults,
        }
    }

    /// Returns the saved action file path.
    pub fn file(&self) -> &str {
        &self.file
    }

    /// Returns the action name inside the file.
    pub fn action(&self) -> &str {
        &self.action
    }

    /// Returns the parameter defaults in sorted key order.
    pub fn defaults(&self) -> &BTreeMap<String, String> {
        &self.defaults
    }

    fn to_json(&self) -> JsonValue {
        JsonValue::object([
            ("action".to_owned(), JsonValue::from(self.action.as_str())),
            (
                "defaults".to_owned(),
                JsonValue::Object(
                    self.defaults
                        .iter()
                        .map(|(key, value)| (key.clone(), JsonValue::from(value.as_str())))
                        .collect(),
                ),
            ),
            ("file".to_owned(), JsonValue::from(self.file.as_str())),
        ])
    }
}

/// A validated, deterministic collection of named tool aliases.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Toolset {
    name: String,
    tools: BTreeMap<String, Tool>,
}

impl Toolset {
    /// Parses a toolset from JSON text and validates it.
    pub fn from_json(text: &str) -> Result<Self, ToolsetError> {
        let value =
            JsonValue::parse(text).map_err(|error: JsonError| ToolsetError::InvalidJson {
                reason: error.to_string(),
            })?;
        let entries = value.as_object().ok_or_else(|| {
            ToolsetError::InvalidShape("toolset must be a JSON object".to_owned())
        })?;

        let version = entries
            .get("version")
            .and_then(JsonValue::as_u64)
            .ok_or_else(|| ToolsetError::InvalidShape("'version' must be an integer".to_owned()))?;
        if version != TOOLSET_VERSION {
            return Err(ToolsetError::Version {
                found: version,
                expected: TOOLSET_VERSION,
            });
        }

        let name = entries
            .get("name")
            .and_then(JsonValue::as_str)
            .ok_or_else(|| ToolsetError::InvalidShape("'name' must be a string".to_owned()))?
            .to_owned();
        let raw_tools = entries
            .get("tools")
            .and_then(JsonValue::as_object)
            .ok_or_else(|| ToolsetError::InvalidShape("'tools' must be an object".to_owned()))?;

        let mut tools = BTreeMap::new();
        for (alias, raw_tool) in raw_tools {
            let fields = raw_tool.as_object().ok_or_else(|| {
                ToolsetError::InvalidShape(format!("tool '{alias}' must be an object"))
            })?;
            let file = require_string_field(fields, "file", alias)?;
            let action = require_string_field(fields, "action", alias)?;
            let mut defaults = BTreeMap::new();
            if let Some(raw_defaults) = fields.get("defaults") {
                let default_entries = raw_defaults.as_object().ok_or_else(|| {
                    ToolsetError::InvalidShape(format!("tool '{alias}' defaults must be an object"))
                })?;
                for (key, value) in default_entries {
                    let text = value.as_str().ok_or_else(|| {
                        ToolsetError::InvalidShape(format!(
                            "default '{key}' of tool '{alias}' must be a string"
                        ))
                    })?;
                    defaults.insert(key.clone(), text.to_owned());
                }
            }
            tools.insert(alias.clone(), Tool::new(file, action, defaults));
        }

        let toolset = Self { name, tools };
        toolset.validate()?;
        Ok(toolset)
    }

    /// Serializes the toolset to deterministic, compact JSON.
    pub fn to_json(&self) -> String {
        JsonValue::object([
            ("name".to_owned(), JsonValue::from(self.name.as_str())),
            (
                "tools".to_owned(),
                JsonValue::Object(
                    self.tools
                        .iter()
                        .map(|(alias, tool)| (alias.clone(), tool.to_json()))
                        .collect(),
                ),
            ),
            ("version".to_owned(), JsonValue::number(TOOLSET_VERSION)),
        ])
        .to_string()
    }

    /// Validates structural invariants and the dangerous-alias denylist.
    pub fn validate(&self) -> Result<(), ToolsetError> {
        if self.name.trim().is_empty() {
            return Err(ToolsetError::EmptyName);
        }
        if self.tools.is_empty() {
            return Err(ToolsetError::EmptyToolset);
        }
        for (alias, tool) in &self.tools {
            if alias.trim().is_empty() {
                return Err(ToolsetError::EmptyAlias);
            }
            if is_dangerous_alias(alias) {
                return Err(ToolsetError::DangerousAlias {
                    alias: alias.clone(),
                });
            }
            if tool.file.trim().is_empty() {
                return Err(ToolsetError::EmptyFile {
                    alias: alias.clone(),
                });
            }
            if tool.action.trim().is_empty() {
                return Err(ToolsetError::EmptyAction {
                    alias: alias.clone(),
                });
            }
            for key in tool.defaults.keys() {
                if key.trim().is_empty() {
                    return Err(ToolsetError::EmptyDefaultKey {
                        alias: alias.clone(),
                    });
                }
            }
        }
        Ok(())
    }

    /// Returns the toolset name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the tool with the given alias, if present.
    pub fn get(&self, alias: &str) -> Option<&Tool> {
        self.tools.get(alias)
    }

    /// Returns aliases in sorted order.
    pub fn aliases(&self) -> impl ExactSizeIterator<Item = &str> {
        self.tools.keys().map(String::as_str)
    }

    /// Returns every `(alias, tool)` pair in sorted alias order.
    pub fn tools(&self) -> impl ExactSizeIterator<Item = (&str, &Tool)> {
        self.tools
            .iter()
            .map(|(alias, tool)| (alias.as_str(), tool))
    }

    /// Returns the number of defined tools.
    pub fn len(&self) -> usize {
        self.tools.len()
    }

    /// Returns `true` when the toolset has no tools.
    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }
}

fn require_string_field(
    fields: &BTreeMap<String, JsonValue>,
    key: &str,
    alias: &str,
) -> Result<String, ToolsetError> {
    let value = fields.get(key).and_then(JsonValue::as_str).ok_or_else(|| {
        ToolsetError::InvalidShape(format!("tool '{alias}' field '{key}' must be a string"))
    })?;
    Ok(value.to_owned())
}

/// A toolset validation failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolsetError {
    /// The JSON text was malformed.
    InvalidJson {
        /// Parser diagnostic.
        reason: String,
    },
    /// A document field had the wrong shape.
    InvalidShape(String),
    /// The toolset document version was not supported.
    Version {
        /// The version found in the document.
        found: u64,
        /// The version expected by this implementation.
        expected: u64,
    },
    /// The toolset name was empty or whitespace only.
    EmptyName,
    /// The toolset defined no tools.
    EmptyToolset,
    /// An alias was empty or whitespace only.
    EmptyAlias,
    /// An alias shadowed a dangerous built-in operation.
    DangerousAlias {
        /// The rejected alias.
        alias: String,
    },
    /// A tool's action file was empty.
    EmptyFile {
        /// The alias whose tool was invalid.
        alias: String,
    },
    /// A tool's action name was empty.
    EmptyAction {
        /// The alias whose tool was invalid.
        alias: String,
    },
    /// A default parameter key was empty.
    EmptyDefaultKey {
        /// The alias whose tool was invalid.
        alias: String,
    },
}

impl ToolsetError {
    /// Returns a stable machine-readable error code.
    pub fn code(&self) -> &'static str {
        match self {
            Self::InvalidJson { .. } => "invalid_toolset_json",
            Self::InvalidShape(_) => "invalid_toolset_shape",
            Self::Version { .. } => "toolset_version_mismatch",
            Self::EmptyName => "empty_toolset_name",
            Self::EmptyToolset => "empty_toolset",
            Self::EmptyAlias => "empty_tool_alias",
            Self::DangerousAlias { .. } => "dangerous_alias",
            Self::EmptyFile { .. } => "empty_tool_file",
            Self::EmptyAction { .. } => "empty_tool_action",
            Self::EmptyDefaultKey { .. } => "empty_default_key",
        }
    }
}

impl fmt::Display for ToolsetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidJson { reason } => write!(f, "invalid toolset JSON: {reason}"),
            Self::InvalidShape(reason) => write!(f, "invalid toolset: {reason}"),
            Self::Version { found, expected } => write!(
                f,
                "unsupported toolset version {found}; expected {expected}"
            ),
            Self::EmptyName => f.write_str("toolset name must not be empty"),
            Self::EmptyToolset => f.write_str("toolset must define at least one tool"),
            Self::EmptyAlias => f.write_str("tool alias must not be empty"),
            Self::DangerousAlias { alias } => write!(
                f,
                "tool alias '{alias}' shadows a dangerous built-in operation"
            ),
            Self::EmptyFile { alias } => {
                write!(f, "tool '{alias}' must name an action file")
            }
            Self::EmptyAction { alias } => {
                write!(f, "tool '{alias}' must name an action in that file")
            }
            Self::EmptyDefaultKey { alias } => write!(
                f,
                "tool '{alias}' has a default parameter with an empty key"
            ),
        }
    }
}

impl std::error::Error for ToolsetError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn toolset_json() -> String {
        "{\"version\":1,\"name\":\"maintenance\",\"tools\":{\"daily\":{\"file\":\"actions.txt\",\"action\":\"nightly\",\"defaults\":{\"source\":\"in.pdf\"}},\"weekly\":{\"file\":\"actions.txt\",\"action\":\"weekend\"}}}"
            .to_owned()
    }

    #[test]
    fn parse_validate_and_serialize_are_deterministic() {
        let toolset = Toolset::from_json(&toolset_json()).expect("valid toolset");
        assert!(toolset.validate().is_ok());
        assert_eq!(toolset.name(), "maintenance");
        assert_eq!(toolset.len(), 2);
        assert_eq!(toolset.aliases().collect::<Vec<_>>(), ["daily", "weekly"]);
        let canonical = toolset.to_json();
        assert_eq!(Toolset::from_json(&canonical).expect("canonical"), toolset);
        assert_eq!(canonical, toolset.to_json());
    }

    #[test]
    fn defaults_are_sorted_and_preserved() {
        let toolset = Toolset::from_json(&toolset_json()).expect("valid toolset");
        let daily = toolset.get("daily").expect("daily alias");
        assert_eq!(daily.file(), "actions.txt");
        assert_eq!(daily.action(), "nightly");
        assert_eq!(
            daily.defaults().get("source").map(String::as_str),
            Some("in.pdf")
        );
    }

    #[test]
    fn dangerous_aliases_are_rejected_case_insensitively() {
        for alias in ["exec", "EXEC", "run_command", "shell", "Custom-Command"] {
            let text = format!(
                "{{\"version\":1,\"name\":\"n\",\"tools\":{{\"{alias}\":{{\"file\":\"f\",\"action\":\"a\"}}}}}}"
            );
            let error = Toolset::from_json(&text).expect_err("dangerous alias must be rejected");
            assert_eq!(error.code(), "dangerous_alias");
        }
        assert!(is_dangerous_alias("PoWeRsHeLL"));
        assert!(!is_dangerous_alias("daily"));
    }

    #[test]
    fn structural_errors_have_stable_codes() {
        let cases = [
            ("{", "invalid_toolset_json"),
            ("[]", "invalid_toolset_shape"),
            (
                "{\"version\":2,\"name\":\"n\",\"tools\":{\"a\":{\"file\":\"f\",\"action\":\"a\"}}}",
                "toolset_version_mismatch",
            ),
            (
                "{\"version\":1,\"name\":\" \",\"tools\":{}}",
                "empty_toolset_name",
            ),
            (
                "{\"version\":1,\"name\":\"n\",\"tools\":{}}",
                "empty_toolset",
            ),
            (
                "{\"version\":1,\"name\":\"n\",\"tools\":{\"a\":{\"file\":\" \",\"action\":\"a\"}}}",
                "empty_tool_file",
            ),
            (
                "{\"version\":1,\"name\":\"n\",\"tools\":{\"a\":{\"file\":\"f\",\"action\":\"\"}}}",
                "empty_tool_action",
            ),
            (
                "{\"version\":1,\"name\":\"n\",\"tools\":{\"a\":{\"file\":\"f\",\"action\":\"a\",\"defaults\":{\"\":\"v\"}}}}",
                "empty_default_key",
            ),
        ];
        for (text, expected) in cases {
            let error = Toolset::from_json(text).expect_err("invalid toolset must fail");
            assert_eq!(error.code(), expected, "for input {text}");
        }
    }
}
