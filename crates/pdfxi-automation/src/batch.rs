//! Deterministic batch execution with cancellation and per-file outcomes.
//!
//! A [`BatchQueue`] owns an ordered list of [`BatchJob`]s. Running the queue is
//! synchronous and deterministic: jobs execute in insertion order, steps
//! execute in action order, and each job records an [`JobOutcome`] regardless of
//! whether its siblings succeeded. Step work goes through the [`StepExecutor`]
//! trait; [`MockExecutor`] is a dependency-free default test double.
//!
//! Cancellation uses [`CancelToken`], which relies on [`std::cell::Cell`] and is
//! therefore intended for single-threaded use. A token can be triggered before
//! a run through [`BatchQueue::cancel_job`] or from inside an executor (see
//! [`MockExecutor::cancel_at_step`]); the runner checks it before every step.

use crate::action::{Action, ActionStep};
use crate::error::AutomationError;
use std::cell::Cell;
use std::fmt;
use std::rc::Rc;

/// The lifecycle state of a [`BatchJob`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum JobState {
    /// Waiting to be run.
    Queued,
    /// Currently executing steps.
    Running,
    /// Every step completed successfully.
    Succeeded,
    /// A step failed; the job stopped at that step.
    Failed,
    /// The job was cancelled; it stopped before completing all steps.
    Cancelled,
}

impl JobState {
    /// Returns the stable lowercase name of this state.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }
}

impl fmt::Display for JobState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The recorded result of a finished [`BatchJob`].
///
/// An outcome is terminal: its [`state`](JobOutcome::state) is one of
/// [`JobState::Succeeded`], [`JobState::Failed`], or [`JobState::Cancelled`].
/// It records the step ids that completed in order, plus the failing step and
/// message when applicable. [`JobOutcome::summary`] produces a stable,
/// one-line rendering suitable for logs and golden tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobOutcome {
    state: JobState,
    completed_steps: Vec<String>,
    failed_step: Option<String>,
    message: Option<String>,
}

impl JobOutcome {
    /// Creates a success outcome with the steps that completed.
    pub fn succeeded(completed_steps: Vec<String>) -> Self {
        Self {
            state: JobState::Succeeded,
            completed_steps,
            failed_step: None,
            message: None,
        }
    }

    /// Creates a failure outcome, naming the step that failed and why.
    pub fn failed(
        completed_steps: Vec<String>,
        failed_step: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            state: JobState::Failed,
            completed_steps,
            failed_step: Some(failed_step.into()),
            message: Some(message.into()),
        }
    }

    /// Creates a cancellation outcome with the steps completed before stopping.
    pub fn cancelled(completed_steps: Vec<String>) -> Self {
        Self {
            state: JobState::Cancelled,
            completed_steps,
            failed_step: None,
            message: None,
        }
    }

    /// Returns the terminal state recorded by this outcome.
    pub fn state(&self) -> JobState {
        self.state
    }

    /// Returns the ids of steps that completed, in execution order.
    pub fn completed_steps(&self) -> &[String] {
        &self.completed_steps
    }

    /// Returns the id of the step that failed, if this is a failure outcome.
    pub fn failed_step(&self) -> Option<&str> {
        self.failed_step.as_deref()
    }

    /// Returns the failure message, if this is a failure outcome.
    pub fn message(&self) -> Option<&str> {
        self.message.as_deref()
    }

    /// Returns a stable one-line summary of the outcome.
    ///
    /// The exact formats are:
    ///
    /// - `succeeded steps=<n>`
    /// - `failed steps=<n> at=<step-id>`
    /// - `cancelled steps=<n>`
    ///
    /// Free-form failure messages are intentionally excluded so summaries stay
    /// stable; use [`JobOutcome::message`] to read them.
    pub fn summary(&self) -> String {
        let count = self.completed_steps.len();
        match self.state {
            JobState::Succeeded => format!("succeeded steps={count}"),
            JobState::Failed => match self.failed_step.as_deref() {
                Some(step) => format!("failed steps={count} at={step}"),
                None => format!("failed steps={count}"),
            },
            JobState::Cancelled => format!("cancelled steps={count}"),
            other => format!("{} steps={count}", other.as_str()),
        }
    }
}

