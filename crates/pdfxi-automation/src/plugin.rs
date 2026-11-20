//! Local WASM-style plugin host contract (no WASM runtime dependency).
//!
//! This module defines the *contract* a local plugin host and a plugin manifest
//! must agree on, without loading or executing any WebAssembly. A plugin is
//! described by a [`PluginManifest`]: immutable metadata plus the set of
//! [`Capability`] values it needs. Capabilities are a closed allowlist of four
//! read/export operations; there is deliberately **no** filesystem, network,
//! process, shell, or environment capability. Anything outside the allowlist is
//! rejected at parse time as [`CapabilityParseError::Forbidden`] or
//! [`CapabilityParseError::Unknown`].
//!
//! The host side is an audited boundary represented by the [`PluginHost`]
//! trait. Grants are **deny by default** ([`CapabilityGrants::new`] is empty),
//! and a [`LocalPluginHost`] concrete implementation is provided for
//! diagnostics and tests. A [`PluginRegistry`] stores validated manifests with
//! deterministic id ordering, enforces one plugin per id, and bounds both the
//! number of plugins and the serialized size of each manifest.
//!
//! Execution is planned, not performed: [`plan_invocation`] turns a
//! [`PluginInvocation`] into an [`InvocationReport`] whose
//! [`InvocationDecision`] allows or denies the call, lists every reason in a
//! canonical (sorted, deduplicated) order, and records the requested
//! [`InvocationBudget`] (fuel/steps, memory bytes, wall-clock milliseconds).
//!
//! All operations are deterministic and allocation-bounded. Serialization goes
//! through the crate's canonical [`crate::JsonValue`], so a manifest round-trips
//! to identical bytes. There is no `unsafe` and no panic in non-test code.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use crate::json::JsonValue;

/// Plugin ABI version implemented by this host contract.
pub const PLUGIN_API_VERSION: u64 = 1;

/// Maximum length, in bytes, of a plugin id.
pub const MAX_ID_BYTES: usize = 64;
/// Maximum length, in bytes, of a plugin display name.
pub const MAX_NAME_BYTES: usize = 128;
/// Maximum length, in bytes, of a plugin version string.
pub const MAX_VERSION_BYTES: usize = 32;
/// Maximum length, in bytes, of an invocation operation name.
pub const MAX_OPERATION_BYTES: usize = 128;
/// Maximum number of distinct capabilities a manifest may request.
pub const MAX_CAPABILITIES: usize = 4;
/// Maximum number of plugins a registry may hold.
pub const MAX_PLUGINS: usize = 256;
/// Maximum serialized byte size of a single manifest.
pub const MAX_MANIFEST_JSON_BYTES: usize = 4096;

/// Absolute ceiling for invocation fuel/steps.
pub const MAX_BUDGET_STEPS: u64 = 10_000_000;
/// Absolute ceiling for invocation memory, in bytes.
pub const MAX_BUDGET_MEMORY_BYTES: u64 = 64 * 1024 * 1024;
/// Absolute ceiling for invocation wall-clock time, in milliseconds.
pub const MAX_BUDGET_WALL_CLOCK_MS: u64 = 30_000;

/// Capabilities a plugin may request.
///
/// This enum is intentionally closed: there is no filesystem, network, process,
/// shell, or environment access. The variants are ordered so sets and reports
/// serialize deterministically.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Capability {
    /// Read document text.
    DocumentText,
    /// Read document metadata.
    DocumentMetadata,
    /// Read rendered page images.
    PageImages,
    /// Export document bytes.
    ExportBytes,
}

impl Capability {
    /// All capabilities, in canonical order.
    pub const ALL: [Self; 4] = [
        Self::DocumentText,
        Self::DocumentMetadata,
        Self::PageImages,
        Self::ExportBytes,
    ];

    /// Stable string identifier for this capability.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::DocumentText => "document-text",
            Self::DocumentMetadata => "document-metadata",
            Self::PageImages => "page-images",
            Self::ExportBytes => "export-bytes",
        }
    }

    /// Parses a capability from its string identifier.
    ///
    /// Names for forbidden operations (filesystem, network, process, shell,
    /// exec, sockets, environment, and similar) are reported as
    /// [`CapabilityParseError::Forbidden`] rather than merely unknown, so a
    /// manifest author gets an actionable message.
    pub fn parse(name: &str) -> Result<Self, CapabilityParseError> {
        let normalized = name.trim().to_ascii_lowercase().replace('_', "-");
        if normalized.is_empty() {
            return Err(CapabilityParseError::Empty);
        }
        if Self::FORBIDDEN_NAMES.contains(&normalized.as_str()) {
            return Err(CapabilityParseError::Forbidden {
                value: name.to_owned(),
            });
        }
        for capability in Self::ALL {
            if capability.as_str() == normalized {
                return Ok(capability);
            }
        }
        Err(CapabilityParseError::Unknown {
            value: name.to_owned(),
        })
    }

    /// Capability identifiers that are explicitly forbidden.
    ///
    /// These never map to a [`Capability`] variant. They are listed here so the
    /// rejection is explicit and auditable rather than incidental.
    pub const FORBIDDEN_NAMES: &'static [&'static str] = &[
        "filesystem",
        "file-system",
        "file",
        "files",
        "network",
        "net",
        "http",
        "https",
        "socket",
        "process",
        "proc",
        "exec",
        "execute",
        "shell",
        "command",
        "spawn",
        "env",
        "environment",
    ];
}

