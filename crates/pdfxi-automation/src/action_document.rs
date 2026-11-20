//! Versioned, deterministic text documents for Action Wizard sequences.
//!
//! This module adds a small serialization layer on top of the existing
//! [`ActionSequence`] model. An [`ActionDocument`] is a versioned envelope that
//! carries one sequence and can be exported to, and imported from, a stable
//! line-oriented text format with no external dependency. The format is
//! byte-for-byte deterministic: exporting a document, importing it, and
//! exporting again yields the identical string.
//!
//! # Format
//!
//! ```text
//! PDFXI-ACTION-DOC<TAB>1
//! NAME<TAB><escaped-name>
//! BUDGET<TAB><fuel><TAB><calls><TAB><loop-iterations><TAB><string-len>
//! STEP<TAB><escaped-id><TAB><kind><TAB><escaped-argument>...
//! END
//! ```
//!
//! `kind` is one of `run-script`, `set-metadata`, `run-command`, or
//! `unsupported`, matching [`WizardStepKind::kind_str`]. Argument counts follow
//! the step kind: `run-script`/`run-command`/`unsupported` take one argument and
//! `set-metadata` takes two. Backslash, tab, newline, and carriage return in
//! values are escaped so round trips are lossless.
//!
//! The version header is checked on import: a document written for a different
//! format version is rejected with a typed
//! [`ActionDocumentError::UnsupportedVersion`] rather than silently guessed.

use crate::action_wizard::{ActionSequence, ScriptBudget, WizardStep, WizardStepKind};
use std::fmt;

/// The text format version understood by this module.
pub const ACTION_DOCUMENT_VERSION: u32 = 1;

/// A typed reason an [`ActionDocument`] import or validation failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActionDocumentError {
    /// The input contained no lines at all.
    EmptyInput,
    /// The text did not follow the documented record grammar.
    InvalidSyntax {
        /// One-based line number of the offending record.
        line: usize,
        /// Human-readable explanation.
        reason: String,
    },
    /// The version header did not match [`ACTION_DOCUMENT_VERSION`].
    UnsupportedVersion {
        /// Version found in the document.
        found: u32,
        /// Version this build supports.
        supported: u32,
    },
    /// The sequence name was empty or whitespace only.
    InvalidName,
    /// A numeric budget field was missing or malformed.
    InvalidBudget {
        /// One-based line number of the budget record.
        line: usize,
        /// Name of the malformed field.
        field: &'static str,
    },
    /// The wrapped [`ActionSequence`] failed structural validation.
    Validation {
        /// Stable machine-readable code from [`crate::WizardValidationError::code`].
        code: &'static str,
        /// Human-readable explanation.
        message: String,
    },
}

impl ActionDocumentError {
    /// Returns a stable machine-readable code for this error.
    pub const fn code(&self) -> &'static str {
        match self {
            Self::EmptyInput => "empty_input",
            Self::InvalidSyntax { .. } => "invalid_syntax",
            Self::UnsupportedVersion { .. } => "unsupported_version",
            Self::InvalidName => "invalid_name",
            Self::InvalidBudget { .. } => "invalid_budget",
            Self::Validation { .. } => "validation",
        }
    }
}

impl fmt::Display for ActionDocumentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyInput => f.write_str("empty action document; expected a version header"),
            Self::InvalidSyntax { line, reason } => {
                write!(f, "invalid action document at line {line}: {reason}")
            }
            Self::UnsupportedVersion { found, supported } => write!(
                f,
                "unsupported action document version {found}; this build supports {supported}"
            ),
            Self::InvalidName => f.write_str("action document name must not be empty"),
            Self::InvalidBudget { line, field } => {
                write!(f, "invalid budget field '{field}' at line {line}")
            }
            Self::Validation { code, message } => {
                write!(f, "invalid action sequence ({code}): {message}")
            }
        }
    }
}

impl std::error::Error for ActionDocumentError {}

/// A versioned, serializable envelope around one [`ActionSequence`].
///
/// Construct one with [`ActionDocument::from_sequence`] or import one with
/// [`ActionDocument::from_text`]. Both paths validate the wrapped sequence so a
/// live document is always structurally sound.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionDocument {
    sequence: ActionSequence,
}

impl ActionDocument {
    /// Wraps and validates a sequence.
    pub fn from_sequence(sequence: ActionSequence) -> Result<Self, ActionDocumentError> {
        validate_sequence(&sequence)?;
        Ok(Self { sequence })
    }

    /// Returns the wrapped sequence.
    pub fn sequence(&self) -> &ActionSequence {
        &self.sequence
    }

    /// Returns the sequence name.
    pub fn name(&self) -> &str {
        self.sequence.name()
    }

    /// Returns the format version this document is written for.
    pub fn version(&self) -> u32 {
        ACTION_DOCUMENT_VERSION
    }

