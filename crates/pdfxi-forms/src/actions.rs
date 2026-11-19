//! Field actions: `/AA` triggers, the safe-action whitelist, and a deterministic
//! plan that a host may execute.
//!
//! # Scope and safety
//!
//! PDF fields can carry a main action (`/A`) and up to four *additional actions*
//! in `/AA`, keyed by `/K` (keystroke), `/F` (format), `/V` (validate) and `/C`
//! (calculate). This module only **models** actions; it never executes them and
//! never interprets JavaScript. A small typed whitelist is accepted:
//!
//! - [`SafeAction::ResetForm`] — reset a list of fields (or all fields when the
//!   list is empty) to their default values.
//! - [`SafeAction::SubmitForm`] — submit a list of fields to a target; the host
//!   performs any I/O, this crate only records the target string.
//!
//! Everything else is rejected with a typed [`ActionError`]. JavaScript, URI,
//! Launch, ImportData and ExportData actions have dedicated rejection variants
//! so a caller can explain exactly why an action was refused. A `javascript:`
//! URI is reported as JavaScript rather than a generic URI to make the danger
//! explicit. Unknown subtypes are reported with [`ActionError::UnknownAction`].
//!
//! # Planning
//!
//! [`ActionCollector`] gathers actions from field dictionaries into an
//! [`ActionReport`]. Supported actions become ordered [`PlannedAction`] steps in
//! an [`ActionPlan`]; rejected ones become [`UnsupportedAction`] records. Both
//! collections are deterministic: steps follow field insertion order (main
//! action first, then `/K`, `/F`, `/V`, `/C`) and unsupported records are sorted
//! by `(field, site, subtype, reason)` when the report is finished.
//!
//! Indirect references are resolved through the same [`FormSource`] boundary
//! used by [`crate::load_acroform`], keeping this crate dependency-free and
//! unit-testable.

use crate::acroform::{FormSource, ObjectRef, PdfValue};
use std::collections::BTreeMap;
use std::fmt;

/// The additional-action trigger key in a field's `/AA` dictionary.
///
/// The four variants correspond to the PDF keys `/K`, `/F`, `/V` and `/C`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ActionTrigger {
    /// `/K` — a keystroke action, fired while editing the field value.
    Keystroke,
    /// `/F` — a format action, fired when the field loses the focus.
    Format,
    /// `/V` — a validate action, fired before a new value is accepted.
    Validate,
    /// `/C` — a calculate action, fired when a referenced field changes.
    Calculate,
}

impl ActionTrigger {
    /// Every trigger in the canonical order used to build action plans.
    pub const ALL: [Self; 4] = [
        Self::Keystroke,
        Self::Format,
        Self::Validate,
        Self::Calculate,
    ];

    /// Maps a PDF `/AA` key (`"K"`, `"F"`, `"V"` or `"C"`) to a trigger.
    ///
    /// The match is case-sensitive, as PDF names are. Unrecognised keys yield
    /// `None` so callers can ignore unknown future triggers.
    pub const fn from_key(key: &str) -> Option<Self> {
        match key.as_bytes() {
            b"K" => Some(Self::Keystroke),
            b"F" => Some(Self::Format),
            b"V" => Some(Self::Validate),
            b"C" => Some(Self::Calculate),
            _ => None,
        }
    }

    /// Returns the PDF key for this trigger.
    pub const fn key(self) -> &'static str {
        match self {
            Self::Keystroke => "K",
            Self::Format => "F",
            Self::Validate => "V",
            Self::Calculate => "C",
        }
    }

    /// Returns a stable, human-readable label for this trigger.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Keystroke => "keystroke",
            Self::Format => "format",
            Self::Validate => "validate",
            Self::Calculate => "calculate",
        }
    }
}

impl fmt::Display for ActionTrigger {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// Where an action is attached on a field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ActionSite {
    /// An additional action in `/AA` for the given trigger.
    Additional(ActionTrigger),
    /// The field's main action in `/A`.
    Main,
}

impl ActionSite {
    /// Returns the PDF key for this site (`A`, `K`, `F`, `V` or `C`).
    pub const fn label(self) -> &'static str {
        match self {
            Self::Main => "A",
            Self::Additional(trigger) => trigger.key(),
        }
    }
}

