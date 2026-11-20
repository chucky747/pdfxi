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
use crate::pdf::{Dict, Value};

use super::error::ReviewError;
use super::registry::ReviewRegistry;
use super::serialize::{dict_to_registry, registry_to_dict};
use super::thread::{CommentThread, Reply, ThreadId};
use super::workflow::ReviewStatus;

/// Dictionary version accepted by [`import_registry_checked`].
pub const REVIEW_REGISTRY_VERSION: i64 = 1;

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

/// Imports a registry after validating its declared dictionary version.
///
/// [`import_registry`] treats `XI_ThreadVersion` as informational; this checked
/// entry point rejects a missing, non-integer, or unsupported version before
/// parsing the thread items so callers can surface a precise
/// [`ReviewError::UnsupportedVersion`] instead of a downstream parse error.
pub fn import_registry_checked(dict: &Dict) -> Result<ReviewRegistry, ReviewError> {
    match dict.get("XI_ThreadVersion") {
        Some(Value::Int(version)) if *version == REVIEW_REGISTRY_VERSION => {}
        Some(Value::Int(version)) => return Err(ReviewError::UnsupportedVersion(*version)),
        Some(_) => {
            return Err(ReviewError::WrongType {
                key: "XI_ThreadVersion".to_string(),
                expected: "an integer version",
            });
        }
        None => return Err(ReviewError::MissingKey("XI_ThreadVersion")),
    }
    import_registry(dict)
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

/// One typed line of a registry print listing (REVIEW-005).
///
/// The variants carry structured values instead of pre-formatted strings, so a
/// caller can render a registry for a text pane, a table, or a printer without
/// parsing the human-readable output. Every variant renders to exactly the
/// same string produced by [`registry_to_print_lines`] for the same input.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PrintLine {
    /// Header line carrying the total number of threads.
    Header {
        /// Number of threads in the registry.
        thread_count: usize,
    },
    /// Summary row for one thread.
    Thread {
        /// Thread identifier.
        id: u64,
        /// Root annotation identifier.
        root: u64,
        /// Page number, absent when the root has no registered page.
        page: Option<usize>,
        /// Current workflow status.
        status: ReviewStatus,
        /// Current assignee, if one is set.
        assignee: Option<String>,
    },
    /// Creation/update metadata for one thread.
    ThreadMeta {
        /// Creation timestamp.
        created: String,
        /// Last update timestamp.
        updated: String,
        /// Number of replies in the thread.
        replies: usize,
    },
    /// One reply row.
    Reply {
        /// Reply identifier.
        id: u64,
        /// Author of the reply.
        author: String,
        /// Reply timestamp.
        timestamp: String,
        /// Status attached to the reply.
        status: ReviewStatus,
        /// Reply body text.
        body: String,
    },
    /// One transition-history row.
    History {
        /// Timestamp of the change.
        timestamp: String,
        /// Status moved from, absent for the initial entry.
        from: Option<ReviewStatus>,
        /// Status moved to.
        to: ReviewStatus,
        /// Actor that performed the change.
        actor: String,
        /// Free-form note recorded with the change.
        note: String,
    },
}

impl PrintLine {
    /// Renders this line in the stable, self-describing print format.
    pub fn render(&self) -> String {
        match self {
            Self::Header { thread_count } => {
                format!("Review registry: {thread_count} thread(s)")
            }
            Self::Thread {
                id,
                root,
                page,
                status,
                assignee,
            } => {
                let page = page
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "?".to_string());
                format!(
                    "Thread {id} root={root} page={page} status={} assignee={}",
                    status.as_str(),
                    assignee.as_deref().unwrap_or("-")
                )
            }
            Self::ThreadMeta {
                created,
                updated,
                replies,
            } => {
                format!("  created={created} updated={updated} replies={replies}")
            }
            Self::Reply {
                id,
                author,
                timestamp,
                status,
                body,
            } => {
                format!(
                    "    reply {id} by {author} at {timestamp} [{}]: {body}",
                    status.as_str()
                )
            }
            Self::History {
                timestamp,
                from,
                to,
                actor,
                note,
            } => {
                let from = from.map(ReviewStatus::as_str).unwrap_or("initial");
                format!(
                    "    history {timestamp} {from}->{} by {actor}: {note}",
                    to.as_str()
                )
            }
        }
    }
}