    /// Re-checks all document invariants.
    pub fn validate(&self) -> Result<(), ActionDocumentError> {
        validate_sequence(&self.sequence)
    }

    /// Exports the document to the deterministic text format.
    pub fn to_text(&self) -> String {
        let mut out = String::new();
        out.push_str("PDFXI-ACTION-DOC\t");
        out.push_str(&ACTION_DOCUMENT_VERSION.to_string());
        out.push('\n');

        out.push_str("NAME\t");
        out.push_str(&escape_value(self.name()));
        out.push('\n');

        let budget = self.sequence.budget();
        out.push_str("BUDGET\t");
        out.push_str(&budget.fuel.to_string());
        out.push('\t');
        out.push_str(&budget.max_calls.to_string());
        out.push('\t');
        out.push_str(&budget.max_loop_iterations.to_string());
        out.push('\t');
        out.push_str(&budget.max_string_len.to_string());
        out.push('\n');

        for step in self.sequence.steps() {
            out.push_str("STEP\t");
            out.push_str(&escape_value(step.id()));
            out.push('\t');
            match step.kind() {
                WizardStepKind::RunScript { script } => {
                    out.push_str("run-script\t");
                    out.push_str(&escape_value(script));
                }
                WizardStepKind::SetMetadata { key, value } => {
                    out.push_str("set-metadata\t");
                    out.push_str(&escape_value(key));
                    out.push('\t');
                    out.push_str(&escape_value(value));
                }
                WizardStepKind::RunCommand { command } => {
                    out.push_str("run-command\t");
                    out.push_str(&escape_value(command));
                }
                WizardStepKind::Unsupported { name } => {
                    out.push_str("unsupported\t");
                    out.push_str(&escape_value(name));
                }
            }
            out.push('\n');
        }
        out.push_str("END\n");
        out
    }

    /// Imports a document from the text format produced by [`Self::to_text`].
    ///
    /// The version header, record grammar, and wrapped sequence are all
    /// validated; any problem is reported as a typed [`ActionDocumentError`].
    pub fn from_text(text: &str) -> Result<Self, ActionDocumentError> {
        let lines: Vec<&str> = text.lines().collect();
        if lines.is_empty() {
            return Err(ActionDocumentError::EmptyInput);
        }

        let mut cursor = 0usize;
        let header = fields(&lines, cursor, 2, "PDFXI-ACTION-DOC")?;
        let found = parse_u32(header[1], cursor + 1, "version")?;
        if found != ACTION_DOCUMENT_VERSION {
            return Err(ActionDocumentError::UnsupportedVersion {
                found,
                supported: ACTION_DOCUMENT_VERSION,
            });
        }
        cursor += 1;

        let name_record = require_record(&lines, cursor, "NAME", 2)?;
        let name = unescape_value(name_record[1], cursor + 1)?;
        cursor += 1;

        let budget_record = require_record(&lines, cursor, "BUDGET", 5)?;
        let line = cursor + 1;
        let budget = ScriptBudget {
            fuel: parse_u64(budget_record[1], line, "fuel")?,
            max_calls: parse_u64(budget_record[2], line, "calls")?,
            max_loop_iterations: parse_u64(budget_record[3], line, "loop_iterations")?,
            max_string_len: parse_usize(budget_record[4], line, "string_len")?,
        };
        cursor += 1;

        let mut steps: Vec<WizardStep> = Vec::new();
        let mut saw_end = false;
        while cursor < lines.len() {
            let line_number = cursor + 1;
            let parts: Vec<&str> = lines[cursor].split('\t').collect();
            if parts.first().copied() == Some("END") {
                if parts.len() != 1 {
                    return Err(ActionDocumentError::InvalidSyntax {
                        line: line_number,
                        reason: "END record must not carry fields".to_owned(),
                    });
                }
                saw_end = true;
                cursor += 1;
                break;
            }
            if parts.first().copied() != Some("STEP") || parts.len() < 3 {
                return Err(ActionDocumentError::InvalidSyntax {
                    line: line_number,
                    reason: "expected STEP<TAB><id><TAB><kind><TAB><args...> or END".to_owned(),
                });
            }
            let id = unescape_value(parts[1], line_number)?;
            let args = &parts[2..];
            let kind = match args[0] {
                "run-script" if args.len() == 2 => WizardStepKind::RunScript {
                    script: unescape_value(args[1], line_number)?,
                },
                "set-metadata" if args.len() == 3 => WizardStepKind::SetMetadata {
                    key: unescape_value(args[1], line_number)?,
                    value: unescape_value(args[2], line_number)?,
                },
                "run-command" if args.len() == 2 => WizardStepKind::RunCommand {
                    command: unescape_value(args[1], line_number)?,
                },
                "unsupported" if args.len() == 2 => WizardStepKind::Unsupported {
                    name: unescape_value(args[1], line_number)?,
                },
                other => {
                    return Err(ActionDocumentError::InvalidSyntax {
                        line: line_number,
                        reason: format!("unknown step kind '{other}' or wrong argument count"),
                    });
                }
            };
            steps.push(WizardStep::new(id, kind));
            cursor += 1;
        }

        if !saw_end {
            return Err(ActionDocumentError::InvalidSyntax {
                line: cursor + 1,
                reason: "missing END record".to_owned(),
            });
        }
        if cursor != lines.len() {
            return Err(ActionDocumentError::InvalidSyntax {
                line: cursor + 1,
                reason: "unexpected content after END record".to_owned(),
            });
        }

        let mut sequence = ActionSequence::new(name).with_budget(budget);
        for step in steps {
            sequence
                .add_step(step)
                .map_err(|error| ActionDocumentError::Validation {
                    code: error.code(),
                    message: error.to_string(),
                })?;
        }
        Self::from_sequence(sequence)
    }
}

