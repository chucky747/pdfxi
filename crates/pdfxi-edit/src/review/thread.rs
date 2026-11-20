//! Comment threads and their ordered replies.
//!
//! A [`CommentThread`] is anchored to one root [`AnnotationId`] and owns an
//! ordered list of [`Reply`] values plus a mutable [`ReviewState`]. Replies are
//! kept sorted by `(timestamp, reply id)` so that a given set of replies always
//! has the same order regardless of insertion sequence.

use crate::annotation::AnnotationId;

use super::Value;
use super::error::ReviewError;
use super::workflow::{HistoryEntry, ReviewState, ReviewStatus};

/// Opaque identifier for a comment thread.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ThreadId(u64);

impl ThreadId {
    /// Creates a thread identifier from a numeric value.
    pub fn new(value: u64) -> Self {
        Self(value)
    }

    /// Returns the numeric value of the identifier.
    pub fn get(self) -> u64 {
        self.0
    }
}

/// Opaque identifier for a reply within a thread.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ReplyId(u64);

impl ReplyId {
    /// Creates a reply identifier from a numeric value.
    pub fn new(value: u64) -> Self {
        Self(value)
    }

    /// Returns the numeric value of the identifier.
    pub fn get(self) -> u64 {
        self.0
    }
}

/// A single reply in a comment thread.
#[derive(Clone, Debug, PartialEq)]
pub struct Reply {
    id: ReplyId,
    author: String,
    timestamp: String,
    body: String,
    status: ReviewStatus,
    extra: Vec<(String, Value)>,
}

impl Reply {
    /// Creates a reply with the given identity and content.
    pub fn new(
        id: ReplyId,
        author: impl Into<String>,
        timestamp: impl Into<String>,
        body: impl Into<String>,
        status: ReviewStatus,
    ) -> Self {
        Self {
            id,
            author: author.into(),
            timestamp: timestamp.into(),
            body: body.into(),
            status,
            extra: Vec::new(),
        }
    }

    /// Reply identifier.
    pub fn id(&self) -> ReplyId {
        self.id
    }

    /// Author of the reply.
    pub fn author(&self) -> &str {
        &self.author
    }

    /// Timestamp of the reply.
    pub fn timestamp(&self) -> &str {
        &self.timestamp
    }

    /// Body text of the reply.
    pub fn body(&self) -> &str {
        &self.body
    }

    /// Status attached to the reply.
    pub fn status(&self) -> ReviewStatus {
        self.status
    }

    /// Additional dictionary keys preserved through a round-trip.
    pub fn extra(&self) -> &[(String, Value)] {
        &self.extra
    }

    /// Replaces the preserved additional keys.
    pub fn with_extra(mut self, extra: Vec<(String, Value)>) -> Self {
        self.extra = extra;
        self
    }

    /// Replaces the preserved additional keys in place.
    pub fn set_extra(&mut self, extra: Vec<(String, Value)>) {
        self.extra = extra;
    }
}

/// A comment thread anchored to a root annotation.
#[derive(Clone, Debug, PartialEq)]
pub struct CommentThread {
    id: ThreadId,
    root: AnnotationId,
    replies: Vec<Reply>,
    state: ReviewState,
    extra: Vec<(String, Value)>,
}

impl CommentThread {
    /// Creates an empty, open thread with no replies and no assignee.
    pub fn new(id: ThreadId, root: AnnotationId, created: impl Into<String>) -> Self {
        Self {
            id,
            root,
            replies: Vec::new(),
            state: ReviewState::new(created),
            extra: Vec::new(),
        }
    }

    /// Builds a thread from its serialized parts.
    ///
    /// Replies are sorted into their canonical order by this constructor, so a
    /// dictionary whose reply array is scrambled still produces a deterministic
    /// in-memory thread.
    pub fn from_parts(
        id: ThreadId,
        root: AnnotationId,
        replies: Vec<Reply>,
        state: ReviewState,
    ) -> Self {
        let mut thread = Self {
            id,
            root,
            replies,
            state,
            extra: Vec::new(),
        };
        thread.sort_replies();
        thread
    }

