//! Real, document-backed execution of batch steps.
//!
//! [`Action`] steps are expressed in terms of the six [`StepKind`]s. This module
//! defines the [`DocumentOperations`] trait: the small, object-safe surface that
//! a CLI or application implements to actually import, convert, watermark,
//! secure, export, and command documents. It is deliberately free of I/O so the
//! trait can be implemented later by `pdfxi-cli` and `pdfxi-app` without changing
//! this crate.
//!
//! [`DocumentExecutor`] maps every [`StepKind`] to one trait method, threads the
//! [`Artifact`] produced by a step into the next step, checks the [`CancelToken`]
//! before and after every step, and records a stable [`StepResult`] per step.
//! [`JobExecutor`] is the object-safe bridge that lets [`BatchQueue`] drive the
//! real executor through `BatchQueue::run_with` while preserving the queue's
//! ordering, failure isolation, and cancellation semantics.

use crate::action::{Action, ActionStep, StepKind};
use crate::batch::CancelToken;
use std::fmt;

/// The working value threaded from one step to the next in a job.
///
/// An artifact is the typed output of an [`OperationOutcome`]. It is either
/// in-memory document bytes, a filesystem destination path, or nothing at all.
/// The first step of a job receives an [`Artifact::Path`] built from the job's
/// input string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Artifact {
    /// In-memory document bytes.
    Bytes(Vec<u8>),
    /// A filesystem destination path produced, for example, by an export step.
    Path(String),
    /// No artifact was produced by the operation.
    Empty,
}

impl Artifact {
    /// Returns the bytes when this is [`Artifact::Bytes`], otherwise `None`.
    pub fn as_bytes(&self) -> Option<&[u8]> {
        match self {
            Self::Bytes(bytes) => Some(bytes.as_slice()),
            _ => None,
        }
    }

    /// Returns the path string when this is [`Artifact::Path`], otherwise `None`.
    pub fn as_path(&self) -> Option<&str> {
        match self {
            Self::Path(path) => Some(path.as_str()),
            _ => None,
        }
    }

    /// Returns a stable lowercase discriminator: `bytes`, `path`, or `empty`.
    ///
    /// Useful for deterministic logs and tests without exposing payloads.
    pub fn kind_str(&self) -> &'static str {
        match self {
            Self::Bytes(_) => "bytes",
            Self::Path(_) => "path",
            Self::Empty => "empty",
        }
    }
}

/// A typed result returned by one [`DocumentOperations`] method.
///
/// It always carries a short human-readable [`message`](OperationOutcome::message)
/// and one [`Artifact`] that becomes the input of the following step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationOutcome {
    artifact: Artifact,
    message: String,
}

impl OperationOutcome {
    /// Creates an outcome carrying in-memory bytes.
    pub fn bytes(bytes: Vec<u8>, message: impl Into<String>) -> Self {
        Self {
            artifact: Artifact::Bytes(bytes),
            message: message.into(),
        }
    }

    /// Creates an outcome carrying a destination path.
    pub fn path(path: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            artifact: Artifact::Path(path.into()),
            message: message.into(),
        }
    }

    /// Creates an outcome with no artifact.
    pub fn empty(message: impl Into<String>) -> Self {
        Self {
            artifact: Artifact::Empty,
            message: message.into(),
        }
    }

    /// Returns the produced artifact.
    pub fn artifact(&self) -> &Artifact {
        &self.artifact
    }

    /// Consumes the outcome and returns its artifact.
    pub fn into_artifact(self) -> Artifact {
        self.artifact
    }

    /// Returns the short result message.
    pub fn message(&self) -> &str {
        &self.message
    }
}

/// An error returned by a [`DocumentOperations`] method.
///
/// The message is carried verbatim into the failing step's outcome and job
/// summary context, so it should be short and self-describing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationError {
    message: String,
}