fn validate_sequence(sequence: &ActionSequence) -> Result<(), ActionDocumentError> {
    if sequence.name().trim().is_empty() {
        return Err(ActionDocumentError::InvalidName);
    }
    sequence
        .validate()
        .map_err(|error| ActionDocumentError::Validation {
            code: error.code(),
            message: error.to_string(),
        })
}

fn fields<'a>(
    lines: &[&'a str],
    index: usize,
    expected: usize,
    tag: &str,
) -> Result<Vec<&'a str>, ActionDocumentError> {
    let line = index + 1;
    let raw = lines
        .get(index)
        .ok_or_else(|| ActionDocumentError::InvalidSyntax {
            line,
            reason: format!("missing {tag} record"),
        })?;
    let parts: Vec<&str> = raw.split('\t').collect();
    if parts.len() != expected || parts.first().copied() != Some(tag) {
        return Err(ActionDocumentError::InvalidSyntax {
            line,
            reason: format!("expected {tag} record with {expected} fields"),
        });
    }
    Ok(parts)
}

fn require_record<'a>(
    lines: &[&'a str],
    index: usize,
    tag: &str,
    expected: usize,
) -> Result<Vec<&'a str>, ActionDocumentError> {
    fields(lines, index, expected, tag)
}

fn parse_u32(value: &str, line: usize, field: &'static str) -> Result<u32, ActionDocumentError> {
    value
        .parse::<u32>()
        .map_err(|_| ActionDocumentError::InvalidBudget { line, field })
}

fn parse_u64(value: &str, line: usize, field: &'static str) -> Result<u64, ActionDocumentError> {
    value
        .parse::<u64>()
        .map_err(|_| ActionDocumentError::InvalidBudget { line, field })
}

fn parse_usize(
    value: &str,
    line: usize,
    field: &'static str,
) -> Result<usize, ActionDocumentError> {
    value
        .parse::<usize>()
        .map_err(|_| ActionDocumentError::InvalidBudget { line, field })
}

fn escape_value(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            other => out.push(other),
        }
    }
    out
}

