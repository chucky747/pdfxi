//! Action Wizard-style sequences bound to the sandboxed JavaScript subset.
//!
//! This module adds a small, dependency-free Action Wizard model on top of the
//! existing [`crate::js`] sandbox and turns it into a deterministic batch
//! runner plan. It deliberately stays at the library level: no UI, filesystem,
//! network, process, clock, or randomness is used, and no real PDF document is
//! mutated.
//!
//! # Model
//!
//! An [`ActionSequence`] is a named, ordered list of [`WizardStep`]s with a
//! per-sequence [`ScriptBudget`]. A step is one of three supported kinds —
//! [`WizardStepKind::RunScript`] (a caller-supplied script for the existing
//! parser/interpreter), [`WizardStepKind::SetMetadata`] (an in-memory metadata
//! key/value pair), and [`WizardStepKind::RunCommand`] (a named automation
//! command) — or [`WizardStepKind::Unsupported`], which exists so an unknown
//! kind can be reported with a typed deny reason rather than guessed into
//! existence. [`ActionSequence::validate`] and [`plan_sequence`] enforce an
//! empty-sequence rule, unique non-empty step ids, and a bounded step count
//! ([`MAX_SEQUENCE_STEPS`]).
//!
//! # Planning
//!
//! [`plan_sequence`] produces a [`RunnerPlan`]: one [`StepPlan`] per step in
//! insertion order, each with an allow/deny [`PlanDecision`] carrying typed
//! [`PlanDenyReason`]s. Reasons are collected exhaustively, then sorted and
//! deduplicated so the plan is byte-for-byte deterministic. Planning never
//! executes a script or command.
//!
//! # Execution boundary
//!
//! Script execution is *always* behind the caller-supplied
//! [`ScriptStepExecutor`] trait; [`SandboxedScriptExecutor`] is the provided
//! default that wires a step's script and budget directly into
//! [`crate::Script::parse_with_limits`] / [`crate::Script::run`]. Command
//! dispatch is behind [`CommandHandler`]; the provided
//! [`RecordingCommandHandler`] performs no work and only records the command
//! name. A [`SequenceRunner`] walks a plan in order, skips denied steps,
//! applies metadata to an in-memory map, collapses every outcome into a
//! [`SequenceReport`], and returns typed [`WizardRunError`]s on failure.
//!
//! No dependency was added and no other crate was modified: the sandbox API is
//! already public from this crate, so the executor can call it directly.

use crate::CancelToken;
use crate::js::{JsError, Limits, Script, Value};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fmt;

/// Hard ceiling on the number of steps in one [`ActionSequence`].
pub const MAX_SEQUENCE_STEPS: usize = 64;

/// Hard ceiling on the UTF-8 byte length of a single script step's source.
pub const MAX_SCRIPT_BYTES: usize = 64 * 1024;

/// Default CPU-step (fuel) ceiling for a script step.
pub const DEFAULT_SCRIPT_FUEL: u64 = 100_000;

/// Deterministic CPU/memory budget applied to a script step.
///
/// The fields map one-to-one onto the corresponding [`Limits`] used by the
/// sandbox. `fuel` is the interpreter step count; the other fields bound calls,
/// loop iterations, and produced string length. Wall-clock time is never used,
/// so execution remains deterministic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScriptBudget {
    /// Maximum number of interpreter steps (statements and expressions).
    pub fuel: u64,
    /// Maximum total number of function calls.
    pub max_calls: u64,
    /// Maximum total iterations across all loops.
    pub max_loop_iterations: u64,
    /// Maximum number of characters in any produced string.
    pub max_string_len: usize,
}

impl ScriptBudget {
    /// A conservative default budget.
    pub const DEFAULT: Self = Self {
        fuel: DEFAULT_SCRIPT_FUEL,
        max_calls: 10_000,
        max_loop_iterations: 10_000,
        max_string_len: 1_000_000,
    };

    /// Replaces the CPU-step (fuel) budget.
    pub fn with_fuel(mut self, fuel: u64) -> Self {
        self.fuel = fuel;
        self
    }

    /// Replaces the total call-count budget.
    pub fn with_max_calls(mut self, max_calls: u64) -> Self {
        self.max_calls = max_calls;
        self
    }

    /// Replaces the total loop-iteration budget.
    pub fn with_max_loop_iterations(mut self, max_loop_iterations: u64) -> Self {
        self.max_loop_iterations = max_loop_iterations;
        self
    }

    /// Replaces the maximum produced string length.
    pub fn with_max_string_len(mut self, max_string_len: usize) -> Self {
        self.max_string_len = max_string_len;
        self
    }

    /// Converts this budget into sandbox [`Limits`].
    ///
    /// Only the fields modeled here are overridden; all other sandbox limits
    /// keep their documented defaults and the wall clock stays disabled.
    pub fn to_limits(&self) -> Limits {
        Limits {
            max_steps: self.fuel,
            max_calls: self.max_calls,
            max_total_iterations: self.max_loop_iterations,
            max_string_len: self.max_string_len,
            ..Limits::default()
        }
    }
}

impl Default for ScriptBudget {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// A budget field that can be denied by a [`SequencePolicy`] ceiling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum WizardBudgetField {
    /// CPU-step / fuel ceiling.
    Fuel,
    /// Total call-count ceiling.
    Calls,
    /// Total loop-iteration ceiling.
    LoopIterations,
    /// Produced string-length ceiling.
    StringLen,
}

impl WizardBudgetField {
    /// Stable machine-readable name for this field.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Fuel => "fuel",
            Self::Calls => "calls",
            Self::LoopIterations => "loop_iterations",
            Self::StringLen => "string_len",
        }
    }
}

impl fmt::Display for WizardBudgetField {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The kind of an Action Wizard [`WizardStep`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WizardStepKind {
    /// Run a caller-supplied script with the sandbox parser/interpreter.
    RunScript {
        /// JavaScript-subset source carried by the step.
        script: String,
    },
    /// Set one in-memory document metadata key/value pair.
    SetMetadata {
        /// Metadata key.
        key: String,
        /// Metadata value.
        value: String,
    },
    /// Dispatch one named automation command through a [`CommandHandler`].
    RunCommand {
        /// Stable command name checked against the policy allowlist.
        command: String,
    },
    /// An unrecognized kind, retained so it can be denied deterministically.
    Unsupported {
        /// The unrecognized kind name.
        name: String,
    },
}

impl WizardStepKind {
    /// Stable machine-readable discriminator for this kind.
    pub const fn kind_str(&self) -> &'static str {
        match self {
            Self::RunScript { .. } => "run-script",
            Self::SetMetadata { .. } => "set-metadata",
            Self::RunCommand { .. } => "run-command",
            Self::Unsupported { .. } => "unsupported",
        }
    }
}

/// One named step inside an [`ActionSequence`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WizardStep {
    id: String,
    kind: WizardStepKind,
}

