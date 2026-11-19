//! A small library of named actions loaded from one text file.
//!
//! [`Action::to_text`](crate::Action::to_text) serializes a single action. This
//! module layers a deterministic multi-action container on top of that format so
//! a file can hold several named actions for `list`, `show`, and `run`. The
//! container deliberately reuses the exact same line format: every action is a
//! block of `ACTION`/`STEP`/`PARAM` records, blocks are separated by one or more
//! blank lines, and a single-block file with no blank lines is still valid.
//!
//! Only the `ACTION` record marker starts a new block, so a title or description
//! that happens to contain the word "ACTION" cannot split a block. Parsing is
//! single-pass, preserves original line numbers in errors, and rejects duplicate
//! names so that a name always selects exactly one action.

use crate::action::Action;
use crate::error::AutomationError;
use std::collections::HashSet;

/// An ordered collection of uniquely named [`Action`]s parsed from one file.
///
/// Actions are kept in file order, which is the order used by
/// [`ActionLibrary::names`] and [`ActionLibrary::actions`]. Names are unique:
/// [`ActionLibrary::from_text`] rejects a file with two actions of the same name
/// because such a file would make name-based selection ambiguous.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionLibrary {
    actions: Vec<Action>,
}

impl ActionLibrary {
    /// Parses a file holding one or more tab-delimited action blocks.
    ///
    /// A block begins with an `ACTION\t<name>\t<description>` record and is
    /// followed by its `STEP` and `PARAM` records. Blank lines separate blocks
    /// but are ignored anywhere else. Each block is validated by
    /// [`Action::from_text`](crate::Action::from_text) before being added, so a
    /// successfully returned library contains only valid actions.
    ///
    /// Line numbers in [`AutomationError::InvalidSerialization`] refer to the
    /// original file, not to the individual block.
    pub fn from_text(text: &str) -> Result<Self, AutomationError> {
        let mut actions = Vec::new();
        // (zero-based index of the block's first line, accumulated block lines).
        let mut current: Option<(usize, Vec<String>)> = None;

        for (index, line) in text.lines().enumerate() {
            if line.trim().is_empty() {
                if let Some((start, lines)) = current.take() {
                    actions.push(parse_block(&lines, start)?);
                }
                continue;
            }

            if line.starts_with("ACTION\t") {
                if let Some((start, lines)) = current.take() {
                    actions.push(parse_block(&lines, start)?);
                }
                current = Some((index, vec![line.to_owned()]));
            } else if let Some((_, lines)) = current.as_mut() {
                lines.push(line.to_owned());
            } else {
                return Err(AutomationError::InvalidSerialization {
                    line: index + 1,
                    reason: "expected an ACTION record before STEP/PARAM records".to_owned(),
                });
            }
        }

        if let Some((start, lines)) = current.take() {
            actions.push(parse_block(&lines, start)?);
        }

        let mut seen = HashSet::new();
        for action in &actions {
            if !seen.insert(action.name()) {
                return Err(AutomationError::DuplicateActionName {
                    name: action.name().to_owned(),
                });
            }
        }

        Ok(Self { actions })
    }

    /// Returns the action with the given name, if the file contains one.
    pub fn get(&self, name: &str) -> Option<&Action> {
        self.actions.iter().find(|action| action.name() == name)
    }

    /// Returns every action name in file order.
    pub fn names(&self) -> impl ExactSizeIterator<Item = &str> {
        self.actions.iter().map(|action| action.name())
    }

    /// Returns every action in file order.
    pub fn actions(&self) -> impl ExactSizeIterator<Item = &Action> {
        self.actions.iter()
    }

    /// Returns the number of actions in the library.
    pub fn len(&self) -> usize {
        self.actions.len()
    }

    /// Returns `true` when the file contained no actions.
    pub fn is_empty(&self) -> bool {
        self.actions.is_empty()
    }

    /// Serializes the library back to the multi-action text format.
    ///
    /// Blocks are written in file order and separated by one blank line. The
    /// result round-trips through [`ActionLibrary::from_text`] byte-for-byte.
    pub fn to_text(&self) -> String {
        let mut out = String::new();
        for action in &self.actions {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(&action.to_text());
        }
        out
    }
}

