//! Review workflow statuses, state, and transition history.
//!
//! A [`ReviewState`] carries the current [`ReviewStatus`] of a comment thread,
//! an optional assignee, creation/update timestamps, and an append-only
//! [`HistoryEntry`] log. Status changes are validated against a fixed transition
//! table; an illegal change is rejected with
//! [`ReviewError::InvalidTransition`] and leaves the state untouched.

use super::error::ReviewError;

/// The lifecycle status of a comment thread.
///
/// The allowed transitions form a small, closed workflow:
///
/// - `Open` → `Resolved`, `Rejected`, `Cancelled`
/// - `Resolved` → `Completed`, `Rejected`, `Open`
/// - `Rejected` → `Open`, `Cancelled`
/// - `Cancelled` → `Open`
/// - `Completed` → `Open`
///
/// Every status can therefore be reopened, and `Resolved` is the only route to
/// `Completed`. The ordering of the variants is the declaration order and is
/// used for deterministic keying in summaries.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ReviewStatus {
    /// A newly opened comment that still needs attention.
    Open,
    /// The reviewer marked the comment as addressed, pending acceptance.
    Resolved,
    /// The comment was rejected and must be revised.
    Rejected,
    /// The comment was cancelled and is closed.
    Cancelled,
    /// The resolved comment was accepted and is closed.
    Completed,
}

impl ReviewStatus {
    /// All statuses in their declaration order.
    pub const ALL: &'static [ReviewStatus] = &[
        ReviewStatus::Open,
        ReviewStatus::Resolved,
        ReviewStatus::Rejected,
        ReviewStatus::Cancelled,
        ReviewStatus::Completed,
    ];

    /// Returns the stable lowercase name used in dictionaries and diagnostics.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Resolved => "resolved",
            Self::Rejected => "rejected",
            Self::Cancelled => "cancelled",
            Self::Completed => "completed",
        }
    }

    /// Parses a status from its stable lowercase name.
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "open" => Some(Self::Open),
            "resolved" => Some(Self::Resolved),
            "rejected" => Some(Self::Rejected),
            "cancelled" => Some(Self::Cancelled),
            "completed" => Some(Self::Completed),
            _ => None,
        }
    }

    /// Returns the statuses reachable from this one in a single transition.
    pub fn allowed_transitions(self) -> &'static [ReviewStatus] {
        match self {
            Self::Open => &[Self::Resolved, Self::Rejected, Self::Cancelled],
            Self::Resolved => &[Self::Completed, Self::Rejected, Self::Open],
            Self::Rejected => &[Self::Open, Self::Cancelled],
            Self::Cancelled => &[Self::Open],
            Self::Completed => &[Self::Open],
        }
    }

    /// Returns `true` when moving directly from `self` to `target` is permitted.
    pub fn can_transition_to(self, target: ReviewStatus) -> bool {
        self.allowed_transitions().contains(&target)
    }

    /// Returns `true` when this status is terminal until the thread is reopened.
    pub fn is_closed(self) -> bool {
        matches!(self, Self::Cancelled | Self::Completed)
    }
}

impl core::fmt::Display for ReviewStatus {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One entry in a thread's transition history.
#[derive(Clone, Debug, PartialEq)]
pub struct HistoryEntry {
    from: Option<ReviewStatus>,
    to: ReviewStatus,
    timestamp: String,
    actor: String,
    note: String,
    extra: Vec<(String, super::Value)>,
}

impl HistoryEntry {
    /// Creates a history entry describing a transition.
    ///
    /// `from` is `None` for the synthetic entry marking initial creation and
    /// `Some` for every real status change.
    pub fn new(
        from: Option<ReviewStatus>,
        to: ReviewStatus,
        timestamp: impl Into<String>,
        actor: impl Into<String>,
        note: impl Into<String>,
    ) -> Self {
        Self {
            from,
            to,
            timestamp: timestamp.into(),
            actor: actor.into(),
            note: note.into(),
            extra: Vec::new(),
        }
    }

    /// Status the thread moved from, or `None` for initial creation.
    pub fn from(&self) -> Option<ReviewStatus> {
        self.from
    }