/// A cancellation signal shared between a job and its executor.
///
/// The flag is set with [`CancelToken::cancel`] and observed with
/// [`CancelToken::is_cancelled`]. It uses interior mutability so an executor
/// holding a reference can cancel the job it is running.
#[derive(Debug)]
pub struct CancelToken {
    cancelled: Cell<bool>,
}

impl CancelToken {
    /// Creates a fresh, uncancelled token.
    pub fn new() -> Self {
        Self {
            cancelled: Cell::new(false),
        }
    }

    /// Signals cancellation. This is idempotent.
    pub fn cancel(&self) {
        self.cancelled.set(true);
    }

    /// Returns `true` if cancellation has been signalled.
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.get()
    }
}

impl Default for CancelToken {
    fn default() -> Self {
        Self::new()
    }
}

/// Executes a single [`ActionStep`] for a single input file.
///
/// Implementations return `Ok(())` when the step succeeds and an `Err` with a
/// human-readable message when it fails. The [`CancelToken`] lets an executor
/// stop a job cooperatively from inside a step.
pub trait StepExecutor: fmt::Debug {
    /// Runs `step` for `file`, optionally consulting `cancel`.
    fn execute(&self, file: &str, step: &ActionStep, cancel: &CancelToken) -> Result<(), String>;
}

/// A deterministic, dependency-free [`StepExecutor`] for tests and defaults.
///
/// By every step succeeds. Builders can mark specific input files or step ids
/// as failing, or request cancellation when a particular step executes. Every
/// decision is a deterministic string comparison with no I/O or randomness.
#[derive(Debug, Default)]
pub struct MockExecutor {
    failing_files: Vec<String>,
    failing_steps: Vec<String>,
    cancel_at_step: Option<String>,
}

impl MockExecutor {
    /// Creates an executor where every step succeeds.
    pub fn new() -> Self {
        Self::default()
    }

    /// Marks an input file as failing every step.
    pub fn fail_file(mut self, file: impl Into<String>) -> Self {
        self.failing_files.push(file.into());
        self
    }

    /// Marks a step id as failing for every file.
    pub fn fail_step(mut self, step_id: impl Into<String>) -> Self {
        self.failing_steps.push(step_id.into());
        self
    }

    /// Cancels the job just after the named step executes successfully.
    pub fn cancel_at_step(mut self, step_id: impl Into<String>) -> Self {
        self.cancel_at_step = Some(step_id.into());
        self
    }
}

impl StepExecutor for MockExecutor {
    fn execute(&self, file: &str, step: &ActionStep, cancel: &CancelToken) -> Result<(), String> {
        if self.failing_files.iter().any(|name| name == file) {
            return Err(format!("mock failure: file '{file}' is marked failing"));
        }
        if self.failing_steps.iter().any(|id| id == step.id()) {
            return Err(format!(
                "mock failure: step '{}' is marked failing",
                step.id()
            ));
        }
        if self.cancel_at_step.as_deref() == Some(step.id()) {
            cancel.cancel();
        }
        Ok(())
    }
}

/// One file's unit of work in a [`BatchQueue`].
///
/// A job owns the [`Action`] it will run, so the queue is self-contained and its
/// execution does not depend on external state. Each job carries its own
/// [`CancelToken`], available through [`BatchJob::cancellation`].
#[derive(Debug, Clone)]
pub struct BatchJob {
    id: String,
    input: String,
    action: Action,
    state: JobState,
    outcome: Option<JobOutcome>,
    cancel_token: Rc<CancelToken>,
}