impl WizardStep {
    /// Creates a step with the given id and kind.
    pub fn new(id: impl Into<String>, kind: WizardStepKind) -> Self {
        Self {
            id: id.into(),
            kind,
        }
    }

    /// Creates a script step.
    pub fn script(id: impl Into<String>, script: impl Into<String>) -> Self {
        Self::new(
            id,
            WizardStepKind::RunScript {
                script: script.into(),
            },
        )
    }

    /// Creates a metadata step.
    pub fn set_metadata(
        id: impl Into<String>,
        key: impl Into<String>,
        value: impl Into<String>,
    ) -> Self {
        Self::new(
            id,
            WizardStepKind::SetMetadata {
                key: key.into(),
                value: value.into(),
            },
        )
    }

    /// Creates a named-command step.
    pub fn run_command(id: impl Into<String>, command: impl Into<String>) -> Self {
        Self::new(
            id,
            WizardStepKind::RunCommand {
                command: command.into(),
            },
        )
    }

    /// Creates an unsupported-kind step for denial and regression tests.
    pub fn unsupported(id: impl Into<String>, name: impl Into<String>) -> Self {
        Self::new(id, WizardStepKind::Unsupported { name: name.into() })
    }

    /// Returns the step id.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Returns the step kind.
    pub fn kind(&self) -> &WizardStepKind {
        &self.kind
    }
}

/// A structural validation error for an [`ActionSequence`].
///
/// Unlike [`PlanDenyReason`], validation is fail-fast and returns the first
/// problem in a stable order. The planner reports every problem at once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WizardValidationError {
    /// The sequence had no steps.
    EmptySequence {
        /// Sequence name.
        name: String,
    },
    /// A step id was empty or whitespace only.
    EmptyStepId {
        /// Sequence name.
        name: String,
    },
    /// Two steps shared an id.
    DuplicateStepId {
        /// Sequence name.
        name: String,
        /// Duplicated id.
        id: String,
    },
    /// The sequence exceeded the bounded step count.
    SequenceTooLong {
        /// Sequence name.
        name: String,
        /// Actual step count.
        len: usize,
        /// Allowed maximum.
        max: usize,
    },
}

impl WizardValidationError {
    /// Stable machine-readable code for this error.
    pub const fn code(&self) -> &'static str {
        match self {
            Self::EmptySequence { .. } => "empty_sequence",
            Self::EmptyStepId { .. } => "empty_step_id",
            Self::DuplicateStepId { .. } => "duplicate_step_id",
            Self::SequenceTooLong { .. } => "sequence_too_long",
        }
    }
}

impl fmt::Display for WizardValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptySequence { name } => write!(f, "action sequence '{name}' has no steps"),
            Self::EmptyStepId { name } => {
                write!(f, "action sequence '{name}' has a step with an empty id")
            }
            Self::DuplicateStepId { name, id } => {
                write!(f, "action sequence '{name}' has a duplicate step id '{id}'")
            }
            Self::SequenceTooLong { name, len, max } => write!(
                f,
                "action sequence '{name}' has {len} steps; at most {max} are allowed"
            ),
        }
    }
}

impl std::error::Error for WizardValidationError {}

/// A named, ordered, bounded sequence of Action Wizard steps.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionSequence {
    name: String,
    steps: Vec<WizardStep>,
    budget: ScriptBudget,
}

impl ActionSequence {
    /// Creates an empty sequence with the default script budget.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            steps: Vec::new(),
            budget: ScriptBudget::default(),
        }
    }

    /// Replaces the script budget applied to every script step.
    pub fn with_budget(mut self, budget: ScriptBudget) -> Self {
        self.budget = budget;
        self
    }

    /// Builds a sequence from a complete step list, validating it immediately.
    pub fn from_steps<I>(name: impl Into<String>, steps: I) -> Result<Self, WizardValidationError>
    where
        I: IntoIterator<Item = WizardStep>,
    {
        let mut sequence = Self::new(name);
        for step in steps {
            sequence.add_step(step)?;
        }
        sequence.validate()?;
        Ok(sequence)
    }

    /// Appends a step, rejecting empty/duplicate ids and an over-long sequence.
    ///
    /// On error the sequence is left unchanged.
    pub fn add_step(&mut self, step: WizardStep) -> Result<(), WizardValidationError> {
        if step.id.trim().is_empty() {
            return Err(WizardValidationError::EmptyStepId {
                name: self.name.clone(),
            });
        }
        if self.steps.iter().any(|existing| existing.id == step.id) {
            return Err(WizardValidationError::DuplicateStepId {
                name: self.name.clone(),
                id: step.id,
            });
        }
        if self.steps.len() >= MAX_SEQUENCE_STEPS {
            return Err(WizardValidationError::SequenceTooLong {
                name: self.name.clone(),
                len: self.steps.len() + 1,
                max: MAX_SEQUENCE_STEPS,
            });
        }
        self.steps.push(step);
        Ok(())
    }

    /// Checks all invariants, reporting the first problem found.
    pub fn validate(&self) -> Result<(), WizardValidationError> {
        if self.steps.is_empty() {
            return Err(WizardValidationError::EmptySequence {
                name: self.name.clone(),
            });
        }
        if self.steps.len() > MAX_SEQUENCE_STEPS {
            return Err(WizardValidationError::SequenceTooLong {
                name: self.name.clone(),
                len: self.steps.len(),
                max: MAX_SEQUENCE_STEPS,
            });
        }
        let mut seen = HashSet::new();
        for step in &self.steps {
            if step.id.trim().is_empty() {
                return Err(WizardValidationError::EmptyStepId {
                    name: self.name.clone(),
                });
            }
            if !seen.insert(step.id.as_str()) {
                return Err(WizardValidationError::DuplicateStepId {
                    name: self.name.clone(),
                    id: step.id.clone(),
                });
            }
        }
        Ok(())
    }

    /// Returns the sequence name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the steps in stable execution order.
    pub fn steps(&self) -> impl ExactSizeIterator<Item = &WizardStep> {
        self.steps.iter()
    }

    /// Returns the step with the given id, if present.
    pub fn step(&self, id: &str) -> Option<&WizardStep> {
        self.steps.iter().find(|step| step.id == id)
    }

    /// Returns the script budget shared by script steps.
    pub fn budget(&self) -> ScriptBudget {
        self.budget
    }

    /// Returns the number of steps.
    pub fn len(&self) -> usize {
        self.steps.len()
    }

    /// Returns `true` when the sequence has no steps.
    pub fn is_empty(&self) -> bool {
        self.steps.is_empty()
    }
}