    /// Status the thread moved to.
    pub fn to(&self) -> ReviewStatus {
        self.to
    }

    /// Timestamp of the change.
    pub fn timestamp(&self) -> &str {
        &self.timestamp
    }

    /// Actor that performed the change.
    pub fn actor(&self) -> &str {
        &self.actor
    }

    /// Free-form note recorded with the change.
    pub fn note(&self) -> &str {
        &self.note
    }

    /// Additional dictionary keys preserved through a round-trip.
    pub fn extra(&self) -> &[(String, super::Value)] {
        &self.extra
    }

    /// Replaces the preserved additional keys.
    pub fn with_extra(mut self, extra: Vec<(String, super::Value)>) -> Self {
        self.extra = extra;
        self
    }

    /// Replaces the preserved additional keys in place.
    pub fn set_extra(&mut self, extra: Vec<(String, super::Value)>) {
        self.extra = extra;
    }
}

/// Mutable workflow state of a comment thread.
#[derive(Clone, Debug, PartialEq)]
pub struct ReviewState {
    status: ReviewStatus,
    assignee: Option<String>,
    created: String,
    updated: String,
    history: Vec<HistoryEntry>,
}

impl ReviewState {
    /// Creates an open state with no assignee, no history, and equal
    /// creation/update timestamps.
    pub fn new(created: impl Into<String>) -> Self {
        let created = created.into();
        Self {
            status: ReviewStatus::Open,
            assignee: None,
            created: created.clone(),
            updated: created,
            history: Vec::new(),
        }
    }

    /// Builds a state from its serialized parts.
    ///
    /// This constructor is used by the dictionary reader; it performs no
    /// transition validation because history entries are treated as an
    /// append-only record of past, already-validated changes.
    pub fn from_parts(
        status: ReviewStatus,
        assignee: Option<String>,
        created: impl Into<String>,
        updated: impl Into<String>,
        history: Vec<HistoryEntry>,
    ) -> Self {
        Self {
            status,
            assignee,
            created: created.into(),
            updated: updated.into(),
            history,
        }
    }

    /// Current status.
    pub fn status(&self) -> ReviewStatus {
        self.status
    }

    /// Current assignee, if one has been set.
    pub fn assignee(&self) -> Option<&str> {
        self.assignee.as_deref()
    }

    /// Creation timestamp.
    pub fn created(&self) -> &str {
        &self.created
    }

    /// Timestamp of the most recent change.
    pub fn updated(&self) -> &str {
        &self.updated
    }

    /// Transition history in chronological order.
    pub fn history(&self) -> &[HistoryEntry] {
        &self.history
    }

    /// Sets or clears the assignee and refreshes the update timestamp.
    pub fn set_assignee(&mut self, assignee: Option<&str>, timestamp: impl Into<String>) {
        self.assignee = assignee.map(str::to_string);
        self.updated = timestamp.into();
    }