impl BatchJob {
    /// Creates a queued job. The action is validated when the queue runs it.
    pub fn new(id: impl Into<String>, input: impl Into<String>, action: Action) -> Self {
        Self {
            id: id.into(),
            input: input.into(),
            action,
            state: JobState::Queued,
            outcome: None,
            cancel_token: Rc::new(CancelToken::new()),
        }
    }

    /// Creates a queued job, validating its action immediately.
    pub fn try_new(
        id: impl Into<String>,
        input: impl Into<String>,
        action: Action,
    ) -> Result<Self, AutomationError> {
        action.validate()?;
        Ok(Self::new(id, input, action))
    }

    /// Returns the job id.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Returns the input file name handled by this job.
    pub fn input(&self) -> &str {
        &self.input
    }

    /// Returns the action this job will run.
    pub fn action(&self) -> &Action {
        &self.action
    }

    /// Returns the current state.
    pub fn state(&self) -> JobState {
        self.state
    }

    /// Returns the recorded outcome, if the job has finished.
    pub fn outcome(&self) -> Option<&JobOutcome> {
        self.outcome.as_ref()
    }

    /// Returns this job's cancellation token.
    pub fn cancellation(&self) -> &CancelToken {
        &self.cancel_token
    }

    /// Signals cancellation for this job.
    pub fn cancel(&self) {
        self.cancel_token.cancel();
    }
}

/// A deterministic, single-threaded queue of [`BatchJob`]s.
///
/// [`BatchQueue::run_all`] visits jobs in insertion order and never stops early
/// because one job failed: failures are isolated to their own outcome. A job
/// whose cancellation token is set before or during its run is recorded as
/// [`JobState::Cancelled`] with the steps completed up to that point.
#[derive(Debug)]
pub struct BatchQueue {
    jobs: Vec<BatchJob>,
    executor: Box<dyn StepExecutor>,
}

impl BatchQueue {
    /// Creates an empty queue using [`MockExecutor`] as the step executor.
    pub fn new() -> Self {
        Self::with_executor(Box::new(MockExecutor::new()))
    }

    /// Creates an empty queue using the supplied step executor.
    pub fn with_executor(executor: Box<dyn StepExecutor>) -> Self {
        Self {
            jobs: Vec::new(),
            executor,
        }
    }

    /// Appends a job, rejecting a duplicate job id.
    pub fn enqueue(&mut self, job: BatchJob) -> Result<(), AutomationError> {
        if self.jobs.iter().any(|existing| existing.id == job.id) {
            return Err(AutomationError::DuplicateJobId { id: job.id.clone() });
        }
        self.jobs.push(job);
        Ok(())
    }

    /// Builds and appends a job from an id, input, and action.
    ///
    /// The action is validated before the job is queued.
    pub fn enqueue_job(
        &mut self,
        id: impl Into<String>,
        input: impl Into<String>,
        action: Action,
    ) -> Result<(), AutomationError> {
        self.enqueue(BatchJob::try_new(id, input, action)?)
    }

    /// Returns all jobs in stable insertion order.
    pub fn jobs(&self) -> impl ExactSizeIterator<Item = &BatchJob> {
        self.jobs.iter()
    }

    /// Returns the job with the given id, if present.
    pub fn job(&self, id: &str) -> Option<&BatchJob> {
        self.jobs.iter().find(|job| job.id == id)
    }

    /// Returns the number of queued jobs.
    pub fn len(&self) -> usize {
        self.jobs.len()
    }

    /// Returns `true` when the queue has no jobs.
    pub fn is_empty(&self) -> bool {
        self.jobs.is_empty()
    }