/// A typed reason a step (or the whole sequence) was denied during planning.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum PlanDenyReason {
    /// The sequence had no steps.
    EmptySequence,
    /// Two steps shared an id.
    DuplicateStepId {
        /// Duplicated id.
        id: String,
    },
    /// A step id was empty or whitespace only.
    EmptyStepId,
    /// The sequence exceeded its bounded step count.
    SequenceTooLong {
        /// Actual count.
        len: usize,
        /// Allowed maximum.
        max: usize,
    },
    /// The step carried an unrecognized kind.
    UnsupportedStepKind {
        /// Unrecognized kind name.
        name: String,
    },
    /// A script step had empty source.
    EmptyScript {
        /// Owning step id.
        step: String,
    },
    /// A script step exceeded the source-size ceiling.
    ScriptTooLarge {
        /// Owning step id.
        step: String,
        /// Actual byte length.
        len: usize,
        /// Allowed maximum.
        max: usize,
    },
    /// A metadata step had an empty key.
    EmptyMetadataKey {
        /// Owning step id.
        step: String,
    },
    /// A command step had an empty command name.
    EmptyCommand {
        /// Owning step id.
        step: String,
    },
    /// A command name was not in the policy allowlist.
    CommandNotAllowed {
        /// Owning step id.
        step: String,
        /// Denied command name.
        command: String,
    },
    /// A script budget exceeded a host ceiling.
    BudgetExceedsLimit {
        /// Owning step id.
        step: String,
        /// Offending budget field.
        field: WizardBudgetField,
        /// Requested value.
        requested: u64,
        /// Allowed value.
        allowed: u64,
    },
}

impl PlanDenyReason {
    /// Stable machine-readable code for this reason.
    pub const fn code(&self) -> &'static str {
        match self {
            Self::EmptySequence => "empty_sequence",
            Self::DuplicateStepId { .. } => "duplicate_step_id",
            Self::EmptyStepId => "empty_step_id",
            Self::SequenceTooLong { .. } => "sequence_too_long",
            Self::UnsupportedStepKind { .. } => "unsupported_step_kind",
            Self::EmptyScript { .. } => "empty_script",
            Self::ScriptTooLarge { .. } => "script_too_large",
            Self::EmptyMetadataKey { .. } => "empty_metadata_key",
            Self::EmptyCommand { .. } => "empty_command",
            Self::CommandNotAllowed { .. } => "command_not_allowed",
            Self::BudgetExceedsLimit { .. } => "budget_exceeds_limit",
        }
    }
}

impl fmt::Display for PlanDenyReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptySequence => f.write_str("sequence has no steps"),
            Self::DuplicateStepId { id } => write!(f, "duplicate step id '{id}'"),
            Self::EmptyStepId => f.write_str("step id must not be empty"),
            Self::SequenceTooLong { len, max } => {
                write!(f, "sequence has {len} steps; at most {max} allowed")
            }
            Self::UnsupportedStepKind { name } => {
                write!(f, "unsupported step kind '{name}'")
            }
            Self::EmptyScript { step } => write!(f, "step '{step}' has an empty script"),
            Self::ScriptTooLarge { step, len, max } => write!(
                f,
                "step '{step}' script is {len} bytes; at most {max} allowed"
            ),
            Self::EmptyMetadataKey { step } => {
                write!(f, "step '{step}' has an empty metadata key")
            }
            Self::EmptyCommand { step } => {
                write!(f, "step '{step}' has an empty command name")
            }
            Self::CommandNotAllowed { step, command } => {
                write!(f, "step '{step}' command '{command}' is not allowed")
            }
            Self::BudgetExceedsLimit {
                step,
                field,
                requested,
                allowed,
            } => write!(
                f,
                "step '{step}' budget field {field} requests {requested} but host allows {allowed}"
            ),
        }
    }
}

impl std::error::Error for PlanDenyReason {}

/// Allow/deny decision with canonical, deduplicated deny reasons.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanDecision {
    allowed: bool,
    reasons: Vec<PlanDenyReason>,
}

impl PlanDecision {
    /// Builds a decision from reasons, sorting and deduplicating them.
    pub fn new(mut reasons: Vec<PlanDenyReason>) -> Self {
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

    /// Returns `true` when there were no deny reasons.
    pub fn is_allowed(&self) -> bool {
        self.allowed
    }

    /// Returns reasons in canonical order (empty when allowed).
    pub fn reasons(&self) -> &[PlanDenyReason] {
        &self.reasons
    }
}

/// The plan for one step of an [`ActionSequence`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepPlan {
    index: usize,
    step_id: String,
    kind: WizardStepKind,
    decision: PlanDecision,
}

impl StepPlan {
    /// Zero-based position of the step in the sequence.
    pub fn index(&self) -> usize {
        self.index
    }

    /// Step id.
    pub fn step_id(&self) -> &str {
        &self.step_id
    }

    /// Step kind.
    pub fn kind(&self) -> &WizardStepKind {
        &self.kind
    }

    /// The step's allow/deny decision.
    pub fn decision(&self) -> &PlanDecision {
        &self.decision
    }

    /// Returns `true` when this step is allowed to execute.
    pub fn is_allowed(&self) -> bool {
        self.decision.is_allowed()
    }
}

/// A deterministic, non-executing plan for a whole sequence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunnerPlan {
    sequence_name: String,
    steps: Vec<StepPlan>,
    decision: PlanDecision,
}

impl RunnerPlan {
    /// Planned sequence name.
    pub fn sequence_name(&self) -> &str {
        &self.sequence_name
    }

    /// Step plans in insertion order.
    pub fn steps(&self) -> impl ExactSizeIterator<Item = &StepPlan> {
        self.steps.iter()
    }

    /// Aggregate decision across the sequence and all of its steps.
    pub fn decision(&self) -> &PlanDecision {
        &self.decision
    }

    /// Returns `true` when every step and the sequence itself are allowed.
    pub fn is_allowed(&self) -> bool {
        self.decision.is_allowed()
    }

    /// Stable one-line-per-step summary: `index\tstep-id\tallow|deny\tcodes`.
    ///
    /// Codes are comma-separated and sorted, or `-` when a step is allowed.
    /// The format is fixed so it can be used as a golden-test snapshot.
    pub fn summary(&self) -> String {
        let mut out = String::new();
        for step in &self.steps {
            out.push_str(&step.index.to_string());
            out.push('\t');
            out.push_str(&step.step_id);
            out.push('\t');
            out.push_str(if step.is_allowed() { "allow" } else { "deny" });
            out.push('\t');
            let mut codes: Vec<&str> = step
                .decision
                .reasons()
                .iter()
                .map(PlanDenyReason::code)
                .collect();
            codes.sort_unstable();
            if codes.is_empty() {
                out.push('-');
            } else {
                out.push_str(&codes.join(","));
            }
            out.push('\n');
        }
        out
    }
}

/// Host policy applied when planning and running a sequence.
#[derive(Debug, Clone)]
pub struct SequencePolicy {
    max_sequence_steps: usize,
    max_script_bytes: usize,
    budget_limits: ScriptBudget,
    allowed_commands: BTreeSet<String>,
}

impl SequencePolicy {
    /// Creates a deny-by-default policy with documented ceilings.
    pub fn new() -> Self {
        Self {
            max_sequence_steps: MAX_SEQUENCE_STEPS,
            max_script_bytes: MAX_SCRIPT_BYTES,
            budget_limits: ScriptBudget::default(),
            allowed_commands: BTreeSet::new(),
        }
    }