    /// Attempts a status transition.
    ///
    /// On success the status changes, `updated` becomes `timestamp`, and a
    /// [`HistoryEntry`] is appended. On failure the state is unchanged and a
    /// [`ReviewError::InvalidTransition`] is returned.
    pub fn transition_to(
        &mut self,
        to: ReviewStatus,
        actor: impl Into<String>,
        timestamp: impl Into<String>,
        note: impl Into<String>,
    ) -> Result<(), ReviewError> {
        if !self.status.can_transition_to(to) {
            return Err(ReviewError::InvalidTransition {
                from: self.status,
                to,
            });
        }
        let timestamp = timestamp.into();
        self.history.push(HistoryEntry::new(
            Some(self.status),
            to,
            timestamp.clone(),
            actor,
            note,
        ));
        self.status = to;
        self.updated = timestamp;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_names_round_trip_and_are_stable() {
        for status in ReviewStatus::ALL {
            assert_eq!(ReviewStatus::from_name(status.as_str()), Some(*status));
            assert_eq!(status.to_string(), status.as_str());
        }
        assert_eq!(ReviewStatus::from_name("bogus"), None);
        assert_eq!(ReviewStatus::ALL.len(), 5);
    }

    #[test]
    fn allowed_transitions_match_the_documented_table() {
        let expected = [
            (
                ReviewStatus::Open,
                vec![
                    ReviewStatus::Resolved,
                    ReviewStatus::Rejected,
                    ReviewStatus::Cancelled,
                ],
            ),
            (
                ReviewStatus::Resolved,
                vec![
                    ReviewStatus::Completed,
                    ReviewStatus::Rejected,
                    ReviewStatus::Open,
                ],
            ),
            (
                ReviewStatus::Rejected,
                vec![ReviewStatus::Open, ReviewStatus::Cancelled],
            ),
            (ReviewStatus::Cancelled, vec![ReviewStatus::Open]),
            (ReviewStatus::Completed, vec![ReviewStatus::Open]),
        ];
        for (status, targets) in expected {
            assert_eq!(status.allowed_transitions(), targets.as_slice());
            for target in ReviewStatus::ALL {
                assert_eq!(status.can_transition_to(*target), targets.contains(target));
            }
        }
        assert!(ReviewStatus::Cancelled.is_closed());
        assert!(ReviewStatus::Completed.is_closed());
        assert!(!ReviewStatus::Open.is_closed());
    }

    #[test]
    fn state_transition_validates_records_history_and_updates_timestamp() {
        let mut state = ReviewState::new("t0");
        assert_eq!(state.status(), ReviewStatus::Open);
        assert_eq!(state.created(), "t0");
        assert_eq!(state.updated(), "t0");
        assert!(state.history().is_empty());

        state.set_assignee(Some("alice"), "t1");
        assert_eq!(state.assignee(), Some("alice"));
        assert_eq!(state.updated(), "t1");

        state
            .transition_to(ReviewStatus::Resolved, "alice", "t2", "looks good")
            .expect("open -> resolved is valid");
        assert_eq!(state.status(), ReviewStatus::Resolved);
        assert_eq!(state.updated(), "t2");
        assert_eq!(state.history().len(), 1);
        let first = &state.history()[0];
        assert_eq!(first.from(), Some(ReviewStatus::Open));
        assert_eq!(first.to(), ReviewStatus::Resolved);
        assert_eq!(first.actor(), "alice");
        assert_eq!(first.note(), "looks good");
        assert_eq!(first.timestamp(), "t2");

        state
            .transition_to(ReviewStatus::Completed, "bob", "t3", "accepted")
            .expect("resolved -> completed is valid");
        assert_eq!(state.history().len(), 2);
        assert_eq!(state.history()[1].timestamp(), "t3");
        assert_eq!(state.history()[1].to(), ReviewStatus::Completed);
    }

    #[test]
    fn invalid_transition_is_rejected_without_mutating_state() {
        let mut state = ReviewState::new("t0");
        let before = state.clone();
        assert_eq!(
            state.transition_to(ReviewStatus::Completed, "alice", "t1", "bad"),
            Err(ReviewError::InvalidTransition {
                from: ReviewStatus::Open,
                to: ReviewStatus::Completed,
            })
        );
        assert_eq!(state, before);
        assert!(state.history().is_empty());

        let mut closed = ReviewState::new("t0");
        closed
            .transition_to(ReviewStatus::Cancelled, "alice", "t1", "drop")
            .expect("open -> cancelled");
        assert_eq!(
            closed.transition_to(ReviewStatus::Completed, "alice", "t2", "no"),
            Err(ReviewError::InvalidTransition {
                from: ReviewStatus::Cancelled,
                to: ReviewStatus::Completed,
            })
        );
        closed
            .transition_to(ReviewStatus::Open, "bob", "t3", "reopen")
            .expect("cancelled -> open");
        assert_eq!(closed.status(), ReviewStatus::Open);
    }

    #[test]
    fn history_entry_extra_keys_are_preserved() {
        let entry = HistoryEntry::new(None, ReviewStatus::Open, "t0", "alice", "created")
            .with_extra(vec![("XI_Custom".to_string(), super::super::Value::Int(7))]);
        assert_eq!(entry.extra().len(), 1);
        assert_eq!(entry.extra()[0].0, "XI_Custom");
    }
}