impl fmt::Display for ActionSite {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// A supported action that a host may execute.
///
/// These are *boundary models*: the crate records the intent and the data, but
/// performs no reset, no submission and no script execution itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SafeAction {
    /// Reset the named fields to their defaults; an empty list means all fields.
    ResetForm {
        /// Fully qualified field names to reset, in declaration order.
        fields: Vec<String>,
    },
    /// Submit the named fields to a target; the host performs the I/O.
    SubmitForm {
        /// The submission target as written in the PDF `/F` entry.
        target: String,
        /// Fully qualified field names to submit, in declaration order.
        fields: Vec<String>,
    },
}

impl SafeAction {
    /// Returns a stable action-subtype label.
    pub const fn subtype(&self) -> &'static str {
        match self {
            Self::ResetForm { .. } => "ResetForm",
            Self::SubmitForm { .. } => "SubmitForm",
        }
    }

    /// Returns the field names referenced by this action.
    pub fn fields(&self) -> &[String] {
        match self {
            Self::ResetForm { fields } | Self::SubmitForm { fields, .. } => fields,
        }
    }
}

impl fmt::Display for SafeAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.subtype())
    }
}

/// A typed error produced while parsing a field action.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ActionError {
    /// A JavaScript action was found; scripts are never parsed or executed.
    #[error("JavaScript actions are not supported")]
    JavaScriptRejected,
    /// A URI action was found; hosts do not open external targets.
    #[error("URI actions are not supported")]
    UriRejected,
    /// A Launch action was found; processes are never started.
    #[error("Launch actions are not supported")]
    LaunchRejected,
    /// An ImportData action was found.
    #[error("ImportData actions are not supported")]
    ImportRejected,
    /// An ExportData action was found.
    #[error("ExportData actions are not supported")]
    ExportRejected,
    /// The action subtype is not part of the safe whitelist.
    #[error("unsupported action subtype '{0}'")]
    UnknownAction(String),
    /// An action dictionary had a missing or malformed entry.
    #[error("malformed {context}: {detail}")]
    Malformed {
        /// The PDF entry being read, for example `"action /S"`.
        context: &'static str,
        /// A human-readable description of the problem.
        detail: String,
    },
    /// An indirect reference could not be resolved by the source.
    #[error("unresolved PDF reference {0}")]
    UnresolvedReference(ObjectRef),
}

/// One action that could not be modelled, recorded in an [`ActionReport`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnsupportedAction {
    /// Fully qualified name of the field carrying the action.
    pub field: String,
    /// Where the action was attached.
    pub site: ActionSite,
    /// The PDF subtype that was found, or `"Unknown"` when unreadable.
    pub action_type: String,
    /// Why the action was rejected.
    pub reason: String,
}

/// One accepted action step in an [`ActionPlan`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedAction {
    /// Fully qualified name of the field carrying the action.
    pub field: String,
    /// Where the action was attached.
    pub site: ActionSite,
    /// The modelled action for the host to run.
    pub action: SafeAction,
}

/// A deterministic list of actions a host may execute in order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ActionPlan {
    steps: Vec<PlannedAction>,
}

impl ActionPlan {
    /// Builds a plan from ordered steps.
    pub fn new(steps: Vec<PlannedAction>) -> Self {
        Self { steps }
    }

    /// Returns the steps in execution order.
    pub fn steps(&self) -> &[PlannedAction] {
        &self.steps
    }

    /// Returns the number of steps.
    pub fn len(&self) -> usize {
        self.steps.len()
    }

    /// Returns `true` when the plan has no steps.
    pub fn is_empty(&self) -> bool {
        self.steps.is_empty()
    }
}

/// The result of collecting actions: an executable plan plus a deterministic
/// list of actions that were reported as unsupported.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ActionReport {
    plan: ActionPlan,
    unsupported: Vec<UnsupportedAction>,
}

impl ActionReport {
    /// Returns the executable plan.
    pub fn plan(&self) -> &ActionPlan {
        &self.plan
    }

    /// Returns unsupported actions in deterministic order.
    pub fn unsupported(&self) -> &[UnsupportedAction] {
        &self.unsupported
    }

    /// Returns the number of unsupported actions.
    pub fn unsupported_count(&self) -> usize {
        self.unsupported.len()
    }

    /// Returns `true` when neither accepted nor unsupported actions were found.
    pub fn is_empty(&self) -> bool {
        self.plan.is_empty() && self.unsupported.is_empty()
    }
}

/// Supported actions parsed from one field's dictionaries.
///
/// Returned by [`parse_field_actions`]; rejected actions are reported
/// separately through that function's `unsupported` output parameter.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FieldActions {
    field: String,
    main: Option<SafeAction>,
    additional: BTreeMap<ActionTrigger, SafeAction>,
}

