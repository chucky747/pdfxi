//! Comment threads and review workflow states.
//!
//! This module extends `pdfxi-edit` with the authoring-side review model used
//! by Acrobat-style comment workflows. It stays dependency-free and reuses the
//! [`Dict`](crate::pdf::Dict)/[`Value`] intermediate from
//! [`crate::pdf`] for serialization.
//!
//! - The `thread` module defines [`ThreadId`], [`ReplyId`], [`Reply`], and
//!   [`CommentThread`], with replies kept in deterministic
//!   `(timestamp, id)` order and threads keyed by their root annotation.
//! - The `workflow` module defines the five review statuses, the closed
//!   transition table, [`ReviewState`], and an append-only history log.
//! - The `registry` module groups annotations into threads, tracks per-page
//!   state, supports filtering, and derives stable per-status summaries.
//! - The `serialize` module round-trips a whole registry through a dictionary
//!   using the private `XI_Thread...` key namespace while preserving unknown
//!   keys.
//! - The `report` module adds documented import/export and print views (with a
//!   typed `PrintLine` format and a version-checked import) plus a sorted
//!   participant/status summary and a deterministic `ReviewTracker` view.
//! - The `share` module adds a local, network-free shared-review file format,
//!   a deterministic merge of two files with conflict reporting, and
//!   participant collaboration states (REVIEW-006).
//!
//! All edits here are copy-on-write by contract: the model owns its data and
//! never mutates source document bytes.

mod error;
mod registry;
mod report;
mod serialize;
mod share;
mod thread;
mod workflow;

pub use error::ReviewError;
pub use registry::{ReviewFilter, ReviewRegistry, ReviewSummary};
pub use report::{
    ParticipantStatusSummary, ParticipantSummary, PrintLine, REVIEW_REGISTRY_VERSION,
    ReviewTracker, export_registry, import_registry, import_registry_checked,
    participant_status_summary, participants, print_registry, registry_to_print_lines,
    registry_to_text, registry_to_typed_print_lines, root_annotation_ids,
    typed_print_lines_to_text,
};
pub use serialize::{dict_to_registry, registry_to_dict};
pub use share::{
    ConflictResolution, MergeConflict, MergeResult, ParticipantState, SHARED_REVIEW_VERSION,
    ShareError, SharedParticipant, SharedReviewFile, derive_roster, merge_shared,
};
pub use thread::{CommentThread, Reply, ReplyId, ThreadId};
pub use workflow::{HistoryEntry, ReviewState, ReviewStatus};

use crate::pdf::Value;
