//! Deterministic batch queue over Action Wizard [`ActionSequence`]s.
//!
//! Where [`crate::BatchQueue`] drives the older [`crate::Action`] model, this
//! module provides a queue whose unit of work is an [`ActionSequence`]. It is a
//! pure, single-threaded model: ordering, state transitions, cancellation, and
//! reporting are all deterministic and perform no I/O. Step effects are supplied
//! through the [`SequenceStepRunner`] trait; [`RecordingRunner`] is a
//! dependency-free default that records no side effects beyond step ids.

use crate::CancelToken;
use crate::action_wizard::{ActionSequence, WizardStep};
use std::fmt;
use std::rc::Rc;

/// Lifecycle state of a [`SequenceBatchItem`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SequenceBatchState {
    /// Waiting to run.
    Queued,
    /// Currently executing steps.
    Running,
    /// Every step completed successfully.
    Succeeded,
    /// A step failed and the item stopped.
    Failed,
    /// The item was cancelled before completing all steps.
    Cancelled,
}

impl SequenceBatchState {
    /// Returns the stable lowercase name of this state.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    /// Returns `true` when no further transition is legal.
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed | Self::Cancelled)
    }

    const fn can_transition_to(self, next: Self) -> bool {
        match self {
            Self::Queued => matches!(next, Self::Running | Self::Failed | Self::Cancelled),
            Self::Running => matches!(next, Self::Succeeded | Self::Failed | Self::Cancelled),
            Self::Succeeded | Self::Failed | Self::Cancelled => false,
        }
    }
}

impl fmt::Display for SequenceBatchState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A typed reason a batch operation was rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SequenceBatchError {
    /// Two items shared an id.
    DuplicateId {
        /// The duplicated id.
        id: String,
    },
    /// An operation targeted an id that is not queued.
    NotFound {
        /// The unknown id.
        id: String,
    },
    /// A state transition was not allowed by the state machine.
    InvalidTransition {
        /// Current state.
        from: SequenceBatchState,
        /// Requested state.
        to: SequenceBatchState,
    },
}

impl SequenceBatchError {
    /// Stable machine-readable code for this error.
    pub const fn code(&self) -> &'static str {
        match self {
            Self::DuplicateId { .. } => "duplicate_id",
            Self::NotFound { .. } => "not_found",
            Self::InvalidTransition { .. } => "invalid_transition",
        }
    }
}

impl fmt::Display for SequenceBatchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateId { id } => write!(f, "batch item '{id}' is already queued"),
            Self::NotFound { id } => write!(f, "no batch item with id '{id}' is queued"),
            Self::InvalidTransition { from, to } => {
                write!(f, "illegal batch state transition {from} -> {to}")
            }
        }
    }
}

impl std::error::Error for SequenceBatchError {}

/// One queued unit of work in a [`SequenceBatchQueue`].
#[derive(Debug, Clone)]
pub struct SequenceBatchItem {
    id: String,
    input: String,
    sequence: ActionSequence,
    state: SequenceBatchState,
    completed: Vec<String>,
    failed_step: Option<String>,
    failure_message: Option<String>,
    cancel: Rc<CancelToken>,
}

impl SequenceBatchItem {
    /// Creates a queued item. The sequence is validated when the queue runs it.
    pub fn new(id: impl Into<String>, input: impl Into<String>, sequence: ActionSequence) -> Self {
        Self {
            id: id.into(),
            input: input.into(),
            sequence,
            state: SequenceBatchState::Queued,
            completed: Vec::new(),
            failed_step: None,
            failure_message: None,
            cancel: Rc::new(CancelToken::new()),
        }
    }

    /// Returns the item id.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Returns the input this item processes.
    pub fn input(&self) -> &str {
        &self.input
    }

    /// Returns the sequence this item runs.
    pub fn sequence(&self) -> &ActionSequence {
        &self.sequence
    }

    /// Returns the current state.
    pub fn state(&self) -> SequenceBatchState {
        self.state
    }

    /// Returns step ids completed so far, in order.
    pub fn completed_steps(&self) -> &[String] {
        &self.completed
    }

    /// Returns the step that failed, if this item failed.
    pub fn failed_step(&self) -> Option<&str> {
        self.failed_step.as_deref()
    }

    /// Returns the failure message, if this item failed.
    pub fn failure_message(&self) -> Option<&str> {
        self.failure_message.as_deref()
    }