    /// Cancels a job by id.
    ///
    /// A job still in [`JobState::Queued`] is immediately recorded as
    /// [`JobState::Cancelled`] with no completed steps. A running job is stopped
    /// by its token at the next step check. Returns
    /// [`AutomationError::JobNotFound`] for an unknown id.
    pub fn cancel_job(&mut self, id: &str) -> Result<(), AutomationError> {
        let index = self
            .jobs
            .iter()
            .position(|job| job.id == id)
            .ok_or_else(|| AutomationError::JobNotFound { id: id.to_owned() })?;
        self.jobs[index].cancel();
        if self.jobs[index].state() == JobState::Queued {
            self.jobs[index].state = JobState::Cancelled;
            self.jobs[index].outcome = Some(JobOutcome::cancelled(Vec::new()));
        }
        Ok(())
    }

    /// Cancels every job that has not already finished.
    pub fn cancel_all(&mut self) {
        for job in &mut self.jobs {
            job.cancel();
            if job.state == JobState::Queued {
                job.state = JobState::Cancelled;
                job.outcome = Some(JobOutcome::cancelled(Vec::new()));
            }
        }
    }

    /// Runs every unfinished job in insertion order.
    ///
    /// This method is deterministic: with a given set of jobs and executor it
    /// always produces the same states, outcomes, and [`BatchQueue::summary`].
    /// A job that fails or is cancelled does not affect its siblings.
    pub fn run_all(&mut self) {
        for index in 0..self.jobs.len() {
            self.run_job(index);
        }
    }

    fn run_job(&mut self, index: usize) {
        if matches!(
            self.jobs[index].state,
            JobState::Succeeded | JobState::Failed | JobState::Cancelled
        ) {
            return;
        }

        if let Err(error) = self.jobs[index].action.validate() {
            let outcome = JobOutcome::failed(Vec::new(), "<action>", error.to_string());
            self.finish(index, JobState::Failed, outcome);
            return;
        }
        if self.jobs[index].cancel_token.is_cancelled() {
            self.finish(
                index,
                JobState::Cancelled,
                JobOutcome::cancelled(Vec::new()),
            );
            return;
        }

        let input = self.jobs[index].input.clone();
        let steps: Vec<ActionStep> = self.jobs[index].action.steps().cloned().collect();
        let token = Rc::clone(&self.jobs[index].cancel_token);
        self.jobs[index].state = JobState::Running;

        let mut completed: Vec<String> = Vec::new();
        let mut failure: Option<(String, String)> = None;
        let mut cancelled = false;

        for step in &steps {
            if token.is_cancelled() {
                cancelled = true;
                break;
            }
            match self.executor.execute(&input, step, &token) {
                Ok(()) => completed.push(step.id().to_owned()),
                Err(message) => {
                    failure = Some((step.id().to_owned(), message));
                    break;
                }
            }
            if token.is_cancelled() {
                cancelled = true;
                break;
            }
        }
        if token.is_cancelled() {
            cancelled = true;
        }

        let (state, outcome) = if cancelled {
            (JobState::Cancelled, JobOutcome::cancelled(completed))
        } else if let Some((step, message)) = failure {
            (
                JobState::Failed,
                JobOutcome::failed(completed, step, message),
            )
        } else {
            (JobState::Succeeded, JobOutcome::succeeded(completed))
        };
        self.finish(index, state, outcome);
    }

    fn finish(&mut self, index: usize, state: JobState, outcome: JobOutcome) {
        self.jobs[index].state = state;
        self.jobs[index].outcome = Some(outcome);
    }

    /// Returns a stable, one-line-per-job summary in insertion order.
    ///
    /// Each line is `<job-id>\t<input>\t<outcome-summary>`. Jobs that have not
    /// run yet use `<state> steps=0` as the outcome field. The format is fixed
    /// so summaries can be used as golden-test snapshots.
    pub fn summary(&self) -> String {
        let mut out = String::new();
        for job in &self.jobs {
            let outcome = match job.outcome() {
                Some(outcome) => outcome.summary(),
                None => format!("{} steps=0", job.state().as_str()),
            };
            out.push_str(job.id());
            out.push('\t');
            out.push_str(job.input());
            out.push('\t');
            out.push_str(&outcome);
            out.push('\n');
        }
        out
    }
}

