//! Structured errors for the comment-thread and review-workflow model.
//!
//! Every validation, transition, registry, or dictionary-conversion failure is
//! reported as a [`ReviewError`]. The variants are deliberately fine grained so
//! callers can distinguish, for example, an invalid workflow transition from a
//! duplicate identifier without inspecting human-readable strings.

use core::fmt;

use crate::annotation::AnnotationId;

use super::{ReplyId, ReviewStatus, ThreadId};

/// A failure while building, mutating, or serializing review objects.
#[derive(Debug, Clone, PartialEq)]
pub enum ReviewError {
    /// A required dictionary key was absent.
    MissingKey(&'static str),
    /// A dictionary value had an unexpected type.
    WrongType {
        /// Key whose value was malformed.
        key: String,
        /// Human-readable name of the expected value type.
        expected: &'static str,
    },
    /// A status name did not match any [`ReviewStatus`].
    UnknownStatus(String),
    /// A workflow transition was not permitted from the current status.
    InvalidTransition {
        /// Status the thread was in.
        from: ReviewStatus,
        /// Status the caller attempted to move to.
        to: ReviewStatus,
    },
    /// A thread id appeared more than once in a registry or dictionary.
    DuplicateThreadId(ThreadId),
    /// More than one thread claimed the same root annotation.
    DuplicateRoot(AnnotationId),
    /// A reply id appeared more than once within a thread.
    DuplicateReplyId(ReplyId),
    /// A thread was referenced that is not present in the registry.
    UnknownThread(AnnotationId),
    /// A reply was referenced that is not present in its thread.
    UnknownReplyId(ReplyId),
    /// A thread contained no replies where at least one was required.
    EmptyThread(ThreadId),
    /// An operation that needs at least one annotation received none.
    EmptyInput,
    /// An id could not be represented as a non-negative integer.
    NegativeId(i64),
    /// An unsigned id was too large to store in the intermediate dictionary.
    IdTooLarge(u64),
}

impl fmt::Display for ReviewError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingKey(key) => write!(f, "missing required review key `/{key}`"),
            Self::WrongType { key, expected } => {
                write!(f, "review key `/{key}` must be {expected}")
            }
            Self::UnknownStatus(name) => write!(f, "unknown review status `{name}`"),
            Self::InvalidTransition { from, to } => {
                write!(f, "invalid review transition from `{from}` to `{to}`")
            }
            Self::DuplicateThreadId(id) => write!(f, "duplicate thread id {}", id.get()),
            Self::DuplicateRoot(id) => {
                write!(f, "duplicate thread root annotation id {}", id.get())
            }
            Self::DuplicateReplyId(id) => write!(f, "duplicate reply id {}", id.get()),
            Self::UnknownThread(id) => {
                write!(f, "no thread is keyed by annotation id {}", id.get())
            }
            Self::UnknownReplyId(id) => write!(f, "reply id {} is not in the thread", id.get()),
            Self::EmptyThread(id) => {
                write!(f, "thread {} has no replies", id.get())
            }
            Self::EmptyInput => write!(f, "at least one annotation is required"),
            Self::NegativeId(value) => {
                write!(f, "review id {value} must not be negative")
            }
            Self::IdTooLarge(value) => write!(f, "review id {value} is too large to serialize"),
        }
    }
}

impl std::error::Error for ReviewError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::annotation::AnnotationId;

    #[test]
    fn review_error_messages_are_descriptive() {
        let cases = [
            ReviewError::MissingKey("XI_ThreadItems"),
            ReviewError::WrongType {
                key: "XI_ThreadId".into(),
                expected: "an integer id",
            },
            ReviewError::UnknownStatus("bogus".into()),
            ReviewError::InvalidTransition {
                from: ReviewStatus::Cancelled,
                to: ReviewStatus::Completed,
            },
            ReviewError::DuplicateThreadId(ThreadId::new(1)),
            ReviewError::DuplicateRoot(AnnotationId::new(2)),
            ReviewError::DuplicateReplyId(ReplyId::new(3)),
            ReviewError::UnknownThread(AnnotationId::new(4)),
            ReviewError::UnknownReplyId(ReplyId::new(5)),
            ReviewError::EmptyThread(ThreadId::new(6)),
            ReviewError::EmptyInput,
            ReviewError::NegativeId(-1),
            ReviewError::IdTooLarge(u64::MAX),
        ];
        for case in cases {
            assert!(!case.to_string().is_empty());
        }
    }
}