    /// Returns this item's cancellation token.
    pub fn cancellation(&self) -> &CancelToken {
        &self.cancel
    }

    /// Signals cancellation for this item.
    pub fn cancel(&self) {
        self.cancel.cancel();
    }

    /// Applies a legal state transition, or returns a typed error.
    ///
    /// Legal transitions are `queued -> running|failed|cancelled` (a queued item
    /// may fail validation before running) and
    /// `running -> succeeded|failed|cancelled`. Terminal states accept none.
    pub fn transition_to(&mut self, next: SequenceBatchState) -> Result<(), SequenceBatchError> {
        if self.state.can_transition_to(next) {
            self.state = next;
            Ok(())
        } else {
            Err(SequenceBatchError::InvalidTransition {
                from: self.state,
                to: next,
            })
        }
    }

    fn record_failure(&mut self, step: impl Into<String>, message: impl Into<String>) {
        self.failed_step = Some(step.into());
        self.failure_message = Some(message.into());
    }
}

/// Executes one [`WizardStep`] of an item.
///
/// Implementations return `Ok(())` on success and an `Err` with a message on
/// failure. The [`CancelToken`] allows an executor to stop the item from inside
/// a step; the queue observes it at the next check.
pub trait SequenceStepRunner: fmt::Debug {
    /// Runs `step` for `input`, optionally consulting `cancel`.
    fn run_step(&self, input: &str, step: &WizardStep, cancel: &CancelToken) -> Result<(), String>;
}

/// A deterministic, side-effect-free [`SequenceStepRunner`].
///
/// Every step succeeds unless its id is marked failing; a configured step can
/// also request cancellation. Decisions are string comparisons only.
#[derive(Debug, Default)]
pub struct RecordingRunner {
    failing_steps: Vec<String>,
    cancel_at_step: Option<String>,
}

impl RecordingRunner {
    /// Creates a runner where every step succeeds.
    pub fn new() -> Self {
        Self::default()
    }

    /// Marks a step id as failing for every item.
    pub fn fail_step(mut self, step_id: impl Into<String>) -> Self {
        self.failing_steps.push(step_id.into());
        self
    }

    /// Cancels an item just after the named step executes successfully.
    pub fn cancel_at_step(mut self, step_id: impl Into<String>) -> Self {
        self.cancel_at_step = Some(step_id.into());
        self
    }
}

impl SequenceStepRunner for RecordingRunner {
    fn run_step(
        &self,
        _input: &str,
        step: &WizardStep,
        cancel: &CancelToken,
    ) -> Result<(), String> {
        if self.failing_steps.iter().any(|id| id == step.id()) {
            return Err(format!(
                "recording failure: step '{}' is marked failing",
                step.id()
            ));
        }
        if self.cancel_at_step.as_deref() == Some(step.id()) {
            cancel.cancel();
        }
        Ok(())
    }
}

/// A FIFO queue of [`SequenceBatchItem`]s with deterministic execution.
#[derive(Debug)]
pub struct SequenceBatchQueue {
    items: Vec<SequenceBatchItem>,
    runner: Box<dyn SequenceStepRunner>,
}

impl SequenceBatchQueue {
    /// Creates an empty queue using [`RecordingRunner`].
    pub fn new() -> Self {
        Self::with_runner(Box::new(RecordingRunner::new()))
    }

    /// Creates an empty queue with the supplied runner.
    pub fn with_runner(runner: Box<dyn SequenceStepRunner>) -> Self {
        Self {
            items: Vec::new(),
            runner,
        }
    }

    /// Appends an item, rejecting a duplicate id.
    pub fn enqueue(&mut self, item: SequenceBatchItem) -> Result<(), SequenceBatchError> {
        if self.items.iter().any(|existing| existing.id == item.id) {
            return Err(SequenceBatchError::DuplicateId { id: item.id });
        }
        self.items.push(item);
        Ok(())
    }

    /// Removes and returns the front item (first in, first out).
    pub fn dequeue(&mut self) -> Option<SequenceBatchItem> {
        if self.items.is_empty() {
            None
        } else {
            Some(self.items.remove(0))
        }
    }

    /// Returns all items in insertion order.
    pub fn items(&self) -> impl ExactSizeIterator<Item = &SequenceBatchItem> {
        self.items.iter()
    }

    /// Returns the item with the given id, if present.
    pub fn item(&self, id: &str) -> Option<&SequenceBatchItem> {
        self.items.iter().find(|item| item.id == id)
    }