impl Default for BatchQueue {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::action::StepKind;

    fn action(steps: &[(&str, StepKind)]) -> Action {
        let built: Vec<ActionStep> = steps
            .iter()
            .map(|(id, kind)| ActionStep::new(*id, *kind))
            .collect();
        Action::from_steps("job-action", "", built).unwrap()
    }

    fn queue_with(files: &[&str], executor: MockExecutor) -> BatchQueue {
        let mut queue = BatchQueue::with_executor(Box::new(executor));
        for (index, file) in files.iter().enumerate() {
            queue
                .enqueue_job(
                    format!("job-{index}"),
                    *file,
                    action(&[
                        ("import", StepKind::Import),
                        ("watermark", StepKind::Watermark),
                        ("export", StepKind::Export),
                    ]),
                )
                .unwrap();
        }
        queue
    }

    #[test]
    fn jobs_run_in_insertion_order_with_per_file_outcomes() {
        let mut queue = queue_with(&["a.pdf", "b.pdf", "c.pdf"], MockExecutor::new());
        queue.run_all();

        let summary = queue.summary();
        assert_eq!(
            summary,
            "job-0\ta.pdf\tsucceeded steps=3\n\
             job-1\tb.pdf\tsucceeded steps=3\n\
             job-2\tc.pdf\tsucceeded steps=3\n"
        );
        for job in queue.jobs() {
            assert_eq!(job.state(), JobState::Succeeded);
            let outcome = job.outcome().unwrap();
            assert_eq!(outcome.completed_steps(), ["import", "watermark", "export"]);
        }
    }

    #[test]
    fn failure_is_isolated_to_one_job() {
        let mut queue = queue_with(
            &["a.pdf", "bad.pdf", "c.pdf"],
            MockExecutor::new().fail_file("bad.pdf"),
        );
        queue.run_all();

        assert_eq!(queue.job("job-0").unwrap().state(), JobState::Succeeded);
        let failed = queue.job("job-1").unwrap();
        assert_eq!(failed.state(), JobState::Failed);
        let outcome = failed.outcome().unwrap();
        assert_eq!(outcome.state(), JobState::Failed);
        assert_eq!(outcome.completed_steps(), &[] as &[String]);
        assert_eq!(outcome.failed_step(), Some("import"));
        assert!(outcome.message().unwrap().contains("bad.pdf"));
        assert_eq!(outcome.summary(), "failed steps=0 at=import");
        assert_eq!(queue.job("job-2").unwrap().state(), JobState::Succeeded);
    }

    #[test]
    fn failing_step_records_steps_completed_before_it() {
        let mut queue = queue_with(&["a.pdf"], MockExecutor::new().fail_step("watermark"));
        queue.run_all();

        let outcome = queue.job("job-0").unwrap().outcome().unwrap();
        assert_eq!(outcome.completed_steps(), ["import"]);
        assert_eq!(outcome.failed_step(), Some("watermark"));
        assert_eq!(outcome.summary(), "failed steps=1 at=watermark");
    }

    #[test]
    fn cancelling_before_run_skips_the_job() {
        let mut queue = queue_with(&["a.pdf", "b.pdf"], MockExecutor::new());
        queue.cancel_job("job-0").unwrap();
        queue.run_all();

        let cancelled = queue.job("job-0").unwrap();
        assert_eq!(cancelled.state(), JobState::Cancelled);
        assert_eq!(cancelled.outcome().unwrap().summary(), "cancelled steps=0");
        assert!(cancelled.cancellation().is_cancelled());
        assert_eq!(queue.job("job-1").unwrap().state(), JobState::Succeeded);
    }