    /// Thread identifier.
    pub fn id(&self) -> ThreadId {
        self.id
    }

    /// Root annotation this thread is anchored to.
    pub fn root(&self) -> AnnotationId {
        self.root
    }

    /// Replies in canonical `(timestamp, id)` order.
    pub fn replies(&self) -> &[Reply] {
        &self.replies
    }

    /// Workflow state of the thread.
    pub fn state(&self) -> &ReviewState {
        &self.state
    }

    /// Current status of the thread.
    pub fn status(&self) -> ReviewStatus {
        self.state.status()
    }

    /// Current assignee, if one has been set.
    pub fn assignee(&self) -> Option<&str> {
        self.state.assignee()
    }

    /// Creation timestamp of the thread state.
    pub fn created(&self) -> &str {
        self.state.created()
    }

    /// Timestamp of the most recent state change.
    pub fn updated(&self) -> &str {
        self.state.updated()
    }

    /// Transition history in chronological order.
    pub fn history(&self) -> &[HistoryEntry] {
        self.state.history()
    }

    /// Number of replies in the thread.
    pub fn reply_count(&self) -> usize {
        self.replies.len()
    }

    /// Returns `true` when the thread has no replies.
    pub fn is_empty(&self) -> bool {
        self.replies.is_empty()
    }

    /// Finds a reply by identifier.
    pub fn reply(&self, id: ReplyId) -> Option<&Reply> {
        self.replies.iter().find(|reply| reply.id == id)
    }

    /// Finds a mutable reply by identifier.
    pub fn reply_mut(&mut self, id: ReplyId) -> Option<&mut Reply> {
        self.replies.iter_mut().find(|reply| reply.id == id)
    }

    /// Returns `true` when at least one reply was authored by `author`.
    pub fn has_reply_by(&self, author: &str) -> bool {
        self.replies.iter().any(|reply| reply.author == author)
    }

    /// Validates the structural invariants of the thread.
    ///
    /// A valid thread has at least one reply, no duplicate reply ids, and a
    /// history whose last entry (when present) ends in the current status. The
    /// last check catches a state that was assembled from mismatched parts;
    /// normal [`Self::transition`] calls always keep it true.
    pub fn validate(&self) -> Result<(), ReviewError> {
        if self.is_empty() {
            return Err(ReviewError::EmptyThread(self.id));
        }
        let mut seen = Vec::with_capacity(self.replies.len());
        for reply in &self.replies {
            if seen.contains(&reply.id) {
                return Err(ReviewError::DuplicateReplyId(reply.id));
            }
            seen.push(reply.id);
        }
        if let Some(last) = self.state.history().last()
            && last.to() != self.status()
        {
            return Err(ReviewError::InconsistentHistory(self.id));
        }
        Ok(())
    }

    /// Merges this thread with `incoming` into one deterministic thread.
    ///
    /// Both sides must share a root annotation. Replies are unioned by reply
    /// id; two replies with the same id but different content are rejected as
    /// [`ReviewError::ConflictingReply`]. The workflow state with the greatest
    /// `(updated, created, status, assignee)` tuple wins, so the result does not
    /// depend on argument order. The lower thread id and the union of both
    /// `extra` key lists (sorted by key) are retained for the same reason.
    pub fn merge(&self, incoming: &Self) -> Result<Self, ReviewError> {
        if self.root != incoming.root {
            return Err(ReviewError::MergeRootMismatch(self.root, incoming.root));
        }

        let mut replies = self.replies.clone();
        for reply in &incoming.replies {
            let existing = replies.iter().find(|candidate| candidate.id == reply.id);
            match existing {
                Some(existing) if existing != reply => {
                    return Err(ReviewError::ConflictingReply(reply.id));
                }
                Some(_) => {}
                None => replies.push(reply.clone()),
            }
        }
        replies.sort_by(|a, b| a.timestamp.cmp(&b.timestamp).then(a.id.cmp(&b.id)));

        let winner = if state_rank(self.state()) >= state_rank(incoming.state()) {
            self.state()
        } else {
            incoming.state()
        };
        let mut merged =
            Self::from_parts(self.id.min(incoming.id), self.root, replies, winner.clone());
        merged.extra = self.extra.clone();
        for (key, value) in &incoming.extra {
            if !merged.extra.iter().any(|(existing, _)| existing == key) {
                merged.extra.push((key.clone(), value.clone()));
            }
        }
        merged.extra.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(merged)
    }