    /// Returns the number of queued items.
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// Returns `true` when the queue is empty.
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Cancels an item by id; a queued item is immediately marked cancelled.
    pub fn cancel(&mut self, id: &str) -> Result<(), SequenceBatchError> {
        let index = self
            .items
            .iter()
            .position(|item| item.id == id)
            .ok_or_else(|| SequenceBatchError::NotFound { id: id.to_owned() })?;
        self.items[index].cancel();
        if self.items[index].state() == SequenceBatchState::Queued {
            let _ = self.items[index].transition_to(SequenceBatchState::Cancelled);
        }
        Ok(())
    }

    /// Cancels every item that has not already finished.
    pub fn cancel_all(&mut self) {
        for item in &mut self.items {
            item.cancel();
            if item.state() == SequenceBatchState::Queued {
                let _ = item.transition_to(SequenceBatchState::Cancelled);
            }
        }
    }

    /// Runs every unfinished item in insertion order.
    ///
    /// Failures and cancellations are isolated to their own item. Repeated calls
    /// are idempotent because terminal items are skipped.
    pub fn run_all(&mut self) {
        for index in 0..self.items.len() {
            self.run_item(index);
        }
    }

    fn run_item(&mut self, index: usize) {
        if self.items[index].state().is_terminal() {
            return;
        }
        if self.items[index].sequence.validate().is_err() {
            let _ = self.items[index].transition_to(SequenceBatchState::Failed);
            self.items[index].record_failure("<sequence>", "invalid action sequence");
            return;
        }
        let token = Rc::clone(&self.items[index].cancel);
        if token.is_cancelled() {
            let _ = self.items[index].transition_to(SequenceBatchState::Cancelled);
            return;
        }

        let input = self.items[index].input.clone();
        let steps: Vec<WizardStep> = self.items[index].sequence.steps().cloned().collect();
        let _ = self.items[index].transition_to(SequenceBatchState::Running);

        let mut failure: Option<(String, String)> = None;
        let mut cancelled = false;
        for step in steps {
            if token.is_cancelled() {
                cancelled = true;
                break;
            }
            match self.runner.run_step(&input, &step, &token) {
                Ok(()) => self.items[index].completed.push(step.id().to_owned()),
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

        if cancelled {
            let _ = self.items[index].transition_to(SequenceBatchState::Cancelled);
        } else if let Some((step, message)) = failure {
            let _ = self.items[index].transition_to(SequenceBatchState::Failed);
            self.items[index].record_failure(step, message);
        } else {
            let _ = self.items[index].transition_to(SequenceBatchState::Succeeded);
        }
    }

    /// Builds a deterministic report of the current queue state.
    pub fn report(&self) -> SequenceBatchReport {
        let entries = self
            .items
            .iter()
            .map(|item| BatchReportEntry {
                id: item.id.clone(),
                input: item.input.clone(),
                state: item.state,
                completed: item.completed.clone(),
                failed_step: item.failed_step.clone(),
            })
            .collect();
        SequenceBatchReport { entries }
    }
}

impl Default for SequenceBatchQueue {
    fn default() -> Self {
        Self::new()
    }
}

/// One item's row in a [`SequenceBatchReport`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchReportEntry {
    id: String,
    input: String,
    state: SequenceBatchState,
    completed: Vec<String>,
    failed_step: Option<String>,
}

impl BatchReportEntry {
    /// Returns the item id.
    pub fn id(&self) -> &str {
        &self.id
    }
    /// Returns the item input.
    pub fn input(&self) -> &str {
        &self.input
    }
    /// Returns the recorded state.
    pub fn state(&self) -> SequenceBatchState {
        self.state
    }
    /// Returns the completed step ids.
    pub fn completed_steps(&self) -> &[String] {
        &self.completed
    }
    /// Returns the failing step id, if any.
    pub fn failed_step(&self) -> Option<&str> {
        self.failed_step.as_deref()
    }
}

/// A deterministic, snapshot-friendly report of a whole queue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SequenceBatchReport {
    entries: Vec<BatchReportEntry>,
}

impl SequenceBatchReport {
    /// Returns the entries in insertion order.
    pub fn entries(&self) -> &[BatchReportEntry] {
        &self.entries
    }