/// Parses one accumulated block, translating its internal line numbers into
/// file-relative ones so errors point at the right line of the original file.
fn parse_block(lines: &[String], start: usize) -> Result<Action, AutomationError> {
    let block = lines.join("\n");
    match Action::from_text(&block) {
        Ok(action) => Ok(action),
        Err(AutomationError::InvalidSerialization { line, reason }) => {
            Err(AutomationError::InvalidSerialization {
                line: line + start,
                reason,
            })
        }
        Err(other) => Err(other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn action_text(name: &str, description: &str) -> String {
        format!(
            "ACTION\t{name}\t{description}\n\
             STEP\tload\timport\n\
             PARAM\tsource\tin.pdf\n\
             STEP\tsave\texport\n\
             PARAM\ttarget\tout.pdf\n"
        )
    }

    #[test]
    fn single_action_file_is_backwards_compatible() {
        let text = action_text("nightly", "Import and stage");
        let library = ActionLibrary::from_text(&text).unwrap();
        assert_eq!(library.len(), 1);
        assert_eq!(library.names().collect::<Vec<_>>(), ["nightly"]);
        assert_eq!(library.get("nightly").unwrap().len(), 2);
        assert_eq!(
            library.to_text(),
            text,
            "single-block round trip is lossless"
        );
    }

    #[test]
    fn multiple_actions_are_parsed_in_file_order() {
        let text = format!(
            "{}\n{}",
            action_text("first", "one"),
            action_text("second", "two")
        );
        let library = ActionLibrary::from_text(&text).unwrap();
        assert_eq!(library.len(), 2);
        assert_eq!(library.names().collect::<Vec<_>>(), ["first", "second"]);
        assert_eq!(library.get("second").unwrap().description(), "two");
        assert_eq!(
            ActionLibrary::from_text(&library.to_text()).unwrap(),
            library
        );
    }

    #[test]
    fn duplicate_names_are_rejected() {
        let text = format!(
            "{}\n{}",
            action_text("same", "one"),
            action_text("same", "two")
        );
        assert_eq!(
            ActionLibrary::from_text(&text).err(),
            Some(AutomationError::DuplicateActionName {
                name: "same".to_owned()
            })
        );
    }

    #[test]
    fn blank_lines_and_crlf_are_tolerated() {
        let text = format!(
            "\n{}\r\n\r\n{}\r\n",
            action_text("a", ""),
            action_text("b", "")
        );
        let library = ActionLibrary::from_text(&text).unwrap();
        assert_eq!(library.names().collect::<Vec<_>>(), ["a", "b"]);
    }

    #[test]
    fn stray_records_and_malformed_blocks_are_rejected() {
        assert_eq!(
            ActionLibrary::from_text("STEP\tload\timport\n").err(),
            Some(AutomationError::InvalidSerialization {
                line: 1,
                reason: "expected an ACTION record before STEP/PARAM records".to_owned(),
            })
        );
        let malformed = ActionLibrary::from_text("ACTION\ta\t\nSTEP\tload\timport\nGARBAGE\n")
            .expect_err("malformed block must fail");
        assert_eq!(
            malformed,
            AutomationError::InvalidSerialization {
                line: 3,
                reason: "unrecognized record 'GARBAGE'".to_owned(),
            }
        );
        let stray = ActionLibrary::from_text(&format!("{}\nGARBAGE\n", action_text("a", "")))
            .expect_err("stray record must fail");
        assert_eq!(
            stray,
            AutomationError::InvalidSerialization {
                line: 7,
                reason: "expected an ACTION record before STEP/PARAM records".to_owned(),
            }
        );
    }

    #[test]
    fn empty_file_yields_empty_library() {
        let library = ActionLibrary::from_text("\n\n").unwrap();
        assert!(library.is_empty());
        assert_eq!(library.len(), 0);
        assert!(library.to_text().is_empty());
        assert!(library.get("missing").is_none());
    }

    #[test]
    fn get_only_returns_an_exact_name() {
        let library = ActionLibrary::from_text(&action_text("nightly", "")).unwrap();
        assert!(library.get("Nightly").is_none());
        assert!(library.get("nightly ").is_none());
    }
}