    /// Inserts a reply, rejecting a duplicate reply id.
    ///
    /// The reply list is re-sorted afterwards, so callers may insert in any
    /// order and still observe canonical ordering through [`Self::replies`].
    pub fn add_reply(&mut self, reply: Reply) -> Result<(), ReviewError> {
        if self.reply(reply.id).is_some() {
            return Err(ReviewError::DuplicateReplyId(reply.id));
        }
        self.replies.push(reply);
        self.sort_replies();
        Ok(())
    }

    /// Removes and returns a reply by identifier.
    pub fn remove_reply(&mut self, id: ReplyId) -> Result<Reply, ReviewError> {
        let position = self
            .replies
            .iter()
            .position(|reply| reply.id == id)
            .ok_or(ReviewError::UnknownReplyId(id))?;
        Ok(self.replies.remove(position))
    }

    /// Sets or clears the assignee.
    pub fn set_assignee(&mut self, assignee: Option<&str>, timestamp: impl Into<String>) {
        self.state.set_assignee(assignee, timestamp);
    }

    /// Attempts a status transition, recording it in the history log.
    pub fn transition(
        &mut self,
        to: ReviewStatus,
        actor: impl Into<String>,
        timestamp: impl Into<String>,
        note: impl Into<String>,
    ) -> Result<(), ReviewError> {
        self.state.transition_to(to, actor, timestamp, note)
    }

    /// Replaces the workflow state.
    pub fn with_state(mut self, state: ReviewState) -> Self {
        self.state = state;
        self
    }

    /// Additional dictionary keys preserved through a round-trip.
    pub fn extra(&self) -> &[(String, Value)] {
        &self.extra
    }

    /// Replaces the preserved additional keys.
    pub fn with_extra(mut self, extra: Vec<(String, Value)>) -> Self {
        self.extra = extra;
        self
    }

    /// Replaces the preserved additional keys in place.
    pub fn set_extra(&mut self, extra: Vec<(String, Value)>) {
        self.extra = extra;
    }

    fn sort_replies(&mut self) {
        self.replies
            .sort_by(|a, b| a.timestamp.cmp(&b.timestamp).then(a.id.cmp(&b.id)));
    }
}