    /// Replaces the step-count ceiling.
    pub fn with_max_sequence_steps(mut self, max: usize) -> Self {
        self.max_sequence_steps = max;
        self
    }

    /// Replaces the script source-size ceiling.
    pub fn with_max_script_bytes(mut self, max: usize) -> Self {
        self.max_script_bytes = max;
        self
    }

    /// Replaces the script budget ceilings.
    pub fn with_budget_limits(mut self, limits: ScriptBudget) -> Self {
        self.budget_limits = limits;
        self
    }

    /// Allows one named command (idempotent).
    pub fn allow_command(mut self, command: impl Into<String>) -> Self {
        self.allowed_commands.insert(command.into());
        self
    }

    /// Returns `true` when the command is on the allowlist.
    pub fn is_command_allowed(&self, command: &str) -> bool {
        self.allowed_commands.contains(command)
    }

    /// Step-count ceiling.
    pub fn max_sequence_steps(&self) -> usize {
        self.max_sequence_steps
    }

    /// Script source-size ceiling in bytes.
    pub fn max_script_bytes(&self) -> usize {
        self.max_script_bytes
    }

    /// Script budget ceilings.
    pub fn budget_limits(&self) -> ScriptBudget {
        self.budget_limits
    }
}

impl Default for SequencePolicy {
    fn default() -> Self {
        Self::new()
    }
}

fn budget_deny_reasons(
    step: &str,
    requested: &ScriptBudget,
    limits: &ScriptBudget,
) -> Vec<PlanDenyReason> {
    let mut reasons = Vec::new();
    if requested.fuel > limits.fuel {
        reasons.push(PlanDenyReason::BudgetExceedsLimit {
            step: step.to_owned(),
            field: WizardBudgetField::Fuel,
            requested: requested.fuel,
            allowed: limits.fuel,
        });
    }
    if requested.max_calls > limits.max_calls {
        reasons.push(PlanDenyReason::BudgetExceedsLimit {
            step: step.to_owned(),
            field: WizardBudgetField::Calls,
            requested: requested.max_calls,
            allowed: limits.max_calls,
        });
    }
    if requested.max_loop_iterations > limits.max_loop_iterations {
        reasons.push(PlanDenyReason::BudgetExceedsLimit {
            step: step.to_owned(),
            field: WizardBudgetField::LoopIterations,
            requested: requested.max_loop_iterations,
            allowed: limits.max_loop_iterations,
        });
    }
    if requested.max_string_len > limits.max_string_len {
        reasons.push(PlanDenyReason::BudgetExceedsLimit {
            step: step.to_owned(),
            field: WizardBudgetField::StringLen,
            requested: requested.max_string_len as u64,
            allowed: limits.max_string_len as u64,
        });
    }
    reasons
}

/// Plans `sequence` against `policy` without executing anything.
///
/// Every problem is collected (not fail-fast), and each step keeps its own
/// typed reasons. The aggregate decision is the union of all step and
/// sequence-level reasons, canonicalized by [`PlanDecision::new`].
pub fn plan_sequence(sequence: &ActionSequence, policy: &SequencePolicy) -> RunnerPlan {
    let mut sequence_reasons: Vec<PlanDenyReason> = Vec::new();
    if sequence.is_empty() {
        sequence_reasons.push(PlanDenyReason::EmptySequence);
    }
    if sequence.len() > policy.max_sequence_steps() {
        sequence_reasons.push(PlanDenyReason::SequenceTooLong {
            len: sequence.len(),
            max: policy.max_sequence_steps(),
        });
    }

    let mut seen: HashSet<&str> = HashSet::new();
    let mut steps: Vec<StepPlan> = Vec::with_capacity(sequence.len());
    let mut all_reasons: Vec<PlanDenyReason> = sequence_reasons.clone();

    for (index, step) in sequence.steps().enumerate() {
        let mut reasons: Vec<PlanDenyReason> = Vec::new();
        if step.id().trim().is_empty() {
            reasons.push(PlanDenyReason::EmptyStepId);
        }
        if !seen.insert(step.id()) {
            reasons.push(PlanDenyReason::DuplicateStepId {
                id: step.id().to_owned(),
            });
        }
        match step.kind() {
            WizardStepKind::RunScript { script } => {
                if script.trim().is_empty() {
                    reasons.push(PlanDenyReason::EmptyScript {
                        step: step.id().to_owned(),
                    });
                }
                if script.len() > policy.max_script_bytes() {
                    reasons.push(PlanDenyReason::ScriptTooLarge {
                        step: step.id().to_owned(),
                        len: script.len(),
                        max: policy.max_script_bytes(),
                    });
                }
                reasons.extend(budget_deny_reasons(
                    step.id(),
                    &sequence.budget(),
                    &policy.budget_limits(),
                ));
            }
            WizardStepKind::SetMetadata { key, .. } => {
                if key.trim().is_empty() {
                    reasons.push(PlanDenyReason::EmptyMetadataKey {
                        step: step.id().to_owned(),
                    });
                }
            }
            WizardStepKind::RunCommand { command } => {
                if command.trim().is_empty() {
                    reasons.push(PlanDenyReason::EmptyCommand {
                        step: step.id().to_owned(),
                    });
                } else if !policy.is_command_allowed(command) {
                    reasons.push(PlanDenyReason::CommandNotAllowed {
                        step: step.id().to_owned(),
                        command: command.clone(),
                    });
                }
            }
            WizardStepKind::Unsupported { name } => {
                reasons.push(PlanDenyReason::UnsupportedStepKind { name: name.clone() });
            }
        }
        let decision = PlanDecision::new(reasons);
        all_reasons.extend(decision.reasons().iter().cloned());
        steps.push(StepPlan {
            index,
            step_id: step.id().to_owned(),
            kind: step.kind().clone(),
            decision,
        });
    }

    RunnerPlan {
        sequence_name: sequence.name().to_owned(),
        steps,
        decision: PlanDecision::new(all_reasons),
    }
}

/// Typed error returned by a caller-supplied [`ScriptStepExecutor`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScriptExecError {
    /// The sandbox rejected the script (parse or runtime, including budgets).
    Rejected {
        /// Owning step id.
        step: String,
        /// Sandbox error.
        error: JsError,
    },
    /// The executor refused or failed for a non-sandbox reason.
    Refused {
        /// Owning step id.
        step: String,
        /// Human-readable reason.
        reason: String,
    },
    /// Cancellation was observed while executing the script.
    Cancelled {
        /// Owning step id.
        step: String,
    },
}

impl ScriptExecError {
    /// Stable machine-readable code for this error.
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Rejected { .. } => "script_rejected",
            Self::Refused { .. } => "script_refused",
            Self::Cancelled { .. } => "script_cancelled",
        }
    }
}

impl fmt::Display for ScriptExecError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Rejected { step, error } => write!(f, "step '{step}' script rejected: {error}"),
            Self::Refused { step, reason } => write!(f, "step '{step}' executor refused: {reason}"),
            Self::Cancelled { step } => write!(f, "step '{step}' script was cancelled"),
        }
    }
}

