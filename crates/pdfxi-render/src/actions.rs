//! Interactive actions and triggers (feature MEDIA-005).
//!
//! This module parses an annotation's `/AA` (additional-actions) events into a
//! typed whitelist. It recognises the standard trigger codes `E`, `X`, `D`,
//! `U`, `Fo`, `Po`, `PV` and `PI`, orders them deterministically, and builds a
//! stable [`ActionPlan`] for a host to consume.
//!
//! # No script execution (honest delegation)
//!
//! This is a data model, not an action executor. Dangerous actions — JavaScript,
//! `Launch`, form submission/import — are rejected with
//! [`ActionError::DangerousAction`] and are never executed or carried into a
//! plan. Actions that need a network or external application (`URI`, remote
//! go-to, multimedia rendition actions) are rejected as
//! [`ActionError::UnsupportedAction`]. The plan contains only navigation and
//! view commands a host can safely interpret.
//!
//! All operations are pure, bounded and deterministic, add no dependency, and
//! need no cancellation token because a single parse is short.

/// Maximum number of additional actions in one annotation.
pub const MAX_ACTIONS: usize = 64;
/// Maximum byte length of a named action target.
pub const MAX_NAME_BYTES: usize = 128;
/// Maximum page number accepted by a `GoTo` action.
pub const MAX_ACTION_PAGE: u32 = 1_000_000;

/// Structured errors produced while parsing `/AA` actions.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ActionError {
    /// An additional-action event code was not recognised.
    #[error("unknown additional-action event '{0}'")]
    UnknownEvent(String),
    /// The action kind was empty.
    #[error("action kind must not be empty")]
    EmptyKind,
    /// The action would execute code or interact with the host OS.
    #[error("dangerous action '{0}' is not allowed")]
    DangerousAction(String),
    /// The action needs a capability this model does not provide.
    #[error("unsupported action '{0}'")]
    UnsupportedAction(String),
    /// A `GoTo` target was not a valid, bounded page number.
    #[error("invalid action target '{0}'")]
    InvalidTarget(String),
    /// A named target exceeded the byte-length cap.
    #[error("action target length {len} exceeds the {limit}-byte limit")]
    TargetTooLong {
        /// Actual byte length of the target.
        len: usize,
        /// Enforced byte-length limit.
        limit: usize,
    },
    /// More actions were supplied than the cap allows.
    #[error("action count {count} exceeds the {limit}-action limit")]
    TooManyActions {
        /// Number of actions supplied.
        count: usize,
        /// Enforced action limit.
        limit: usize,
    },
}

/// A recognised annotation additional-action (`/AA`) trigger event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AaEvent {
    /// The mouse enters the annotation (`E`).
    MouseEnter,
    /// The mouse exits the annotation (`X`).
    MouseExit,
    /// The mouse button is pressed over the annotation (`D`).
    MouseDown,
    /// The mouse button is released over the annotation (`U`).
    MouseUp,
    /// The annotation receives focus (`Fo`).
    Focus,
    /// The annotation loses focus (`Po`).
    Blur,
    /// The containing page becomes visible (`PV`).
    PageVisible,
    /// The containing page becomes invisible (`PI`).
    PageInvisible,
}

impl AaEvent {
    /// Parse an `/AA` event code.
    ///
    /// Returns [`ActionError::UnknownEvent`] for any other code.
    pub fn parse(code: &str) -> Result<Self, ActionError> {
        match code {
            "E" => Ok(Self::MouseEnter),
            "X" => Ok(Self::MouseExit),
            "D" => Ok(Self::MouseDown),
            "U" => Ok(Self::MouseUp),
            "Fo" => Ok(Self::Focus),
            "Po" => Ok(Self::Blur),
            "PV" => Ok(Self::PageVisible),
            "PI" => Ok(Self::PageInvisible),
            other => Err(ActionError::UnknownEvent(other.to_owned())),
        }
    }