    /// Returns a stable one-line-per-item summary.
    ///
    /// Each line is `<id>\t<input>\t<state>\tsteps=<n>[<TAB>at=<step>]`.
    pub fn summary(&self) -> String {
        let mut out = String::new();
        for entry in &self.entries {
            out.push_str(&entry.id);
            out.push('\t');
            out.push_str(&entry.input);
            out.push('\t');
            out.push_str(entry.state.as_str());
            out.push_str("\tsteps=");
            out.push_str(&entry.completed.len().to_string());
            if let Some(step) = entry.failed_step.as_deref() {
                out.push_str("\tat=");
                out.push_str(step);
            }
            out.push('\n');
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sequence(ids: &[&str]) -> ActionSequence {
        let mut sequence = ActionSequence::new("batch-sequence");
        for id in ids {
            sequence
                .add_step(WizardStep::run_command(*id, "run-batch"))
                .unwrap_or_else(|_| unreachable!());
        }
        sequence
    }

    fn filled_queue(ids: &[&str], files: &[&str]) -> SequenceBatchQueue {
        let mut queue = SequenceBatchQueue::new();
        for (index, file) in files.iter().enumerate() {
            queue
                .enqueue(SequenceBatchItem::new(
                    format!("item-{index}"),
                    *file,
                    sequence(ids),
                ))
                .unwrap_or_else(|_| unreachable!());
        }
        queue
    }

    #[test]
    fn enqueue_dequeue_and_insertion_order() {
        let mut queue = filled_queue(&["a", "b"], &["one.pdf", "two.pdf"]);
        let first = queue.dequeue().unwrap();
        assert_eq!(first.id(), "item-0");
        assert_eq!(first.input(), "one.pdf");
        assert_eq!(queue.len(), 1);
        let second = queue.dequeue().unwrap();
        assert_eq!(second.id(), "item-1");
        assert!(queue.dequeue().is_none());
        assert!(queue.is_empty());
    }

    #[test]
    fn duplicate_ids_and_unknown_ids_are_typed_errors() {
        let mut queue = filled_queue(&["a"], &["one.pdf"]);
        assert_eq!(
            queue.enqueue(SequenceBatchItem::new(
                "item-0",
                "two.pdf",
                sequence(&["a"])
            )),
            Err(SequenceBatchError::DuplicateId {
                id: "item-0".to_owned()
            })
        );
        assert_eq!(
            queue.cancel("missing"),
            Err(SequenceBatchError::NotFound {
                id: "missing".to_owned()
            })
        );
    }

    #[test]
    fn state_transitions_follow_the_state_machine() {
        let mut item = SequenceBatchItem::new("x", "in.pdf", sequence(&["a"]));
        assert_eq!(item.state(), SequenceBatchState::Queued);
        assert!(item.transition_to(SequenceBatchState::Running).is_ok());
        assert!(item.transition_to(SequenceBatchState::Succeeded).is_ok());
        let error = item.transition_to(SequenceBatchState::Running).unwrap_err();
        assert_eq!(
            error,
            SequenceBatchError::InvalidTransition {
                from: SequenceBatchState::Succeeded,
                to: SequenceBatchState::Running,
            }
        );
        assert_eq!(error.code(), "invalid_transition");
        let mut other = SequenceBatchItem::new("y", "in.pdf", sequence(&["a"]));
        assert_eq!(
            other.transition_to(SequenceBatchState::Succeeded),
            Err(SequenceBatchError::InvalidTransition {
                from: SequenceBatchState::Queued,
                to: SequenceBatchState::Succeeded,
            })
        );
    }

    #[test]
    fn run_all_is_ordered_and_deterministic() {
        let first = {
            let mut queue = filled_queue(&["a", "b", "c"], &["one.pdf", "two.pdf"]);
            queue.run_all();
            queue.report().summary()
        };
        let second = {
            let mut queue = filled_queue(&["a", "b", "c"], &["one.pdf", "two.pdf"]);
            queue.run_all();
            queue.report().summary()
        };
        assert_eq!(first, second);
        assert_eq!(
            first,
            "item-0\tone.pdf\tsucceeded\tsteps=3\nitem-1\ttwo.pdf\tsucceeded\tsteps=3\n"
        );
    }

    #[test]
    fn failure_is_isolated_and_recorded() {
        let mut queue =
            SequenceBatchQueue::with_runner(Box::new(RecordingRunner::new().fail_step("b")));
        for (index, file) in ["one.pdf", "two.pdf"].iter().enumerate() {
            queue
                .enqueue(SequenceBatchItem::new(
                    format!("item-{index}"),
                    *file,
                    sequence(&["a", "b", "c"]),
                ))
                .unwrap_or_else(|_| unreachable!());
        }
        queue.run_all();
        let failed = queue.item("item-0").unwrap();
        assert_eq!(failed.state(), SequenceBatchState::Failed);
        assert_eq!(failed.completed_steps(), ["a"]);
        assert_eq!(failed.failed_step(), Some("b"));
        assert_eq!(
            failed.failure_message(),
            Some("recording failure: step 'b' is marked failing")
        );
        assert_eq!(
            queue.item("item-1").unwrap().state(),
            SequenceBatchState::Failed
        );
        assert_eq!(
            queue.report().summary(),
            "item-0\tone.pdf\tfailed\tsteps=1\tat=b\nitem-1\ttwo.pdf\tfailed\tsteps=1\tat=b\n"
        );
    }

    #[test]
    fn cancellation_before_run_marks_item_cancelled() {
        let mut queue = filled_queue(&["a", "b"], &["one.pdf", "two.pdf"]);
        queue.cancel("item-0").unwrap();
        queue.run_all();
        let cancelled = queue.item("item-0").unwrap();
        assert_eq!(cancelled.state(), SequenceBatchState::Cancelled);
        assert!(cancelled.cancellation().is_cancelled());
        assert_eq!(cancelled.completed_steps(), &[] as &[String]);
        assert_eq!(
            queue.item("item-1").unwrap().state(),
            SequenceBatchState::Succeeded
        );
    }

    #[test]
    fn cancellation_mid_run_keeps_completed_steps() {
        let mut queue =
            SequenceBatchQueue::with_runner(Box::new(RecordingRunner::new().cancel_at_step("b")));
        queue
            .enqueue(SequenceBatchItem::new(
                "item-0",
                "one.pdf",
                sequence(&["a", "b", "c"]),
            ))
            .unwrap_or_else(|_| unreachable!());
        queue.run_all();
        let item = queue.item("item-0").unwrap();
        assert_eq!(item.state(), SequenceBatchState::Cancelled);
        assert_eq!(item.completed_steps(), ["a", "b"]);
        assert_eq!(
            queue.report().summary(),
            "item-0\tone.pdf\tcancelled\tsteps=2\n"
        );
    }

    #[test]
    fn cancel_all_marks_every_pending_item_cancelled() {
        let mut queue = filled_queue(&["a"], &["one.pdf", "two.pdf"]);
        queue.cancel_all();
        queue.run_all();
        assert!(
            queue
                .items()
                .all(|item| item.state() == SequenceBatchState::Cancelled)
        );
    }

    #[test]
    fn empty_queue_has_an_empty_report() {
        let queue = SequenceBatchQueue::new();
        let report = queue.report();
        assert!(report.entries().is_empty());
        assert_eq!(report.summary(), "");
    }

    #[test]
    fn invalid_sequence_fails_only_its_item() {
        let mut queue = SequenceBatchQueue::new();
        queue
            .enqueue(SequenceBatchItem::new(
                "bad",
                "a.pdf",
                ActionSequence::new("bad"),
            ))
            .unwrap_or_else(|_| unreachable!());
        queue
            .enqueue(SequenceBatchItem::new("good", "b.pdf", sequence(&["a"])))
            .unwrap_or_else(|_| unreachable!());
        queue.run_all();
        assert_eq!(
            queue.item("bad").unwrap().state(),
            SequenceBatchState::Failed
        );
        assert_eq!(queue.item("bad").unwrap().failed_step(), Some("<sequence>"));
        assert_eq!(
            queue.item("good").unwrap().state(),
            SequenceBatchState::Succeeded
        );
    }

    #[test]
    fn finished_items_are_not_rerun() {
        let mut queue = filled_queue(&["a"], &["one.pdf"]);
        queue.run_all();
        queue.run_all();
        assert_eq!(queue.item("item-0").unwrap().completed_steps(), ["a"]);
    }

    #[test]
    fn state_names_are_stable() {
        assert_eq!(SequenceBatchState::Queued.as_str(), "queued");
        assert_eq!(SequenceBatchState::Running.as_str(), "running");
        assert_eq!(SequenceBatchState::Succeeded.as_str(), "succeeded");
        assert_eq!(SequenceBatchState::Failed.as_str(), "failed");
        assert_eq!(SequenceBatchState::Cancelled.as_str(), "cancelled");
    }
}