impl FieldActions {
    /// Returns the fully qualified field name.
    pub fn field(&self) -> &str {
        &self.field
    }

    /// Returns the parsed main `/A` action, if present and supported.
    pub fn main(&self) -> Option<&SafeAction> {
        self.main.as_ref()
    }

    /// Returns the parsed additional `/AA` actions by trigger.
    pub fn additional(&self) -> &BTreeMap<ActionTrigger, SafeAction> {
        &self.additional
    }
}

/// Collects field actions and produces a deterministic [`ActionReport`].
///
/// Dictionaries are added with [`ActionCollector::add_field`]; unsupported
/// actions are recorded instead of aborting the collection so a single hostile
/// action cannot hide the rest of a form's behaviour.
#[derive(Debug, Clone, Default)]
pub struct ActionCollector {
    fields: Vec<FieldActions>,
    unsupported: Vec<UnsupportedAction>,
}

impl ActionCollector {
    /// Creates an empty collector.
    pub fn new() -> Self {
        Self::default()
    }

    /// Parses the `/A` and `/AA` entries of `dict` for a field named `field`.
    ///
    /// Supported actions are retained and rejected ones are appended to the
    /// unsupported list; this method never fails.
    pub fn add_field(
        &mut self,
        field: impl Into<String>,
        dict: &BTreeMap<String, PdfValue>,
        source: &dyn FormSource,
    ) -> &mut Self {
        let parsed = parse_field_actions(field, dict, source, &mut self.unsupported);
        self.fields.push(parsed);
        self
    }

    /// Adds one already-parsed result for a field and site.
    ///
    /// This is the low-level entry point used by hosts that parse actions
    /// themselves. An error is recorded as an unsupported action.
    pub fn add_result(
        &mut self,
        field: impl Into<String>,
        site: ActionSite,
        result: Result<SafeAction, ActionError>,
    ) -> &mut Self {
        let field = field.into();
        match result {
            Ok(action) => {
                let existing = self.fields.iter().position(|entry| entry.field == field);
                let index = match existing {
                    Some(index) => index,
                    None => {
                        self.fields.push(FieldActions {
                            field: field.clone(),
                            ..FieldActions::default()
                        });
                        self.fields.len() - 1
                    }
                };
                let entry = &mut self.fields[index];
                match site {
                    ActionSite::Main => entry.main = Some(action),
                    ActionSite::Additional(trigger) => {
                        entry.additional.insert(trigger, action);
                    }
                }
            }
            Err(error) => self.unsupported.push(UnsupportedAction {
                field,
                site,
                action_type: "Unknown".to_owned(),
                reason: error.to_string(),
            }),
        }
        self
    }

    /// Finishes collection, returning the deterministic plan and report.
    pub fn finish(self) -> ActionReport {
        let mut steps = Vec::new();
        for entry in &self.fields {
            if let Some(action) = &entry.main {
                steps.push(PlannedAction {
                    field: entry.field.clone(),
                    site: ActionSite::Main,
                    action: action.clone(),
                });
            }
            for trigger in ActionTrigger::ALL {
                if let Some(action) = entry.additional.get(&trigger) {
                    steps.push(PlannedAction {
                        field: entry.field.clone(),
                        site: ActionSite::Additional(trigger),
                        action: action.clone(),
                    });
                }
            }
        }

        let mut unsupported = self.unsupported;
        unsupported.sort_by(|a, b| {
            a.field
                .cmp(&b.field)
                .then_with(|| a.site.label().cmp(b.site.label()))
                .then_with(|| a.action_type.cmp(&b.action_type))
                .then_with(|| a.reason.cmp(&b.reason))
        });

        ActionReport {
            plan: ActionPlan::new(steps),
            unsupported,
        }
    }
}

/// Resolves one value through the source, mapping failures to a typed error.
fn resolve(source: &dyn FormSource, value: &PdfValue) -> Result<PdfValue, ActionError> {
    match source.resolve(value) {
        Some(resolved) => Ok(resolved),
        None => match value {
            PdfValue::Reference(id) => Err(ActionError::UnresolvedReference(*id)),
            _ => Err(ActionError::Malformed {
                context: "reference",
                detail: "source failed to resolve a direct value".to_owned(),
            }),
        },
    }
}

/// Extracts a PDF text or name value as an owned string.
fn pdf_string(value: &PdfValue) -> Option<String> {
    match value {
        PdfValue::Text(text) | PdfValue::Name(text) => Some(text.clone()),
        _ => None,
    }
}

