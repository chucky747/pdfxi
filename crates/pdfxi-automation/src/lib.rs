//! Action Wizard, batch processing, droplets, JavaScript sandbox, and plugin host.
//!
//! This crate currently provides the dependency-free in-memory model for
//! actions and batch jobs. The core abstractions are:
//!
//! - [`Action`] / [`ActionStep`] / [`StepKind`] â€” a named, ordered, validatable
//!   sequence of operations with stable step-kind identifiers and lossless text
//!   import/export.
//! - [`BatchJob`] / [`BatchQueue`] â€” a deterministic, single-threaded runner
//!   that executes jobs through a [`StepExecutor`], isolates failures, supports
//!   cooperative cancellation, and records a [`JobOutcome`] per file.
//! - [`DocumentOperations`] / [`DocumentExecutor`] â€” the object-safe document
//!   interface and its real executor. [`DocumentExecutor`] maps each step kind to
//!   one operation, threads the produced [`Artifact`] between steps, honours
//!   cancellation, and records stable [`StepResult`]s. [`BatchQueue::run_with`]
//!   drives it through the object-safe [`JobExecutor`] bridge.
//! - [`ActionLibrary`] â€” a multi-action container that parses several named
//!   actions from one tab-delimited text file, the format consumed by the CLI's
//!   `action list`, `action show`, `action run`, and `droplet` commands.
//! - [`AutomationError`] â€” actionable, self-describing errors for every
//!   rejection path.
//! - [`ApiEngine`] / [`Request`] / [`Response`] â€” a versioned, line-delimited
//!   JSON API over a bounded, socket-free text protocol. Commands point at
//!   saved action files, run batches, and perform dry-run validation through
//!   an injected [`ApiHost`]. [`JsonValue`] provides canonical parsing and
//!   serialization with no external dependency.
//! - [`Toolset`] / [`Tool`] â€” named JSON files mapping friendly aliases to
//!   saved actions plus parameter defaults, validated and runnable through the
//!   same API, with dangerous aliases and step kinds rejected.
//! - [`Script`] / [`Limits`] / [`run_script`] â€” a dependency-free, deterministic
//!   JavaScript subset for document actions, with explicit CPU-step, call,
//!   iteration, size, wall-clock, and cancellation budgets. The [`HostObject`]
//!   trait is an audited boundary for exposing future document operations; no
//!   real PDF mutation is wired up in this milestone.
//! - [`AcrobatContext`] / [`FieldStore`] / [`EventData`] / [`run_acrobat_script`]
//!   â€” an explicitly injected, in-memory Acrobat compatibility layer providing
//!   `this.getField` read/write, `event`, the `AFNumber_*`, `AFPercent_*`, and
//!   `AFDate_*` helpers, and a documented `util.printf` subset for field format,
//!   validate, and calculate scripts. No I/O, clock, randomness, or ambient
//!   globals; no real PDF mutation.
//!
//! All behavior is deterministic and allocates only as needed. The crate has no
//! external dependencies, forbids `unsafe`, and keeps panics out of non-test
//! code. [`WatchedFolder`] adds a deterministic watched-folder plan and execution
//! adapter with host-driven `poll_once` calls, validated rooted folder config,
//! per-file outcomes, and in-memory idempotency markers; no thread or OS watcher
//! is started.
//! - [`PluginManifest`] / [`Capability`] / [`CapabilityGrants`] / [`PluginHost`]
//!   / [`PluginRegistry`] / [`PluginInvocation`] / [`InvocationReport`] — a
//!   local, dependency-free WASM-style plugin host *contract* with a closed
//!   capability allowlist (no filesystem/network/process), deny-by-default
//!   grants, deterministic id-ordered registration, and an allow/deny
//!   [`plan_invocation`] report with budgets and sorted deny reasons. No WASM
//!   runtime is loaded or executed.
//! - [`ActionSequence`] / [`WizardStep`] / [`WizardStepKind`] / [`RunnerPlan`] /
//!   [`SequenceRunner`] — an Action Wizard-style binding for the sandboxed JS
//!   subset. A sequence carries named, ordered script / metadata / command
//!   steps with a [`ScriptBudget`]; [`plan_sequence`] reports per-step allow or
//!   deny with typed [`PlanDenyReason`]s, and [`SequenceRunner`] executes an
//!   allowed plan through the caller-supplied [`ScriptStepExecutor`] and
//!   [`CommandHandler`] boundaries. [`SandboxedScriptExecutor`] wires scripts
//!   directly into the existing parser/interpreter with the same budgets and
//!   cancellation token. All execution is deterministic and I/O-free; no real
//!   PDF document is mutated.
//! - [`ActionDocument`] — a versioned, deterministic text serialization of an
//!   [`ActionSequence`]. [`ActionDocument::to_text`] /
//!   [`ActionDocument::from_text`] round-trip losslessly, validate the wrapped
//!   sequence, and reject unsupported format versions with typed errors.
//! - [`SequenceBatchQueue`] — a FIFO batch model over [`ActionSequence`] with
//!   enqueue/dequeue, a validated per-item state machine, cooperative
//!   cancellation, and a byte-stable [`SequenceBatchReport`]. Step effects stay
//!   behind the [`SequenceStepRunner`] trait; no I/O is performed.
//! - [`OperationRegistry`] — a deterministic registry of scriptable operations
//!   with typed parameter descriptors. [`OperationRegistry::resolve`] validates
//!   `name=value` arguments and produces a typed, stable [`CallPlan`] without
//!   executing the operation.
//! - [`PollSchedule`] / [`PollInterval`] / [`BackoffPolicy`] / [`MarkerSnapshot`]
//!   — host-driven watched-folder scheduling and a caller-supplied idempotency
//!   marker persistence contract. Decisions are tick-based and deterministic;
//!   no thread, clock, or OS watcher is started.
//! - [`PluginValue`] / [`PluginOutput`] / [`ExecutionPlan`] — a typed, size-bounded
//!   plugin output contract and a deterministic, non-executing execution plan
//!   with capability and budget checks. No WASM runtime is loaded.

