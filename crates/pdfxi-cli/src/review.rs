//! `pdfxi review status`: report the review tracker over a real annotated PDF.
//!
//! The tracker model lives in `pdfxi-edit::review`. This module is the CLI
//! host: it reads the annotations already saved in a document, rebuilds the
//! root/reply structure (`/IRT` links replies to their root), maps each
//! annotation's `/State` into a review status, and prints the status board,
//! participant roster and deterministic digest.
//!
//! It is a reader. Nothing is written, and a document with no annotations
//! reports an empty tracker rather than inventing threads.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, bail};
use pdfxi_core::Document;
use pdfxi_edit::{
    AnnotationId, AnnotationView, CommentThread, PageIndex, Reply, ReplyId, ReviewRegistry,
    ReviewState, ReviewStatus, ReviewTracker, ThreadId, list_annotations, participants,
};
use serde::Serialize;

/// One status bucket in the JSON report.
#[derive(Serialize)]
struct StatusBucket {
    /// Stable lowercase status name.
    status: &'static str,
    /// Thread ids in registry order.
    threads: Vec<u64>,
}

/// One participant row in the JSON report.
#[derive(Serialize)]
struct RosterRow {
    /// Author name as it appears on replies, assignments and transitions.
    author: String,
    /// Replies authored by this participant.
    replies: usize,
    /// Distinct threads this participant replied to.
    threads: usize,
    /// Threads currently assigned to this participant.
    assigned: usize,
    /// Transitions performed by this participant.
    transitions: usize,
}

/// JSON shape of `pdfxi review status`.
#[derive(Serialize)]
struct ReviewReport {
    /// Input file.
    file: String,
    /// Leaf page count.
    pages: usize,
    /// Registered threads.
    threads: usize,
    /// Replies across all threads.
    replies: usize,
    /// Thread ids grouped by status in declaration order.
    board: Vec<StatusBucket>,
    /// Participant roster sorted by author.
    roster: Vec<RosterRow>,
    /// Stable digest over counts, roster and board.
    digest: String,
}

/// Map a PDF `/State` name to a review status.
///
/// `Accepted` and `Marked` mean the comment was addressed but not yet
/// accepted, which is the tracker's `Resolved`. A missing or unknown state is
/// an open comment.
fn status_from_state(state: Option<&str>) -> ReviewStatus {
    match state {
        Some("Accepted" | "Marked") => ReviewStatus::Resolved,
        Some("Completed") => ReviewStatus::Completed,
        Some("Rejected") => ReviewStatus::Rejected,
        Some("Cancelled") => ReviewStatus::Cancelled,
        _ => ReviewStatus::Open,
    }
}

/// Load a document and its page count.
fn load(file: &Path) -> anyhow::Result<(Document, usize)> {
    let bytes = std::fs::read(file).with_context(|| format!("read {}", file.display()))?;
    let document = Document::from_bytes(bytes)
        .map_err(|error| anyhow::anyhow!("parse {}: {error}", file.display()))?;
    let pages = document
        .page_count()
        .map_err(|error| anyhow::anyhow!("count pages: {error}"))?;
    if pages == 0 {
        bail!("{} has no pages", file.display());
    }
    Ok((document, pages))
}