/// Failure to parse a capability identifier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CapabilityParseError {
    /// The identifier was empty or whitespace only.
    Empty,
    /// The identifier names an operation the host never grants.
    Forbidden {
        /// The rejected identifier as supplied.
        value: String,
    },
    /// The identifier is neither supported nor in the forbidden list.
    Unknown {
        /// The unrecognized identifier as supplied.
        value: String,
    },
}

impl fmt::Display for CapabilityParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => f.write_str("capability identifier must not be empty"),
            Self::Forbidden { value } => write!(
                f,
                "capability '{value}' is forbidden: plugins have no filesystem, network, process, shell, or environment access"
            ),
            Self::Unknown { value } => write!(f, "unknown capability '{value}'"),
        }
    }
}

impl std::error::Error for CapabilityParseError {}

/// An error produced while building, validating, or registering a plugin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PluginError {
    /// A required string field was empty or whitespace only.
    EmptyField {
        /// Name of the field.
        field: &'static str,
    },
    /// A string field exceeded its byte bound.
    TooLong {
        /// Name of the field.
        field: &'static str,
        /// Maximum permitted bytes.
        max: usize,
        /// Actual byte length.
        actual: usize,
    },
    /// A plugin id contained characters outside the allowed set.
    InvalidId {
        /// The rejected id.
        id: String,
    },
    /// A required JSON field was missing.
    MissingField {
        /// Name of the field.
        field: &'static str,
    },
    /// A JSON field had an unexpected type.
    InvalidFieldType {
        /// Name of the field.
        field: &'static str,
    },
    /// A capability string could not be parsed.
    Capability(CapabilityParseError),
    /// A manifest requested more than [`MAX_CAPABILITIES`] capabilities.
    TooManyCapabilities {
        /// Number requested.
        count: usize,
    },
    /// The manifest's ABI version is not supported by this host.
    UnsupportedApiVersion {
        /// Version declared by the manifest.
        found: u64,
        /// Version expected by the host.
        expected: u64,
    },
    /// A plugin with this id is already registered.
    DuplicatePluginId {
        /// The duplicated id.
        id: String,
    },
    /// No plugin with this id is registered.
    PluginNotFound {
        /// The unknown id.
        id: String,
    },
    /// The registry is full.
    TooManyPlugins {
        /// Maximum number of plugins.
        max: usize,
    },
    /// A serialized manifest exceeded [`MAX_MANIFEST_JSON_BYTES`].
    ManifestTooLarge {
        /// Actual byte length.
        bytes: usize,
        /// Maximum permitted bytes.
        max: usize,
    },
    /// An invocation budget field was zero or otherwise malformed.
    InvalidBudget {
        /// Name of the offending field.
        field: &'static str,
    },
}

impl fmt::Display for PluginError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyField { field } => write!(f, "plugin {field} must not be empty"),
            Self::TooLong { field, max, actual } => {
                write!(f, "plugin {field} is {actual} bytes; maximum is {max}")
            }
            Self::InvalidId { id } => write!(
                f,
                "invalid plugin id '{id}': use letters, digits, '-', '_' or '.'"
            ),
            Self::MissingField { field } => write!(f, "manifest is missing field '{field}'"),
            Self::InvalidFieldType { field } => {
                write!(f, "manifest field '{field}' has the wrong type")
            }
            Self::Capability(error) => write!(f, "{error}"),
            Self::TooManyCapabilities { count } => write!(
                f,
                "manifest requests {count} capabilities; maximum is {MAX_CAPABILITIES}"
            ),
            Self::UnsupportedApiVersion { found, expected } => write!(
                f,
                "plugin ABI version {found} is not supported; expected {expected}"
            ),
            Self::DuplicatePluginId { id } => {
                write!(f, "a plugin with id '{id}' is already registered")
            }
            Self::PluginNotFound { id } => write!(f, "no plugin with id '{id}' is registered"),
            Self::TooManyPlugins { max } => {
                write!(f, "plugin registry is full; maximum is {max} plugins")
            }
            Self::ManifestTooLarge { bytes, max } => {
                write!(f, "serialized manifest is {bytes} bytes; maximum is {max}")
            }
            Self::InvalidBudget { field } => {
                write!(f, "invocation budget field '{field}' must be non-zero")
            }
        }
    }
}

impl std::error::Error for PluginError {}

impl From<CapabilityParseError> for PluginError {
    fn from(value: CapabilityParseError) -> Self {
        Self::Capability(value)
    }
}

fn validate_bounded(field: &'static str, value: &str, max: usize) -> Result<(), PluginError> {
    if value.trim().is_empty() {
        return Err(PluginError::EmptyField { field });
    }
    let actual = value.len();
    if actual > max {
        return Err(PluginError::TooLong { field, max, actual });
    }
    if field == "id" {
        let valid = value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.');
        if !valid {
            return Err(PluginError::InvalidId {
                id: value.to_owned(),
            });
        }
    }
    if value.chars().any(char::is_control) {
        return match field {
            "id" => Err(PluginError::InvalidId {
                id: value.to_owned(),
            }),
            _ => Err(PluginError::EmptyField { field }),
        };
    }
    Ok(())
}

