//! Actions: stable step kinds, ordered steps, validation, and text I/O.
//!
//! An [`Action`] is a named, ordered list of [`ActionStep`]s. Step kinds have
//! stable string identifiers (see [`StepKind::as_str`]) so an action can be
//! exported and re-imported without losing meaning. The crate is deliberately
//! dependency-free, so import/export uses a small, documented line-oriented
//! format rather than an external serialization library.

use crate::error::AutomationError;
use std::collections::HashSet;
use std::fmt;
use std::str::FromStr;

/// The stable kind of an [`ActionStep`].
///
/// Each variant maps to a fixed lowercase string ([`StepKind::as_str`]) that is
/// used by the text import/export format. The strings are part of the public
/// contract and must not change without a format version bump.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum StepKind {
    /// Bring a document into the workflow.
    Import,
    /// Transform a document from one format to another.
    Convert,
    /// Apply a watermark.
    Watermark,
    /// Apply security settings such as encryption or permissions.
    Security,
    /// Write a document out to its destination.
    Export,
    /// Run a user-defined command.
    CustomCommand,
}

impl StepKind {
    /// Every stable kind string, in declaration order.
    pub const ALL: &'static [&'static str] = &[
        "import",
        "convert",
        "watermark",
        "security",
        "export",
        "custom-command",
    ];

    /// Returns the stable string identifier for this kind.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Import => "import",
            Self::Convert => "convert",
            Self::Watermark => "watermark",
            Self::Security => "security",
            Self::Export => "export",
            Self::CustomCommand => "custom-command",
        }
    }
}

impl fmt::Display for StepKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for StepKind {
    type Err = AutomationError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "import" => Ok(Self::Import),
            "convert" => Ok(Self::Convert),
            "watermark" => Ok(Self::Watermark),
            "security" => Ok(Self::Security),
            "export" => Ok(Self::Export),
            "custom-command" => Ok(Self::CustomCommand),
            other => Err(AutomationError::UnknownStepKind {
                value: other.to_owned(),
            }),
        }
    }
}

/// A single operation inside an [`Action`].
///
/// A step has a unique [`id`](ActionStep::id), a stable [`kind`](StepKind), and
/// an ordered list of string parameters. Parameter order is preserved so text
/// export is byte-for-byte stable for a given action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionStep {
    id: String,
    kind: StepKind,
    parameters: Vec<(String, String)>,
}

impl ActionStep {
    /// Creates a step with the given id and kind and no parameters.
    pub fn new(id: impl Into<String>, kind: StepKind) -> Self {
        Self {
            id: id.into(),
            kind,
            parameters: Vec::new(),
        }
    }

    /// Adds a parameter, returning the step for chaining.
    pub fn with_parameter(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.add_parameter(key, value);
        self
    }

    /// Adds a parameter to the end of the step's parameter list.
    pub fn add_parameter(&mut self, key: impl Into<String>, value: impl Into<String>) {
        self.parameters.push((key.into(), value.into()));
    }

    /// Returns the step id.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Returns the stable step kind.
    pub fn kind(&self) -> StepKind {
        self.kind
    }

    /// Returns the parameters in insertion order as `(key, value)` pairs.
    pub fn parameters(&self) -> impl ExactSizeIterator<Item = (&str, &str)> {
        self.parameters
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str()))
    }

    /// Returns the value of the first parameter with the given key.
    pub fn parameter(&self, key: &str) -> Option<&str> {
        self.parameters
            .iter()
            .find(|(stored, _)| stored == key)
            .map(|(_, value)| value.as_str())
    }
}

/// A named, ordered sequence of [`ActionStep`]s.
///
/// An action is valid when it has a non-empty name, at least one step, unique
/// non-empty step ids, and non-empty parameter keys. [`Action::validate`]
/// enforces these rules; [`Action::from_steps`] and [`Action::add_step`] enforce
/// them at construction/mutation time so a well-built action stays valid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Action {
    name: String,
    description: String,
    steps: Vec<ActionStep>,
}

