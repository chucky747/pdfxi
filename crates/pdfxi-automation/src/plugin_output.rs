//! Typed plugin output contract and deterministic execution planning.
//!
//! This module completes the library-level plugin-host contract without loading
//! or executing WebAssembly. It adds two things to the manifest/invocation model
//! in [`crate::plugin`]:
//!
//! - A typed [`PluginValue`] result with bounded text, byte, and total sizes.
//!   [`PluginOutput::validate`] rejects oversized or wrong-typed returns with a
//!   typed [`PluginOutputError`] before a host ever consumes them.
//! - A deterministic [`ExecutionPlan`] built from the existing
//!   [`plan_invocation`](crate::plugin::plan_invocation) decision plus an output
//!   budget. The plan is an ordered list of [`ExecutionStep`]s with stable codes;
//!   no plugin code is executed and no I/O occurs.

use std::fmt;

use crate::json::JsonValue;
use crate::plugin::{
    DenyReason, InvocationReport, PluginHost, PluginInvocation, PluginRegistry, plan_invocation,
};

/// Default maximum length of a text output value, in bytes.
pub const OUTPUT_DEFAULT_MAX_TEXT_BYTES: usize = 64 * 1024;
/// Default maximum length of a byte output value, in bytes.
pub const OUTPUT_DEFAULT_MAX_BYTES: usize = 1024 * 1024;
/// Default maximum total encoded size of one output value, in bytes.
pub const OUTPUT_DEFAULT_MAX_TOTAL_BYTES: usize = 2 * 1024 * 1024;

/// Absolute ceiling for a text output value, in bytes.
pub const OUTPUT_ABSOLUTE_MAX_TEXT_BYTES: usize = 16 * 1024 * 1024;
/// Absolute ceiling for a byte output value, in bytes.
pub const OUTPUT_ABSOLUTE_MAX_BYTES: usize = 64 * 1024 * 1024;

/// The closed set of typed return values a plugin may produce.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum OutputType {
    /// No value.
    Null,
    /// A boolean value.
    Bool,
    /// A signed integer value.
    Int,
    /// UTF-8 text.
    Text,
    /// Opaque bytes.
    Bytes,
}

impl OutputType {
    /// All output types in canonical order.
    pub const ALL: [Self; 5] = [Self::Null, Self::Bool, Self::Int, Self::Text, Self::Bytes];

    /// Stable machine-readable name of this type.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Null => "null",
            Self::Bool => "bool",
            Self::Int => "int",
            Self::Text => "text",
            Self::Bytes => "bytes",
        }
    }
}

impl fmt::Display for OutputType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A typed value returned by a plugin invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PluginValue {
    /// No value.
    Null,
    /// A boolean value.
    Bool(bool),
    /// A signed integer value.
    Int(i64),
    /// UTF-8 text.
    Text(String),
    /// Opaque bytes.
    Bytes(Vec<u8>),
}

impl PluginValue {
    /// The output type of this value.
    #[must_use]
    pub const fn output_type(&self) -> OutputType {
        match self {
            Self::Null => OutputType::Null,
            Self::Bool(_) => OutputType::Bool,
            Self::Int(_) => OutputType::Int,
            Self::Text(_) => OutputType::Text,
            Self::Bytes(_) => OutputType::Bytes,
        }
    }

    /// A deterministic byte count used for total-output budget accounting.
    #[must_use]
    pub fn encoded_len(&self) -> usize {
        match self {
            Self::Null => 4,
            Self::Bool(_) => 5,
            Self::Int(value) => value.to_string().len(),
            Self::Text(text) => text.len(),
            Self::Bytes(bytes) => bytes.len(),
        }
    }

    /// Convert to canonical JSON (bytes as lowercase hex text).
    #[must_use]
    pub fn to_json(&self) -> JsonValue {
        match self {
            Self::Null => JsonValue::Null,
            Self::Bool(value) => JsonValue::Bool(*value),
            Self::Int(value) => JsonValue::Number(value.to_string()),
            Self::Text(text) => JsonValue::String(text.clone()),
            Self::Bytes(bytes) => JsonValue::String(to_hex(bytes)),
        }
    }
}