    /// Return the canonical PDF code for this event.
    pub const fn code(self) -> &'static str {
        match self {
            Self::MouseEnter => "E",
            Self::MouseExit => "X",
            Self::MouseDown => "D",
            Self::MouseUp => "U",
            Self::Focus => "Fo",
            Self::Blur => "Po",
            Self::PageVisible => "PV",
            Self::PageInvisible => "PI",
        }
    }

    /// Return the canonical ordering rank of this event.
    ///
    /// The order follows the `/AA` event listing (`E`, `X`, `D`, `U`, `Fo`,
    /// `Po`, `PV`, `PI`) and is used to sort an [`ActionPlan`] deterministically.
    pub const fn order(self) -> usize {
        match self {
            Self::MouseEnter => 0,
            Self::MouseExit => 1,
            Self::MouseDown => 2,
            Self::MouseUp => 3,
            Self::Focus => 4,
            Self::Blur => 5,
            Self::PageVisible => 6,
            Self::PageInvisible => 7,
        }
    }
}

/// A safe action a host may carry out; no code execution is represented.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SafeAction {
    /// Jump to a zero-based page number.
    GoTo {
        /// The destination page number.
        page: u32,
    },
    /// Jump to a named destination.
    GoToNamed {
        /// The bounded destination name.
        name: String,
    },
    /// Advance to the next page.
    NextPage,
    /// Move to the previous page.
    PrevPage,
    /// Move to the first page.
    FirstPage,
    /// Move to the last page.
    LastPage,
    /// Toggle full-screen presentation.
    ToggleFullScreen,
}

impl SafeAction {
    /// Return a stable token for this action in an [`ActionPlan`] descriptor.
    pub fn code(&self) -> String {
        match self {
            Self::GoTo { page } => format!("goto:{page}"),
            Self::GoToNamed { name } => {
                let mut out = String::from("goto-named:");
                push_field(&mut out, name);
                out
            }
            Self::NextPage => "next-page".to_owned(),
            Self::PrevPage => "prev-page".to_owned(),
            Self::FirstPage => "first-page".to_owned(),
            Self::LastPage => "last-page".to_owned(),
            Self::ToggleFullScreen => "toggle-full-screen".to_owned(),
        }
    }
}

/// Owned intermediate form of one `/AA` action.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdditionalActionInput {
    /// The trigger event code (`E`, `X`, `D`, `U`, `Fo`, `Po`, `PV`, `PI`).
    pub event: String,
    /// The action kind (for example `GoTo`, `NextPage`, `JavaScript`).
    pub kind: String,
    /// The action target: a page number or a bounded name, empty if unused.
    pub target: String,
}

/// A validated additional action bound to its trigger event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlannedAction {
    event: AaEvent,
    action: SafeAction,
    sequence: usize,
}

impl PlannedAction {
    /// Return the trigger event.
    pub fn event(&self) -> AaEvent {
        self.event
    }

    /// Return the safe action.
    pub fn action(&self) -> &SafeAction {
        &self.action
    }

    /// Return the original declaration index, used as a stable tie-breaker.
    pub fn sequence(&self) -> usize {
        self.sequence
    }
}

/// A validated set of `/AA` additional actions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AaActionSet {
    actions: Vec<PlannedAction>,
}

impl AaActionSet {
    /// Parse and validate a list of additional actions.
    pub fn parse(inputs: Vec<AdditionalActionInput>) -> Result<Self, ActionError> {
        if inputs.len() > MAX_ACTIONS {
            return Err(ActionError::TooManyActions {
                count: inputs.len(),
                limit: MAX_ACTIONS,
            });
        }

        let mut actions = Vec::with_capacity(inputs.len());
        for (sequence, input) in inputs.into_iter().enumerate() {
            let event = AaEvent::parse(&input.event)?;
            let action = parse_safe_action(&input.kind, &input.target)?;
            actions.push(PlannedAction {
                event,
                action,
                sequence,
            });
        }

        // Stable sort keeps declaration order for actions sharing an event.
        actions.sort_by_key(|planned| planned.event.order());
        Ok(Self { actions })
    }

    /// Return the actions in canonical trigger order.
    pub fn actions(&self) -> &[PlannedAction] {
        &self.actions
    }

    /// Return `true` when the set holds no actions.
    pub fn is_empty(&self) -> bool {
        self.actions.is_empty()
    }

    /// Return the distinct trigger events present, in canonical order.
    pub fn events(&self) -> Vec<AaEvent> {
        let mut events = Vec::new();
        for planned in &self.actions {
            if !events.contains(&planned.event) {
                events.push(planned.event);
            }
        }
        events
    }