impl Action {
    /// Creates an empty action. Add steps with [`Action::add_step`] before use.
    pub fn new(name: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            steps: Vec::new(),
        }
    }

    /// Builds an action from a complete step list, validating it immediately.
    pub fn from_steps<I>(
        name: impl Into<String>,
        description: impl Into<String>,
        steps: I,
    ) -> Result<Self, AutomationError>
    where
        I: IntoIterator<Item = ActionStep>,
    {
        let name = name.into();
        let steps: Vec<ActionStep> = steps.into_iter().collect();
        Self::validate_parts(&name, &steps)?;
        Ok(Self {
            name,
            description: description.into(),
            steps,
        })
    }

    /// Checks all action invariants, returning the first problem found.
    ///
    /// Validation is deterministic: it checks the name, then the step count,
    /// then walks steps and parameters in order reporting the first issue.
    pub fn validate(&self) -> Result<(), AutomationError> {
        Self::validate_parts(&self.name, &self.steps)
    }

    fn validate_parts(name: &str, steps: &[ActionStep]) -> Result<(), AutomationError> {
        if name.trim().is_empty() {
            return Err(AutomationError::EmptyActionName);
        }
        if steps.is_empty() {
            return Err(AutomationError::EmptyAction {
                name: name.to_owned(),
            });
        }
        let mut seen = HashSet::new();
        for step in steps {
            if step.id.trim().is_empty() {
                return Err(AutomationError::EmptyStepId {
                    action: name.to_owned(),
                });
            }
            if !seen.insert(step.id.as_str()) {
                return Err(AutomationError::DuplicateStepId {
                    action: name.to_owned(),
                    id: step.id.clone(),
                });
            }
            for (key, _) in step.parameters() {
                if key.trim().is_empty() {
                    return Err(AutomationError::EmptyParameterKey {
                        step: step.id.clone(),
                    });
                }
            }
        }
        Ok(())
    }

    /// Appends a step, rejecting empty or duplicate ids.
    ///
    /// On error the action is left unchanged.
    pub fn add_step(&mut self, step: ActionStep) -> Result<(), AutomationError> {
        if step.id.trim().is_empty() {
            return Err(AutomationError::EmptyStepId {
                action: self.name.clone(),
            });
        }
        if self.steps.iter().any(|existing| existing.id == step.id) {
            return Err(AutomationError::DuplicateStepId {
                action: self.name.clone(),
                id: step.id,
            });
        }
        for (key, _) in step.parameters() {
            if key.trim().is_empty() {
                return Err(AutomationError::EmptyParameterKey {
                    step: step.id.clone(),
                });
            }
        }
        self.steps.push(step);
        Ok(())
    }

    /// Returns the action name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the action description.
    pub fn description(&self) -> &str {
        &self.description
    }

    /// Returns the steps in stable execution order.
    pub fn steps(&self) -> impl ExactSizeIterator<Item = &ActionStep> {
        self.steps.iter()
    }

    /// Returns the step with the given id, if present.
    pub fn step(&self, id: &str) -> Option<&ActionStep> {
        self.steps.iter().find(|step| step.id == id)
    }

    /// Applies toolset parameter defaults to every step that lacks the key.
    ///
    /// Existing step parameters always win: a default is only added when the
    /// step has no parameter with that key. Pairs are applied in order, and the
    /// action remains valid because defaults always have non-empty keys when
    /// they come from a validated [`Toolset`](crate::Toolset). This gives
    /// named toolsets a deterministic way to supply common action parameters
    /// without editing the saved action file.
    pub fn apply_defaults(&mut self, defaults: &[(String, String)]) {
        for step in &mut self.steps {
            for (key, value) in defaults {
                if step.parameter(key).is_none() {
                    step.add_parameter(key.clone(), value.clone());
                }
            }
        }
    }

    /// Returns the number of steps.
    pub fn len(&self) -> usize {
        self.steps.len()
    }

    /// Returns `true` when the action has no steps.
    pub fn is_empty(&self) -> bool {
        self.steps.is_empty()
    }

    /// Exports the action to the stable text format.
    ///
    /// The format is line-oriented with tab-separated fields:
    ///
    /// ```text
    /// ACTION\t<name>\t<description>
    /// STEP\t<id>\t<kind>
    /// PARAM\t<key>\t<value>
    /// ```
    ///
    /// Each `PARAM` line belongs to the most recent `STEP`. Backslash, tab,
    /// newline, and carriage-return characters in values are escaped, so a
    /// round trip through [`Action::from_text`] is lossless.
    pub fn to_text(&self) -> String {
        let mut out = String::new();
        out.push_str("ACTION\t");
        out.push_str(&escape(&self.name));
        out.push('\t');
        out.push_str(&escape(&self.description));
        out.push('\n');
        for step in &self.steps {
            out.push_str("STEP\t");
            out.push_str(&escape(&step.id));
            out.push('\t');
            out.push_str(step.kind.as_str());
            out.push('\n');
            for (key, value) in step.parameters() {
                out.push_str("PARAM\t");
                out.push_str(&escape(key));
                out.push('\t');
                out.push_str(&escape(value));
                out.push('\n');
            }
        }
        out
    }

    /// Imports an action from the text format produced by [`Action::to_text`].
    ///
    /// The imported action is validated before it is returned, so a malformed
    /// or invalid document is rejected with an actionable
    /// [`AutomationError`].
    pub fn from_text(text: &str) -> Result<Self, AutomationError> {
        let mut lines = text.lines().enumerate();
        let (index, header) =
            lines
                .next()
                .ok_or_else(|| AutomationError::InvalidSerialization {
                    line: 0,
                    reason: "empty input; expected an ACTION record".to_owned(),
                })?;
        let header_line = index + 1;
        let fields: Vec<&str> = header.split('\t').collect();
        if fields.len() != 3 || fields[0] != "ACTION" {
            return Err(AutomationError::InvalidSerialization {
                line: header_line,
                reason: "expected: ACTION\\t<name>\\t<description>".to_owned(),
            });
        }
        let name = unescape(fields[1], header_line)?;
        let description = unescape(fields[2], header_line)?;

        let mut steps: Vec<ActionStep> = Vec::new();
        for (index, line) in lines {
            let line_number = index + 1;
            let fields: Vec<&str> = line.split('\t').collect();
            match fields.as_slice() {
                ["STEP", raw_id, raw_kind] => {
                    let id = unescape(raw_id, line_number)?;
                    let kind_text = unescape(raw_kind, line_number)?;
                    let kind = StepKind::from_str(&kind_text)?;
                    steps.push(ActionStep::new(id, kind));
                }
                ["PARAM", raw_key, raw_value] => {
                    let key = unescape(raw_key, line_number)?;
                    let value = unescape(raw_value, line_number)?;
                    let step =
                        steps
                            .last_mut()
                            .ok_or_else(|| AutomationError::InvalidSerialization {
                                line: line_number,
                                reason: "PARAM record appears before any STEP".to_owned(),
                            })?;
                    step.add_parameter(key, value);
                }
                _ => {
                    return Err(AutomationError::InvalidSerialization {
                        line: line_number,
                        reason: format!(
                            "unrecognized record '{}'",
                            fields.first().copied().unwrap_or("")
                        ),
                    });
                }
            }
        }
        Self::from_steps(name, description, steps)
    }
}