fn to_hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        out.push(char::from(DIGITS[usize::from(byte >> 4)]));
        out.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    out
}

/// Bounds applied when validating a plugin output value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutputBudget {
    /// Maximum byte length of a text value.
    pub max_text_bytes: usize,
    /// Maximum byte length of a byte value.
    pub max_bytes: usize,
    /// Maximum total encoded size of the value.
    pub max_total_bytes: usize,
}

impl OutputBudget {
    /// Conservative default bounds.
    pub const DEFAULT: Self = Self {
        max_text_bytes: OUTPUT_DEFAULT_MAX_TEXT_BYTES,
        max_bytes: OUTPUT_DEFAULT_MAX_BYTES,
        max_total_bytes: OUTPUT_DEFAULT_MAX_TOTAL_BYTES,
    };

    /// Validate that the budget fields are positive and within the ceilings.
    ///
    /// # Errors
    ///
    /// Returns [`PluginOutputError::InvalidBudget`] naming the first bad field.
    pub fn validate(&self) -> Result<(), PluginOutputError> {
        if self.max_text_bytes == 0 || self.max_text_bytes > OUTPUT_ABSOLUTE_MAX_TEXT_BYTES {
            return Err(PluginOutputError::InvalidBudget {
                field: "max_text_bytes",
            });
        }
        if self.max_bytes == 0 || self.max_bytes > OUTPUT_ABSOLUTE_MAX_BYTES {
            return Err(PluginOutputError::InvalidBudget { field: "max_bytes" });
        }
        if self.max_total_bytes == 0 || self.max_total_bytes > OUTPUT_ABSOLUTE_MAX_BYTES {
            return Err(PluginOutputError::InvalidBudget {
                field: "max_total_bytes",
            });
        }
        Ok(())
    }
}

/// A typed failure while validating a plugin output value or budget.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PluginOutputError {
    /// A text value was empty.
    EmptyText,
    /// A text value exceeded the text budget.
    TextTooLong {
        /// Observed byte length.
        bytes: usize,
        /// Maximum permitted bytes.
        max: usize,
    },
    /// A byte value exceeded the byte budget.
    BytesTooLong {
        /// Observed byte length.
        bytes: usize,
        /// Maximum permitted bytes.
        max: usize,
    },
    /// The value exceeded the total-output budget.
    TotalTooLarge {
        /// Observed encoded byte length.
        bytes: usize,
        /// Maximum permitted bytes.
        max: usize,
    },
    /// The produced type did not match the expected type.
    TypeMismatch {
        /// Type the plan expected.
        expected: OutputType,
        /// Type the plugin produced.
        found: OutputType,
    },
    /// An output budget field was zero or otherwise malformed.
    InvalidBudget {
        /// Name of the offending field.
        field: &'static str,
    },
    /// The producing plugin id was empty.
    EmptyPluginId,
    /// The operation name was empty.
    EmptyOperation,
}

impl PluginOutputError {
    /// Stable machine-readable code for this error.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::EmptyText => "empty_text",
            Self::TextTooLong { .. } => "text_too_long",
            Self::BytesTooLong { .. } => "bytes_too_long",
            Self::TotalTooLarge { .. } => "total_too_large",
            Self::TypeMismatch { .. } => "type_mismatch",
            Self::InvalidBudget { .. } => "invalid_budget",
            Self::EmptyPluginId => "empty_plugin_id",
            Self::EmptyOperation => "empty_operation",
        }
    }
}

