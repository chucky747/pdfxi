//! Deterministic import/export/print views and participant summaries.
//!
//! This module builds on the existing dictionary round-trip in [`super::serialize`]
//! and the aggregate model in [`super::registry`]. It adds three *views* of a
//! [`ReviewRegistry`] that are useful to a comment workbench:
//!
//! - [`export_registry`] / [`import_registry`] wrap the existing
//!   `registry_to_dict` / `dict_to_registry` pair with a documented,
//!   lossless-by-contract entry point (REVIEW-005 import/export).
//! - [`registry_to_print_lines`] and [`registry_to_text`] produce a stable,
//!   human-readable listing suitable for printing or a text pane (REVIEW-005
//!   print).
//! - [`participants`] and [`ParticipantSummary`] derive a sorted roster with a
//!   per-participant status breakdown (REVIEW-007 participant/status summary).
//!
//! Every view is a pure function of the registry: repeated calls return exactly
//! the same strings and values, and no source document bytes are mutated.

use std::collections::{BTreeMap, BTreeSet};

use crate::annotation::AnnotationId;
use crate::pdf::Dict;

use super::error::ReviewError;
use super::registry::ReviewRegistry;
use super::serialize::{dict_to_registry, registry_to_dict};
use super::thread::{Reply, ThreadId};
use super::workflow::ReviewStatus;

/// One participant in a review session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParticipantSummary {
    /// Author name as it appears on replies and history entries.
    pub author: String,
    /// Number of replies authored by this participant.
    pub reply_count: usize,
    /// Number of distinct threads this participant replied to.
    pub thread_count: usize,
    /// Number of threads currently assigned to this participant.
    pub assigned_count: usize,
    /// Number of transitions this participant performed.
    pub transition_count: usize,
    /// Per-status counts of the threads this participant replied to.
    pub status_counts: BTreeMap<ReviewStatus, usize>,
}

/// Exports a registry through the existing private-namespace dictionary
/// round-trip.
///
/// This is a thin, documented wrapper around
/// [`registry_to_dict`](super::registry_to_dict); the dictionary itself remains
/// lossless and deterministic.
pub fn export_registry(registry: &ReviewRegistry) -> Dict {
    registry_to_dict(registry)
}

/// Imports a registry previously produced by [`export_registry`].
///
/// This delegates to [`dict_to_registry`](super::dict_to_registry), which
/// validates ids/statuses and reindexes the monotonic counters.
pub fn import_registry(dict: &Dict) -> Result<ReviewRegistry, ReviewError> {
    dict_to_registry(dict)
}

/// Builds a sorted roster of everyone who replied to, was assigned to, or
/// transitioned a thread in `registry`.
///
/// Participants are ordered lexicographically by author, so the summary is
/// stable regardless of insertion order. Every participant's `status_counts`
/// contains all five statuses.
pub fn participants(registry: &ReviewRegistry) -> Vec<ParticipantSummary> {
    #[derive(Default)]
    struct Accumulator {
        reply_count: usize,
        threads: BTreeSet<ThreadId>,
        assigned_count: usize,
        transition_count: usize,
        status_counts: BTreeMap<ReviewStatus, usize>,
    }

    let mut map: BTreeMap<String, Accumulator> = BTreeMap::new();
    for thread in registry.threads() {
        if let Some(assignee) = thread.assignee() {
            map.entry(assignee.to_string()).or_default().assigned_count += 1;
        }
        for entry in thread.history() {
            map.entry(entry.actor().to_string())
                .or_default()
                .transition_count += 1;
        }
        for reply in thread.replies() {
            let accumulated = map.entry(reply.author().to_string()).or_default();
            accumulated.reply_count += 1;
            accumulated.threads.insert(thread.id());
            *accumulated
                .status_counts
                .entry(thread.status())
                .or_insert(0) += 1;
        }
    }

    map.into_iter()
        .map(|(author, accumulated)| {
            let mut status_counts = accumulated.status_counts;
            for status in ReviewStatus::ALL {
                status_counts.entry(*status).or_insert(0);
            }
            ParticipantSummary {
                author,
                reply_count: accumulated.reply_count,
                thread_count: accumulated.threads.len(),
                assigned_count: accumulated.assigned_count,
                transition_count: accumulated.transition_count,
                status_counts,
            }
        })
        .collect()
}