impl OperationError {
    /// Creates an error with the given message.
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    /// Returns the error message.
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for OperationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for OperationError {}

/// The document operations that the six [`StepKind`]s map onto.
///
/// This trait is object-safe: every method takes `&self`, uses only concrete
/// argument types, and returns a typed [`OperationOutcome`]. A CLI or application
/// can implement it with real file and document I/O; tests can supply a scripted
/// fake. Each method receives the current working [`Artifact`] so a caller that
/// invokes operations directly can thread results without going through a
/// [`BatchQueue`](crate::batch::BatchQueue).
pub trait DocumentOperations {
    /// Imports a source brought in as a path (or bytes wrapped in `input`).
    fn import(&self, input: &Artifact) -> Result<OperationOutcome, OperationError>;

    /// Converts `input` to the named target `format`.
    fn convert(&self, input: &Artifact, format: &str) -> Result<OperationOutcome, OperationError>;

    /// Applies a watermark with the given `text` to `input`.
    fn watermark(&self, input: &Artifact, text: &str) -> Result<OperationOutcome, OperationError>;

    /// Applies security settings (password and a permissions description) to `input`.
    fn apply_security(
        &self,
        input: &Artifact,
        password: &str,
        permissions: &str,
    ) -> Result<OperationOutcome, OperationError>;

    /// Writes `input` out to the destination `target` path.
    fn export(&self, input: &Artifact, target: &str) -> Result<OperationOutcome, OperationError>;

    /// Runs a named custom `command` with an ordered list of `arguments`.
    fn run_command(
        &self,
        input: &Artifact,
        name: &str,
        arguments: &[String],
    ) -> Result<OperationOutcome, OperationError>;
}

/// Documented parameter keys that [`DocumentExecutor`] reads from an [`ActionStep`].
///
/// These keys are the stable contract between an action's text format and the
/// document operations. Missing values fall back to documented defaults, except
/// `target` (export) and `command` (custom command), which are required.
pub mod params {
    /// Optional import override: a source path that replaces the job input.
    pub const SOURCE: &str = "source";
    /// Optional convert target format. Defaults to `"pdf"`.
    pub const FORMAT: &str = "format";
    /// Optional watermark text. Defaults to the empty string.
    pub const TEXT: &str = "text";
    /// Optional security password. Defaults to the empty string.
    pub const PASSWORD: &str = "password";
    /// Optional security permissions description. Defaults to the empty string.
    pub const PERMISSIONS: &str = "permissions";
    /// Required export destination path.
    pub const TARGET: &str = "target";
    /// Required custom-command name.
    pub const COMMAND: &str = "command";
    /// Optional custom-command arguments, separated by the `|` character.
    ///
    /// Each element is trimmed and empty elements are dropped. For example,
    /// `"--verbose|out/final.pdf|"` yields `["--verbose", "out/final.pdf"]`.
    pub const ARGUMENTS: &str = "arguments";
    /// Default convert target format when [`params::FORMAT`](FORMAT) is absent.
    pub const DEFAULT_FORMAT: &str = "pdf";
}

/// A stable record of one step executed by a [`DocumentExecutor`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepResult {
    step_id: String,
    kind: StepKind,
    message: String,
    artifact: Artifact,
}

impl StepResult {
    /// Creates a step result.
    pub fn new(
        step_id: impl Into<String>,
        kind: StepKind,
        message: impl Into<String>,
        artifact: Artifact,
    ) -> Self {
        Self {
            step_id: step_id.into(),
            kind,
            message: message.into(),
            artifact,
        }
    }

    /// Returns the id of the step that produced this result.
    pub fn step_id(&self) -> &str {
        &self.step_id
    }

    /// Returns the kind of the step that produced this result.
    pub fn kind(&self) -> StepKind {
        self.kind
    }

    /// Returns the short message from the step's [`OperationOutcome`].
    pub fn message(&self) -> &str {
        &self.message
    }