impl fmt::Display for PluginOutputError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyText => f.write_str("plugin text output must not be empty"),
            Self::TextTooLong { bytes, max } => {
                write!(f, "plugin text output is {bytes} bytes; maximum is {max}")
            }
            Self::BytesTooLong { bytes, max } => {
                write!(f, "plugin byte output is {bytes} bytes; maximum is {max}")
            }
            Self::TotalTooLarge { bytes, max } => {
                write!(
                    f,
                    "plugin output is {bytes} encoded bytes; maximum is {max}"
                )
            }
            Self::TypeMismatch { expected, found } => write!(
                f,
                "plugin output type '{found}' does not match expected type '{expected}'"
            ),
            Self::InvalidBudget { field } => {
                write!(f, "output budget field '{field}' is invalid")
            }
            Self::EmptyPluginId => f.write_str("plugin id must not be empty"),
            Self::EmptyOperation => f.write_str("operation must not be empty"),
        }
    }
}

impl std::error::Error for PluginOutputError {}

/// Validate one value against the budget.
fn validate_value(value: &PluginValue, budget: &OutputBudget) -> Result<(), PluginOutputError> {
    match value {
        PluginValue::Text(text) => {
            if text.is_empty() {
                return Err(PluginOutputError::EmptyText);
            }
            if text.len() > budget.max_text_bytes {
                return Err(PluginOutputError::TextTooLong {
                    bytes: text.len(),
                    max: budget.max_text_bytes,
                });
            }
        }
        PluginValue::Bytes(bytes) => {
            if bytes.len() > budget.max_bytes {
                return Err(PluginOutputError::BytesTooLong {
                    bytes: bytes.len(),
                    max: budget.max_bytes,
                });
            }
        }
        PluginValue::Null | PluginValue::Bool(_) | PluginValue::Int(_) => {}
    }
    let encoded = value.encoded_len();
    if encoded > budget.max_total_bytes {
        return Err(PluginOutputError::TotalTooLarge {
            bytes: encoded,
            max: budget.max_total_bytes,
        });
    }
    Ok(())
}

/// A typed, size-bounded result from a plugin invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginOutput {
    plugin_id: String,
    operation: String,
    value: PluginValue,
}

impl PluginOutput {
    /// Build an output and validate it against [`OutputBudget::DEFAULT`].
    ///
    /// # Errors
    ///
    /// Returns a typed [`PluginOutputError`] for an empty id/operation or an
    /// invalid, oversized, or empty value.
    pub fn new(
        plugin_id: impl Into<String>,
        operation: impl Into<String>,
        value: PluginValue,
    ) -> Result<Self, PluginOutputError> {
        let plugin_id = plugin_id.into();
        let operation = operation.into();
        if plugin_id.trim().is_empty() {
            return Err(PluginOutputError::EmptyPluginId);
        }
        if operation.trim().is_empty() {
            return Err(PluginOutputError::EmptyOperation);
        }
        let output = Self {
            plugin_id,
            operation,
            value,
        };
        output.validate(&OutputBudget::DEFAULT)?;
        Ok(output)
    }

    /// Validate this output against `budget`.
    ///
    /// # Errors
    ///
    /// Returns [`PluginOutputError`] for an invalid budget or an invalid value.
    pub fn validate(&self, budget: &OutputBudget) -> Result<(), PluginOutputError> {
        budget.validate()?;
        validate_value(&self.value, budget)
    }

    /// Validate the value's type as well as its size.
    ///
    /// # Errors
    ///
    /// Returns [`PluginOutputError::TypeMismatch`] when the produced type differs
    /// from `expected`, or a size/structure error from [`Self::validate`].
    pub fn validate_expected(
        &self,
        expected: OutputType,
        budget: &OutputBudget,
    ) -> Result<(), PluginOutputError> {
        self.validate(budget)?;
        if self.value.output_type() != expected {
            return Err(PluginOutputError::TypeMismatch {
                expected,
                found: self.value.output_type(),
            });
        }
        Ok(())
    }

    /// The producing plugin id.
    #[must_use]
    pub fn plugin_id(&self) -> &str {
        &self.plugin_id
    }

    /// The operation that produced this output.
    #[must_use]
    pub fn operation(&self) -> &str {
        &self.operation
    }

    /// The typed value.
    #[must_use]
    pub const fn value(&self) -> &PluginValue {
        &self.value
    }