/// Reads the `/Fields` array of a reset/submit action.
///
/// Only text and name entries are modelled; indirect field references are
/// ignored because this crate addresses fields by fully qualified name. A
/// missing or non-array `/Fields` yields an empty list, meaning "all fields" for
/// a reset action and "no fields" for a submit action.
fn action_fields(dict: &BTreeMap<String, PdfValue>, source: &dyn FormSource) -> Vec<String> {
    let Some(value) = dict.get("Fields") else {
        return Vec::new();
    };
    let Ok(value) = resolve(source, value) else {
        return Vec::new();
    };
    let Some(items) = value.as_array() else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|item| resolve(source, item).ok())
        .filter_map(|item| pdf_string(&item))
        .collect()
}

/// Reads the `/S` subtype name of an action dictionary.
fn subtype_of(value: &PdfValue, source: &dyn FormSource) -> Option<String> {
    let dict = resolve(source, value).ok()?;
    let entries = dict.as_dict()?;
    let subtype = entries.get("S")?;
    pdf_string(&resolve(source, subtype).ok()?)
}

/// Parses one action dictionary into a [`SafeAction`].
///
/// # Errors
///
/// Returns a typed [`ActionError`] for dangerous or unknown action subtypes and
/// for malformed action dictionaries. This function never executes an action.
pub fn parse_action(value: &PdfValue, source: &dyn FormSource) -> Result<SafeAction, ActionError> {
    let resolved = resolve(source, value)?;
    let dict = resolved.as_dict().ok_or_else(|| ActionError::Malformed {
        context: "action",
        detail: "action is not a dictionary".to_owned(),
    })?;

    let subtype_value = dict.get("S").ok_or_else(|| ActionError::Malformed {
        context: "action /S",
        detail: "missing action subtype".to_owned(),
    })?;
    let subtype =
        pdf_string(&resolve(source, subtype_value)?).ok_or_else(|| ActionError::Malformed {
            context: "action /S",
            detail: "action subtype is not a name".to_owned(),
        })?;

    match subtype.as_str() {
        "ResetForm" => Ok(SafeAction::ResetForm {
            fields: action_fields(dict, source),
        }),
        "SubmitForm" => {
            let target = dict
                .get("F")
                .and_then(|value| resolve(source, value).ok())
                .and_then(|value| pdf_string(&value))
                .unwrap_or_default();
            Ok(SafeAction::SubmitForm {
                target,
                fields: action_fields(dict, source),
            })
        }
        "JavaScript" => Err(ActionError::JavaScriptRejected),
        "URI" => {
            let uri = dict
                .get("URI")
                .and_then(|value| resolve(source, value).ok())
                .and_then(|value| pdf_string(&value))
                .unwrap_or_default();
            if uri.to_ascii_lowercase().starts_with("javascript:") {
                Err(ActionError::JavaScriptRejected)
            } else {
                Err(ActionError::UriRejected)
            }
        }
        "Launch" => Err(ActionError::LaunchRejected),
        "ImportData" => Err(ActionError::ImportRejected),
        "ExportData" => Err(ActionError::ExportRejected),
        other => Err(ActionError::UnknownAction(other.to_owned())),
    }
}