    /// Produce the deterministic action plan for a host to consume.
    pub fn resolve_plan(&self) -> ActionPlan {
        ActionPlan {
            actions: self.actions.clone(),
            events: self.events(),
        }
    }
}

/// The deterministic action plan derived from an [`AaActionSet`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActionPlan {
    actions: Vec<PlannedAction>,
    events: Vec<AaEvent>,
}

impl ActionPlan {
    /// Return the actions in canonical trigger order.
    pub fn actions(&self) -> &[PlannedAction] {
        &self.actions
    }

    /// Return the distinct trigger events present, in canonical order.
    pub fn events(&self) -> &[AaEvent] {
        &self.events
    }

    /// Return only the actions bound to `event`, preserving plan order.
    pub fn actions_for(&self, event: AaEvent) -> Vec<&PlannedAction> {
        self.actions
            .iter()
            .filter(|planned| planned.event == event)
            .collect()
    }

    /// Return `true` when the plan contains no actions.
    pub fn is_empty(&self) -> bool {
        self.actions.is_empty()
    }

    /// Return a byte-stable, single-line descriptor of this plan.
    ///
    /// Actions are emitted in canonical trigger order with their declaration
    /// sequence as a tie-breaker, so equal inputs always give an identical
    /// string. Named targets are escaped and never carry code or host commands.
    pub fn descriptor(&self) -> String {
        let mut out = String::from("action-plan/v1|actions=");
        for (index, planned) in self.actions.iter().enumerate() {
            if index > 0 {
                out.push(',');
            }
            out.push_str(planned.event.code());
            out.push(':');
            out.push_str(&planned.action.code());
            out.push('#');
            out.push_str(&planned.sequence.to_string());
        }
        out.push_str("|events=");
        for (index, event) in self.events.iter().enumerate() {
            if index > 0 {
                out.push(',');
            }
            out.push_str(event.code());
        }
        out
    }
}

/// Append `value` to `out`, escaping the descriptor's delimiter characters so
/// distinct field values can never produce the same descriptor string.
fn push_field(out: &mut String, value: &str) {
    for ch in value.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '|' => out.push_str("\\|"),
            ',' => out.push_str("\\,"),
            ':' => out.push_str("\\:"),
            '#' => out.push_str("\\#"),
            _ => out.push(ch),
        }
    }
}

fn parse_safe_action(kind: &str, target: &str) -> Result<SafeAction, ActionError> {
    match kind {
        "" => Err(ActionError::EmptyKind),
        "JavaScript" | "JS" | "Launch" | "ImportData" | "SubmitForm" | "ResetForm" => {
            Err(ActionError::DangerousAction(kind.to_owned()))
        }
        "URI" | "GoToR" | "GoToRemote" | "Rendition" | "Movie" | "Sound" => {
            Err(ActionError::UnsupportedAction(kind.to_owned()))
        }
        "GoTo" => {
            let page = target
                .parse::<u32>()
                .map_err(|_| ActionError::InvalidTarget(target.to_owned()))?;
            if page > MAX_ACTION_PAGE {
                return Err(ActionError::InvalidTarget(target.to_owned()));
            }
            Ok(SafeAction::GoTo { page })
        }
        "GoToNamed" => {
            if target.is_empty() {
                return Err(ActionError::InvalidTarget(target.to_owned()));
            }
            if target.len() > MAX_NAME_BYTES {
                return Err(ActionError::TargetTooLong {
                    len: target.len(),
                    limit: MAX_NAME_BYTES,
                });
            }
            Ok(SafeAction::GoToNamed {
                name: target.to_owned(),
            })
        }
        "NextPage" => Ok(SafeAction::NextPage),
        "PrevPage" => Ok(SafeAction::PrevPage),
        "FirstPage" => Ok(SafeAction::FirstPage),
        "LastPage" => Ok(SafeAction::LastPage),
        "ToggleFullScreen" => Ok(SafeAction::ToggleFullScreen),
        other => Err(ActionError::UnsupportedAction(other.to_owned())),
    }
}

#[cfg(test)]
fn input(event: &str, kind: &str, target: &str) -> AdditionalActionInput {
    AdditionalActionInput {
        event: event.to_owned(),
        kind: kind.to_owned(),
        target: target.to_owned(),
    }
}