    /// Canonical JSON rendering with sorted object keys.
    #[must_use]
    pub fn to_json(&self) -> JsonValue {
        JsonValue::object([
            (
                "operation".to_owned(),
                JsonValue::from(self.operation.as_str()),
            ),
            (
                "plugin_id".to_owned(),
                JsonValue::from(self.plugin_id.as_str()),
            ),
            ("value".to_owned(), self.value.to_json()),
        ])
    }

    /// A stable one-line summary: `<plugin>\t<operation>\t<type>`.
    #[must_use]
    pub fn summary(&self) -> String {
        format!(
            "{}\t{}\t{}",
            self.plugin_id,
            self.operation,
            self.value.output_type().as_str()
        )
    }
}

/// The fixed kind of a step in an [`ExecutionPlan`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ExecutionStepKind {
    /// Plugin registration and manifest validity.
    Registration,
    /// ABI version compatibility.
    ApiVersion,
    /// Required capabilities are granted.
    Capabilities,
    /// Requested budgets are within host limits.
    Budgets,
    /// Output budget is valid.
    OutputBudget,
    /// Final permission to execute.
    Execute,
}

impl ExecutionStepKind {
    /// Stable machine-readable code for this step kind.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Registration => "registration",
            Self::ApiVersion => "api_version",
            Self::Capabilities => "capabilities",
            Self::Budgets => "budgets",
            Self::OutputBudget => "output_budget",
            Self::Execute => "execute",
        }
    }
}

/// One deterministic step of an execution plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionStep {
    kind: ExecutionStepKind,
    allowed: bool,
    codes: Vec<String>,
}

impl ExecutionStep {
    fn new(kind: ExecutionStepKind, mut codes: Vec<String>) -> Self {
        codes.sort();
        codes.dedup();
        Self {
            kind,
            allowed: codes.is_empty(),
            codes,
        }
    }

    /// The step kind.
    #[must_use]
    pub const fn kind(&self) -> ExecutionStepKind {
        self.kind
    }

    /// Whether this step is allowed (no deny codes).
    #[must_use]
    pub const fn is_allowed(&self) -> bool {
        self.allowed
    }

    /// Stable, sorted deny codes attached to this step.
    #[must_use]
    pub fn codes(&self) -> &[String] {
        &self.codes
    }
}

/// A deterministic, non-executing plan for one plugin invocation.
///
/// Steps always appear in the fixed order given by [`ExecutionStepKind`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionPlan {
    plugin_id: String,
    operation: String,
    expected_output: OutputType,
    steps: Vec<ExecutionStep>,
}

impl ExecutionPlan {
    /// Plugin id being planned.
    #[must_use]
    pub fn plugin_id(&self) -> &str {
        &self.plugin_id
    }

    /// Operation being planned.
    #[must_use]
    pub fn operation(&self) -> &str {
        &self.operation
    }

    /// Output type the plan expects.
    #[must_use]
    pub const fn expected_output(&self) -> OutputType {
        self.expected_output
    }

    /// Ordered plan steps.
    #[must_use]
    pub fn steps(&self) -> &[ExecutionStep] {
        &self.steps
    }

    /// Whether every step is allowed.
    #[must_use]
    pub fn is_allowed(&self) -> bool {
        self.steps.iter().all(ExecutionStep::is_allowed)
    }

    /// Stable, byte-for-byte reproducible rendering of the whole plan.
    #[must_use]
    pub fn summary(&self) -> String {
        let mut out = format!(
            "plugin={}\toperation={}\texpect={}\n",
            self.plugin_id,
            self.operation,
            self.expected_output.as_str()
        );
        for step in &self.steps {
            out.push_str(step.kind.as_str());
            out.push(':');
            if step.allowed {
                out.push_str("allow");
            } else {
                out.push_str("deny(");
                out.push_str(&step.codes.join(","));
                out.push(')');
            }
            out.push('\n');
        }
        out
    }
}