/// Immutable description of a plugin and the capabilities it requires.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginManifest {
    id: String,
    name: String,
    version: String,
    api_version: u64,
    required_capabilities: BTreeSet<Capability>,
}

impl PluginManifest {
    /// Builds a manifest. Call [`validate`](Self::validate) before registering.
    pub fn new(
        id: impl Into<String>,
        name: impl Into<String>,
        version: impl Into<String>,
        api_version: u64,
        required_capabilities: BTreeSet<Capability>,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            version: version.into(),
            api_version,
            required_capabilities,
        }
    }

    /// Plugin id, unique within a registry.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Human-readable plugin name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Plugin version string.
    pub fn version(&self) -> &str {
        &self.version
    }

    /// ABI version declared by the plugin.
    pub fn api_version(&self) -> u64 {
        self.api_version
    }

    /// Required capabilities, in canonical order.
    pub fn required_capabilities(&self) -> &BTreeSet<Capability> {
        &self.required_capabilities
    }

    /// Validates structure, field bounds, and capability count.
    ///
    /// ABI compatibility is checked separately by
    /// [`check_compatibility`](Self::check_compatibility).
    pub fn validate(&self) -> Result<(), PluginError> {
        validate_bounded("id", &self.id, MAX_ID_BYTES)?;
        validate_bounded("name", &self.name, MAX_NAME_BYTES)?;
        validate_bounded("version", &self.version, MAX_VERSION_BYTES)?;
        if self.required_capabilities.len() > MAX_CAPABILITIES {
            return Err(PluginError::TooManyCapabilities {
                count: self.required_capabilities.len(),
            });
        }
        Ok(())
    }

    /// Checks that this manifest's ABI version matches `expected`.
    pub fn check_compatibility(&self, expected: u64) -> Result<(), PluginError> {
        if self.api_version != expected {
            return Err(PluginError::UnsupportedApiVersion {
                found: self.api_version,
                expected,
            });
        }
        Ok(())
    }

    /// Serializes the manifest to a canonical JSON value.
    pub fn to_json(&self) -> JsonValue {
        JsonValue::object([
            (
                "api_version".to_owned(),
                JsonValue::number(self.api_version),
            ),
            (
                "capabilities".to_owned(),
                JsonValue::from(
                    self.required_capabilities
                        .iter()
                        .map(|capability| capability.as_str().to_owned())
                        .collect::<Vec<String>>(),
                ),
            ),
            ("id".to_owned(), JsonValue::from(self.id.as_str())),
            ("name".to_owned(), JsonValue::from(self.name.as_str())),
            ("version".to_owned(), JsonValue::from(self.version.as_str())),
        ])
    }

    /// Serializes the manifest to a canonical JSON string.
    pub fn to_json_string(&self) -> String {
        self.to_json().to_string()
    }

    /// Parses and validates a manifest from a canonical JSON value.
    pub fn from_json(value: &JsonValue) -> Result<Self, PluginError> {
        let id = json_string(value, "id")?;
        let name = json_string(value, "name")?;
        let version = json_string(value, "version")?;
        let api_version = value.get("api_version").and_then(JsonValue::as_u64).ok_or(
            PluginError::InvalidFieldType {
                field: "api_version",
            },
        )?;
        let mut required_capabilities = BTreeSet::new();
        match value.get("capabilities").and_then(JsonValue::as_array) {
            Some(values) => {
                if values.len() > MAX_CAPABILITIES {
                    return Err(PluginError::TooManyCapabilities {
                        count: values.len(),
                    });
                }
                for entry in values {
                    let text = entry.as_str().ok_or(PluginError::InvalidFieldType {
                        field: "capabilities",
                    })?;
                    required_capabilities.insert(Capability::parse(text)?);
                }
            }
            None => {
                return Err(value
                    .get("capabilities")
                    .map(|_| PluginError::InvalidFieldType {
                        field: "capabilities",
                    })
                    .unwrap_or(PluginError::MissingField {
                        field: "capabilities",
                    }));
            }
        }
        let manifest = Self {
            id,
            name,
            version,
            api_version,
            required_capabilities,
        };
        manifest.validate()?;
        Ok(manifest)
    }

    /// Parses a canonical JSON string into a validated manifest.
    pub fn parse_json(text: &str) -> Result<Self, PluginError> {
        let value = crate::json::JsonValue::parse(text)
            .map_err(|_| PluginError::InvalidFieldType { field: "manifest" })?;
        Self::from_json(&value)
    }
}

fn json_string(value: &JsonValue, field: &'static str) -> Result<String, PluginError> {
    match value.get(field) {
        Some(entry) => entry
            .as_str()
            .map(str::to_owned)
            .ok_or(PluginError::InvalidFieldType { field }),
        None => Err(PluginError::MissingField { field }),
    }
}