/// Build a tracker registry from the annotations of a real document.
///
/// Roots are annotations without an `/IRT` parent; replies are annotations
/// whose `/IRT` names a root's `/NM`. Roots keep document order, and every
/// reply is attached to the thread its parent names. Orphan replies (an `/IRT`
/// that names nothing on the page) are reported by their own thread so no
/// content is silently dropped.
fn registry_from_document(
    document: &Document,
    pages: usize,
) -> anyhow::Result<(ReviewRegistry, usize)> {
    let mut roots: Vec<(usize, AnnotationView)> = Vec::new();
    let mut replies: Vec<(usize, AnnotationView)> = Vec::new();
    for page in 0..pages {
        for view in list_annotations(document, page)
            .with_context(|| format!("list annotations on page {}", page + 1))?
        {
            if view.parent_id.is_some() {
                replies.push((page, view));
            } else {
                roots.push((page, view));
            }
        }
    }

    // The `/IRT` link uses `/NM` identifiers; index roots by id for replies.
    let mut by_id: BTreeMap<String, AnnotationId> = BTreeMap::new();
    for (_, view) in &roots {
        if !view.id.is_empty() {
            by_id.insert(
                view.id.clone(),
                AnnotationId::new(u64::from(view.object.number)),
            );
        }
    }

    let mut registry = ReviewRegistry::new();
    let mut next_thread = 0u64;
    for (page, view) in &roots {
        let root = AnnotationId::new(u64::from(view.object.number));
        let created = view.date.clone().unwrap_or_default();
        let status = status_from_state(view.state.as_deref());
        let state = ReviewState::from_parts(status, None, created.clone(), created, Vec::new());
        let thread = CommentThread::from_parts(ThreadId::new(next_thread), root, Vec::new(), state);
        next_thread += 1;
        registry.add_thread(thread, PageIndex::new(*page))?;
    }

    let mut reply_count = 0usize;
    let mut next_reply = 0u64;
    for (page, view) in &replies {
        let Some(parent) = view.parent_id.as_deref() else {
            continue;
        };
        let root = match by_id.get(parent) {
            Some(root) => *root,
            None => {
                // An orphan reply still gets its own thread so it is visible.
                let root = AnnotationId::new(u64::from(view.object.number));
                let created = view.date.clone().unwrap_or_default();
                let status = status_from_state(view.state.as_deref());
                let state =
                    ReviewState::from_parts(status, None, created.clone(), created, Vec::new());
                let thread =
                    CommentThread::from_parts(ThreadId::new(next_thread), root, Vec::new(), state);
                next_thread += 1;
                registry.add_thread(thread, PageIndex::new(*page))?;
                root
            }
        };
        let reply = Reply::new(
            ReplyId::new(next_reply),
            view.author.clone(),
            view.date.clone().unwrap_or_default(),
            view.contents.clone(),
            status_from_state(view.state.as_deref()),
        );
        next_reply += 1;
        reply_count += 1;
        registry.add_reply(
            root,
            reply.author(),
            reply.timestamp(),
            reply.body(),
            reply.status(),
        )?;
    }
    Ok((registry, reply_count))
}

/// Render the tracker as a stable report.
fn report(file: &Path, pages: usize, registry: &ReviewRegistry, replies: usize) -> ReviewReport {
    let tracker = ReviewTracker::new(registry);
    let board = tracker
        .status_board()
        .into_iter()
        .map(|(status, ids)| StatusBucket {
            status: status.as_str(),
            threads: ids.iter().map(|id| id.get()).collect(),
        })
        .collect();
    let roster = participants(registry)
        .into_iter()
        .map(|participant| RosterRow {
            author: participant.author,
            replies: participant.reply_count,
            threads: participant.thread_count,
            assigned: participant.assigned_count,
            transitions: participant.transition_count,
        })
        .collect();
    ReviewReport {
        file: file.display().to_string(),
        pages,
        threads: registry.len(),
        replies,
        board,
        roster,
        digest: tracker.digest(),
    }
}

/// `pdfxi review status`.
pub(crate) fn run_status(file: &Path, json: bool) -> anyhow::Result<()> {
    let (document, pages) = load(file)?;
    let (registry, replies) = registry_from_document(&document, pages)?;
    let report = report(file, pages, &registry, replies);
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!("file:    {}", report.file);
        println!("pages:   {}", report.pages);
        println!("threads: {}", report.threads);
        println!("replies: {}", report.replies);
        for bucket in &report.board {
            let ids: Vec<String> = bucket.threads.iter().map(u64::to_string).collect();
            println!("{}: [{}]", bucket.status, ids.join(","));
        }
        for participant in &report.roster {
            println!(
                "participant {} replies={} threads={} assigned={} transitions={}",
                participant.author,
                participant.replies,
                participant.threads,
                participant.assigned,
                participant.transitions
            );
        }
        println!("digest:  {}", report.digest);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::status_from_state;
    use pdfxi_edit::ReviewStatus;

    #[test]
    fn pdf_states_map_to_tracker_statuses() {
        assert_eq!(status_from_state(Some("Accepted")), ReviewStatus::Resolved);
        assert_eq!(status_from_state(Some("Marked")), ReviewStatus::Resolved);
        assert_eq!(
            status_from_state(Some("Completed")),
            ReviewStatus::Completed
        );
        assert_eq!(status_from_state(Some("Rejected")), ReviewStatus::Rejected);
        assert_eq!(
            status_from_state(Some("Cancelled")),
            ReviewStatus::Cancelled
        );
        assert_eq!(status_from_state(Some("Unmarked")), ReviewStatus::Open);
        assert_eq!(status_from_state(None), ReviewStatus::Open);
    }
}