/// Build a deterministic execution plan without executing any plugin code.
///
/// Capability, ABI, registration, and budget checks are reused from
/// [`plan_invocation`](crate::plugin::plan_invocation); the output budget is
/// validated here. Every deny reason is mapped into a fixed step, sorted and
/// deduplicated, so the plan is byte-identical across runs and hosts.
#[must_use]
pub fn build_execution_plan<H: PluginHost + ?Sized>(
    host: &H,
    registry: &PluginRegistry,
    invocation: &PluginInvocation,
    expected_output: OutputType,
    output_budget: OutputBudget,
) -> ExecutionPlan {
    let report: InvocationReport = plan_invocation(host, registry, invocation);
    let mut registration = Vec::new();
    let mut api = Vec::new();
    let mut capabilities = Vec::new();
    let mut budgets = Vec::new();
    for reason in report.decision().reasons() {
        let code = reason.code().to_owned();
        match reason {
            DenyReason::UnknownPlugin { .. } => registration.push(code),
            DenyReason::UnsupportedApiVersion { .. } | DenyReason::ManifestInvalid { .. } => {
                api.push(code)
            }
            DenyReason::CapabilityNotGranted { .. } => capabilities.push(code),
            DenyReason::BudgetExceedsLimit { .. } | DenyReason::EmptyOperation => {
                budgets.push(code)
            }
        }
    }
    let output = match output_budget.validate() {
        Ok(()) => Vec::new(),
        Err(error) => vec![error.code().to_owned()],
    };

    let mut steps = vec![
        ExecutionStep::new(ExecutionStepKind::Registration, registration),
        ExecutionStep::new(ExecutionStepKind::ApiVersion, api),
        ExecutionStep::new(ExecutionStepKind::Capabilities, capabilities),
        ExecutionStep::new(ExecutionStepKind::Budgets, budgets),
        ExecutionStep::new(ExecutionStepKind::OutputBudget, output),
    ];
    let prior_allowed = steps.iter().all(ExecutionStep::is_allowed);
    let execute_codes = if prior_allowed {
        Vec::new()
    } else {
        vec!["upstream_denied".to_owned()]
    };
    steps.push(ExecutionStep::new(
        ExecutionStepKind::Execute,
        execute_codes,
    ));

    ExecutionPlan {
        plugin_id: report.plugin_id().to_owned(),
        operation: report.operation().to_owned(),
        expected_output,
        steps,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::{
        Capability, InvocationBudget, LocalPluginHost, PLUGIN_API_VERSION, PluginManifest,
    };

    fn manifest(id: &str, caps: &[Capability]) -> PluginManifest {
        PluginManifest::new(
            id,
            format!("Plugin {id}"),
            "1.0.0",
            PLUGIN_API_VERSION,
            caps.iter().copied().collect(),
        )
    }

    fn registry_with(id: &str, caps: &[Capability]) -> PluginRegistry {
        let mut registry = PluginRegistry::new();
        assert_eq!(registry.register(manifest(id, caps)), Ok(()));
        registry
    }

    fn invocation() -> PluginInvocation {
        PluginInvocation::new("p", "extract", InvocationBudget::DEFAULT).expect("test invocation")
    }

    #[test]
    fn typed_values_report_their_type_and_stable_encoded_length() {
        assert_eq!(PluginValue::Null.output_type(), OutputType::Null);
        assert_eq!(PluginValue::Bool(true).output_type(), OutputType::Bool);
        assert_eq!(PluginValue::Int(-7).output_type(), OutputType::Int);
        assert_eq!(
            PluginValue::Text("hi".into()).output_type(),
            OutputType::Text
        );
        assert_eq!(
            PluginValue::Bytes(vec![0, 1]).output_type(),
            OutputType::Bytes
        );
        assert_eq!(PluginValue::Null.encoded_len(), 4);
        assert_eq!(PluginValue::Int(-7).encoded_len(), 2);
        assert_eq!(PluginValue::Text("hi".into()).encoded_len(), 2);
        assert_eq!(PluginValue::Bytes(vec![0, 1]).encoded_len(), 2);
    }

    #[test]
    fn output_contract_accepts_all_valid_typed_values() {
        let values = [
            PluginValue::Null,
            PluginValue::Bool(false),
            PluginValue::Int(42),
            PluginValue::Text("text".to_owned()),
            PluginValue::Bytes(vec![1, 2, 3]),
        ];
        for value in values {
            let output = PluginOutput::new("plugin.one", "run", value).expect("valid output");
            assert_eq!(output.plugin_id(), "plugin.one");
            assert_eq!(output.operation(), "run");
            assert_eq!(output.validate(&OutputBudget::DEFAULT), Ok(()));
        }
    }

    #[test]
    fn oversized_text_and_bytes_are_rejected_with_typed_errors() {
        let tiny = OutputBudget {
            max_text_bytes: 3,
            max_bytes: 2,
            max_total_bytes: 4,
        };
        let text = PluginOutput::new("p", "run", PluginValue::Text("abcd".to_owned()))
            .expect("default allows it");
        assert_eq!(
            text.validate(&tiny),
            Err(PluginOutputError::TextTooLong { bytes: 4, max: 3 })
        );
        let bytes = PluginOutput::new("p", "run", PluginValue::Bytes(vec![1, 2, 3]))
            .expect("default allows it");
        assert_eq!(
            bytes.validate(&tiny),
            Err(PluginOutputError::BytesTooLong { bytes: 3, max: 2 })
        );

        let huge = "x".repeat(OUTPUT_DEFAULT_MAX_TEXT_BYTES + 1);
        assert!(matches!(
            PluginOutput::new("p", "run", PluginValue::Text(huge)),
            Err(PluginOutputError::TextTooLong { .. })
        ));
    }

    #[test]
    fn total_output_budget_is_enforced() {
        let small_total = OutputBudget {
            max_text_bytes: 100,
            max_bytes: 100,
            max_total_bytes: 4,
        };
        let output = PluginOutput::new("p", "run", PluginValue::Text("abcdef".to_owned()))
            .expect("default allows it");
        assert_eq!(
            output.validate(&small_total),
            Err(PluginOutputError::TotalTooLarge { bytes: 6, max: 4 })
        );
    }

    #[test]
    fn empty_text_and_empty_identity_are_typed_errors() {
        assert_eq!(
            PluginOutput::new("p", "run", PluginValue::Text(String::new())),
            Err(PluginOutputError::EmptyText)
        );
        assert_eq!(
            PluginOutput::new(" ", "run", PluginValue::Null),
            Err(PluginOutputError::EmptyPluginId)
        );
        assert_eq!(
            PluginOutput::new("p", "", PluginValue::Null),
            Err(PluginOutputError::EmptyOperation)
        );
    }

    #[test]
    fn type_mismatch_is_a_typed_error() {
        let output = PluginOutput::new("p", "run", PluginValue::Int(1)).expect("output");
        assert_eq!(
            output.validate_expected(OutputType::Text, &OutputBudget::DEFAULT),
            Err(PluginOutputError::TypeMismatch {
                expected: OutputType::Text,
                found: OutputType::Int,
            })
        );
        assert_eq!(
            output.validate_expected(OutputType::Int, &OutputBudget::DEFAULT),
            Ok(())
        );
    }

    #[test]
    fn invalid_output_budget_is_rejected() {
        let bad = OutputBudget {
            max_text_bytes: 0,
            ..OutputBudget::DEFAULT
        };
        assert_eq!(
            bad.validate(),
            Err(PluginOutputError::InvalidBudget {
                field: "max_text_bytes"
            })
        );
        let over = OutputBudget {
            max_bytes: OUTPUT_ABSOLUTE_MAX_BYTES + 1,
            ..OutputBudget::DEFAULT
        };
        assert!(over.validate().is_err());
    }

    #[test]
    fn capability_check_gates_the_execution_plan() {
        let registry = registry_with("p", &[Capability::DocumentText]);
        let denied = build_execution_plan(
            &LocalPluginHost::new(),
            &registry,
            &invocation(),
            OutputType::Text,
            OutputBudget::DEFAULT,
        );
        assert!(!denied.is_allowed());
        let cap_step = &denied.steps()[2];
        assert_eq!(cap_step.kind(), ExecutionStepKind::Capabilities);
        assert_eq!(cap_step.codes(), ["capability_not_granted"]);
        assert_eq!(denied.steps()[5].codes(), ["upstream_denied"]);

        let allowed_host = LocalPluginHost::new().grant(Capability::DocumentText);
        let allowed = build_execution_plan(
            &allowed_host,
            &registry,
            &invocation(),
            OutputType::Text,
            OutputBudget::DEFAULT,
        );
        assert!(allowed.is_allowed());
        assert!(allowed.steps().iter().all(ExecutionStep::is_allowed));
    }

    #[test]
    fn budget_enforcement_gates_the_execution_plan() {
        let registry = registry_with("p", &[]);
        let tight_host = LocalPluginHost::new().with_budget_limits(InvocationBudget {
            max_steps: 10,
            max_memory_bytes: 10,
            max_wall_clock_ms: 10,
        });
        let plan = build_execution_plan(
            &tight_host,
            &registry,
            &invocation(),
            OutputType::Bytes,
            OutputBudget::DEFAULT,
        );
        let budget_step = &plan.steps()[3];
        assert_eq!(budget_step.kind(), ExecutionStepKind::Budgets);
        // Three fields exceed their limits, but the step carries sorted,
        // deduplicated codes, so the stable code appears once.
        assert_eq!(budget_step.codes(), ["budget_exceeds_limit"]);
        assert!(!plan.is_allowed());
    }

    #[test]
    fn execution_plan_is_byte_for_byte_deterministic() {
        let registry = registry_with("p", &[Capability::DocumentText]);
        let host = LocalPluginHost::new().grant(Capability::DocumentText);
        let a = build_execution_plan(
            &host,
            &registry,
            &invocation(),
            OutputType::Text,
            OutputBudget::DEFAULT,
        );
        let b = build_execution_plan(
            &host,
            &registry,
            &invocation(),
            OutputType::Text,
            OutputBudget::DEFAULT,
        );
        assert_eq!(a, b);
        assert_eq!(a.summary(), b.summary());
        assert!(
            a.summary()
                .contains("plugin=p\toperation=extract\texpect=text\n")
        );
        assert!(a.summary().ends_with("execute:allow\n"));
    }

    #[test]
    fn output_json_is_canonical_and_bytes_are_hex_encoded() {
        let output =
            PluginOutput::new("p", "run", PluginValue::Bytes(vec![0x00, 0xab])).expect("output");
        let json = output.to_json().to_string();
        assert_eq!(
            json,
            r#"{"operation":"run","plugin_id":"p","value":"00ab"}"#
        );
        assert_eq!(output.summary(), "p\trun\tbytes");
        let again =
            PluginOutput::new("p", "run", PluginValue::Bytes(vec![0x00, 0xab])).expect("output");
        assert_eq!(again.to_json().to_string(), json);
    }

    #[test]
    fn typed_error_codes_and_messages_are_actionable() {
        assert_eq!(PluginOutputError::EmptyText.code(), "empty_text");
        let message = PluginOutputError::TextTooLong { bytes: 9, max: 4 }.to_string();
        assert!(message.contains("9 bytes"));
        assert!(message.contains("maximum is 4"));
        let mismatch = PluginOutputError::TypeMismatch {
            expected: OutputType::Text,
            found: OutputType::Null,
        }
        .to_string();
        assert!(mismatch.contains("text"));
        assert!(mismatch.contains("null"));
        let budget_msg = PluginOutputError::InvalidBudget { field: "max_steps" }.to_string();
        assert!(budget_msg.contains("max_steps"));
        assert_eq!(PluginOutputError::EmptyOperation.code(), "empty_operation");
    }
}