fn escape(value: &str) -> String {
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

fn unescape(value: &str, line: usize) -> Result<String, AutomationError> {
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
                return Err(AutomationError::InvalidSerialization {
                    line,
                    reason: format!("invalid escape sequence '\\{other}'"),
                });
            }
            None => {
                return Err(AutomationError::InvalidSerialization {
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

    fn sample_action() -> Action {
        Action::from_steps(
            "nightly",
            "Import and export a document",
            [
                ActionStep::new("load", StepKind::Import).with_parameter("source", "in.pdf"),
                ActionStep::new("save", StepKind::Export)
                    .with_parameter("target", "out.pdf")
                    .with_parameter("overwrite", "true"),
            ],
        )
        .unwrap()
    }

    #[test]
    fn empty_action_is_rejected() {
        let action = Action::new("empty", "no steps");
        assert_eq!(
            action.validate(),
            Err(AutomationError::EmptyAction {
                name: "empty".to_owned()
            })
        );
        assert!(Action::from_steps("empty", "", [] as [ActionStep; 0]).is_err());
    }

    #[test]
    fn duplicate_step_ids_are_rejected() {
        let result = Action::from_steps(
            "dup",
            "",
            [
                ActionStep::new("same", StepKind::Import),
                ActionStep::new("same", StepKind::Export),
            ],
        );
        assert_eq!(
            result.err(),
            Some(AutomationError::DuplicateStepId {
                action: "dup".to_owned(),
                id: "same".to_owned(),
            })
        );
    }

    #[test]
    fn empty_name_and_step_id_are_rejected() {
        assert_eq!(
            Action::new("   ", "").validate(),
            Err(AutomationError::EmptyActionName)
        );
        let action = Action::from_steps("a", "", [ActionStep::new("", StepKind::Import)]);
        assert_eq!(
            action.err(),
            Some(AutomationError::EmptyStepId {
                action: "a".to_owned()
            })
        );
    }

    #[test]
    fn empty_parameter_key_is_rejected() {
        let action = Action::from_steps(
            "a",
            "",
            [ActionStep::new("s", StepKind::Convert).with_parameter("", "v")],
        );
        assert_eq!(
            action.err(),
            Some(AutomationError::EmptyParameterKey {
                step: "s".to_owned()
            })
        );
    }

    #[test]
    fn add_step_rejects_duplicates_without_mutating() {
        let mut action = sample_action();
        assert_eq!(
            action.add_step(ActionStep::new("load", StepKind::Watermark)),
            Err(AutomationError::DuplicateStepId {
                action: "nightly".to_owned(),
                id: "load".to_owned(),
            })
        );
        assert_eq!(action.len(), 2, "rejected step must not be appended");
        assert!(
            action
                .add_step(ActionStep::new("stamp", StepKind::Watermark))
                .is_ok()
        );
        assert_eq!(action.len(), 3);
    }

    #[test]
    fn step_kind_strings_are_stable_and_round_trip() {
        let kinds = [
            (StepKind::Import, "import"),
            (StepKind::Convert, "convert"),
            (StepKind::Watermark, "watermark"),
            (StepKind::Security, "security"),
            (StepKind::Export, "export"),
            (StepKind::CustomCommand, "custom-command"),
        ];
        for (kind, text) in kinds {
            assert_eq!(kind.as_str(), text);
            assert_eq!(text.parse::<StepKind>(), Ok(kind));
        }
        assert_eq!(StepKind::ALL.len(), 6);
        assert_eq!(
            "teleport".parse::<StepKind>().err(),
            Some(AutomationError::UnknownStepKind {
                value: "teleport".to_owned()
            })
        );
    }

    #[test]
    fn text_export_import_is_lossless() {
        let action = sample_action();
        let text = action.to_text();
        let restored = Action::from_text(&text).unwrap();
        assert_eq!(restored, action);
        assert_eq!(restored.name(), "nightly");
        assert_eq!(
            restored.step("save").unwrap().parameter("target"),
            Some("out.pdf")
        );
    }

    #[test]
    fn text_export_preserves_special_characters() {
        let action = Action::from_steps(
            "special\nname",
            "line\twith\\backslash",
            [ActionStep::new("id\t1", StepKind::CustomCommand).with_parameter("k\ty", "v\nn\\")],
        )
        .unwrap();
        let restored = Action::from_text(&action.to_text()).unwrap();
        assert_eq!(restored, action);
    }

    #[test]
    fn text_import_rejects_unknown_kind_and_bad_records() {
        let bad_kind = "ACTION\ta\t\nSTEP\ts\tteleport\n";
        assert_eq!(
            Action::from_text(bad_kind).err(),
            Some(AutomationError::UnknownStepKind {
                value: "teleport".to_owned()
            })
        );
        let bad_record = "ACTION\ta\t\nGARBAGE\n";
        assert!(matches!(
            Action::from_text(bad_record),
            Err(AutomationError::InvalidSerialization { line: 2, .. })
        ));
        let param_first = "ACTION\ta\t\nPARAM\tk\tv\n";
        assert!(matches!(
            Action::from_text(param_first),
            Err(AutomationError::InvalidSerialization { line: 2, .. })
        ));
    }

    #[test]
    fn parameters_are_enumerated_in_insertion_order() {
        let step = sample_action().step("save").unwrap().clone();
        let keys: Vec<&str> = step.parameters().map(|(key, _)| key).collect();
        assert_eq!(keys, ["target", "overwrite"]);
    }
}
