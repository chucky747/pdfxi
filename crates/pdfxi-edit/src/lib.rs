//! Page operations, content editing, annotations, watermarks, Bates numbering, and redaction.
//!
//! This crate owns the authoring-side document model. It is renderer-agnostic
//! and depends only on the standard library: [`geometry`] defines validated
//! page-space primitives, [`annotation`] defines the review and drawing
//! annotation model with hit-testing, bounds, and clamping helpers, and [`pdf`]
//! maps annotations to and from a dependency-free PDF dictionary intermediate
//! with validation and deterministic appearance geometry, and [`navigation`]
//! models bookmarks, links, and attachments with the same dictionary
//! round-trip approach. [`review`] models comment threads, workflow states, and
//! a filterable review registry with a dictionary round-trip.
//!
//! Edits in this crate are copy-on-write by contract; no API here mutates
//! source document bytes in place.

#![deny(missing_docs)]
#![forbid(unsafe_code)]

pub mod annotation;
pub mod geometry;
pub mod navigation;
pub mod pdf;
pub mod review;

pub use annotation::{
    Annotation, AnnotationError, AnnotationId, AnnotationKind, Color, bounds_union,
};
pub use geometry::{GeometryError, PageGeometry, PageIndex, Point, Rect};
pub use navigation::{
    Attachment, Bookmark, Destination, FitType, Link, LinkBorder, LinkHighlight, LinkTarget,
    MAX_NAME_BYTES, NameTree, NavigationError, Outline, dict_to_link, dict_to_name_tree,
    dict_to_outline, link_to_dict, name_tree_to_dict, outline_to_dict,
};
pub use review::{
    CommentThread, HistoryEntry, Reply, ReplyId, ReviewError, ReviewFilter, ReviewRegistry,
    ReviewState, ReviewStatus, ReviewSummary, ThreadId, dict_to_registry, registry_to_dict,
};

/// Return the implementation name for diagnostics.
pub fn engine_name() -> &'static str {
    "pdfxi-edit"
}
