//! Actionable errors produced by the automation model.
//!
//! Every variant carries enough context for a caller (or an end user) to
//! understand what was rejected and how to fix it. This crate has no external
//! dependencies, so [`std::fmt::Display`] and [`std::error::Error`] are
//! implemented directly rather than through a derive macro.

use std::fmt;

/// An error produced while building, validating, serializing, or running an
/// automation model.
///
/// The variants are deliberately specific and self-describing: each one names
/// the offending action, step, job, or file and states the corrective action in
/// its [`Display`](fmt::Display) text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AutomationError {
    /// An action had no steps. Add at least one step before running it.
    EmptyAction {
        /// The name of the empty action.
        name: String,
    },
    /// An action's name was empty or whitespace only. Give the action a name.
    EmptyActionName,
    /// A step had an empty id. Every step needs a unique, non-empty id.
    EmptyStepId {
        /// The name of the action containing the invalid step.
        action: String,
    },
    /// Two steps in one action shared an id. Rename one of the steps.
    DuplicateStepId {
        /// The name of the action containing the duplicate.
        action: String,
        /// The duplicated step id.
        id: String,
    },
    /// A parameter had an empty key. Give the parameter a non-empty key.
    EmptyParameterKey {
        /// The id of the step containing the invalid parameter.
        step: String,
    },
    /// A step-kind string was not recognized. Use one of the accepted kinds.
    UnknownStepKind {
        /// The unrecognized kind string.
        value: String,
    },
    /// A serialized action could not be parsed.
    InvalidSerialization {
        /// One-based line number, or `0` when the problem is not line-specific.
        line: usize,
        /// A human-readable explanation of the parse problem.
        reason: String,
    },
    /// Two actions in one action file shared a name. Rename one of them so the
    /// name can be used to select a single action unambiguously.
    DuplicateActionName {
        /// The duplicated action name.
        name: String,
    },
    /// A queue operation targeted a job id that is not in the queue.
    JobNotFound {
        /// The unknown job id.
        id: String,
    },
    /// A job with this id is already in the queue. Use a unique job id.
    DuplicateJobId {
        /// The duplicated job id.
        id: String,
    },
    /// A step failed while a job was running.
    StepFailed {
        /// The id of the job whose step failed.
        job: String,
        /// The id of the step that failed.
        step: String,
        /// The underlying failure message.
        message: String,
    },
}

impl fmt::Display for AutomationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyAction { name } => {
                write!(f, "action '{name}' has no steps; add at least one step")
            }
            Self::EmptyActionName => {
                write!(f, "action name must not be empty; give the action a name")
            }
            Self::EmptyStepId { action } => {
                write!(
                    f,
                    "action '{action}' has a step with an empty id; assign a unique id"
                )
            }
            Self::DuplicateStepId { action, id } => write!(
                f,
                "action '{action}' already has a step with id '{id}'; use a unique step id"
            ),
            Self::EmptyParameterKey { step } => {
                write!(
                    f,
                    "step '{step}' has a parameter with an empty key; assign a key"
                )
            }
            Self::UnknownStepKind { value } => write!(
                f,
                "unknown step kind '{value}'; expected one of: import, convert, watermark, security, export, custom-command"
            ),
            Self::InvalidSerialization { line, reason } => {
                if *line == 0 {
                    write!(f, "invalid serialized action: {reason}")
                } else {
                    write!(f, "invalid serialized action at line {line}: {reason}")
                }
            }
            Self::DuplicateActionName { name } => write!(
                f,
                "action file contains two actions named '{name}'; rename one so it can be selected by name"
            ),
            Self::JobNotFound { id } => {
                write!(f, "no job with id '{id}' exists in this queue")
            }
            Self::DuplicateJobId { id } => {
                write!(
                    f,
                    "a job with id '{id}' is already queued; use a unique job id"
                )
            }
            Self::StepFailed { job, step, message } => {
                write!(f, "job '{job}' failed at step '{step}': {message}")
            }
        }
    }
}

impl std::error::Error for AutomationError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_name_the_offending_value() {
        let message = AutomationError::DuplicateStepId {
            action: "nightly".to_owned(),
            id: "export".to_owned(),
        }
        .to_string();
        assert!(message.contains("nightly"), "{message}");
        assert!(message.contains("export"), "{message}");
        assert!(message.contains("unique"), "{message}");
    }

    #[test]
    fn unknown_kind_lists_accepted_kinds() {
        let message = AutomationError::UnknownStepKind {
            value: "teleport".to_owned(),
        }
        .to_string();
        for kind in [
            "import",
            "convert",
            "watermark",
            "security",
            "export",
            "custom-command",
        ] {
            assert!(message.contains(kind), "{message}");
        }
    }
}