fn unescape_value(value: &str, line: usize) -> Result<String, ActionDocumentError> {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            out.push(ch);
            continue;
        }
        match chars.next() {
            Some('\\') => out.push('\\'),
            Some('t') => out.push('\t'),
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some(other) => {
                return Err(ActionDocumentError::InvalidSyntax {
                    line,
                    reason: format!("invalid escape sequence '\\{other}'"),
                });
            }
            None => {
                return Err(ActionDocumentError::InvalidSyntax {
                    line,
                    reason: "trailing backslash in escaped value".to_owned(),
                });
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_sequence() -> ActionSequence {
        let mut sequence = ActionSequence::new("nightly").with_budget(
            ScriptBudget::default()
                .with_fuel(12_345)
                .with_max_calls(42)
                .with_max_loop_iterations(9)
                .with_max_string_len(1_024),
        );
        sequence
            .add_step(WizardStep::script("load", "var x = 1;"))
            .unwrap_or_else(|_| unreachable!());
        sequence
            .add_step(WizardStep::set_metadata("meta", "title", "A\tB"))
            .unwrap_or_else(|_| unreachable!());
        sequence
            .add_step(WizardStep::run_command("save", "run-batch"))
            .unwrap_or_else(|_| unreachable!());
        sequence
            .add_step(WizardStep::unsupported("unknown", "teleport"))
            .unwrap_or_else(|_| unreachable!());
        sequence
    }

    #[test]
    fn format_round_trip_is_lossless() {
        let document = ActionDocument::from_sequence(sample_sequence()).unwrap();
        let text = document.to_text();
        let restored = ActionDocument::from_text(&text).unwrap();
        assert_eq!(restored, document);
        assert_eq!(restored.to_text(), text);
        assert_eq!(restored.name(), "nightly");
        assert_eq!(restored.version(), ACTION_DOCUMENT_VERSION);
        assert_eq!(restored.sequence().budget().fuel, 12_345);
        assert_eq!(restored.sequence().len(), 4);
    }

    #[test]
    fn version_mismatch_is_rejected() {
        let text = ActionDocument::from_sequence(sample_sequence())
            .unwrap()
            .to_text();
        let altered = text.replacen("PDFXI-ACTION-DOC\t1", "PDFXI-ACTION-DOC\t2", 1);
        assert_eq!(
            ActionDocument::from_text(&altered),
            Err(ActionDocumentError::UnsupportedVersion {
                found: 2,
                supported: ACTION_DOCUMENT_VERSION,
            })
        );
    }

    #[test]
    fn invalid_syntax_is_typed_with_line_numbers() {
        assert!(matches!(
            ActionDocument::from_text("GARBAGE\t1\n"),
            Err(ActionDocumentError::InvalidSyntax { line: 1, .. })
        ));
        let no_end = "PDFXI-ACTION-DOC\t1\nNAME\ta\nBUDGET\t1\t1\t1\t1\nSTEP\ts\trun-script\tx\n";
        assert!(matches!(
            ActionDocument::from_text(no_end),
            Err(ActionDocumentError::InvalidSyntax { .. })
        ));
        let bad_kind =
            "PDFXI-ACTION-DOC\t1\nNAME\ta\nBUDGET\t1\t1\t1\t1\nSTEP\ts\tteleport\tx\nEND\n";
        assert!(matches!(
            ActionDocument::from_text(bad_kind),
            Err(ActionDocumentError::InvalidSyntax { line: 4, .. })
        ));
        let bad_number = "PDFXI-ACTION-DOC\t1\nNAME\ta\nBUDGET\tx\t1\t1\t1\nEND\n";
        assert_eq!(
            ActionDocument::from_text(bad_number),
            Err(ActionDocumentError::InvalidBudget {
                line: 3,
                field: "fuel"
            })
        );
    }

    #[test]
    fn invalid_escape_is_rejected() {
        let text = "PDFXI-ACTION-DOC\t1\nNAME\tbad\\q\nBUDGET\t1\t1\t1\t1\nEND\n";
        assert!(matches!(
            ActionDocument::from_text(text),
            Err(ActionDocumentError::InvalidSyntax { line: 2, .. })
        ));
    }

    #[test]
    fn empty_input_is_rejected() {
        assert_eq!(
            ActionDocument::from_text(""),
            Err(ActionDocumentError::EmptyInput)
        );
        assert!(matches!(
            ActionDocument::from_text("PDFXI-ACTION-DOC\t1\n"),
            Err(ActionDocumentError::InvalidSyntax { line: 2, .. })
        ));
    }

    #[test]
    fn duplicate_step_ids_fail_validation_on_import() {
        let text = "PDFXI-ACTION-DOC\t1\nNAME\ta\nBUDGET\t1\t1\t1\t1\n\
                    STEP\ts\trun-script\tx\nSTEP\ts\trun-command\ty\nEND\n";
        let error = ActionDocument::from_text(text).unwrap_err();
        assert_eq!(error.code(), "validation");
        assert!(error.to_string().contains("duplicate_step_id"), "{error}");
    }

    #[test]
    fn empty_name_is_rejected() {
        let sequence = ActionSequence::new("   ");
        assert_eq!(
            ActionDocument::from_sequence(sequence),
            Err(ActionDocumentError::InvalidName)
        );
    }

    #[test]
    fn special_characters_round_trip() {
        let mut sequence = ActionSequence::new("name\twith\nspecials\\");
        sequence
            .add_step(WizardStep::script("id\\1", "var s = \"line\\nbreak\";"))
            .unwrap_or_else(|_| unreachable!());
        let document = ActionDocument::from_sequence(sequence).unwrap();
        let restored = ActionDocument::from_text(&document.to_text()).unwrap();
        assert_eq!(restored, document);
    }

    #[test]
    fn format_is_deterministic_across_runs() {
        let first = ActionDocument::from_sequence(sample_sequence())
            .unwrap()
            .to_text();
        let second = ActionDocument::from_sequence(sample_sequence())
            .unwrap()
            .to_text();
        let third = ActionDocument::from_text(&first).unwrap().to_text();
        assert_eq!(first, second);
        assert_eq!(first, third);
    }

    #[test]
    fn errors_are_actionable() {
        let error = ActionDocumentError::UnsupportedVersion {
            found: 9,
            supported: 1,
        };
        assert_eq!(error.code(), "unsupported_version");
        let message = error.to_string();
        assert!(message.contains('9') && message.contains('1'), "{message}");
    }
}