#![deny(missing_docs)]
#![forbid(unsafe_code)]

mod action;
mod action_document;
mod action_wizard;
mod api;
mod batch;
mod document;
mod error;
mod js;
mod json;
mod library;
mod operation_registry;
mod plugin;
mod plugin_output;
mod sequence_batch;
mod toolset;
mod watched;
mod watched_schedule;

pub use action::{Action, ActionStep, StepKind};
pub use action_document::{ACTION_DOCUMENT_VERSION, ActionDocument, ActionDocumentError};
pub use action_wizard::{
    ActionSequence, CommandExecError, CommandHandler, DEFAULT_SCRIPT_FUEL, MAX_SCRIPT_BYTES,
    MAX_SEQUENCE_STEPS, PlanDecision, PlanDenyReason, RecordingCommandHandler, RunnerPlan,
    SandboxedScriptExecutor, ScriptBudget, ScriptExecError, ScriptStepExecutor, SequencePolicy,
    SequenceReport, SequenceRunner, StepPlan, StepRunOutcome, WizardBudgetField, WizardRunError,
    WizardStep, WizardStepKind, WizardValidationError, plan_sequence, run_sequence,
};
pub use api::{
    API_VERSION, ApiEngine, ApiError, ApiHost, MAX_LINE_BYTES, MemoryHost, Request, Response,
    SUPPORTED_COMMANDS,
};
pub use batch::{
    BatchJob, BatchQueue, CancelToken, JobOutcome, JobState, MockExecutor, StepExecutor,
};
pub use document::params;
pub use document::{
    Artifact, DocumentExecutor, DocumentOperations, ExecutionStop, JobExecution, JobExecutor,
    OperationError, OperationOutcome, RealExecutor, StepResult,
};
pub use error::AutomationError;
pub use js::{
    AcrobatContext, BuiltinFn, EventData, FieldStore, HostObject, HostRegistry, JsError,
    JsFunction, Limits, NoHostObject, Pos, Script, Value, run_acrobat_script, run_script,
};
pub use json::{JsonError, JsonValue, MAX_DEPTH, MAX_NODES};
pub use library::ActionLibrary;
pub use operation_registry::{
    CallPlan, DEFAULT_OPERATIONS, OperationDescriptor, OperationRegistry, ParamDescriptor,
    ParamType, ResolveError, TypedValue,
};
pub use plugin::{
    BudgetField, Capability, CapabilityGrants, CapabilityParseError, DenyReason, InvocationBudget,
    InvocationDecision, InvocationReport, LocalPluginHost, MAX_BUDGET_MEMORY_BYTES,
    MAX_BUDGET_STEPS, MAX_BUDGET_WALL_CLOCK_MS, MAX_CAPABILITIES, MAX_ID_BYTES,
    MAX_MANIFEST_JSON_BYTES, MAX_NAME_BYTES, MAX_OPERATION_BYTES, MAX_PLUGINS, MAX_VERSION_BYTES,
    PLUGIN_API_VERSION, PluginError, PluginHost, PluginInvocation, PluginManifest, PluginRegistry,
    plan_invocation, validate_manifest_json_size,
};
pub use plugin_output::{
    ExecutionPlan, ExecutionStep, ExecutionStepKind, OUTPUT_ABSOLUTE_MAX_BYTES,
    OUTPUT_ABSOLUTE_MAX_TEXT_BYTES, OUTPUT_DEFAULT_MAX_BYTES, OUTPUT_DEFAULT_MAX_TEXT_BYTES,
    OUTPUT_DEFAULT_MAX_TOTAL_BYTES, OutputBudget, OutputType, PluginOutput, PluginOutputError,
    PluginValue, build_execution_plan,
};
pub use sequence_batch::{
    BatchReportEntry, RecordingRunner, SequenceBatchError, SequenceBatchItem, SequenceBatchQueue,
    SequenceBatchReport, SequenceBatchState, SequenceStepRunner,
};
pub use toolset::{TOOLSET_VERSION, Tool, Toolset, ToolsetError};
pub use watched::{
    FileOutcome, FileOutcomeState, FolderConverter, FolderPlan, PassthroughConverter, PollReport,
    ProcessedMarkers, WATCHED_MAX_DEPTH, WATCHED_MAX_FILE_BYTES, WATCHED_MAX_FILES, WatchedFolder,
    WatchedFolderConfig, WatchedFolderError,
};
pub use watched_schedule::{
    BackoffPolicy, MARKER_PATH_MAX_BYTES, MARKER_SNAPSHOT_MAX_BYTES, MARKER_SNAPSHOT_MAX_PATHS,
    MARKER_SNAPSHOT_VERSION, MarkerSnapshot, PollInterval, PollSchedule, ScheduleDecision,
    deserialize_markers, serialize_markers,
};

/// Return the implementation name for diagnostics.
pub fn engine_name() -> &'static str {
    "pdfxi-automation"
}
