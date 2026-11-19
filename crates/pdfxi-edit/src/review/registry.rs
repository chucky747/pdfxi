//! Review registry grouping annotations into threads.
//!
//! The registry is the aggregate entry point for a review session. It keys
//! threads by their root [`AnnotationId`] in a `BTreeMap`, which makes iteration
//! and every derived view deterministic. It also records the page of each
//! thread so it can filter by page, and generates reply identifiers from a
//! monotonic counter.

use std::collections::{BTreeMap, BTreeSet};

use crate::annotation::{Annotation, AnnotationId};
use crate::geometry::PageIndex;

use super::Value;
use super::error::ReviewError;
use super::thread::{CommentThread, Reply, ReplyId, ThreadId};
use super::workflow::{ReviewState, ReviewStatus};

/// Criteria for filtering threads in a [`ReviewRegistry`].
///
/// `None` for a criterion means "do not constrain on this field". When several
/// criteria are set they are combined with logical AND.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReviewFilter {
    /// Restrict results to threads in this status.
    pub status: Option<ReviewStatus>,
    /// Restrict results to threads containing a reply by this author.
    pub author: Option<String>,
    /// Restrict results to threads anchored on this page.
    pub page: Option<PageIndex>,
}

impl ReviewFilter {
    /// Creates an empty filter that matches every thread.
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the status criterion.
    pub fn with_status(mut self, status: ReviewStatus) -> Self {
        self.status = Some(status);
        self
    }

    /// Sets the author criterion.
    pub fn with_author(mut self, author: impl Into<String>) -> Self {
        self.author = Some(author.into());
        self
    }

    /// Sets the page criterion.
    pub fn with_page(mut self, page: PageIndex) -> Self {
        self.page = Some(page);
        self
    }
}

/// A stable count of threads per review status.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReviewSummary {
    counts: BTreeMap<ReviewStatus, usize>,
    total: usize,
}

impl ReviewSummary {
    /// Builds a summary from a status-to-count map.
    ///
    /// All five statuses are present in the returned map, including those with
    /// a zero count, so the shape of a summary never depends on the data.
    pub fn new(mut counts: BTreeMap<ReviewStatus, usize>) -> Self {
        for status in ReviewStatus::ALL {
            counts.entry(*status).or_insert(0);
        }
        let total = counts.values().sum();
        Self { counts, total }
    }

    /// Number of threads in `status`.
    pub fn count(&self, status: ReviewStatus) -> usize {
        self.counts.get(&status).copied().unwrap_or(0)
    }

    /// Total number of threads summarized.
    pub fn total(&self) -> usize {
        self.total
    }

    /// Counts keyed by status, in status declaration order.
    pub fn counts(&self) -> &BTreeMap<ReviewStatus, usize> {
        &self.counts
    }
}

/// A collection of comment threads keyed by their root annotation.
#[derive(Clone, Debug, PartialEq)]
pub struct ReviewRegistry {
    threads: BTreeMap<AnnotationId, CommentThread>,
    pages: BTreeMap<AnnotationId, PageIndex>,
    thread_ids: BTreeSet<ThreadId>,
    next_thread_id: u64,
    next_reply_id: u64,
    extra: Vec<(String, Value)>,
}

impl ReviewRegistry {
    /// Creates an empty registry.
    pub fn new() -> Self {
        Self {
            threads: BTreeMap::new(),
            pages: BTreeMap::new(),
            thread_ids: BTreeSet::new(),
            next_thread_id: 0,
            next_reply_id: 0,
            extra: Vec::new(),
        }
    }