/// Rejects a serialized manifest larger than [`MAX_MANIFEST_JSON_BYTES`].
///
/// Returns the byte length on success. This bounds untrusted manifest input
/// before it is parsed or stored.
pub fn validate_manifest_json_size(json: &str) -> Result<usize, PluginError> {
    let bytes = json.len();
    if bytes > MAX_MANIFEST_JSON_BYTES {
        return Err(PluginError::ManifestTooLarge {
            bytes,
            max: MAX_MANIFEST_JSON_BYTES,
        });
    }
    Ok(bytes)
}

/// The host's capability allowlist. Grants are deny by default.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CapabilityGrants {
    granted: BTreeSet<Capability>,
}

impl CapabilityGrants {
    /// Creates an empty grant set: every capability is denied.
    pub fn new() -> Self {
        Self::default()
    }

    /// Grants `capability`.
    pub fn grant(&mut self, capability: Capability) {
        self.granted.insert(capability);
    }

    /// Revokes `capability`.
    pub fn revoke(&mut self, capability: Capability) {
        self.granted.remove(&capability);
    }

    /// Returns `true` only when `capability` is explicitly granted.
    pub fn is_granted(&self, capability: Capability) -> bool {
        self.granted.contains(&capability)
    }

    /// Granted capabilities in canonical order.
    pub fn granted(&self) -> &BTreeSet<Capability> {
        &self.granted
    }
}

/// Per-invocation resource budgets.
///
/// Every field must be non-zero and within the absolute ceilings documented by
/// [`MAX_BUDGET_STEPS`], [`MAX_BUDGET_MEMORY_BYTES`], and
/// [`MAX_BUDGET_WALL_CLOCK_MS`]. Hosts may impose tighter ceilings during
/// planning; those produce a [`DenyReason::BudgetExceedsLimit`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct InvocationBudget {
    /// Maximum fuel/steps the plugin may consume.
    pub max_steps: u64,
    /// Maximum memory in bytes the plugin may allocate.
    pub max_memory_bytes: u64,
    /// Maximum wall-clock time in milliseconds the plugin may run.
    pub max_wall_clock_ms: u64,
}

impl InvocationBudget {
    /// A conservative default budget well within the absolute ceilings.
    pub const DEFAULT: Self = Self {
        max_steps: 1_000_000,
        max_memory_bytes: 16 * 1024 * 1024,
        max_wall_clock_ms: 5_000,
    };

    /// Validates that every field is non-zero and within the absolute ceiling.
    pub fn validate(&self) -> Result<(), PluginError> {
        if self.max_steps == 0 || self.max_steps > MAX_BUDGET_STEPS {
            return Err(PluginError::InvalidBudget { field: "max_steps" });
        }
        if self.max_memory_bytes == 0 || self.max_memory_bytes > MAX_BUDGET_MEMORY_BYTES {
            return Err(PluginError::InvalidBudget {
                field: "max_memory_bytes",
            });
        }
        if self.max_wall_clock_ms == 0 || self.max_wall_clock_ms > MAX_BUDGET_WALL_CLOCK_MS {
            return Err(PluginError::InvalidBudget {
                field: "max_wall_clock_ms",
            });
        }
        Ok(())
    }
}

/// Budget field named in a [`DenyReason::BudgetExceedsLimit`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum BudgetField {
    /// Fuel/steps.
    Steps,
    /// Memory in bytes.
    MemoryBytes,
    /// Wall-clock milliseconds.
    WallClockMs,
}

impl BudgetField {
    /// Stable string name for this field.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Steps => "max_steps",
            Self::MemoryBytes => "max_memory_bytes",
            Self::WallClockMs => "max_wall_clock_ms",
        }
    }
}

/// A planned invocation of a registered plugin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginInvocation {
    plugin_id: String,
    operation: String,
    budget: InvocationBudget,
}

impl PluginInvocation {
    /// Builds and structurally validates an invocation plan.
    pub fn new(
        plugin_id: impl Into<String>,
        operation: impl Into<String>,
        budget: InvocationBudget,
    ) -> Result<Self, PluginError> {
        let plugin_id = plugin_id.into();
        let operation = operation.into();
        validate_bounded("plugin_id", &plugin_id, MAX_ID_BYTES)?;
        validate_bounded("operation", &operation, MAX_OPERATION_BYTES)?;
        budget.validate()?;
        Ok(Self {
            plugin_id,
            operation,
            budget,
        })
    }

    /// Id of the plugin to invoke.
    pub fn plugin_id(&self) -> &str {
        &self.plugin_id
    }

    /// Operation name the plugin should run.
    pub fn operation(&self) -> &str {
        &self.operation
    }

    /// Requested resource budget.
    pub fn budget(&self) -> InvocationBudget {
        self.budget
    }
}

/// A reason an invocation was denied.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum DenyReason {
    /// No plugin with the requested id is registered.
    UnknownPlugin {
        /// The unknown plugin id.
        id: String,
    },
    /// The plugin declares an unsupported ABI version.
    UnsupportedApiVersion {
        /// Declared version.
        found: u64,
        /// Expected version.
        expected: u64,
    },
    /// A required capability is not granted by the host.
    CapabilityNotGranted {
        /// The ungranted capability.
        capability: Capability,
    },
    /// A requested budget exceeds a host ceiling.
    BudgetExceedsLimit {
        /// The offending budget field.
        field: BudgetField,
        /// Requested value.
        requested: u64,
        /// Maximum allowed value.
        allowed: u64,
    },
    /// The operation name was empty.
    EmptyOperation,
    /// The registered manifest failed validation.
    ManifestInvalid {
        /// The affected plugin id.
        id: String,
    },
}