impl std::error::Error for ScriptExecError {}

/// Executes a script step. Implementations must not perform I/O or networking.
pub trait ScriptStepExecutor: fmt::Debug {
    /// Runs `script` for `step_id` under `budget`, honoring `cancel`.
    fn run_script(
        &self,
        step_id: &str,
        script: &str,
        budget: &ScriptBudget,
        cancel: &CancelToken,
    ) -> Result<Value, ScriptExecError>;
}

fn map_js_error(step_id: &str, error: JsError) -> ScriptExecError {
    match error {
        JsError::Cancelled { .. } => ScriptExecError::Cancelled {
            step: step_id.to_owned(),
        },
        other => ScriptExecError::Rejected {
            step: step_id.to_owned(),
            error: other,
        },
    }
}

/// Default [`ScriptStepExecutor`] that calls into the existing JS sandbox.
///
/// The step's source is parsed with the budget's parser limits and run with the
/// same budget and the caller's cancellation token. It has no filesystem,
/// network, process, clock, or ambient-global access.
#[derive(Debug, Clone, Copy, Default)]
pub struct SandboxedScriptExecutor;

impl ScriptStepExecutor for SandboxedScriptExecutor {
    fn run_script(
        &self,
        step_id: &str,
        script: &str,
        budget: &ScriptBudget,
        cancel: &CancelToken,
    ) -> Result<Value, ScriptExecError> {
        let limits = budget.to_limits();
        let parsed = Script::parse_with_limits(script, &limits)
            .map_err(|error| map_js_error(step_id, error))?;
        parsed
            .run(&limits, cancel)
            .map_err(|error| map_js_error(step_id, error))
    }
}

/// Typed error returned by a caller-supplied [`CommandHandler`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandExecError {
    /// The command failed in the host.
    Failed {
        /// Owning step id.
        step: String,
        /// Command name.
        command: String,
        /// Human-readable reason.
        reason: String,
    },
    /// Cancellation was observed while dispatching the command.
    Cancelled {
        /// Owning step id.
        step: String,
    },
}

impl CommandExecError {
    /// Stable machine-readable code for this error.
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Failed { .. } => "command_failed",
            Self::Cancelled { .. } => "command_cancelled",
        }
    }
}

impl fmt::Display for CommandExecError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Failed {
                step,
                command,
                reason,
            } => write!(f, "step '{step}' command '{command}' failed: {reason}"),
            Self::Cancelled { step } => write!(f, "step '{step}' command was cancelled"),
        }
    }
}

impl std::error::Error for CommandExecError {}

/// Dispatches a named automation command. Implementations own any real work;
/// the runner and this crate perform no I/O or networking.
pub trait CommandHandler: fmt::Debug {
    /// Dispatches `command` for `step_id`, honoring `cancel`.
    fn run(
        &self,
        step_id: &str,
        command: &str,
        cancel: &CancelToken,
    ) -> Result<(), CommandExecError>;
}

/// A no-op [`CommandHandler`] that only records success through the report.
#[derive(Debug, Clone, Copy, Default)]
pub struct RecordingCommandHandler;

impl CommandHandler for RecordingCommandHandler {
    fn run(
        &self,
        _step_id: &str,
        _command: &str,
        _cancel: &CancelToken,
    ) -> Result<(), CommandExecError> {
        Ok(())
    }
}

/// Typed error for the whole sequence run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WizardRunError {
    /// The plan denied the sequence or a step.
    PlanDenied(Vec<PlanDenyReason>),
    /// A script step failed.
    Script(ScriptExecError),
    /// A command step failed.
    Command(CommandExecError),
    /// A metadata step could not be applied.
    Metadata {
        /// Owning step id.
        step: String,
        /// Reason.
        reason: String,
    },
}

impl WizardRunError {
    /// Stable machine-readable code for this error.
    pub const fn code(&self) -> &'static str {
        match self {
            Self::PlanDenied(_) => "plan_denied",
            Self::Script(error) => error.code(),
            Self::Command(error) => error.code(),
            Self::Metadata { .. } => "metadata_failed",
        }
    }
}

impl fmt::Display for WizardRunError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PlanDenied(reasons) => {
                f.write_str("sequence plan denied: ")?;
                let messages: Vec<String> = reasons.iter().map(ToString::to_string).collect();
                f.write_str(&messages.join("; "))
            }
            Self::Script(error) => write!(f, "{error}"),
            Self::Command(error) => write!(f, "{error}"),
            Self::Metadata { step, reason } => {
                write!(f, "step '{step}' metadata update failed: {reason}")
            }
        }
    }
}

impl std::error::Error for WizardRunError {}

/// Outcome of executing one planned step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StepRunOutcome {
    /// Script completed; holds a stable rendering of the returned value.
    ScriptRan {
        /// Rendered result value.
        value: String,
    },
    /// Metadata key/value was applied to the in-memory map.
    MetadataSet {
        /// Metadata key.
        key: String,
        /// Metadata value.
        value: String,
    },
    /// A named command was dispatched successfully.
    CommandRan {
        /// Command name.
        command: String,
    },
    /// The step was denied by the plan and skipped.
    Denied {
        /// Typed deny reasons.
        reasons: Vec<PlanDenyReason>,
    },
    /// The step failed with a typed error.
    Failed {
        /// Typed failure.
        error: WizardRunError,
    },
}

impl StepRunOutcome {
    /// Stable status token used in [`SequenceReport::summary`].
    pub const fn status(&self) -> &'static str {
        match self {
            Self::ScriptRan { .. } | Self::MetadataSet { .. } | Self::CommandRan { .. } => "ran",
            Self::Denied { .. } => "denied",
            Self::Failed { .. } => "failed",
        }
    }
}

/// Deterministic report produced by a [`SequenceRunner`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SequenceReport {
    sequence_name: String,
    outcomes: Vec<(String, StepRunOutcome)>,
    metadata: BTreeMap<String, String>,
}

impl SequenceReport {
    /// Planned sequence name.
    pub fn sequence_name(&self) -> &str {
        &self.sequence_name
    }

    /// `(step-id, outcome)` pairs in execution order.
    pub fn outcomes(&self) -> &[(String, StepRunOutcome)] {
        &self.outcomes
    }

    /// In-memory metadata produced by metadata steps, keyed in sorted order.
    pub fn metadata(&self) -> &BTreeMap<String, String> {
        &self.metadata
    }

    /// Returns `true` when every step ran successfully.
    pub fn all_ok(&self) -> bool {
        self.outcomes.iter().all(|(_, outcome)| {
            matches!(
                outcome,
                StepRunOutcome::ScriptRan { .. }
                    | StepRunOutcome::MetadataSet { .. }
                    | StepRunOutcome::CommandRan { .. }
            )
        })
    }