/// Deterministic ranking key used when merging two workflow states.
fn state_rank(state: &ReviewState) -> (&str, &str, &str, &str) {
    (
        state.updated(),
        state.created(),
        state.status().as_str(),
        state.assignee().unwrap_or(""),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::annotation::AnnotationId;

    fn reply(id: u64, author: &str, timestamp: &str, body: &str) -> Reply {
        Reply::new(
            ReplyId::new(id),
            author,
            timestamp,
            body,
            ReviewStatus::Open,
        )
    }

    fn thread() -> CommentThread {
        CommentThread::new(ThreadId::new(1), AnnotationId::new(10), "t0")
    }

    #[test]
    fn ids_expose_their_numeric_values() {
        assert_eq!(ThreadId::new(7).get(), 7);
        assert_eq!(ReplyId::new(9).get(), 9);
    }

    #[test]
    fn thread_assembly_orders_replies_by_timestamp_then_id() {
        let mut t = thread();
        t.add_reply(reply(3, "carol", "t3", "third"))
            .expect("insert third");
        t.add_reply(reply(1, "alice", "t1", "first"))
            .expect("insert first");
        t.add_reply(reply(2, "bob", "t2", "second"))
            .expect("insert second");
        let order: Vec<u64> = t.replies().iter().map(|r| r.id().get()).collect();
        assert_eq!(order, vec![1, 2, 3]);
        assert_eq!(t.reply(ReplyId::new(2)).map(Reply::body), Some("second"));

        // Equal timestamps fall back to the reply id, so results stay stable.
        let mut tied = thread();
        tied.add_reply(reply(5, "z", "same", "z"))
            .expect("insert z");
        tied.add_reply(reply(4, "a", "same", "a"))
            .expect("insert a");
        let tied_order: Vec<u64> = tied.replies().iter().map(|r| r.id().get()).collect();
        assert_eq!(tied_order, vec![4, 5]);
    }

    #[test]
    fn duplicate_reply_ids_are_rejected() {
        let mut t = thread();
        t.add_reply(reply(1, "alice", "t1", "first"))
            .expect("first insert");
        assert_eq!(
            t.add_reply(reply(1, "bob", "t2", "duplicate")),
            Err(ReviewError::DuplicateReplyId(ReplyId::new(1)))
        );
        assert_eq!(t.reply_count(), 1);
    }

    #[test]
    fn reply_removal_returns_the_removed_reply_and_reports_missing_ids() {
        let mut t = thread();
        t.add_reply(reply(1, "alice", "t1", "first"))
            .expect("insert");
        let removed = t.remove_reply(ReplyId::new(1)).expect("remove");
        assert_eq!(removed.body(), "first");
        assert_eq!(removed.author(), "alice");
        assert!(t.is_empty());
        assert_eq!(
            t.remove_reply(ReplyId::new(1)),
            Err(ReviewError::UnknownReplyId(ReplyId::new(1)))
        );
    }

    #[test]
    fn empty_thread_is_rejected_by_validate() {
        let t = thread();
        assert!(t.is_empty());
        assert_eq!(
            t.validate(),
            Err(ReviewError::EmptyThread(ThreadId::new(1)))
        );
    }

    #[test]
    fn thread_transition_and_author_lookup_delegate_to_state_and_replies() {
        let mut t = thread();
        t.add_reply(reply(1, "alice", "t1", "first"))
            .expect("insert");
        assert!(t.has_reply_by("alice"));
        assert!(!t.has_reply_by("nobody"));
        t.set_assignee(Some("bob"), "t2");
        assert_eq!(t.assignee(), Some("bob"));
        assert_eq!(t.updated(), "t2");

        t.transition(ReviewStatus::Resolved, "bob", "t3", "done")
            .expect("open -> resolved");
        assert_eq!(t.status(), ReviewStatus::Resolved);
        assert_eq!(t.created(), "t0");
        assert_eq!(t.history().len(), 1);
        // Resolved -> Cancelled is not in the transition table.
        assert_eq!(
            t.transition(ReviewStatus::Cancelled, "bob", "t4", "skip"),
            Err(ReviewError::InvalidTransition {
                from: ReviewStatus::Resolved,
                to: ReviewStatus::Cancelled,
            })
        );
        assert_eq!(t.status(), ReviewStatus::Resolved);
    }

    #[test]
    fn from_parts_sorts_scrambled_replies_and_keeps_state() {
        let state = ReviewState::from_parts(
            ReviewStatus::Rejected,
            Some("alice".to_string()),
            "t0",
            "t2",
            Vec::new(),
        );
        let t = CommentThread::from_parts(
            ThreadId::new(2),
            AnnotationId::new(11),
            vec![reply(2, "b", "t2", "b"), reply(1, "a", "t1", "a")],
            state,
        );
        assert_eq!(t.status(), ReviewStatus::Rejected);
        assert_eq!(t.assignee(), Some("alice"));
        let order: Vec<u64> = t.replies().iter().map(|r| r.id().get()).collect();
        assert_eq!(order, vec![1, 2]);
    }

    #[test]
    fn thread_merge_unions_replies_and_is_symmetric() {
        let mut left = CommentThread::new(ThreadId::new(2), AnnotationId::new(10), "t0");
        left.add_reply(reply(2, "bob", "t2", "second"))
            .expect("second reply");
        left.add_reply(reply(1, "alice", "t1", "first"))
            .expect("first reply");
        let right = CommentThread::new(ThreadId::new(5), AnnotationId::new(10), "t0");

        let merged = left.merge(&right).expect("merge");
        assert_eq!(merged.id(), ThreadId::new(2));
        assert_eq!(merged.root(), AnnotationId::new(10));
        let order: Vec<u64> = merged.replies().iter().map(|r| r.id().get()).collect();
        assert_eq!(order, vec![1, 2]);
        assert_eq!(merged, right.merge(&left).expect("mirrored merge"));
        assert_eq!(merged, merged.merge(&merged).expect("idempotent merge"));

        let mut newer = right.clone();
        newer
            .transition(ReviewStatus::Resolved, "bob", "t9", "done")
            .expect("resolved");
        let resolved = left.merge(&newer).expect("merge with newer state");
        assert_eq!(resolved.status(), ReviewStatus::Resolved);
        assert_eq!(
            left.merge(&newer).expect("merge again"),
            newer.merge(&left).expect("mirrored newer merge")
        );
    }

    #[test]
    fn thread_merge_rejects_foreign_roots_and_conflicting_reply_ids() {
        let mut left = CommentThread::new(ThreadId::new(1), AnnotationId::new(10), "t0");
        left.add_reply(reply(1, "alice", "t1", "first"))
            .expect("reply");
        let foreign = CommentThread::new(ThreadId::new(2), AnnotationId::new(11), "t0");
        assert_eq!(
            left.merge(&foreign),
            Err(ReviewError::MergeRootMismatch(
                AnnotationId::new(10),
                AnnotationId::new(11)
            ))
        );

        let mut conflicting = CommentThread::new(ThreadId::new(3), AnnotationId::new(10), "t0");
        conflicting
            .add_reply(reply(1, "mallory", "t2", "different"))
            .expect("conflicting reply");
        assert_eq!(
            left.merge(&conflicting),
            Err(ReviewError::ConflictingReply(ReplyId::new(1)))
        );
        // An identical reply id and content collapses instead of conflicting.
        let mut identical = CommentThread::new(ThreadId::new(4), AnnotationId::new(10), "t0");
        identical
            .add_reply(reply(1, "alice", "t1", "first"))
            .expect("identical reply");
        assert_eq!(left.merge(&identical).expect("collapse").reply_count(), 1);
    }

    #[test]
    fn thread_validate_detects_duplicate_reply_ids_and_inconsistent_history() {
        let mut valid = thread();
        valid
            .add_reply(reply(1, "alice", "t1", "first"))
            .expect("reply");
        assert_eq!(valid.validate(), Ok(()));
        valid
            .transition(ReviewStatus::Resolved, "alice", "t2", "done")
            .expect("resolved");
        assert_eq!(valid.validate(), Ok(()));

        let duplicates = CommentThread::from_parts(
            ThreadId::new(1),
            AnnotationId::new(10),
            vec![reply(1, "alice", "t1", "a"), reply(1, "bob", "t2", "b")],
            ReviewState::new("t0"),
        );
        assert_eq!(
            duplicates.validate(),
            Err(ReviewError::DuplicateReplyId(ReplyId::new(1)))
        );

        let state = ReviewState::from_parts(
            ReviewStatus::Open,
            None,
            "t0",
            "t2",
            vec![HistoryEntry::new(
                None,
                ReviewStatus::Resolved,
                "t2",
                "alice",
                "mismatched",
            )],
        );
        let inconsistent = CommentThread::from_parts(
            ThreadId::new(2),
            AnnotationId::new(10),
            vec![reply(1, "alice", "t1", "a")],
            state,
        );
        assert_eq!(
            inconsistent.validate(),
            Err(ReviewError::InconsistentHistory(ThreadId::new(2)))
        );
    }
}