/// Aggregates the per-participant view into a single participant status summary.
///
/// `counts` holds, for each status, how many *thread participations* (a reply
/// by a participant on a thread in that status) exist. `total` is the number of
/// such participations, not the number of threads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParticipantStatusSummary {
    /// Participations keyed by status, with all five statuses present.
    pub counts: BTreeMap<ReviewStatus, usize>,
    /// Total number of thread participations.
    pub total: usize,
}

/// Derives a stable per-status summary of participant activity.
pub fn participant_status_summary(registry: &ReviewRegistry) -> ParticipantStatusSummary {
    let mut counts = BTreeMap::new();
    for participant in participants(registry) {
        for (status, count) in participant.status_counts {
            *counts.entry(status).or_insert(0) += count;
        }
    }
    for status in ReviewStatus::ALL {
        counts.entry(*status).or_insert(0);
    }
    let total = counts.values().sum();
    ParticipantStatusSummary { counts, total }
}

/// Renders a registry as stable, printable lines.
///
/// Threads follow root-annotation order, replies follow canonical
/// `(timestamp, id)` order, and history is chronological. The header line makes
/// the listing self-describing.
pub fn registry_to_print_lines(registry: &ReviewRegistry) -> Vec<String> {
    let mut lines = Vec::new();
    lines.push(format!("Review registry: {} thread(s)", registry.len()));
    for thread in registry.threads() {
        let page: String = match registry.page_of(thread.root()) {
            Some(index) => index.get().to_string(),
            None => "?".to_string(),
        };
        let assignee = thread.assignee().unwrap_or("-");
        lines.push(format!(
            "Thread {} root={} page={} status={} assignee={}",
            thread.id().get(),
            thread.root().get(),
            page,
            thread.status().as_str(),
            assignee
        ));
        lines.push(format!(
            "  created={} updated={} replies={}",
            thread.created(),
            thread.updated(),
            thread.reply_count()
        ));
        for reply in thread.replies() {
            lines.push(format_reply(reply));
        }
        for entry in thread.history() {
            let from = entry.from().map(ReviewStatus::as_str).unwrap_or("initial");
            lines.push(format!(
                "    history {} {}->{} by {}: {}",
                entry.timestamp(),
                from,
                entry.to().as_str(),
                entry.actor(),
                entry.note()
            ));
        }
    }
    lines
}

/// Renders a registry as a single deterministic text block with trailing
/// newlines, suitable for printing or a plain-text panes.
pub fn registry_to_text(registry: &ReviewRegistry) -> String {
    let mut text = registry_to_print_lines(registry).join("\n");
    text.push('\n');
    text
}

fn format_reply(reply: &Reply) -> String {
    format!(
        "    reply {} by {} at {} [{}]: {}",
        reply.id().get(),
        reply.author(),
        reply.timestamp(),
        reply.status().as_str(),
        reply.body()
    )
}

/// Convenience alias used by callers that think in terms of "print a comment
/// registry" rather than "render text".
pub fn print_registry(registry: &ReviewRegistry) -> String {
    registry_to_text(registry)
}