impl DenyReason {
    /// Stable machine-readable code for this reason.
    pub const fn code(&self) -> &'static str {
        match self {
            Self::UnknownPlugin { .. } => "unknown_plugin",
            Self::UnsupportedApiVersion { .. } => "unsupported_api_version",
            Self::CapabilityNotGranted { .. } => "capability_not_granted",
            Self::BudgetExceedsLimit { .. } => "budget_exceeds_limit",
            Self::EmptyOperation => "empty_operation",
            Self::ManifestInvalid { .. } => "manifest_invalid",
        }
    }
}

impl fmt::Display for DenyReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownPlugin { id } => write!(f, "no plugin with id '{id}' is registered"),
            Self::UnsupportedApiVersion { found, expected } => {
                write!(
                    f,
                    "plugin ABI version {found} is not supported; expected {expected}"
                )
            }
            Self::CapabilityNotGranted { capability } => {
                write!(f, "capability '{}' is not granted", capability.as_str())
            }
            Self::BudgetExceedsLimit {
                field,
                requested,
                allowed,
            } => write!(
                f,
                "budget field {} requests {requested} but host allows {allowed}",
                field.as_str()
            ),
            Self::EmptyOperation => f.write_str("operation must not be empty"),
            Self::ManifestInvalid { id } => write!(f, "manifest for '{id}' is invalid"),
        }
    }
}

/// Allow/deny outcome of planning an invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvocationDecision {
    allowed: bool,
    reasons: Vec<DenyReason>,
}

impl InvocationDecision {
    /// Builds a decision from deny reasons; reasons are sorted and deduplicated.
    pub fn new(mut reasons: Vec<DenyReason>) -> Self {
        reasons.sort();
        reasons.dedup();
        Self {
            allowed: reasons.is_empty(),
            reasons,
        }
    }

    /// Builds an allow decision with no reasons.
    pub fn allow() -> Self {
        Self::new(Vec::new())
    }

    /// Returns `true` when the invocation is allowed.
    pub fn is_allowed(&self) -> bool {
        self.allowed
    }

    /// Deny reasons in canonical order. Empty when allowed.
    pub fn reasons(&self) -> &[DenyReason] {
        &self.reasons
    }
}

/// Deterministic report produced by [`plan_invocation`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvocationReport {
    plugin_id: String,
    operation: String,
    decision: InvocationDecision,
    budget: InvocationBudget,
}

impl InvocationReport {
    /// Plugin id that was planned.
    pub fn plugin_id(&self) -> &str {
        &self.plugin_id
    }

    /// Operation that was planned.
    pub fn operation(&self) -> &str {
        &self.operation
    }

    /// The allow/deny decision.
    pub fn decision(&self) -> &InvocationDecision {
        &self.decision
    }

    /// The requested budget.
    pub fn budget(&self) -> InvocationBudget {
        self.budget
    }
}

/// Audited host boundary for plugins.
///
/// Implementations expose the ABI version they support, the capability allowlist
/// (deny by default), and the hard budget ceilings they enforce. The provided
/// [`plan`](Self::plan) method produces a deterministic [`InvocationReport`].
pub trait PluginHost {
    /// ABI version implemented by this host.
    fn api_version(&self) -> u64;

    /// Capability allowlist.
    fn grants(&self) -> &CapabilityGrants;

    /// Hard budget ceilings enforced during planning.
    fn budget_limits(&self) -> &InvocationBudget;

    /// Plans an invocation against this host and a registry.
    fn plan(&self, registry: &PluginRegistry, invocation: &PluginInvocation) -> InvocationReport {
        plan_invocation(self, registry, invocation)
    }
}

/// A concrete, dependency-free host with deny-by-default grants.
#[derive(Debug, Clone)]
pub struct LocalPluginHost {
    grants: CapabilityGrants,
    budget_limits: InvocationBudget,
}

impl LocalPluginHost {
    /// Creates a host that grants nothing and uses [`InvocationBudget::DEFAULT`].
    pub fn new() -> Self {
        Self {
            grants: CapabilityGrants::new(),
            budget_limits: InvocationBudget::DEFAULT,
        }
    }

    /// Replaces the capability grants.
    pub fn with_grants(mut self, grants: CapabilityGrants) -> Self {
        self.grants = grants;
        self
    }

    /// Replaces the budget ceilings.
    pub fn with_budget_limits(mut self, budget_limits: InvocationBudget) -> Self {
        self.budget_limits = budget_limits;
        self
    }

    /// Grants a single capability.
    pub fn grant(mut self, capability: Capability) -> Self {
        self.grants.grant(capability);
        self
    }
}

impl Default for LocalPluginHost {
    fn default() -> Self {
        Self::new()
    }
}

impl PluginHost for LocalPluginHost {
    fn api_version(&self) -> u64 {
        PLUGIN_API_VERSION
    }