/// Builds a registry print listing as typed [`PrintLine`] values (REVIEW-005).
///
/// Threads follow root-annotation order, replies follow canonical
/// `(timestamp, id)` order, and history is chronological, so the result is
/// deterministic and byte-stable when rendered.
pub fn registry_to_typed_print_lines(registry: &ReviewRegistry) -> Vec<PrintLine> {
    let mut lines = vec![PrintLine::Header {
        thread_count: registry.len(),
    }];
    for thread in registry.threads() {
        lines.push(PrintLine::Thread {
            id: thread.id().get(),
            root: thread.root().get(),
            page: registry.page_of(thread.root()).map(|index| index.get()),
            status: thread.status(),
            assignee: thread.assignee().map(str::to_string),
        });
        lines.push(PrintLine::ThreadMeta {
            created: thread.created().to_string(),
            updated: thread.updated().to_string(),
            replies: thread.reply_count(),
        });
        for reply in thread.replies() {
            lines.push(PrintLine::Reply {
                id: reply.id().get(),
                author: reply.author().to_string(),
                timestamp: reply.timestamp().to_string(),
                status: reply.status(),
                body: reply.body().to_string(),
            });
        }
        for entry in thread.history() {
            lines.push(PrintLine::History {
                timestamp: entry.timestamp().to_string(),
                from: entry.from(),
                to: entry.to(),
                actor: entry.actor().to_string(),
                note: entry.note().to_string(),
            });
        }
    }
    lines
}

/// Renders typed print lines into the trailing-newline text view.
pub fn typed_print_lines_to_text(lines: &[PrintLine]) -> String {
    let mut text = lines
        .iter()
        .map(PrintLine::render)
        .collect::<Vec<String>>()
        .join("\n");
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

/// A deterministic review tracker view over a registry (REVIEW-007).
///
/// The tracker combines the registry status summary, a status board of thread
/// ids, and the participant roster into a single stable view. It stores only a
/// borrow of the registry, so constructing a tracker never copies or mutates
/// source data.
#[derive(Clone, Debug)]
pub struct ReviewTracker<'a> {
    registry: &'a ReviewRegistry,
}

impl<'a> ReviewTracker<'a> {
    /// Wraps `registry` in a tracker view.
    pub fn new(registry: &'a ReviewRegistry) -> Self {
        Self { registry }
    }