    /// Creates one open thread per annotation, using each annotation's page.
    ///
    /// Annotation ids are duplicated-safe: a repeated id is rejected with
    /// [`ReviewError::DuplicateRoot`] rather than silently overwriting a thread.
    /// An empty input is rejected with [`ReviewError::EmptyInput`].
    pub fn from_annotations<'a, I>(
        annotations: I,
        created: impl Into<String>,
    ) -> Result<Self, ReviewError>
    where
        I: IntoIterator<Item = &'a Annotation>,
    {
        let created = created.into();
        let mut registry = Self::new();
        let mut seen = false;
        for annotation in annotations {
            seen = true;
            let id = ThreadId::new(registry.next_thread_id);
            registry.next_thread_id += 1;
            let thread = CommentThread::new(id, annotation.id(), created.clone());
            registry.add_thread(thread, annotation.page())?;
        }
        if !seen {
            return Err(ReviewError::EmptyInput);
        }
        Ok(registry)
    }

    /// Number of registered threads.
    pub fn len(&self) -> usize {
        self.threads.len()
    }

    /// Returns `true` when the registry holds no threads.
    pub fn is_empty(&self) -> bool {
        self.threads.is_empty()
    }

    /// Adds a thread with a known page.
    ///
    /// Duplicate thread ids and duplicate root annotations are rejected.
    pub fn add_thread(
        &mut self,
        thread: CommentThread,
        page: PageIndex,
    ) -> Result<(), ReviewError> {
        if self.threads.contains_key(&thread.root()) {
            return Err(ReviewError::DuplicateRoot(thread.root()));
        }
        if self.thread_ids.contains(&thread.id()) {
            return Err(ReviewError::DuplicateThreadId(thread.id()));
        }
        self.next_thread_id = self.next_thread_id.max(thread.id().get().saturating_add(1));
        self.thread_ids.insert(thread.id());
        self.pages.insert(thread.root(), page);
        self.threads.insert(thread.root(), thread);
        Ok(())
    }

    /// Finds a thread by its root annotation.
    pub fn thread(&self, root: AnnotationId) -> Option<&CommentThread> {
        self.threads.get(&root)
    }

    /// Finds a mutable thread by its root annotation.
    pub fn thread_mut(&mut self, root: AnnotationId) -> Option<&mut CommentThread> {
        self.threads.get_mut(&root)
    }

    /// Page a thread is anchored to, if registered.
    pub fn page_of(&self, root: AnnotationId) -> Option<PageIndex> {
        self.pages.get(&root).copied()
    }

    /// Threads in deterministic root-annotation order.
    pub fn threads(&self) -> impl Iterator<Item = &CommentThread> {
        self.threads.values()
    }

    /// Adds a reply to a thread and returns the generated reply id.
    ///
    /// Reply ids are allocated from a monotonic counter that is advanced only
    /// after the target thread has been found, so a failed call does not create
    /// a gap in the id sequence.
    pub fn add_reply(
        &mut self,
        root: AnnotationId,
        author: impl Into<String>,
        timestamp: impl Into<String>,
        body: impl Into<String>,
        status: ReviewStatus,
    ) -> Result<ReplyId, ReviewError> {
        if !self.threads.contains_key(&root) {
            return Err(ReviewError::UnknownThread(root));
        }
        let id = ReplyId::new(self.next_reply_id);
        let thread = self
            .threads
            .get_mut(&root)
            .ok_or(ReviewError::UnknownThread(root))?;
        let reply = Reply::new(id, author, timestamp, body, status);
        thread.add_reply(reply)?;
        self.next_reply_id = self.next_reply_id.saturating_add(1);
        Ok(id)
    }

    /// Removes a reply from a thread.
    pub fn remove_reply(&mut self, root: AnnotationId, id: ReplyId) -> Result<Reply, ReviewError> {
        let thread = self
            .threads
            .get_mut(&root)
            .ok_or(ReviewError::UnknownThread(root))?;
        thread.remove_reply(id)
    }

    /// Transitions the state of a thread.
    pub fn transition(
        &mut self,
        root: AnnotationId,
        to: ReviewStatus,
        actor: impl Into<String>,
        timestamp: impl Into<String>,
        note: impl Into<String>,
    ) -> Result<(), ReviewError> {
        let thread = self
            .threads
            .get_mut(&root)
            .ok_or(ReviewError::UnknownThread(root))?;
        thread.transition(to, actor, timestamp, note)
    }

    /// Filters threads by status, author, and/or page.
    ///
    /// The result preserves the registry's deterministic root-annotation order.
    pub fn filter(&self, query: &ReviewFilter) -> Vec<&CommentThread> {
        self.threads
            .values()
            .filter(|thread| match query.status {
                Some(status) => thread.status() == status,
                None => true,
            })
            .filter(|thread| match &query.author {
                Some(author) => thread.has_reply_by(author),
                None => true,
            })
            .filter(|thread| match query.page {
                Some(page) => self.pages.get(&thread.root()) == Some(&page),
                None => true,
            })
            .collect()
    }

    /// Convenience wrapper for [`Self::filter`] by status.
    pub fn threads_by_status(&self, status: ReviewStatus) -> Vec<&CommentThread> {
        self.filter(&ReviewFilter::new().with_status(status))
    }

    /// Convenience wrapper for [`Self::filter`] by page.
    pub fn threads_by_page(&self, page: PageIndex) -> Vec<&CommentThread> {
        self.filter(&ReviewFilter::new().with_page(page))
    }

    /// Produces a stable per-status count of all threads.
    pub fn summary(&self) -> ReviewSummary {
        let mut counts = BTreeMap::new();
        for thread in self.threads.values() {
            *counts.entry(thread.status()).or_insert(0) += 1;
        }
        ReviewSummary::new(counts)
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

    /// Rebuilds id counters and the thread-id set after a dictionary import.
    pub(crate) fn reindex(&mut self) {
        self.thread_ids = self.threads.values().map(CommentThread::id).collect();
        self.next_thread_id = self
            .thread_ids
            .iter()
            .map(|id| id.get().saturating_add(1))
            .max()
            .unwrap_or(0);
        self.next_reply_id = self
            .threads
            .values()
            .flat_map(|thread| thread.replies())
            .map(|reply| reply.id().get().saturating_add(1))
            .max()
            .unwrap_or(0);
    }

    /// Replaces a thread without changing its page (used by dictionary import).
    pub(crate) fn insert_parsed(&mut self, thread: CommentThread, page: PageIndex) {
        self.pages.insert(thread.root(), page);
        self.threads.insert(thread.root(), thread);
    }

    /// Builds a registry from parsed threads and pages.
    pub(crate) fn from_parsed(
        threads: Vec<(CommentThread, PageIndex)>,
        extra: Vec<(String, Value)>,
    ) -> Result<Self, ReviewError> {
        let mut registry = Self::new();
        for (thread, page) in threads {
            if registry.threads.contains_key(&thread.root()) {
                return Err(ReviewError::DuplicateRoot(thread.root()));
            }
            if registry.thread_ids.contains(&thread.id()) {
                return Err(ReviewError::DuplicateThreadId(thread.id()));
            }
            registry.thread_ids.insert(thread.id());
            registry.insert_parsed(thread, page);
        }
        registry.extra = extra;
        registry.reindex();
        Ok(registry)
    }

    /// Returns a reference to the workflow state of a thread, if present.
    pub fn state_of(&self, root: AnnotationId) -> Option<&ReviewState> {
        self.threads.get(&root).map(CommentThread::state)
    }
}