#[cfg(test)]
fn sample_inputs() -> Vec<AdditionalActionInput> {
    vec![
        input("PV", "GoTo", "4"),
        input("E", "NextPage", ""),
        input("D", "GoToNamed", "appendix"),
        input("E", "ToggleFullScreen", ""),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aa_event_parsing_round_trips_every_code() {
        for (code, event) in [
            ("E", AaEvent::MouseEnter),
            ("X", AaEvent::MouseExit),
            ("D", AaEvent::MouseDown),
            ("U", AaEvent::MouseUp),
            ("Fo", AaEvent::Focus),
            ("Po", AaEvent::Blur),
            ("PV", AaEvent::PageVisible),
            ("PI", AaEvent::PageInvisible),
        ] {
            assert_eq!(AaEvent::parse(code), Ok(event));
            assert_eq!(event.code(), code);
        }
        assert_eq!(
            AaEvent::parse("ZZ"),
            Err(ActionError::UnknownEvent("ZZ".to_owned()))
        );
    }

    #[test]
    fn actions_are_ordered_by_canonical_trigger_rank() {
        let set = AaActionSet::parse(sample_inputs()).expect("valid actions");
        let plan = set.resolve_plan();
        let events = plan.events();
        assert_eq!(
            events,
            &[
                AaEvent::MouseEnter,
                AaEvent::MouseDown,
                AaEvent::PageVisible,
            ]
        );
        // Two E actions keep their declaration order (sequence 1 then 3).
        let enter = plan.actions_for(AaEvent::MouseEnter);
        assert_eq!(enter.len(), 2);
        assert_eq!(enter[0].sequence(), 1);
        assert_eq!(enter[0].action(), &SafeAction::NextPage);
        assert_eq!(enter[1].sequence(), 3);
        assert_eq!(enter[1].action(), &SafeAction::ToggleFullScreen);
        assert_eq!(
            plan.actions()[2].action(),
            &SafeAction::GoToNamed {
                name: "appendix".to_owned()
            }
        );
    }

    #[test]
    fn dangerous_actions_are_rejected_without_a_plan() {
        for kind in [
            "JavaScript",
            "JS",
            "Launch",
            "ImportData",
            "SubmitForm",
            "ResetForm",
        ] {
            assert_eq!(
                AaActionSet::parse(vec![input("E", kind, "")]),
                Err(ActionError::DangerousAction(kind.to_owned())),
                "dangerous action {kind} must be rejected"
            );
        }
        // A dangerous action anywhere in the set prevents the whole plan.
        assert_eq!(
            AaActionSet::parse(vec![
                input("E", "NextPage", ""),
                input("D", "JavaScript", "app.alert(1)"),
            ]),
            Err(ActionError::DangerousAction("JavaScript".to_owned()))
        );
    }

    #[test]
    fn network_and_mmultimedia_actions_are_unsupported() {
        for kind in ["URI", "GoToR", "GoToRemote", "Rendition", "Movie", "Sound"] {
            assert_eq!(
                AaActionSet::parse(vec![input("PV", kind, "")]),
                Err(ActionError::UnsupportedAction(kind.to_owned())),
                "action {kind} must be unsupported"
            );
        }
        assert_eq!(
            AaActionSet::parse(vec![input("E", "UnknownFutureAction", "")]),
            Err(ActionError::UnsupportedAction(
                "UnknownFutureAction".to_owned()
            ))
        );
    }

    #[test]
    fn safe_navigation_actions_are_planned() {
        let set = AaActionSet::parse(vec![
            input("E", "FirstPage", ""),
            input("X", "LastPage", ""),
            input("D", "PrevPage", ""),
            input("U", "NextPage", ""),
            input("Fo", "GoTo", "0"),
            input("Po", "GoTo", "999999"),
            input("PV", "ToggleFullScreen", ""),
        ])
        .expect("all safe");
        let plan = set.resolve_plan();
        assert_eq!(plan.actions().len(), 7);
        assert_eq!(plan.events().len(), 7);
        assert_eq!(plan.actions()[4].action(), &SafeAction::GoTo { page: 0 });
    }

    #[test]
    fn invalid_targets_and_empty_kinds_are_typed_errors() {
        assert_eq!(
            AaActionSet::parse(vec![input("E", "", "")]),
            Err(ActionError::EmptyKind)
        );
        assert_eq!(
            AaActionSet::parse(vec![input("E", "GoTo", "not-a-number")]),
            Err(ActionError::InvalidTarget("not-a-number".to_owned()))
        );
        assert_eq!(
            AaActionSet::parse(vec![input("E", "GoTo", "2000000")]),
            Err(ActionError::InvalidTarget("2000000".to_owned()))
        );
        assert_eq!(
            AaActionSet::parse(vec![input("E", "GoToNamed", "")]),
            Err(ActionError::InvalidTarget(String::new()))
        );
        let long = "n".repeat(MAX_NAME_BYTES + 1);
        assert_eq!(
            AaActionSet::parse(vec![input("E", "GoToNamed", &long)]),
            Err(ActionError::TargetTooLong {
                len: MAX_NAME_BYTES + 1,
                limit: MAX_NAME_BYTES
            })
        );
    }

    #[test]
    fn bounded_input_is_enforced() {
        let inputs = vec![input("E", "NextPage", ""); MAX_ACTIONS + 1];
        assert_eq!(
            AaActionSet::parse(inputs),
            Err(ActionError::TooManyActions {
                count: MAX_ACTIONS + 1,
                limit: MAX_ACTIONS
            })
        );
        let empty = AaActionSet::parse(vec![]).expect("empty set");
        assert!(empty.is_empty());
        assert!(empty.resolve_plan().is_empty());
    }

    #[test]
    fn plan_is_deterministic_regardless_of_input_order() {
        let set = AaActionSet::parse(sample_inputs()).expect("valid");
        let plan = set.resolve_plan();
        assert_eq!(plan, set.resolve_plan());

        // Reversing declaration order yields the same event ordering and the
        // same actions; only the stable sequence numbers differ for ties.
        let mut reversed = sample_inputs();
        reversed.reverse();
        let reversed_plan = AaActionSet::parse(reversed).expect("valid").resolve_plan();
        assert_eq!(plan.events(), reversed_plan.events());
        let codes: Vec<&str> = plan
            .actions()
            .iter()
            .map(|planned| planned.event().code())
            .collect();
        let reversed_codes: Vec<&str> = reversed_plan
            .actions()
            .iter()
            .map(|planned| planned.event().code())
            .collect();
        assert_eq!(codes, reversed_codes);
    }

    #[test]
    fn plan_descriptor_is_byte_stable_and_reflects_actions() {
        let set = AaActionSet::parse(sample_inputs()).expect("valid");
        let summary = set.resolve_plan().descriptor();
        assert_eq!(summary, set.resolve_plan().descriptor());
        assert!(summary.starts_with("action-plan/v1|actions=E:next-page#1,E:toggle-full-screen#3,D:goto-named:appendix#2,PV:goto:4#0|"));
        assert!(summary.ends_with("|events=E,D,PV"));

        // A different action set produces a different descriptor, and a named
        // target with delimiter characters is escaped rather than collapsed.
        let mut inputs = sample_inputs();
        inputs[2] = input("D", "GoToNamed", "app,endix|x");
        let changed = AaActionSet::parse(inputs)
            .expect("valid")
            .resolve_plan()
            .descriptor();
        assert_ne!(changed, summary);
        assert!(changed.contains("goto-named:app\\,endix\\|x"));
    }

    #[test]
    fn every_safe_action_has_a_stable_code() {
        assert_eq!(SafeAction::GoTo { page: 7 }.code(), "goto:7");
        assert_eq!(
            SafeAction::GoToNamed {
                name: "a:b".to_owned()
            }
            .code(),
            "goto-named:a\\:b"
        );
        assert_eq!(SafeAction::NextPage.code(), "next-page");
        assert_eq!(SafeAction::PrevPage.code(), "prev-page");
        assert_eq!(SafeAction::FirstPage.code(), "first-page");
        assert_eq!(SafeAction::LastPage.code(), "last-page");
        assert_eq!(SafeAction::ToggleFullScreen.code(), "toggle-full-screen");

        // The empty set still has a well-formed, deterministic descriptor.
        let empty = AaActionSet::parse(vec![]).expect("empty set");
        assert_eq!(
            empty.resolve_plan().descriptor(),
            "action-plan/v1|actions=|events="
        );
    }
}