    fn grants(&self) -> &CapabilityGrants {
        &self.grants
    }

    fn budget_limits(&self) -> &InvocationBudget {
        &self.budget_limits
    }
}

/// Plans `invocation` against `registry` and `host` without executing anything.
///
/// Reasons are collected exhaustively (not fail-fast), then sorted and
/// deduplicated by [`InvocationDecision::new`] so the report is deterministic.
pub fn plan_invocation<H: PluginHost + ?Sized>(
    host: &H,
    registry: &PluginRegistry,
    invocation: &PluginInvocation,
) -> InvocationReport {
    let mut reasons = Vec::new();
    match registry.get(invocation.plugin_id()) {
        None => reasons.push(DenyReason::UnknownPlugin {
            id: invocation.plugin_id().to_owned(),
        }),
        Some(manifest) => {
            if manifest.api_version() != host.api_version() {
                reasons.push(DenyReason::UnsupportedApiVersion {
                    found: manifest.api_version(),
                    expected: host.api_version(),
                });
            }
            if manifest.validate().is_err() {
                reasons.push(DenyReason::ManifestInvalid {
                    id: invocation.plugin_id().to_owned(),
                });
            }
            for capability in manifest.required_capabilities() {
                if !host.grants().is_granted(*capability) {
                    reasons.push(DenyReason::CapabilityNotGranted {
                        capability: *capability,
                    });
                }
            }
            let requested = invocation.budget();
            let limits = *host.budget_limits();
            if requested.max_steps > limits.max_steps {
                reasons.push(DenyReason::BudgetExceedsLimit {
                    field: BudgetField::Steps,
                    requested: requested.max_steps,
                    allowed: limits.max_steps,
                });
            }
            if requested.max_memory_bytes > limits.max_memory_bytes {
                reasons.push(DenyReason::BudgetExceedsLimit {
                    field: BudgetField::MemoryBytes,
                    requested: requested.max_memory_bytes,
                    allowed: limits.max_memory_bytes,
                });
            }
            if requested.max_wall_clock_ms > limits.max_wall_clock_ms {
                reasons.push(DenyReason::BudgetExceedsLimit {
                    field: BudgetField::WallClockMs,
                    requested: requested.max_wall_clock_ms,
                    allowed: limits.max_wall_clock_ms,
                });
            }
        }
    }
    InvocationReport {
        plugin_id: invocation.plugin_id().to_owned(),
        operation: invocation.operation().to_owned(),
        decision: InvocationDecision::new(reasons),
        budget: invocation.budget(),
    }
}

/// Deterministic container of validated plugin manifests.
///
/// Backed by a [`BTreeMap`] keyed by id, so iteration order is always sorted by
/// id regardless of insertion order. Registration enforces one plugin per id,
/// a bounded plugin count, and a bounded serialized manifest size.
#[derive(Debug, Clone, Default)]
pub struct PluginRegistry {
    plugins: BTreeMap<String, PluginManifest>,
}

impl PluginRegistry {
    /// Creates an empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a validated manifest.
    ///
    /// Fails on structural problems, an unsupported ABI version, a duplicate
    /// id, a full registry, or an oversized serialized manifest.
    pub fn register(&mut self, manifest: PluginManifest) -> Result<(), PluginError> {
        manifest.validate()?;
        manifest.check_compatibility(PLUGIN_API_VERSION)?;
        validate_manifest_json_size(&manifest.to_json_string())?;
        if self.plugins.contains_key(manifest.id()) {
            return Err(PluginError::DuplicatePluginId {
                id: manifest.id().to_owned(),
            });
        }
        if self.plugins.len() >= MAX_PLUGINS {
            return Err(PluginError::TooManyPlugins { max: MAX_PLUGINS });
        }
        let id = manifest.id().to_owned();
        self.plugins.insert(id, manifest);
        Ok(())
    }

    /// Returns the manifest registered under `id`.
    pub fn get(&self, id: &str) -> Option<&PluginManifest> {
        self.plugins.get(id)
    }

    /// Number of registered plugins.
    pub fn len(&self) -> usize {
        self.plugins.len()
    }

    /// Returns `true` when no plugins are registered.
    pub fn is_empty(&self) -> bool {
        self.plugins.is_empty()
    }

    /// Plugin ids in sorted order.
    pub fn ids(&self) -> impl Iterator<Item = &str> {
        self.plugins.keys().map(String::as_str)
    }