    /// Returns the artifact produced by the step.
    pub fn artifact(&self) -> &Artifact {
        &self.artifact
    }
}

/// Why an execution stopped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExecutionStop {
    /// Every step completed successfully.
    Completed,
    /// A step failed. The failing step id and message are recorded.
    Failed {
        /// The id of the step that failed.
        step: String,
        /// The failure message.
        message: String,
    },
    /// Cancellation was requested before, during, or after a step.
    Cancelled,
}

/// The complete result of executing one job's steps through a [`JobExecutor`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobExecution {
    completed_steps: Vec<String>,
    step_results: Vec<StepResult>,
    stop: ExecutionStop,
}

impl JobExecution {
    /// Creates a result where every step completed successfully.
    pub fn completed(completed_steps: Vec<String>, step_results: Vec<StepResult>) -> Self {
        Self {
            completed_steps,
            step_results,
            stop: ExecutionStop::Completed,
        }
    }

    /// Creates a result where a step failed.
    pub fn failed(
        completed_steps: Vec<String>,
        step_results: Vec<StepResult>,
        step: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            completed_steps,
            step_results,
            stop: ExecutionStop::Failed {
                step: step.into(),
                message: message.into(),
            },
        }
    }

    /// Creates a result where the job was cancelled.
    pub fn cancelled(completed_steps: Vec<String>, step_results: Vec<StepResult>) -> Self {
        Self {
            completed_steps,
            step_results,
            stop: ExecutionStop::Cancelled,
        }
    }

    /// Returns the ids of steps that completed, in execution order.
    pub fn completed_steps(&self) -> &[String] {
        &self.completed_steps
    }

    /// Returns the stable per-step results, in execution order.
    pub fn step_results(&self) -> &[StepResult] {
        &self.step_results
    }

    /// Returns why the execution stopped.
    pub fn stop(&self) -> &ExecutionStop {
        &self.stop
    }
}

/// Executes a whole job's steps for one input file.
///
/// This is the object-safe bridge used by [`BatchQueue::run_with`](crate::batch::BatchQueue::run_with).
/// An implementation owns artifact threading across a job's steps, checks the
/// [`CancelToken`], and returns a [`JobExecution`] describing where the job
/// stopped and what each completed step produced.
pub trait JobExecutor {
    /// Runs all steps of `action` for `input`, consulting `cancel` before and
    /// after each step, and returns the recorded execution.
    fn execute_job(&self, input: &str, action: &Action, cancel: &CancelToken) -> JobExecution;
}

/// A document-backed [`JobExecutor`] generic over a [`DocumentOperations`] implementation.
///
/// The executor maps each [`StepKind`] to exactly one trait method, threads the
/// produced [`Artifact`] into the next step, and honours cancellation before and
/// after every step. `DocumentExecutor` is dependency-free and deterministic:
/// given the same operations, action, input, and token state, it always produces
/// the same [`JobExecution`].
#[derive(Debug)]
pub struct DocumentExecutor<O: DocumentOperations> {
    operations: O,
}

impl<O: DocumentOperations> DocumentExecutor<O> {
    /// Creates an executor backed by `operations`.
    pub fn new(operations: O) -> Self {
        Self { operations }
    }

    /// Returns a reference to the underlying operations.
    pub fn operations(&self) -> &O {
        &self.operations
    }

    /// Convenience wrapper around [`JobExecutor::execute_job`] so callers can
    /// run an action without importing the trait.
    pub fn execute_action(
        &self,
        input: &str,
        action: &Action,
        cancel: &CancelToken,
    ) -> JobExecution {
        JobExecutor::execute_job(self, input, action, cancel)
    }

