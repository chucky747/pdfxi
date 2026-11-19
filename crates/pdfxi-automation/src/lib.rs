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
//! - [`AutomationError`] — actionable, self-describing errors for every
//!   rejection path.
//!
//! All behavior is deterministic and allocates only as needed. The crate has no
//! external dependencies, forbids `unsafe`, and keeps panics out of non-test
//! code. Future milestones will add droplets, watched folders, the JavaScript
//! sandbox, and the plugin host on top of these contracts.

#![deny(missing_docs)]
#![forbid(unsafe_code)]

mod action;
mod batch;
mod document;
mod error;

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

/// Return the implementation name for diagnostics.
pub fn engine_name() -> &'static str {
    "pdfxi-automation"
}