impl Default for ReviewRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::annotation::{AnnotationId, AnnotationKind};
    use crate::geometry::{PageIndex, Rect};

    fn rect() -> Rect {
        Rect::new(0.0, 0.0, 10.0, 10.0).expect("valid rect")
    }

    fn annotation(id: u64, page: usize) -> Annotation {
        Annotation::new(
            AnnotationId::new(id),
            AnnotationKind::Note,
            PageIndex::new(page),
            rect(),
            "author",
            "body",
        )
        .expect("valid annotation")
    }

    fn populated() -> ReviewRegistry {
        let annotations = [
            annotation(1, 0),
            annotation(2, 0),
            annotation(3, 1),
            annotation(4, 1),
        ];
        let mut registry = ReviewRegistry::from_annotations(annotations.iter(), "t0")
            .expect("annotations must group");
        registry
            .add_reply(
                AnnotationId::new(1),
                "alice",
                "t1",
                "first",
                ReviewStatus::Open,
            )
            .expect("reply 1");
        registry
            .add_reply(
                AnnotationId::new(2),
                "bob",
                "t2",
                "second",
                ReviewStatus::Open,
            )
            .expect("reply 2");
        registry
            .add_reply(
                AnnotationId::new(3),
                "alice",
                "t3",
                "third",
                ReviewStatus::Open,
            )
            .expect("reply 3");
        registry
            .transition(
                AnnotationId::new(1),
                ReviewStatus::Resolved,
                "alice",
                "t4",
                "done",
            )
            .expect("resolve thread 1");
        registry
            .transition(
                AnnotationId::new(2),
                ReviewStatus::Rejected,
                "bob",
                "t5",
                "revise",
            )
            .expect("reject thread 2");
        registry
    }

    #[test]
    fn from_annotations_groups_by_annotation_in_deterministic_order() {
        let registry = populated();
        assert_eq!(registry.len(), 4);
        assert_eq!(
            registry.page_of(AnnotationId::new(1)),
            Some(PageIndex::new(0))
        );
        assert_eq!(
            registry.page_of(AnnotationId::new(4)),
            Some(PageIndex::new(1))
        );
        let roots: Vec<u64> = registry.threads().map(|t| t.root().get()).collect();
        assert_eq!(roots, vec![1, 2, 3, 4]);
        assert!(registry.thread(AnnotationId::new(3)).is_some());
        assert!(registry.thread(AnnotationId::new(99)).is_none());
    }

    #[test]
    fn duplicate_and_empty_inputs_are_rejected() {
        let duplicates = [annotation(7, 0), annotation(7, 1)];
        assert_eq!(
            ReviewRegistry::from_annotations(duplicates.iter(), "t0"),
            Err(ReviewError::DuplicateRoot(AnnotationId::new(7)))
        );
        let empty: [Annotation; 0] = [];
        assert_eq!(
            ReviewRegistry::from_annotations(empty.iter(), "t0"),
            Err(ReviewError::EmptyInput)
        );
    }

    #[test]
    fn add_thread_rejects_duplicate_ids_and_roots() {
        let mut registry = populated();
        let duplicate_root = CommentThread::new(ThreadId::new(99), AnnotationId::new(1), "t0");
        assert_eq!(
            registry.add_thread(duplicate_root, PageIndex::new(0)),
            Err(ReviewError::DuplicateRoot(AnnotationId::new(1)))
        );
        let duplicate_id = CommentThread::new(ThreadId::new(1), AnnotationId::new(50), "t0");
        assert_eq!(
            registry.add_thread(duplicate_id, PageIndex::new(0)),
            Err(ReviewError::DuplicateThreadId(ThreadId::new(1)))
        );
    }

    #[test]
    fn reply_ids_are_stable_and_unknown_threads_are_rejected() {
        let mut registry = populated();
        let id = registry
            .add_reply(
                AnnotationId::new(4),
                "carol",
                "t6",
                "fourth",
                ReviewStatus::Open,
            )
            .expect("new reply");
        assert_eq!(id.get(), 3);
        let thread = registry.thread(AnnotationId::new(4)).expect("thread 4");
        assert_eq!(thread.replies().len(), 1);
        assert_eq!(thread.replies()[0].author(), "carol");

        assert_eq!(
            registry.add_reply(AnnotationId::new(99), "x", "t7", "y", ReviewStatus::Open),
            Err(ReviewError::UnknownThread(AnnotationId::new(99)))
        );
        let removed = registry
            .remove_reply(AnnotationId::new(4), id)
            .expect("remove reply");
        assert_eq!(removed.body(), "fourth");
        assert_eq!(
            registry.remove_reply(AnnotationId::new(4), id),
            Err(ReviewError::UnknownReplyId(id))
        );
    }

    #[test]
    fn filtering_by_status_author_and_page_is_deterministic() {
        let registry = populated();

        let resolved = registry.threads_by_status(ReviewStatus::Resolved);
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].root(), AnnotationId::new(1));

        let alice = registry.filter(&ReviewFilter::new().with_author("alice"));
        let alice_roots: Vec<u64> = alice.iter().map(|t| t.root().get()).collect();
        assert_eq!(alice_roots, vec![1, 3]);

        let page_one = registry.threads_by_page(PageIndex::new(1));
        let page_roots: Vec<u64> = page_one.iter().map(|t| t.root().get()).collect();
        assert_eq!(page_roots, vec![3, 4]);

        let combined = registry.filter(
            &ReviewFilter::new()
                .with_author("alice")
                .with_page(PageIndex::new(1))
                .with_status(ReviewStatus::Open),
        );
        assert_eq!(combined.len(), 1);
        assert_eq!(combined[0].root(), AnnotationId::new(3));

        // Repeated calls return identical results.
        assert_eq!(
            registry.filter(&ReviewFilter::new().with_status(ReviewStatus::Rejected)),
            registry.filter(&ReviewFilter::new().with_status(ReviewStatus::Rejected))
        );
    }

    #[test]
    fn summary_counts_are_stable_and_include_zero_entries() {
        let registry = populated();
        let summary = registry.summary();
        assert_eq!(summary.total(), 4);
        assert_eq!(summary.count(ReviewStatus::Resolved), 1);
        assert_eq!(summary.count(ReviewStatus::Rejected), 1);
        assert_eq!(summary.count(ReviewStatus::Open), 2);
        assert_eq!(summary.count(ReviewStatus::Cancelled), 0);
        assert_eq!(summary.count(ReviewStatus::Completed), 0);
        assert_eq!(summary.counts().len(), ReviewStatus::ALL.len());
        assert_eq!(summary, registry.summary());

        let empty = ReviewRegistry::new();
        let empty_summary = empty.summary();
        assert_eq!(empty_summary.total(), 0);
        assert_eq!(empty_summary.counts().len(), ReviewStatus::ALL.len());
    }

    #[test]
    fn state_transition_on_registry_updates_history() {
        let mut registry = populated();
        registry
            .transition(
                AnnotationId::new(1),
                ReviewStatus::Completed,
                "bob",
                "t9",
                "accepted",
            )
            .expect("resolved -> completed");
        let thread = registry.thread(AnnotationId::new(1)).expect("thread");
        assert_eq!(thread.status(), ReviewStatus::Completed);
        assert_eq!(thread.history().len(), 2);
        assert_eq!(thread.history()[1].to(), ReviewStatus::Completed);
        assert_eq!(
            registry
                .state_of(AnnotationId::new(1))
                .map(ReviewState::status),
            Some(ReviewStatus::Completed)
        );
        assert_eq!(
            registry.transition(
                AnnotationId::new(3),
                ReviewStatus::Completed,
                "bob",
                "t10",
                "skip",
            ),
            Err(ReviewError::InvalidTransition {
                from: ReviewStatus::Open,
                to: ReviewStatus::Completed,
            })
        );
    }
}