/// Convenience alias returning the root annotation ids present in a registry.
pub fn root_annotation_ids(registry: &ReviewRegistry) -> Vec<AnnotationId> {
    registry
        .threads()
        .map(super::thread::CommentThread::root)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::annotation::{Annotation, AnnotationId, AnnotationKind};
    use crate::geometry::{PageIndex, Rect};

    fn annotation(id: u64, page: usize) -> Annotation {
        let rect = Rect::new(0.0, 0.0, 10.0, 10.0).expect("valid rect");
        Annotation::new(
            AnnotationId::new(id),
            AnnotationKind::Note,
            PageIndex::new(page),
            rect,
            "author",
            "body",
        )
        .expect("valid annotation")
    }

    fn populated() -> ReviewRegistry {
        let annotations = [annotation(1, 0), annotation(2, 1)];
        let mut registry =
            ReviewRegistry::from_annotations(annotations.iter(), "t0").expect("valid registry");
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
                AnnotationId::new(1),
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
            .expect("resolve");
        registry
            .transition(
                AnnotationId::new(2),
                ReviewStatus::Rejected,
                "bob",
                "t5",
                "revise",
            )
            .expect("reject");
        registry
    }

    #[test]
    fn export_import_round_trip_preserves_every_thread() {
        let registry = populated();
        let dict = export_registry(&registry);
        let restored = import_registry(&dict).expect("import");
        assert_eq!(restored, registry);
        assert_eq!(export_registry(&restored), dict);
    }

    #[test]
    fn print_lines_are_stable_and_cover_threads_replies_and_history() {
        let registry = populated();
        let lines = registry_to_print_lines(&registry);
        assert_eq!(lines, registry_to_print_lines(&registry));
        assert_eq!(
            lines.first().map(String::as_str),
            Some("Review registry: 2 thread(s)")
        );
        let joined = lines.join("\n");
        assert!(joined.contains("status=resolved"));
        assert!(joined.contains("by alice at t1 [open]: first"));
        assert!(joined.contains("history t4 open->resolved by alice: done"));
        assert!(registry_to_text(&registry).ends_with('\n'));
        assert_eq!(print_registry(&registry), registry_to_text(&registry));
    }

    #[test]
    fn participants_are_sorted_and_count_replies_threads_and_transitions() {
        let registry = populated();
        let roster = participants(&registry);
        let authors: Vec<&str> = roster.iter().map(|p| p.author.as_str()).collect();
        assert_eq!(authors, vec!["alice", "bob"]);

        let alice = &roster[0];
        assert_eq!(alice.reply_count, 2);
        assert_eq!(alice.thread_count, 1);
        assert_eq!(alice.transition_count, 1);
        assert_eq!(alice.status_counts.len(), ReviewStatus::ALL.len());
        assert_eq!(alice.status_counts.get(&ReviewStatus::Resolved), Some(&2));

        let bob = &roster[1];
        assert_eq!(bob.reply_count, 1);
        assert_eq!(bob.thread_count, 1);
        assert_eq!(bob.status_counts.get(&ReviewStatus::Rejected), Some(&1));
    }

    #[test]
    fn assigned_and_unassigned_threads_are_reflected_in_participant_summary() {
        let mut registry = populated();
        assert_eq!(
            participants(&registry)
                .into_iter()
                .find(|p| p.author == "alice")
                .map(|p| p.assigned_count),
            Some(0)
        );
        registry
            .thread_mut(AnnotationId::new(1))
            .expect("thread")
            .set_assignee(Some("alice"), "t6");
        let roster = participants(&registry);
        let alice = roster.iter().find(|p| p.author == "alice").expect("alice");
        assert_eq!(alice.assigned_count, 1);
    }

    #[test]
    fn participant_status_summary_is_stable_and_includes_zero_entries() {
        let registry = populated();
        let summary = participant_status_summary(&registry);
        assert_eq!(summary, participant_status_summary(&registry));
        assert_eq!(summary.total, 3); // three replies / participations
        assert_eq!(summary.counts.get(&ReviewStatus::Resolved), Some(&2));
        assert_eq!(summary.counts.get(&ReviewStatus::Rejected), Some(&1));
        assert_eq!(summary.counts.len(), ReviewStatus::ALL.len());
        assert_eq!(summary.counts.get(&ReviewStatus::Cancelled), Some(&0));

        let empty = ReviewRegistry::new();
        let empty_summary = participant_status_summary(&empty);
        assert_eq!(empty_summary.total, 0);
        assert_eq!(empty_summary.counts.len(), ReviewStatus::ALL.len());
    }

    #[test]
    fn registry_summary_helper_still_agrees_with_the_existing_summary() {
        let registry = populated();
        let legacy = registry.summary();
        assert_eq!(legacy.total(), 2);
        assert_eq!(legacy.count(ReviewStatus::Resolved), 1);
        assert_eq!(legacy.count(ReviewStatus::Rejected), 1);
        assert_eq!(
            root_annotation_ids(&registry),
            vec![AnnotationId::new(1), AnnotationId::new(2)]
        );
    }
}