    /// Stable one-line-per-step summary: `index\tstep-id\tstatus\terror-code`.
    ///
    /// `error-code` is `-` for successful steps; denied/failed steps use the
    /// first typed reason/error code so summaries stay short and deterministic.
    pub fn summary(&self) -> String {
        let mut out = String::new();
        for (index, (step_id, outcome)) in self.outcomes.iter().enumerate() {
            let detail = match outcome {
                StepRunOutcome::Denied { reasons } => {
                    reasons.first().map_or("-", PlanDenyReason::code).to_owned()
                }
                StepRunOutcome::Failed { error } => error.code().to_owned(),
                _ => "-".to_owned(),
            };
            out.push_str(&index.to_string());
            out.push('\t');
            out.push_str(step_id);
            out.push('\t');
            out.push_str(outcome.status());
            out.push('\t');
            out.push_str(&detail);
            out.push('\n');
        }
        out
    }
}

fn render_value(value: &Value) -> String {
    match value {
        Value::Undefined => "undefined".to_owned(),
        Value::Null => "null".to_owned(),
        Value::Bool(value) => value.to_string(),
        Value::Number(value) => value.to_string(),
        Value::Str(value) => value.clone(),
        other => other.type_name().to_owned(),
    }
}

/// Runs planned Action Wizard steps deterministically with caller-supplied
/// executors.
#[derive(Debug)]
pub struct SequenceRunner<'a, S, C>
where
    S: ScriptStepExecutor + ?Sized,
    C: CommandHandler + ?Sized,
{
    policy: &'a SequencePolicy,
    scripts: &'a S,
    commands: &'a C,
}

impl<'a, S, C> SequenceRunner<'a, S, C>
where
    S: ScriptStepExecutor + ?Sized,
    C: CommandHandler + ?Sized,
{
    /// Creates a runner from a policy and the two executor boundaries.
    pub fn new(policy: &'a SequencePolicy, scripts: &'a S, commands: &'a C) -> Self {
        Self {
            policy,
            scripts,
            commands,
        }
    }

    /// Returns the plan this runner would execute.
    pub fn plan(&self, sequence: &ActionSequence) -> RunnerPlan {
        plan_sequence(sequence, self.policy)
    }

    /// Plans and executes `sequence`, reporting every step without stopping.
    pub fn run(&self, sequence: &ActionSequence, cancel: &CancelToken) -> SequenceReport {
        let plan = self.plan(sequence);
        let budget = sequence.budget();
        let mut outcomes: Vec<(String, StepRunOutcome)> = Vec::with_capacity(plan.steps().len());
        let mut metadata: BTreeMap<String, String> = BTreeMap::new();

        for step in plan.steps() {
            if !step.is_allowed() {
                outcomes.push((
                    step.step_id().to_owned(),
                    StepRunOutcome::Denied {
                        reasons: step.decision().reasons().to_vec(),
                    },
                ));
                continue;
            }
            let outcome = match step.kind() {
                WizardStepKind::RunScript { script } => {
                    match self
                        .scripts
                        .run_script(step.step_id(), script, &budget, cancel)
                    {
                        Ok(value) => StepRunOutcome::ScriptRan {
                            value: render_value(&value),
                        },
                        Err(error) => StepRunOutcome::Failed {
                            error: WizardRunError::Script(error),
                        },
                    }
                }
                WizardStepKind::SetMetadata { key, value } => {
                    if key.trim().is_empty() {
                        StepRunOutcome::Failed {
                            error: WizardRunError::Metadata {
                                step: step.step_id().to_owned(),
                                reason: "empty metadata key".to_owned(),
                            },
                        }
                    } else {
                        metadata.insert(key.clone(), value.clone());
                        StepRunOutcome::MetadataSet {
                            key: key.clone(),
                            value: value.clone(),
                        }
                    }
                }
                WizardStepKind::RunCommand { command } => {
                    match self.commands.run(step.step_id(), command, cancel) {
                        Ok(()) => StepRunOutcome::CommandRan {
                            command: command.clone(),
                        },
                        Err(error) => StepRunOutcome::Failed {
                            error: WizardRunError::Command(error),
                        },
                    }
                }
                WizardStepKind::Unsupported { name } => StepRunOutcome::Denied {
                    reasons: vec![PlanDenyReason::UnsupportedStepKind { name: name.clone() }],
                },
            };
            outcomes.push((step.step_id().to_owned(), outcome));
        }

        SequenceReport {
            sequence_name: sequence.name().to_owned(),
            outcomes,
            metadata,
        }
    }
}