    #[test]
    fn cancelling_mid_run_records_completed_steps() {
        let mut queue = queue_with(&["a.pdf"], MockExecutor::new().cancel_at_step("watermark"));
        queue.run_all();

        let job = queue.job("job-0").unwrap();
        assert_eq!(job.state(), JobState::Cancelled);
        let outcome = job.outcome().unwrap();
        // "import" finished; "watermark" executed successfully and requested
        // cancellation, so it counts as completed; "export" was never attempted.
        assert_eq!(outcome.completed_steps(), ["import", "watermark"]);
        assert_eq!(outcome.summary(), "cancelled steps=2");
    }

    #[test]
    fn cancel_all_marks_every_pending_job_cancelled() {
        let mut queue = queue_with(&["a.pdf", "b.pdf"], MockExecutor::new());
        queue.cancel_all();
        queue.run_all();
        assert!(queue.jobs().all(|job| job.state() == JobState::Cancelled));
        assert_eq!(
            queue.summary(),
            "job-0\ta.pdf\tcancelled steps=0\njob-1\tb.pdf\tcancelled steps=0\n"
        );
    }

    #[test]
    fn cancel_unknown_job_reports_job_not_found() {
        let mut queue = BatchQueue::new();
        assert_eq!(
            queue.cancel_job("missing"),
            Err(AutomationError::JobNotFound {
                id: "missing".to_owned()
            })
        );
    }

    #[test]
    fn duplicate_job_ids_are_rejected() {
        let mut queue = BatchQueue::new();
        queue
            .enqueue_job("same", "a.pdf", action(&[("import", StepKind::Import)]))
            .unwrap();
        let result = queue.enqueue_job("same", "b.pdf", action(&[("import", StepKind::Import)]));
        assert_eq!(
            result,
            Err(AutomationError::DuplicateJobId {
                id: "same".to_owned()
            })
        );
        assert_eq!(queue.len(), 1);
    }

    #[test]
    fn invalid_action_fails_only_its_job() {
        let mut queue = BatchQueue::new();
        // enqueue_job validates eagerly, so use the lower-level enqueue path to
        // exercise the runner's defensive validation of an invalid action.
        queue
            .enqueue(BatchJob::new("bad", "a.pdf", Action::new("bad", "")))
            .unwrap();
        queue
            .enqueue_job("good", "b.pdf", action(&[("import", StepKind::Import)]))
            .unwrap();
        queue.run_all();

        assert_eq!(queue.job("bad").unwrap().state(), JobState::Failed);
        assert_eq!(
            queue.job("bad").unwrap().outcome().unwrap().failed_step(),
            Some("<action>")
        );
        assert_eq!(queue.job("good").unwrap().state(), JobState::Succeeded);
    }

    #[test]
    fn outcomes_are_stable_across_repeated_runs() {
        let first = {
            let mut queue = queue_with(&["a.pdf"], MockExecutor::new());
            queue.run_all();
            queue.summary()
        };
        let second = {
            let mut queue = queue_with(&["a.pdf"], MockExecutor::new());
            queue.run_all();
            queue.summary()
        };
        assert_eq!(first, second);
        assert_eq!(first, "job-0\ta.pdf\tsucceeded steps=3\n");
    }

    #[test]
    fn finished_jobs_are_not_rerun() {
        let mut queue = queue_with(&["a.pdf"], MockExecutor::new());
        queue.run_all();
        queue.run_all();
        assert_eq!(
            queue.summary(),
            "job-0\ta.pdf\tsucceeded steps=3\n",
            "a second run must not append to completed steps"
        );
    }

    #[test]
    fn job_state_strings_are_stable() {
        assert_eq!(JobState::Queued.as_str(), "queued");
        assert_eq!(JobState::Running.as_str(), "running");
        assert_eq!(JobState::Succeeded.as_str(), "succeeded");
        assert_eq!(JobState::Failed.as_str(), "failed");
        assert_eq!(JobState::Cancelled.as_str(), "cancelled");
    }
}