    /// The registry this tracker reads.
    pub fn registry(&self) -> &'a ReviewRegistry {
        self.registry
    }

    /// Thread ids grouped by status in [`ReviewStatus::ALL`] order (REVIEW-007).
    ///
    /// Every status has an entry, including those with no threads, and the ids
    /// within each bucket follow the registry's deterministic root order.
    pub fn status_board(&self) -> Vec<(ReviewStatus, Vec<ThreadId>)> {
        ReviewStatus::ALL
            .iter()
            .map(|status| {
                (
                    *status,
                    self.registry
                        .threads()
                        .filter(|thread| thread.status() == *status)
                        .map(CommentThread::id)
                        .collect(),
                )
            })
            .collect()
    }

    /// The sorted participant roster derived from the registry.
    pub fn roster(&self) -> Vec<ParticipantSummary> {
        participants(self.registry)
    }

    /// A stable textual digest of the whole tracker view (REVIEW-007).
    ///
    /// The digest concatenates per-status thread counts, per-participant
    /// reply/assignment counts, and the status board. Repeated calls, and
    /// registries that differ only in reply insertion order or that were
    /// exported and re-imported, produce the same digest.
    pub fn digest(&self) -> String {
        let summary = self.registry.summary();
        let counts: Vec<String> = ReviewStatus::ALL
            .iter()
            .map(|status| format!("{}:{}", status.as_str(), summary.count(*status)))
            .collect();
        let roster: Vec<String> = self
            .roster()
            .into_iter()
            .map(|participant| {
                format!(
                    "{}:{}:{}",
                    participant.author, participant.reply_count, participant.assigned_count
                )
            })
            .collect();
        let board: Vec<String> = self
            .status_board()
            .into_iter()
            .map(|(status, ids)| {
                let ids: Vec<String> = ids.iter().map(|id| id.get().to_string()).collect();
                format!("{}:[{}]", status.as_str(), ids.join(","))
            })
            .collect();
        format!(
            "{}|{}|{}",
            counts.join(","),
            roster.join(","),
            board.join(",")
        )
    }
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

    #[test]
    fn typed_print_lines_match_the_stable_text_view() {
        let registry = populated();
        let typed = registry_to_typed_print_lines(&registry);
        assert_eq!(typed, registry_to_typed_print_lines(&registry));
        assert!(matches!(
            typed.first(),
            Some(PrintLine::Header { thread_count: 2 })
        ));
        let rendered: Vec<String> = typed.iter().map(PrintLine::render).collect();
        assert_eq!(rendered, registry_to_print_lines(&registry));
        assert_eq!(
            typed_print_lines_to_text(&typed),
            registry_to_text(&registry)
        );
    }

    #[test]
    fn typed_print_lines_cover_threads_replies_and_history() {
        let registry = populated();
        let typed = registry_to_typed_print_lines(&registry);
        let text = typed_print_lines_to_text(&typed);
        assert!(text.ends_with('\n'));
        assert_eq!(text.lines().count(), typed.len());
        assert!(typed.iter().any(|line| matches!(
            line,
            PrintLine::Thread {
                status: ReviewStatus::Resolved,
                ..
            }
        )));
        assert!(typed.iter().any(|line| matches!(
            line,
            PrintLine::Reply { author, .. } if author == "alice"
        )));
        assert!(text.contains("history t4 open->resolved by alice: done"));
    }

    #[test]
    fn checked_import_validates_dictionary_version() {
        let registry = populated();
        let dict = export_registry(&registry);
        assert_eq!(
            import_registry_checked(&dict).expect("valid import"),
            registry
        );

        let mut wrong = dict.clone();
        wrong.insert("XI_ThreadVersion".to_string(), Value::Int(2));
        assert_eq!(
            import_registry_checked(&wrong),
            Err(ReviewError::UnsupportedVersion(2))
        );

        let mut bad_type = dict.clone();
        bad_type.insert("XI_ThreadVersion".to_string(), Value::Text("1".to_string()));
        assert_eq!(
            import_registry_checked(&bad_type),
            Err(ReviewError::WrongType {
                key: "XI_ThreadVersion".to_string(),
                expected: "an integer version",
            })
        );

        let mut missing = dict;
        missing.remove("XI_ThreadVersion");
        assert_eq!(
            import_registry_checked(&missing),
            Err(ReviewError::MissingKey("XI_ThreadVersion"))
        );
    }

    #[test]
    fn tracker_status_board_and_roster_are_ordered_and_complete() {
        let registry = populated();
        let tracker = ReviewTracker::new(&registry);
        assert_eq!(tracker.registry(), &registry);

        let board = tracker.status_board();
        let statuses: Vec<ReviewStatus> = board.iter().map(|(status, _)| *status).collect();
        assert_eq!(statuses, ReviewStatus::ALL);
        let occupancy: Vec<usize> = board.iter().map(|(_, ids)| ids.len()).collect();
        assert_eq!(occupancy, vec![0, 1, 1, 0, 0]);
        assert_eq!(
            board[1].1,
            vec![registry.threads().next().map(|t| t.id()).expect("thread")]
        );

        let roster = tracker.roster();
        let authors: Vec<&str> = roster.iter().map(|p| p.author.as_str()).collect();
        assert_eq!(authors, vec!["alice", "bob"]);
    }

    #[test]
    fn tracker_digest_is_deterministic_across_insertion_order_and_round_trip() {
        let build = |first_bob: bool| {
            let annotations = [annotation(1, 0), annotation(2, 1)];
            let mut registry =
                ReviewRegistry::from_annotations(annotations.iter(), "t0").expect("registry");
            let order: &[(u64, &str, &str)] = if first_bob {
                &[(2, "bob", "t2"), (1, "alice", "t1")]
            } else {
                &[(1, "alice", "t1"), (2, "bob", "t2")]
            };
            for (root, author, timestamp) in order {
                registry
                    .add_reply(
                        AnnotationId::new(*root),
                        *author,
                        *timestamp,
                        "note",
                        ReviewStatus::Open,
                    )
                    .expect("reply");
            }
            registry
        };
        let left = build(false);
        let right = build(true);
        let left_digest = ReviewTracker::new(&left).digest();
        assert_eq!(left_digest, ReviewTracker::new(&left).digest());
        assert_eq!(left_digest, ReviewTracker::new(&right).digest());

        let restored = import_registry(&export_registry(&left)).expect("round-trip");
        assert_eq!(left_digest, ReviewTracker::new(&restored).digest());
        assert!(!left_digest.is_empty());
    }
}