    /// Maps one step and the current artifact to a document operation call.
    fn dispatch(&self, step: &ActionStep, input: &Artifact) -> Result<OperationOutcome, String> {
        let failure = |error: OperationError| error.message().to_owned();
        match step.kind() {
            StepKind::Import => self.operations.import(input).map_err(failure),
            StepKind::Convert => {
                let format = step
                    .parameter(params::FORMAT)
                    .unwrap_or(params::DEFAULT_FORMAT);
                self.operations.convert(input, format).map_err(failure)
            }
            StepKind::Watermark => {
                let text = step.parameter(params::TEXT).unwrap_or("");
                self.operations.watermark(input, text).map_err(failure)
            }
            StepKind::Security => {
                let password = step.parameter(params::PASSWORD).unwrap_or("");
                let permissions = step.parameter(params::PERMISSIONS).unwrap_or("");
                self.operations
                    .apply_security(input, password, permissions)
                    .map_err(failure)
            }
            StepKind::Export => {
                let target = step.parameter(params::TARGET).ok_or_else(|| {
                    format!(
                        "export step '{}' has no '{}' parameter",
                        step.id(),
                        params::TARGET
                    )
                })?;
                self.operations.export(input, target).map_err(failure)
            }
            StepKind::CustomCommand => {
                let name = step.parameter(params::COMMAND).ok_or_else(|| {
                    format!(
                        "custom-command step '{}' has no '{}' parameter",
                        step.id(),
                        params::COMMAND
                    )
                })?;
                let arguments = step
                    .parameter(params::ARGUMENTS)
                    .map(split_arguments)
                    .unwrap_or_default();
                self.operations
                    .run_command(input, name, &arguments)
                    .map_err(failure)
            }
        }
    }
}

impl<O: DocumentOperations> JobExecutor for DocumentExecutor<O> {
    fn execute_job(&self, input: &str, action: &Action, cancel: &CancelToken) -> JobExecution {
        let mut artifact = Artifact::Path(input.to_owned());
        let mut completed: Vec<String> = Vec::new();
        let mut step_results: Vec<StepResult> = Vec::new();

        for step in action.steps() {
            if cancel.is_cancelled() {
                return JobExecution::cancelled(completed, step_results);
            }

            if step.kind() == StepKind::Import
                && let Some(source) = step.parameter(params::SOURCE)
            {
                artifact = Artifact::Path(source.to_owned());
            }

            match self.dispatch(step, &artifact) {
                Ok(outcome) => {
                    let message = outcome.message().to_owned();
                    let next = outcome.into_artifact();
                    step_results.push(StepResult::new(
                        step.id().to_owned(),
                        step.kind(),
                        message,
                        next.clone(),
                    ));
                    artifact = next;
                    completed.push(step.id().to_owned());
                }
                Err(message) => {
                    return JobExecution::failed(
                        completed,
                        step_results,
                        step.id().to_owned(),
                        message,
                    );
                }
            }

            if cancel.is_cancelled() {
                return JobExecution::cancelled(completed, step_results);
            }
        }

        if cancel.is_cancelled() {
            JobExecution::cancelled(completed, step_results)
        } else {
            JobExecution::completed(completed, step_results)
        }
    }
}

/// An alias for [`DocumentExecutor`] emphasising that it is the real,
/// document-backed executor (as opposed to the test [`MockExecutor`](crate::batch::MockExecutor)).
pub type RealExecutor<O> = DocumentExecutor<O>;