    /// Manifests in sorted id order.
    pub fn manifests(&self) -> impl Iterator<Item = &PluginManifest> {
        self.plugins.values()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(id: &str, caps: &[Capability]) -> PluginManifest {
        PluginManifest::new(
            id,
            format!("Plugin {id}"),
            "1.0.0",
            PLUGIN_API_VERSION,
            caps.iter().copied().collect(),
        )
    }

    #[test]
    fn manifest_validation_accepts_valid_and_rejects_empty_fields() {
        let valid = manifest("good.plugin-1", &[Capability::DocumentText]);
        assert_eq!(valid.validate(), Ok(()));

        let empty_id =
            PluginManifest::new("", "Name", "1.0.0", PLUGIN_API_VERSION, BTreeSet::new());
        assert_eq!(
            empty_id.validate(),
            Err(PluginError::EmptyField { field: "id" })
        );

        let bad_id = PluginManifest::new(
            "bad id/with:chars",
            "Name",
            "1.0.0",
            PLUGIN_API_VERSION,
            BTreeSet::new(),
        );
        assert!(matches!(
            bad_id.validate(),
            Err(PluginError::InvalidId { .. })
        ));
    }

    #[test]
    fn unsupported_api_version_is_rejected() {
        let old = PluginManifest::new(
            "old",
            "Old",
            "1.0.0",
            PLUGIN_API_VERSION - 1,
            BTreeSet::new(),
        );
        assert_eq!(
            old.check_compatibility(PLUGIN_API_VERSION),
            Err(PluginError::UnsupportedApiVersion {
                found: PLUGIN_API_VERSION - 1,
                expected: PLUGIN_API_VERSION,
            })
        );
        let mut registry = PluginRegistry::new();
        assert!(matches!(
            registry.register(old),
            Err(PluginError::UnsupportedApiVersion { .. })
        ));
    }

    #[test]
    fn duplicate_id_is_rejected_by_registry() {
        let mut registry = PluginRegistry::new();
        assert_eq!(registry.register(manifest("alpha", &[])), Ok(()));
        assert_eq!(
            registry.register(manifest("alpha", &[Capability::PageImages])),
            Err(PluginError::DuplicatePluginId {
                id: "alpha".to_owned()
            })
        );
        assert_eq!(registry.len(), 1);
    }

    #[test]
    fn unknown_and_forbidden_capabilities_are_rejected() {
        assert_eq!(
            Capability::parse("teleport"),
            Err(CapabilityParseError::Unknown {
                value: "teleport".to_owned()
            })
        );
        for forbidden in [
            "filesystem",
            "network",
            "process",
            "shell",
            "EXEC",
            "file-system",
        ] {
            assert!(
                matches!(
                    Capability::parse(forbidden),
                    Err(CapabilityParseError::Forbidden { .. })
                ),
                "{forbidden} should be forbidden"
            );
        }
        assert_eq!(
            Capability::parse("Document_Text"),
            Ok(Capability::DocumentText)
        );
        assert_eq!(Capability::parse("  "), Err(CapabilityParseError::Empty));
    }

    #[test]
    fn capability_grants_are_deny_by_default_and_enforced() {
        let grants = CapabilityGrants::new();
        assert!(grants.granted().is_empty());
        assert!(!grants.is_granted(Capability::DocumentText));
        let mut grants = grants;
        grants.grant(Capability::DocumentText);
        assert!(grants.is_granted(Capability::DocumentText));
        grants.revoke(Capability::DocumentText);
        assert!(!grants.is_granted(Capability::DocumentText));
    }

    #[test]
    fn budget_validation_checks_zero_and_ceiling() {
        assert_eq!(InvocationBudget::DEFAULT.validate(), Ok(()));
        assert_eq!(
            InvocationBudget {
                max_steps: 0,
                ..InvocationBudget::DEFAULT
            }
            .validate(),
            Err(PluginError::InvalidBudget { field: "max_steps" })
        );
        assert!(matches!(
            InvocationBudget {
                max_memory_bytes: MAX_BUDGET_MEMORY_BYTES + 1,
                ..InvocationBudget::DEFAULT
            }
            .validate(),
            Err(PluginError::InvalidBudget {
                field: "max_memory_bytes"
            })
        ));
        assert!(matches!(
            PluginInvocation::new("p", "", InvocationBudget::DEFAULT),
            Err(PluginError::EmptyField { field: "operation" })
        ));
    }

    #[test]
    fn registry_order_is_deterministic_regardless_of_insertion_order() {
        let mut a = PluginRegistry::new();
        for id in ["zeta", "alpha", "mid"] {
            assert_eq!(a.register(manifest(id, &[])), Ok(()));
        }
        let mut b = PluginRegistry::new();
        for id in ["mid", "zeta", "alpha"] {
            assert_eq!(b.register(manifest(id, &[])), Ok(()));
        }
        let ids_a: Vec<&str> = a.ids().collect();
        let ids_b: Vec<&str> = b.ids().collect();
        assert_eq!(ids_a, vec!["alpha", "mid", "zeta"]);
        assert_eq!(ids_a, ids_b);
        let names: Vec<&str> = a.manifests().map(PluginManifest::id).collect();
        assert_eq!(names, ids_a);
    }

    #[test]
    fn invocation_deny_reasons_are_ordered_and_cover_each_case() {
        let mut registry = PluginRegistry::new();
        assert_eq!(
            registry.register(manifest(
                "p",
                &[Capability::DocumentMetadata, Capability::DocumentText]
            )),
            Ok(())
        );

        // Unknown plugin: single, deterministic reason.
        let unknown = PluginInvocation::new("missing", "run", InvocationBudget::DEFAULT)
            .expect("test-only construction");
        let report = plan_invocation(&LocalPluginHost::new(), &registry, &unknown);
        assert!(!report.decision().is_allowed());
        assert_eq!(
            report.decision().reasons(),
            &[DenyReason::UnknownPlugin {
                id: "missing".to_owned()
            }]
        );

        // Known plugin, no grants and tight budget: several reasons, sorted.
        let limited_host = LocalPluginHost::new().with_budget_limits(InvocationBudget {
            max_steps: 10,
            max_memory_bytes: 10,
            max_wall_clock_ms: 10,
        });
        let big = PluginInvocation::new(
            "p",
            "run",
            InvocationBudget {
                max_steps: 100,
                max_memory_bytes: 100,
                max_wall_clock_ms: 100,
            },
        )
        .expect("test-only construction");
        let report = plan_invocation(&limited_host, &registry, &big);
        let reasons = report.decision().reasons();
        assert!(!report.decision().is_allowed());
        assert_eq!(reasons.len(), 5);
        // Canonical order is the derived enum `Ord` (variant order then fields),
        // which is stable regardless of the order checks ran in.
        let mut sorted: Vec<DenyReason> = reasons.to_vec();
        sorted.sort();
        sorted.dedup();
        assert_eq!(
            reasons,
            sorted.as_slice(),
            "reasons must be in canonical order"
        );
        assert!(reasons.iter().any(|r| matches!(
            r,
            DenyReason::CapabilityNotGranted {
                capability: Capability::DocumentText
            }
        )));
        assert!(reasons.iter().any(|r| matches!(
            r,
            DenyReason::BudgetExceedsLimit {
                field: BudgetField::Steps,
                ..
            }
        )));
    }

    #[test]
    fn allowed_invocation_has_empty_reasons_and_requested_budget() {
        let mut registry = PluginRegistry::new();
        assert_eq!(
            registry.register(manifest("p", &[Capability::DocumentText])),
            Ok(())
        );
        let host = LocalPluginHost::new().grant(Capability::DocumentText);
        let invocation =
            PluginInvocation::new("p", "extract", InvocationBudget::DEFAULT).expect("test-only");
        let report = host.plan(&registry, &invocation);
        assert!(report.decision().is_allowed());
        assert!(report.decision().reasons().is_empty());
        assert_eq!(report.budget(), InvocationBudget::DEFAULT);
        assert_eq!(report.plugin_id(), "p");
        assert_eq!(report.operation(), "extract");
    }

    #[test]
    fn oversized_manifest_and_field_bounds_are_rejected() {
        let huge = "x".repeat(MAX_MANIFEST_JSON_BYTES + 1);
        assert_eq!(
            validate_manifest_json_size(&huge),
            Err(PluginError::ManifestTooLarge {
                bytes: huge.len(),
                max: MAX_MANIFEST_JSON_BYTES,
            })
        );
        assert_eq!(validate_manifest_json_size("{}"), Ok(2));

        let long_name = PluginManifest::new(
            "p",
            "n".repeat(MAX_NAME_BYTES + 1),
            "1.0.0",
            PLUGIN_API_VERSION,
            BTreeSet::new(),
        );
        assert_eq!(
            long_name.validate(),
            Err(PluginError::TooLong {
                field: "name",
                max: MAX_NAME_BYTES,
                actual: MAX_NAME_BYTES + 1,
            })
        );
    }

    #[test]
    fn serialization_round_trips_byte_for_byte() {
        let caps = [
            Capability::ExportBytes,
            Capability::DocumentText,
            Capability::PageImages,
        ];
        let original = manifest("round.trip", &caps);
        let json = original.to_json_string();
        // Canonical form: sorted keys, no whitespace.
        assert!(json.starts_with('{') && json.ends_with('}'));
        assert!(!json.contains(", "));
        let reparsed = PluginManifest::parse_json(&json).expect("test-only round trip");
        assert_eq!(reparsed, original);
        assert_eq!(reparsed.to_json_string(), json);
        // Parse/serialize a second time: still identical (stability).
        let again = PluginManifest::parse_json(&json).expect("test-only round trip");
        assert_eq!(again.to_json_string(), json);
    }

    #[test]
    fn json_parsing_rejects_forbidden_capability_and_bad_types() {
        let json =
            r#"{"api_version":1,"capabilities":["filesystem"],"id":"p","name":"P","version":"1"}"#;
        assert!(matches!(
            PluginManifest::parse_json(json),
            Err(PluginError::Capability(
                CapabilityParseError::Forbidden { .. }
            ))
        ));
        let missing = r#"{"api_version":1,"capabilities":[],"name":"P","version":"1"}"#;
        assert_eq!(
            PluginManifest::parse_json(missing),
            Err(PluginError::MissingField { field: "id" })
        );
        let bad_type = r#"{"api_version":"1","capabilities":[],"id":"p","name":"P","version":"1"}"#;
        assert_eq!(
            PluginManifest::parse_json(bad_type),
            Err(PluginError::InvalidFieldType {
                field: "api_version"
            })
        );
    }

    #[test]
    fn error_messages_are_actionable() {
        let message = PluginError::DuplicatePluginId {
            id: "dup".to_owned(),
        }
        .to_string();
        assert!(message.contains("dup"));
        assert!(message.contains("already registered"));
        let forbidden = CapabilityParseError::Forbidden {
            value: "network".to_owned(),
        }
        .to_string();
        assert!(forbidden.contains("no filesystem, network, process"));
    }
}