/// Convenience entry point that builds a [`SequenceRunner`] and runs once.
pub fn run_sequence<S, C>(
    sequence: &ActionSequence,
    policy: &SequencePolicy,
    scripts: &S,
    commands: &C,
    cancel: &CancelToken,
) -> SequenceReport
where
    S: ScriptStepExecutor + ?Sized,
    C: CommandHandler + ?Sized,
{
    SequenceRunner::new(policy, scripts, commands).run(sequence, cancel)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    fn valid_sequence() -> ActionSequence {
        ActionSequence::from_steps(
            "nightly",
            [
                WizardStep::script("compute", "var x = 2 + 3; x;"),
                WizardStep::set_metadata("tag", "title", "Nightly"),
            ],
        )
        .unwrap_or_else(|error| panic!("valid fixture: {error}"))
    }

    #[derive(Debug, Default)]
    struct RecordingScriptExecutor {
        calls: RefCell<Vec<String>>,
        fail_at: Option<String>,
    }

    impl ScriptStepExecutor for RecordingScriptExecutor {
        fn run_script(
            &self,
            step_id: &str,
            _script: &str,
            _budget: &ScriptBudget,
            _cancel: &CancelToken,
        ) -> Result<Value, ScriptExecError> {
            self.calls.borrow_mut().push(step_id.to_owned());
            if self.fail_at.as_deref() == Some(step_id) {
                return Err(ScriptExecError::Refused {
                    step: step_id.to_owned(),
                    reason: "injected failure".to_owned(),
                });
            }
            Ok(Value::Number(7.0))
        }
    }

    #[derive(Debug)]
    struct FailingCommandHandler;

    impl CommandHandler for FailingCommandHandler {
        fn run(
            &self,
            step_id: &str,
            command: &str,
            _cancel: &CancelToken,
        ) -> Result<(), CommandExecError> {
            Err(CommandExecError::Failed {
                step: step_id.to_owned(),
                command: command.to_owned(),
                reason: "injected command failure".to_owned(),
            })
        }
    }

    #[test]
    fn valid_sequence_passes_validation() {
        let sequence = valid_sequence();
        assert_eq!(sequence.validate(), Ok(()));
        assert_eq!(sequence.name(), "nightly");
        assert_eq!(sequence.len(), 2);
        assert_eq!(
            sequence.step("compute").unwrap().kind().kind_str(),
            "run-script"
        );
        assert_eq!(sequence.budget(), ScriptBudget::default());
    }

    #[test]
    fn empty_sequence_is_rejected_by_validation_and_plan() {
        let sequence = ActionSequence::new("empty");
        assert_eq!(
            sequence.validate(),
            Err(WizardValidationError::EmptySequence {
                name: "empty".to_owned()
            })
        );
        let plan = plan_sequence(&sequence, &SequencePolicy::new());
        assert!(!plan.is_allowed());
        assert_eq!(plan.decision().reasons(), &[PlanDenyReason::EmptySequence]);
        assert_eq!(plan.summary(), "");
    }

    #[test]
    fn duplicate_step_ids_are_rejected() {
        assert_eq!(
            ActionSequence::from_steps(
                "dup",
                [
                    WizardStep::script("same", "1;"),
                    WizardStep::script("same", "2;"),
                ],
            )
            .err(),
            Some(WizardValidationError::DuplicateStepId {
                name: "dup".to_owned(),
                id: "same".to_owned(),
            })
        );
        // The planner reports the duplicate on the second occurrence only.
        let plan = plan_sequence(
            &ActionSequence {
                name: "dup".to_owned(),
                steps: vec![
                    WizardStep::script("same", "1;"),
                    WizardStep::script("same", "2;"),
                ],
                budget: ScriptBudget::default(),
            },
            &SequencePolicy::new(),
        );
        assert_eq!(
            plan.steps().nth(1).unwrap().decision().reasons(),
            &[PlanDenyReason::DuplicateStepId {
                id: "same".to_owned()
            }]
        );
        assert!(plan.steps().next().unwrap().is_allowed());
    }

    #[test]
    fn bounded_step_count_is_enforced() {
        let mut sequence = ActionSequence::new("bounded");
        for index in 0..MAX_SEQUENCE_STEPS {
            sequence
                .add_step(WizardStep::set_metadata(format!("s{index}"), "k", "v"))
                .unwrap_or_else(|error| panic!("fixture: {error}"));
        }
        assert_eq!(sequence.validate(), Ok(()));
        assert_eq!(
            sequence.add_step(WizardStep::set_metadata("overflow", "k", "v")),
            Err(WizardValidationError::SequenceTooLong {
                name: "bounded".to_owned(),
                len: MAX_SEQUENCE_STEPS + 1,
                max: MAX_SEQUENCE_STEPS,
            })
        );
        // A tighter policy reports the sequence-level reason in the plan.
        let plan = plan_sequence(&sequence, &SequencePolicy::new().with_max_sequence_steps(2));
        assert!(
            plan.decision()
                .reasons()
                .contains(&PlanDenyReason::SequenceTooLong {
                    len: MAX_SEQUENCE_STEPS,
                    max: 2,
                })
        );
        assert!(plan.steps().all(StepPlan::is_allowed));
    }

    #[test]
    fn plan_preserves_step_order_and_sorts_reasons() {
        let sequence = ActionSequence::from_steps(
            "order",
            [
                WizardStep::script("first", "1;"),
                WizardStep::set_metadata("second", "k", "v"),
                WizardStep::run_command("third", "cmd"),
            ],
        )
        .unwrap_or_else(|error| panic!("fixture: {error}"));
        let policy = SequencePolicy::new().allow_command("cmd");
        let plan = plan_sequence(&sequence, &policy);
        let ids: Vec<&str> = plan.steps().map(StepPlan::step_id).collect();
        assert_eq!(ids, ["first", "second", "third"]);
        let indices: Vec<usize> = plan.steps().map(StepPlan::index).collect();
        assert_eq!(indices, [0, 1, 2]);
        assert!(plan.is_allowed());
        assert!(plan.decision().reasons().is_empty());

        // With an empty deny list the reasons are canonically ordered.
        let denied = plan_sequence(&sequence, &SequencePolicy::new());
        let reasons = denied.decision().reasons();
        assert!(reasons.windows(2).all(|pair| pair[0] <= pair[1]));
        assert_eq!(
            denied.summary(),
            "0\tfirst\tallow\t-\n1\tsecond\tallow\t-\n2\tthird\tdeny\tcommand_not_allowed\n"
        );
    }

    #[test]
    fn per_step_deny_reasons_are_typed() {
        let sequence = ActionSequence::from_steps(
            "mixed",
            [
                WizardStep::script("empty-script", "   "),
                WizardStep::set_metadata("empty-key", "", "v"),
                WizardStep::run_command("bad-command", ""),
                WizardStep::unsupported("odd", "teleport"),
            ],
        )
        .unwrap_or_else(|error| panic!("fixture: {error}"));
        let plan = plan_sequence(&sequence, &SequencePolicy::new());
        let steps: Vec<&StepPlan> = plan.steps().collect();
        assert_eq!(
            steps[0].decision().reasons(),
            &[PlanDenyReason::EmptyScript {
                step: "empty-script".to_owned()
            }]
        );
        assert_eq!(
            steps[1].decision().reasons(),
            &[PlanDenyReason::EmptyMetadataKey {
                step: "empty-key".to_owned()
            }]
        );
        assert_eq!(
            steps[2].decision().reasons(),
            &[PlanDenyReason::EmptyCommand {
                step: "bad-command".to_owned()
            }]
        );
        assert_eq!(
            steps[3].decision().reasons(),
            &[PlanDenyReason::UnsupportedStepKind {
                name: "teleport".to_owned()
            }]
        );
        assert!(!plan.is_allowed());
    }

    #[test]
    fn unsupported_step_kind_is_denied_with_typed_reason() {
        let sequence =
            ActionSequence::from_steps("odd", [WizardStep::unsupported("s", "unknown-kind")])
                .unwrap_or_else(|error| panic!("fixture: {error}"));
        let plan = plan_sequence(&sequence, &SequencePolicy::new());
        assert_eq!(
            plan.decision().reasons(),
            &[PlanDenyReason::UnsupportedStepKind {
                name: "unknown-kind".to_owned()
            }]
        );
        assert_eq!(plan.decision().reasons()[0].code(), "unsupported_step_kind");
    }

    #[test]
    fn oversized_script_and_budget_overages_are_denied() {
        let sequence =
            ActionSequence::from_steps("over", [WizardStep::script("big", "1234567890")])
                .unwrap_or_else(|error| panic!("fixture: {error}"))
                .with_budget(ScriptBudget::default().with_fuel(999_999));
        let policy = SequencePolicy::new()
            .with_max_script_bytes(4)
            .with_budget_limits(ScriptBudget::default().with_fuel(10));
        let plan = plan_sequence(&sequence, &policy);
        let reasons = plan.steps().next().unwrap().decision().reasons();
        assert!(reasons.contains(&PlanDenyReason::ScriptTooLarge {
            step: "big".to_owned(),
            len: 10,
            max: 4,
        }));
        assert!(reasons.contains(&PlanDenyReason::BudgetExceedsLimit {
            step: "big".to_owned(),
            field: WizardBudgetField::Fuel,
            requested: 999_999,
            allowed: 10,
        }));
    }

    #[test]
    fn sandboxed_executor_runs_a_valid_script() {
        let script = SandboxedScriptExecutor;
        let value = script
            .run_script(
                "s",
                "var x = 2 + 3; x;",
                &ScriptBudget::default(),
                &CancelToken::new(),
            )
            .unwrap_or_else(|error| panic!("valid script: {error}"));
        assert_eq!(value.as_number(), Some(5.0));
    }

    #[test]
    fn script_budget_is_enforced_with_a_typed_error() {
        let script = SandboxedScriptExecutor;
        let result = script.run_script(
            "loop",
            "while (true) { }",
            &ScriptBudget::default().with_fuel(25),
            &CancelToken::new(),
        );
        let error = result
            .err()
            .unwrap_or_else(|| panic!("expected budget denial"));
        assert!(
            matches!(
                error,
                ScriptExecError::Rejected {
                    error: JsError::StepBudgetExceeded { .. },
                    ..
                }
            ),
            "unexpected error: {error}"
        );
        assert_eq!(error.code(), "script_rejected");
    }

    #[test]
    fn cancelled_script_returns_typed_cancelled_error() {
        let script = SandboxedScriptExecutor;
        let token = CancelToken::new();
        token.cancel();
        let result = script.run_script("s", "1 + 1;", &ScriptBudget::default(), &token);
        assert_eq!(
            result.err(),
            Some(ScriptExecError::Cancelled {
                step: "s".to_owned()
            })
        );
    }

    #[test]
    fn executor_success_and_failure_propagate_through_runner() {
        let sequence = ActionSequence::from_steps(
            "scripts",
            [
                WizardStep::script("ok", "1;"),
                WizardStep::script("bad", "2;"),
            ],
        )
        .unwrap_or_else(|error| panic!("fixture: {error}"));
        let executor = RecordingScriptExecutor {
            fail_at: Some("bad".to_owned()),
            ..RecordingScriptExecutor::default()
        };
        let report = run_sequence(
            &sequence,
            &SequencePolicy::new(),
            &executor,
            &RecordingCommandHandler,
            &CancelToken::new(),
        );
        assert_eq!(*executor.calls.borrow(), ["ok", "bad"]);
        assert_eq!(
            report.outcomes()[0].1,
            StepRunOutcome::ScriptRan {
                value: "7".to_owned()
            }
        );
        assert!(matches!(
            &report.outcomes()[1].1,
            StepRunOutcome::Failed {
                error: WizardRunError::Script(ScriptExecError::Refused { step, reason })
            } if step == "bad" && reason == "injected failure"
        ));
        assert!(!report.all_ok());
        assert_eq!(
            report.summary(),
            "0\tok\tran\t-\n1\tbad\tfailed\tscript_refused\n"
        );
    }

    #[test]
    fn metadata_step_applies_to_in_memory_report() {
        let sequence = valid_sequence();
        let report = run_sequence(
            &sequence,
            &SequencePolicy::new(),
            &SandboxedScriptExecutor,
            &RecordingCommandHandler,
            &CancelToken::new(),
        );
        assert_eq!(
            report.metadata().get("title").map(String::as_str),
            Some("Nightly")
        );
        assert_eq!(
            report.outcomes()[1].1,
            StepRunOutcome::MetadataSet {
                key: "title".to_owned(),
                value: "Nightly".to_owned(),
            }
        );
        assert!(report.all_ok());
    }

    #[test]
    fn command_step_success_and_failure_are_typed() {
        let sequence = ActionSequence::from_steps(
            "commands",
            [WizardStep::run_command("dispatch", "archive")],
        )
        .unwrap_or_else(|error| panic!("fixture: {error}"));
        let policy = SequencePolicy::new().allow_command("archive");
        let ok = run_sequence(
            &sequence,
            &policy,
            &SandboxedScriptExecutor,
            &RecordingCommandHandler,
            &CancelToken::new(),
        );
        assert_eq!(
            ok.outcomes()[0].1,
            StepRunOutcome::CommandRan {
                command: "archive".to_owned()
            }
        );
        let failed = run_sequence(
            &sequence,
            &policy,
            &SandboxedScriptExecutor,
            &FailingCommandHandler,
            &CancelToken::new(),
        );
        assert!(matches!(
            &failed.outcomes()[0].1,
            StepRunOutcome::Failed {
                error: WizardRunError::Command(CommandExecError::Failed { command, .. })
            } if command == "archive"
        ));
        assert_eq!(failed.summary(), "0\tdispatch\tfailed\tcommand_failed\n");
    }

    #[test]
    fn denied_steps_are_skipped_without_executing() {
        let sequence = ActionSequence::from_steps(
            "mixed",
            [
                WizardStep::run_command("denied", "not-allowed"),
                WizardStep::script("allowed", "1;"),
            ],
        )
        .unwrap_or_else(|error| panic!("fixture: {error}"));
        let executor = RecordingScriptExecutor::default();
        let report = run_sequence(
            &sequence,
            &SequencePolicy::new(),
            &executor,
            &RecordingCommandHandler,
            &CancelToken::new(),
        );
        assert_eq!(*executor.calls.borrow(), ["allowed"]);
        assert!(matches!(
            &report.outcomes()[0].1,
            StepRunOutcome::Denied { reasons } if reasons[0].code() == "command_not_allowed"
        ));
        assert_eq!(report.outcomes()[1].1.status(), "ran");
    }

    #[test]
    fn reports_and_plans_are_deterministic_across_runs() {
        let sequence = valid_sequence();
        let policy = SequencePolicy::new();
        let first_plan = plan_sequence(&sequence, &policy);
        let second_plan = plan_sequence(&sequence, &policy);
        assert_eq!(first_plan, second_plan);
        assert_eq!(first_plan.summary(), second_plan.summary());

        let first = run_sequence(
            &sequence,
            &policy,
            &SandboxedScriptExecutor,
            &RecordingCommandHandler,
            &CancelToken::new(),
        );
        let second = run_sequence(
            &sequence,
            &policy,
            &SandboxedScriptExecutor,
            &RecordingCommandHandler,
            &CancelToken::new(),
        );
        assert_eq!(first, second);
        assert_eq!(first.summary(), second.summary());
    }

    #[test]
    fn typed_error_codes_and_messages_are_actionable() {
        let validation = WizardValidationError::DuplicateStepId {
            name: "nightly".to_owned(),
            id: "export".to_owned(),
        };
        assert_eq!(validation.code(), "duplicate_step_id");
        assert!(validation.to_string().contains("export"));
        let deny = PlanDenyReason::CommandNotAllowed {
            step: "s".to_owned(),
            command: "wipe".to_owned(),
        };
        assert_eq!(deny.code(), "command_not_allowed");
        assert!(deny.to_string().contains("wipe"));
        let run = WizardRunError::Metadata {
            step: "s".to_owned(),
            reason: "empty metadata key".to_owned(),
        };
        assert_eq!(run.code(), "metadata_failed");
        assert!(run.to_string().contains("metadata key"));
        assert_eq!(WizardBudgetField::Fuel.as_str(), "fuel");
    }
}