/// Splits an `arguments` parameter value into trimmed, non-empty arguments.
fn split_arguments(value: &str) -> Vec<String> {
    value
        .split('|')
        .map(str::trim)
        .filter(|argument| !argument.is_empty())
        .map(str::to_owned)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::action::ActionStep;
    use crate::batch::{BatchJob, BatchQueue, JobState};
    use std::cell::RefCell;
    use std::rc::Rc;

    /// A scripted [`DocumentOperations`] that records every call and derives
    /// outputs deterministically from its inputs.
    #[derive(Debug, Default)]
    struct ScriptedOperations {
        log: RefCell<Vec<String>>,
        /// Import produces the `BAD` sentinel when the source contains this text.
        bad_marker: Option<String>,
        /// Cancels the supplied token when the named step kind executes.
        cancel_on: Option<(String, Rc<CancelToken>)>,
    }

    impl ScriptedOperations {
        fn with_marker(marker: &str) -> Self {
            Self {
                bad_marker: Some(marker.to_owned()),
                ..Self::default()
            }
        }

        fn cancelling_on(kind: &str, token: Rc<CancelToken>) -> Self {
            Self {
                cancel_on: Some((kind.to_owned(), token)),
                ..Self::default()
            }
        }

        fn maybe_cancel(&self, kind: &str) {
            if let Some((target, token)) = &self.cancel_on
                && target == kind
            {
                token.cancel();
            }
        }

        fn record(&self, entry: String) {
            self.log.borrow_mut().push(entry);
        }

        fn entries(&self) -> Vec<String> {
            self.log.borrow().clone()
        }
    }

    fn describe(input: &Artifact) -> String {
        match input {
            Artifact::Bytes(bytes) => format!("bytes:{}", String::from_utf8_lossy(bytes)),
            Artifact::Path(path) => format!("path:{path}"),
            Artifact::Empty => "empty".to_owned(),
        }
    }

    impl DocumentOperations for ScriptedOperations {
        fn import(&self, input: &Artifact) -> Result<OperationOutcome, OperationError> {
            let source = input.as_path().unwrap_or("<bytes>");
            self.record(format!("import:{}", describe(input)));
            self.maybe_cancel("import");
            let bytes: &[u8] = if self
                .bad_marker
                .as_deref()
                .is_some_and(|marker| source.contains(marker))
            {
                b"BAD"
            } else {
                source.as_bytes()
            };
            Ok(OperationOutcome::bytes(bytes.to_vec(), "imported"))
        }

        fn convert(
            &self,
            input: &Artifact,
            format: &str,
        ) -> Result<OperationOutcome, OperationError> {
            self.record(format!("convert:{}:{format}", describe(input)));
            self.maybe_cancel("convert");
            if input.as_bytes() == Some(&b"BAD"[..]) {
                return Err(OperationError::new("convert failed for BAD input"));
            }
            let mut out = b"conv(".to_vec();
            out.extend_from_slice(input.as_bytes().unwrap_or(&[]));
            out.push(b')');
            Ok(OperationOutcome::bytes(out, "converted"))
        }

        fn watermark(
            &self,
            input: &Artifact,
            text: &str,
        ) -> Result<OperationOutcome, OperationError> {
            self.record(format!("watermark:{}:{text}", describe(input)));
            self.maybe_cancel("watermark");
            let mut out = b"wm(".to_vec();
            out.extend_from_slice(input.as_bytes().unwrap_or(&[]));
            out.extend_from_slice(b"):");
            out.extend_from_slice(text.as_bytes());
            Ok(OperationOutcome::bytes(out, "watermarked"))
        }

        fn apply_security(
            &self,
            input: &Artifact,
            password: &str,
            permissions: &str,
        ) -> Result<OperationOutcome, OperationError> {
            self.record(format!(
                "security:{}:pw={password}:perm={permissions}",
                describe(input)
            ));
            self.maybe_cancel("security");
            let mut out = b"sec(".to_vec();
            out.extend_from_slice(input.as_bytes().unwrap_or(&[]));
            out.push(b')');
            Ok(OperationOutcome::bytes(out, "secured"))
        }

        fn export(
            &self,
            input: &Artifact,
            target: &str,
        ) -> Result<OperationOutcome, OperationError> {
            self.record(format!("export:{}:{target}", describe(input)));
            self.maybe_cancel("export");
            Ok(OperationOutcome::path(target, "exported"))
        }

        fn run_command(
            &self,
            input: &Artifact,
            name: &str,
            arguments: &[String],
        ) -> Result<OperationOutcome, OperationError> {
            self.record(format!(
                "command:{}:{name}:args=[{}]",
                describe(input),
                arguments.join(", ")
            ));
            self.maybe_cancel("command");
            let mut out = b"cmd(".to_vec();
            out.extend_from_slice(input.as_bytes().unwrap_or(&[]));
            out.push(b')');
            Ok(OperationOutcome::bytes(out, "commanded"))
        }
    }

    fn step(id: &str, kind: StepKind, params: &[(&str, &str)]) -> ActionStep {
        let mut built = ActionStep::new(id, kind);
        for (key, value) in params {
            built.add_parameter(*key, *value);
        }
        built
    }

    fn action(steps: Vec<ActionStep>) -> Action {
        Action::from_steps("real", "", steps).unwrap()
    }

    #[test]
    fn document_operations_is_object_safe() {
        fn assert_object_safe(_: &dyn DocumentOperations) {}
        let ops = ScriptedOperations::default();
        assert_object_safe(&ops);
    }

    #[test]
    fn artifact_and_outcome_accessors_are_consistent() {
        let outcome = OperationOutcome::bytes(vec![1, 2, 3], "ok");
        assert_eq!(outcome.message(), "ok");
        assert_eq!(outcome.artifact().as_bytes(), Some(&[1, 2, 3][..]));
        assert_eq!(outcome.artifact().as_path(), None);
        assert_eq!(outcome.artifact().kind_str(), "bytes");
        assert_eq!(outcome.into_artifact(), Artifact::Bytes(vec![1, 2, 3]));

        let path = OperationOutcome::path("out.pdf", "wrote");
        assert_eq!(path.artifact().as_path(), Some("out.pdf"));
        assert_eq!(path.artifact().kind_str(), "path");
        assert_eq!(
            OperationOutcome::empty("none").into_artifact(),
            Artifact::Empty
        );
        assert_eq!(Artifact::Empty.kind_str(), "empty");
        assert_eq!(OperationError::new("bad").to_string(), "bad");
    }

    #[test]
    fn step_chaining_passes_outputs_forward() {
        let ops = ScriptedOperations::default();
        let executor = DocumentExecutor::new(ops);
        let action = action(vec![
            step("load", StepKind::Import, &[]),
            step("convert", StepKind::Convert, &[]),
            step("stamp", StepKind::Watermark, &[("text", "DRAFT")]),
            step("save", StepKind::Export, &[("target", "out/result.pdf")]),
        ]);
        let cancel = CancelToken::new();
        let execution = executor.execute_action("in.pdf", &action, &cancel);

        assert_eq!(execution.stop(), &ExecutionStop::Completed);
        assert_eq!(
            execution.completed_steps(),
            ["load", "convert", "stamp", "save"]
        );
        assert_eq!(execution.step_results().len(), 4);

        let ops = executor.operations();
        let log = ops.entries();
        assert_eq!(
            log,
            vec![
                "import:path:in.pdf",
                "convert:bytes:in.pdf:pdf",
                "watermark:bytes:conv(in.pdf):DRAFT",
                "export:bytes:wm(conv(in.pdf)):DRAFT:out/result.pdf",
            ]
        );

        let export_result = &execution.step_results()[3];
        assert_eq!(export_result.step_id(), "save");
        assert_eq!(export_result.kind(), StepKind::Export);
        assert_eq!(export_result.message(), "exported");
        assert_eq!(export_result.artifact().as_path(), Some("out/result.pdf"));
    }

    #[test]
    fn failing_convert_isolates_to_one_file() {
        let ops = ScriptedOperations::with_marker("bad");
        let executor = DocumentExecutor::new(ops);
        let mut queue = BatchQueue::new();
        let steps = || {
            action(vec![
                step("load", StepKind::Import, &[]),
                step("convert", StepKind::Convert, &[]),
                step("save", StepKind::Export, &[("target", "out.pdf")]),
            ])
        };
        queue.enqueue_job("good", "good.pdf", steps()).unwrap();
        queue.enqueue_job("bad", "bad.pdf", steps()).unwrap();
        queue.enqueue_job("last", "last.pdf", steps()).unwrap();
        queue.run_with(&executor);

        assert_eq!(queue.job("good").unwrap().state(), JobState::Succeeded);
        let failed = queue.job("bad").unwrap();
        assert_eq!(failed.state(), JobState::Failed);
        let outcome = failed.outcome().unwrap();
        assert_eq!(outcome.completed_steps(), ["load"]);
        assert_eq!(outcome.failed_step(), Some("convert"));
        assert_eq!(outcome.message(), Some("convert failed for BAD input"));
        assert_eq!(outcome.step_results().len(), 1);
        assert_eq!(queue.job("last").unwrap().state(), JobState::Succeeded);
        assert_eq!(
            queue.summary(),
            "good\tgood.pdf\tsucceeded steps=3\n\
             bad\tbad.pdf\tfailed steps=1 at=convert\n\
             last\tlast.pdf\tsucceeded steps=3\n"
        );
    }

    #[test]
    fn cancellation_mid_chain_marks_job_cancelled() {
        let job = BatchJob::new(
            "job",
            "in.pdf",
            action(vec![
                step("load", StepKind::Import, &[]),
                step("convert", StepKind::Convert, &[]),
                step("save", StepKind::Export, &[("target", "out.pdf")]),
            ]),
        );
        let handle = job.cancellation_handle();
        let ops = ScriptedOperations::cancelling_on("convert", handle);
        let executor = DocumentExecutor::new(ops);
        let mut queue = BatchQueue::new();
        queue.enqueue(job).unwrap();
        queue.run_with(&executor);

        let job = queue.job("job").unwrap();
        assert_eq!(job.state(), JobState::Cancelled);
        assert!(job.cancellation().is_cancelled());
        let outcome = job.outcome().unwrap();
        assert_eq!(outcome.completed_steps(), ["load", "convert"]);
        assert_eq!(outcome.step_results().len(), 2);
        assert_eq!(outcome.summary(), "cancelled steps=2");
    }

    #[test]
    fn cancellation_before_run_skips_the_job() {
        let ops = ScriptedOperations::default();
        let executor = DocumentExecutor::new(ops);
        let mut queue = BatchQueue::new();
        queue
            .enqueue_job(
                "job",
                "in.pdf",
                action(vec![step("load", StepKind::Import, &[])]),
            )
            .unwrap();
        queue.cancel_job("job").unwrap();
        queue.run_with(&executor);
        let job = queue.job("job").unwrap();
        assert_eq!(job.state(), JobState::Cancelled);
        assert_eq!(job.outcome().unwrap().completed_steps(), &[] as &[String]);
        assert!(executor.operations().entries().is_empty());
    }

    #[test]
    fn security_step_receives_the_right_parameters() {
        let ops = ScriptedOperations::default();
        let executor = DocumentExecutor::new(ops);
        let action = action(vec![
            step("load", StepKind::Import, &[]),
            step(
                "secure",
                StepKind::Security,
                &[("password", "s3cret"), ("permissions", "print,copy")],
            ),
        ]);
        let cancel = CancelToken::new();
        let execution = executor.execute_action("in.pdf", &action, &cancel);

        assert_eq!(execution.stop(), &ExecutionStop::Completed);
        assert!(executor.operations().entries().contains(&String::from(
            "security:bytes:in.pdf:pw=s3cret:perm=print,copy"
        )));
    }

    #[test]
    fn export_step_writes_the_expected_path() {
        let ops = ScriptedOperations::default();
        let executor = DocumentExecutor::new(ops);
        let action = action(vec![
            step("load", StepKind::Import, &[]),
            step("save", StepKind::Export, &[("target", "exports/final.pdf")]),
        ]);
        let cancel = CancelToken::new();
        let execution = executor.execute_action("in.pdf", &action, &cancel);

        assert!(
            executor
                .operations()
                .entries()
                .contains(&String::from("export:bytes:in.pdf:exports/final.pdf"))
        );
        let last = execution.step_results().last().unwrap();
        assert_eq!(last.artifact().as_path(), Some("exports/final.pdf"));
    }

    #[test]
    fn custom_command_receives_documented_arguments() {
        let ops = ScriptedOperations::default();
        let executor = DocumentExecutor::new(ops);
        let action = action(vec![
            step("load", StepKind::Import, &[]),
            step(
                "cmd",
                StepKind::CustomCommand,
                &[
                    ("command", "stitch"),
                    ("arguments", " --verbose | out/final.pdf |  "),
                ],
            ),
        ]);
        let cancel = CancelToken::new();
        let execution = executor.execute_action("in.pdf", &action, &cancel);

        assert_eq!(execution.stop(), &ExecutionStop::Completed);
        assert!(executor.operations().entries().contains(&String::from(
            "command:bytes:in.pdf:stitch:args=[--verbose, out/final.pdf]"
        )));
    }

    #[test]
    fn missing_required_parameters_fail_only_that_step() {
        let ops = ScriptedOperations::default();
        let executor = DocumentExecutor::new(ops);
        let cancel = CancelToken::new();

        let bad_export = action(vec![step("save", StepKind::Export, &[])]);
        let execution = executor.execute_action("in.pdf", &bad_export, &cancel);
        assert_eq!(
            execution.stop(),
            &ExecutionStop::Failed {
                step: "save".to_owned(),
                message: "export step 'save' has no 'target' parameter".to_owned(),
            }
        );

        let bad_command = action(vec![step("cmd", StepKind::CustomCommand, &[])]);
        let execution = executor.execute_action("in.pdf", &bad_command, &cancel);
        assert_eq!(
            execution.stop(),
            &ExecutionStop::Failed {
                step: "cmd".to_owned(),
                message: "custom-command step 'cmd' has no 'command' parameter".to_owned(),
            }
        );
    }

    #[test]
    fn import_source_parameter_overrides_job_input() {
        let ops = ScriptedOperations::default();
        let executor = DocumentExecutor::new(ops);
        let action = action(vec![step(
            "load",
            StepKind::Import,
            &[("source", "override.pdf")],
        )]);
        let cancel = CancelToken::new();
        executor.execute_action("job-input.pdf", &action, &cancel);
        assert!(
            executor
                .operations()
                .entries()
                .contains(&String::from("import:path:override.pdf"))
        );
    }

    #[test]
    fn outcomes_and_summaries_are_deterministic() {
        let run = || {
            let ops = ScriptedOperations::default();
            let executor = DocumentExecutor::new(ops);
            let mut queue = BatchQueue::new();
            queue
                .enqueue_job(
                    "job",
                    "in.pdf",
                    action(vec![
                        step("load", StepKind::Import, &[]),
                        step("save", StepKind::Export, &[("target", "out.pdf")]),
                    ]),
                )
                .unwrap();
            queue.run_with(&executor);
            (
                queue.summary(),
                queue.job("job").unwrap().outcome().unwrap().clone(),
            )
        };
        let (first_summary, first_outcome) = run();
        let (second_summary, second_outcome) = run();
        assert_eq!(first_summary, second_summary);
        assert_eq!(first_outcome, second_outcome);
        assert_eq!(first_summary, "job\tin.pdf\tsucceeded steps=2\n");
    }

    #[test]
    fn real_executor_alias_is_usable() {
        let ops = ScriptedOperations::default();
        let executor: RealExecutor<ScriptedOperations> = RealExecutor::new(ops);
        let action = action(vec![step("load", StepKind::Import, &[])]);
        let execution = executor.execute_action("in.pdf", &action, &CancelToken::new());
        assert_eq!(execution.completed_steps(), ["load"]);
    }
}