/// Parses a field's `/A` and `/AA` dictionaries, recording rejected actions in
/// `unsupported` instead of failing.
pub fn parse_field_actions(
    field: impl Into<String>,
    dict: &BTreeMap<String, PdfValue>,
    source: &dyn FormSource,
    unsupported: &mut Vec<UnsupportedAction>,
) -> FieldActions {
    let field = field.into();
    let mut parsed = FieldActions {
        field: field.clone(),
        ..FieldActions::default()
    };

    if let Some(aa_value) = dict.get("AA")
        && let Ok(aa) = resolve(source, aa_value)
        && let Some(entries) = aa.as_dict()
    {
        for (key, value) in entries {
            let Some(trigger) = ActionTrigger::from_key(key) else {
                continue;
            };
            let site = ActionSite::Additional(trigger);
            let action_type = subtype_of(value, source).unwrap_or_else(|| "Unknown".to_owned());
            match parse_action(value, source) {
                Ok(action) => {
                    parsed.additional.insert(trigger, action);
                }
                Err(error) => unsupported.push(UnsupportedAction {
                    field: field.clone(),
                    site,
                    action_type,
                    reason: error.to_string(),
                }),
            }
        }
    }

    if let Some(a_value) = dict.get("A") {
        let site = ActionSite::Main;
        let action_type = subtype_of(a_value, source).unwrap_or_else(|| "Unknown".to_owned());
        match parse_action(a_value, source) {
            Ok(action) => parsed.main = Some(action),
            Err(error) => unsupported.push(UnsupportedAction {
                field: field.clone(),
                site,
                action_type,
                reason: error.to_string(),
            }),
        }
    }

    parsed
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    struct TestSource;

    impl FormSource for TestSource {
        fn catalog(&self) -> PdfValue {
            PdfValue::Dictionary(BTreeMap::new())
        }

        fn resolve(&self, value: &PdfValue) -> Option<PdfValue> {
            match value {
                PdfValue::Reference(_) => None,
                other => Some(other.clone()),
            }
        }
    }

    fn name(value: &str) -> PdfValue {
        PdfValue::Name(value.to_owned())
    }

    fn text(value: &str) -> PdfValue {
        PdfValue::Text(value.to_owned())
    }

    fn action(subtype: &str, extra: &[(&str, PdfValue)]) -> PdfValue {
        let mut entries = BTreeMap::new();
        entries.insert("S".to_owned(), name(subtype));
        for (key, value) in extra {
            entries.insert((*key).to_owned(), value.clone());
        }
        PdfValue::Dictionary(entries)
    }

    fn field_dict(aa: &[(&str, PdfValue)], main: Option<PdfValue>) -> BTreeMap<String, PdfValue> {
        let mut dict = BTreeMap::new();
        if !aa.is_empty() {
            let mut entries = BTreeMap::new();
            for (key, value) in aa {
                entries.insert((*key).to_owned(), value.clone());
            }
            dict.insert("AA".to_owned(), PdfValue::Dictionary(entries));
        }
        if let Some(main) = main {
            dict.insert("A".to_owned(), main);
        }
        dict
    }

    #[test]
    fn trigger_keys_map_to_all_four_aa_entries() {
        assert_eq!(ActionTrigger::from_key("K"), Some(ActionTrigger::Keystroke));
        assert_eq!(ActionTrigger::from_key("F"), Some(ActionTrigger::Format));
        assert_eq!(ActionTrigger::from_key("V"), Some(ActionTrigger::Validate));
        assert_eq!(ActionTrigger::from_key("C"), Some(ActionTrigger::Calculate));
        assert_eq!(ActionTrigger::from_key("Z"), None);
        assert_eq!(ActionTrigger::Keystroke.key(), "K");
    }

    #[test]
    fn parses_aa_entries_for_k_f_v_c() {
        let reset = action(
            "ResetForm",
            &[("Fields", PdfValue::Array(vec![text("a"), text("b")]))],
        );
        let submit = action("SubmitForm", &[("F", text("https://example.test/submit"))]);
        let dict = field_dict(
            &[
                ("K", reset.clone()),
                ("F", submit.clone()),
                ("V", reset.clone()),
                ("C", submit.clone()),
            ],
            None,
        );

        let mut unsupported = Vec::new();
        let parsed = parse_field_actions("order", &dict, &TestSource, &mut unsupported);

        assert!(unsupported.is_empty());
        assert_eq!(parsed.additional.len(), 4);
        assert_eq!(
            parsed.additional.get(&ActionTrigger::Keystroke),
            Some(&SafeAction::ResetForm {
                fields: vec!["a".to_owned(), "b".to_owned()]
            })
        );
        assert_eq!(
            parsed.additional.get(&ActionTrigger::Calculate),
            Some(&SafeAction::SubmitForm {
                target: "https://example.test/submit".to_owned(),
                fields: Vec::new(),
            })
        );
    }

    #[test]
    fn dangerous_actions_are_rejected_with_typed_errors() {
        let source = TestSource;
        assert_eq!(
            parse_action(
                &action("JavaScript", &[("JS", text("app.alert(1)"))]),
                &source
            ),
            Err(ActionError::JavaScriptRejected)
        );
        assert_eq!(
            parse_action(
                &action("URI", &[("URI", text("https://example.test"))]),
                &source
            ),
            Err(ActionError::UriRejected)
        );
        assert_eq!(
            parse_action(&action("Launch", &[]), &source),
            Err(ActionError::LaunchRejected)
        );
        assert_eq!(
            parse_action(&action("ImportData", &[]), &source),
            Err(ActionError::ImportRejected)
        );
        assert_eq!(
            parse_action(&action("ExportData", &[]), &source),
            Err(ActionError::ExportRejected)
        );
    }

    #[test]
    fn javascript_uri_is_reported_as_javascript() {
        let source = TestSource;
        let js_uri = action("URI", &[("URI", text("JaVaScRiPt:alert(1)"))]);
        assert_eq!(
            parse_action(&js_uri, &source),
            Err(ActionError::JavaScriptRejected)
        );
    }

    #[test]
    fn reset_and_submit_actions_are_modelled() {
        let source = TestSource;
        let reset = action(
            "ResetForm",
            &[("Fields", PdfValue::Array(vec![text("x"), name("y")]))],
        );
        assert_eq!(
            parse_action(&reset, &source),
            Ok(SafeAction::ResetForm {
                fields: vec!["x".to_owned(), "y".to_owned()]
            })
        );

        let submit = action(
            "SubmitForm",
            &[
                ("F", text("fdf://localhost/submit")),
                ("Fields", PdfValue::Array(vec![text("x")])),
            ],
        );
        assert_eq!(
            parse_action(&submit, &source),
            Ok(SafeAction::SubmitForm {
                target: "fdf://localhost/submit".to_owned(),
                fields: vec!["x".to_owned()],
            })
        );
    }

    #[test]
    fn empty_fields_array_means_reset_all() {
        let source = TestSource;
        let reset = action("ResetForm", &[("Fields", PdfValue::Array(Vec::new()))]);
        assert_eq!(
            parse_action(&reset, &source),
            Ok(SafeAction::ResetForm { fields: Vec::new() })
        );
    }

    #[test]
    fn main_action_is_limited_to_reset_and_submit() {
        let dict = field_dict(
            &[],
            Some(action(
                "ResetForm",
                &[("Fields", PdfValue::Array(vec![text("a")]))],
            )),
        );
        let mut unsupported = Vec::new();
        let parsed = parse_field_actions("submit_button", &dict, &TestSource, &mut unsupported);
        assert_eq!(
            parsed.main,
            Some(SafeAction::ResetForm {
                fields: vec!["a".to_owned()]
            })
        );
        assert!(unsupported.is_empty());

        let dict = field_dict(&[], Some(action("JavaScript", &[])));
        let mut unsupported = Vec::new();
        let parsed = parse_field_actions("bad_button", &dict, &TestSource, &mut unsupported);
        assert_eq!(parsed.main, None);
        assert_eq!(unsupported.len(), 1);
        assert_eq!(unsupported[0].action_type, "JavaScript");
    }

    #[test]
    fn unknown_action_is_reported_not_fatal() {
        let dict = field_dict(&[("K", action("GoToR", &[]))], None);
        let mut collector = ActionCollector::new();
        collector.add_field("field", &dict, &TestSource);
        let report = collector.finish();

        assert!(report.plan().is_empty());
        assert_eq!(report.unsupported_count(), 1);
        assert_eq!(report.unsupported()[0].action_type, "GoToR");
        assert!(report.unsupported()[0].reason.contains("GoToR"));
    }

    #[test]
    fn report_is_deterministic_and_orders_steps() {
        let mut collector = ActionCollector::new();
        let bad = field_dict(&[("K", action("JavaScript", &[]))], None);
        collector.add_field("b", &bad, &TestSource);
        let good = field_dict(
            &[
                ("C", action("ResetForm", &[])),
                ("K", action("SubmitForm", &[("F", text("t"))])),
            ],
            Some(action("ResetForm", &[])),
        );
        collector.add_field("a", &good, &TestSource);

        let report = collector.finish();
        let steps = report.plan().steps();
        assert_eq!(steps.len(), 3);
        assert_eq!(steps[0].site, ActionSite::Main);
        assert_eq!(
            steps[1].site,
            ActionSite::Additional(ActionTrigger::Keystroke)
        );
        assert_eq!(
            steps[2].site,
            ActionSite::Additional(ActionTrigger::Calculate)
        );
        assert!(steps.iter().all(|step| step.field == "a"));
        assert_eq!(report.unsupported()[0].field, "b");
    }

    #[test]
    fn malformed_action_is_reported() {
        let source = TestSource;
        let not_a_dict = PdfValue::Text("not a dictionary".to_owned());
        assert!(matches!(
            parse_action(&not_a_dict, &source),
            Err(ActionError::Malformed { .. })
        ));

        let missing_subtype = PdfValue::Dictionary(BTreeMap::new());
        assert!(matches!(
            parse_action(&missing_subtype, &source),
            Err(ActionError::Malformed { .. })
        ));
    }
}
