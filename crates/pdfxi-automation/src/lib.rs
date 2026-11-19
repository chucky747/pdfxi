//! Action Wizard, batch processing, droplets, JavaScript sandbox, and plugin host.
//!
//! This crate currently provides the dependency-free in-memory model for
//! actions and batch jobs. The core abstractions are:
//!
//! - [`Action`] / [`ActionStep`] / [`StepKind`] — a named, ordered, validatable
//!   sequence of operations with stable step-kind identifiers and lossless text
//!   import/export.
//! - [`BatchJob`] / [`BatchQueue`] — a deterministic, single-threaded runner
//!   that executes jobs through a [`StepExecutor`], isolates failures, supports
//!   cooperative cancellation, and records a [`JobOutcome`] per file.
//! - [`DocumentOperations`] / [`DocumentExecutor`] — the object-safe document
//!   interface and its real executor. [`DocumentExecutor`] maps each step kind to
//!   one operation, threads the produced [`Artifact`] between steps, honours
//!   cancellation, and records stable [`StepResult`]s. [`BatchQueue::run_with`]
//!   drives it through the object-safe [`JobExecutor`] bridge.
//! - [`ActionLibrary`] — a multi-action container that parses several named
//!   actions from one tab-delimited text file, the format consumed by the CLI's
//!   `action list`, `action show`, `action run`, and `droplet` commands.
//! - [`AutomationError`] — actionable, self-describing errors for every
//!   rejection path.
//! - [`Script`] / [`Limits`] / [`run_script`] — a dependency-free, deterministic
//!   JavaScript subset for document actions, with explicit CPU-step, call,
//!   iteration, size, wall-clock, and cancellation budgets. The [`HostObject`]
//!   trait is an audited boundary for exposing future document operations; no
//!   real PDF mutation is wired up in this milestone.
//! - [`AcrobatContext`] / [`FieldStore`] / [`EventData`] / [`run_acrobat_script`]
//!   — an explicitly injected, in-memory Acrobat compatibility layer providing
//!   `this.getField` read/write, `event`, the `AFNumber_*`, `AFPercent_*`, and
//!   `AFDate_*` helpers, and a documented `util.printf` subset for field format,
//!   validate, and calculate scripts. No I/O, clock, randomness, or ambient
//!   globals; no real PDF mutation.
//!
//! All behavior is deterministic and allocates only as needed. The crate has no
//! external dependencies, forbids `unsafe`, and keeps panics out of non-test
//! code. Future milestones will add watched folders and the plugin host on top
//! of these contracts.

#![deny(missing_docs)]
#![forbid(unsafe_code)]

mod action;
mod batch;
mod document;
mod error;
mod js;
mod library;

pub use action::{Action, ActionStep, StepKind};
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
pub use library::ActionLibrary;

/// Return the implementation name for diagnostics.
pub fn engine_name() -> &'static str {
    "pdfxi-automation"
}
