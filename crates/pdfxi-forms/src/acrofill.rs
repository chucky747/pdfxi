//! Filling an existing `/AcroForm` dictionary model and writing appearances.
//!
//! [`fill_acroform`] applies typed [`FieldAssignment`]s to a concrete
//! [`PdfValue`] AcroForm dictionary (the same model emitted by
//! [`crate::acroform_dictionary`]). For each assignment it resolves the field by
//! fully qualified name, checks the `/Ff` flags, normalises the value, writes
//! `/V` (and optionally `/DV`), and regenerates every widget's `/AP /N` stream
//! with [`crate::build_appearance`]. Stateful widgets also receive the correct
//! `/AS` entry.
//!
//! # What "byte-stable" means here
//!
//! This crate has no PDF writer, so "byte-stable" is defined at the model
//! boundary: a widget that is not part of a successful fill is never visited
//! and its [`PdfValue`] subtree compares equal to the input. A host writer that
//! serialises dictionaries deterministically therefore emits identical bytes
//! for those widgets. Widgets of a field that *is* filled are all updated
//! together, so a multi-widget field cannot end up with mixed states.
//!
//! # Value normalisation
//!
//! - Dates supplied with [`Format::Date`] accept the configured layout or ISO
//!   `YYYY-MM-DD` and are stored as canonical ISO text with a validated
//!   calendar date.
//! - Numbers supplied with [`Format::Numeric`] or [`Format::Currency`] accept a
//!   finite number or a decimal string (with an optional currency symbol and
//!   grouping separators) and are stored as a finite [`FieldValue::Number`].
//! - Choice and radio fields accept either an export/display selection or a
//!   zero-based numeric index into `/Opt`. Display labels are mapped to export
//!   values for `/V` and kept for the appearance text.
//! - Checkbox values become `On`/`Off` names; radio `/V` is a name; choice `/V`
//!   is a text string or an array of text strings.
//!
//! All outcomes are collected, not fail-fast: unknown, read-only, malformed and
//! otherwise invalid assignments appear in [`AcroFillReport::rejected`], valid
//! no-ops in [`AcroFillReport::skipped`], and successful writes in
//! [`AcroFillReport::filled`]. Every list is sorted by fully qualified field
//! name, so reports are independent of assignment order.
//!
//! No I/O, no unsafe code, and no panics are used. Indirect widget references
//! are left untouched because this crate deliberately has no object resolver;
//! such a field is reported as malformed rather than silently half-updated.

use crate::acroform::{PdfValue, classify_field_type, coerce_value, decode_pdf_name, textish};
use crate::appearance::{
    Alignment, AppearanceDictionary, AppearanceError, AppearanceStream, NormalAppearance, RgbColor,
    Rotation, STATE_OFF, STATE_ON, WidgetAppearance, WidgetRect, build_appearance,
};
use crate::field::{Field, FieldFlags, FieldType, FieldValue, ValueKind};
use crate::rules::{CurrencyFormat, DateLayout, Format};
use std::collections::BTreeMap;
use std::fmt;

/// Maximum field-tree depth accepted during a fill, matching the loader.
const MAX_DEPTH: usize = 128;
/// PDF `/Ff` required bit, common to all field types.
const FF_REQUIRED: u32 = 1 << 0;

/// One requested write to a named field.
///
/// Use [`FieldAssignment::new`] and the builder methods, or convert a
/// `(name, value)` tuple with [`From`]. A format hint enables date/number
/// normalisation; without one, text and numeric values are stored as supplied.
#[derive(Debug, Clone, PartialEq)]
pub struct FieldAssignment {
    name: String,
    value: FieldValue,
    format: Option<Format>,
    set_default: bool,
}

impl FieldAssignment {
    /// Creates an assignment for the fully qualified field `name`.
    pub fn new(name: impl Into<String>, value: FieldValue) -> Self {
        Self {
            name: name.into(),
            value,
            format: None,
            set_default: false,
        }
    }

    /// Enables date or number normalisation for this assignment.
    #[must_use]
    pub fn with_format(mut self, format: Format) -> Self {
        self.format = Some(format);
        self
    }

    /// Also writes the normalised value to the field's `/DV` entry.
    #[must_use]
    pub const fn also_set_default(mut self) -> Self {
        self.set_default = true;
        self
    }

    /// Returns the fully qualified field name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the requested value.
    pub fn value(&self) -> &FieldValue {
        &self.value
    }

    /// Returns the format hint, if any.
    pub fn format(&self) -> Option<&Format> {
        self.format.as_ref()
    }

    /// Returns `true` when `/DV` should be written too.
    pub const fn sets_default(&self) -> bool {
        self.set_default
    }
}

impl From<(String, FieldValue)> for FieldAssignment {
    fn from((name, value): (String, FieldValue)) -> Self {
        Self::new(name, value)
    }
}

impl From<(&str, FieldValue)> for FieldAssignment {
    fn from((name, value): (&str, FieldValue)) -> Self {
        Self::new(name, value)
    }
}

/// A typed failure while applying one [`FieldAssignment`].
#[derive(Debug, Clone, PartialEq)]
pub enum AcroFillError {
    /// No terminal field has the requested fully qualified name.
    UnknownField(String),
    /// The field carries the read-only (locked) `/Ff` bit.
    ReadOnlyField(String),
    /// A required field was assigned an empty value.
    RequiredField(String),
    /// The value shape does not fit the widget type.
    TypeMismatch {
        /// Fully qualified field name.
        field: String,
        /// Slash-separated accepted value kinds.
        expected: String,
        /// The rejected value's kind.
        found: ValueKind,
    },
    /// A choice or radio value was not one of the declared options.
    UnknownChoice {
        /// Fully qualified field name.
        field: String,
        /// The rejected selection.
        value: String,
    },
    /// A numeric choice index fell outside `/Opt`.
    ChoiceIndexOutOfRange {
        /// Fully qualified field name.
        field: String,
        /// The requested index.
        index: i64,
        /// Number of available options.
        count: usize,
    },
    /// A date string was not a valid calendar date in the requested layout.
    InvalidDate {
        /// Fully qualified field name.
        field: String,
        /// The rejected string.
        value: String,
    },
    /// A numeric string could not be parsed as a finite number.
    InvalidNumber {
        /// Fully qualified field name.
        field: String,
        /// The rejected string.
        value: String,
    },
    /// The AcroForm or a field node had an unexpected structure.
    Malformed {
        /// The entry being read, for example `"AcroForm /Fields"`.
        context: &'static str,
        /// A human-readable explanation.
        detail: String,
    },
    /// A widget could not produce an appearance stream.
    Appearance {
        /// Fully qualified field name.
        field: String,
        /// The underlying appearance error.
        source: AppearanceError,
    },
}

impl fmt::Display for AcroFillError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownField(name) => write!(f, "no field named '{name}'"),
            Self::ReadOnlyField(name) => write!(f, "field '{name}' is read-only"),
            Self::RequiredField(name) => {
                write!(f, "required field '{name}' cannot be set to empty")
            }
            Self::TypeMismatch {
                field,
                expected,
                found,
            } => write!(
                f,
                "field '{field}' holds a {} value; expected one of {expected}",
                found.label()
            ),
            Self::UnknownChoice { field, value } => {
                write!(f, "field '{field}' has no option '{value}'")
            }
            Self::ChoiceIndexOutOfRange {
                field,
                index,
                count,
            } => write!(
                f,
                "choice index {index} for field '{field}' is outside 0..{count}"
            ),
            Self::InvalidDate { field, value } => {
                write!(f, "field '{field}' has invalid date '{value}'")
            }
            Self::InvalidNumber { field, value } => {
                write!(f, "field '{field}' has invalid number '{value}'")
            }
            Self::Malformed { context, detail } => write!(f, "malformed {context}: {detail}"),
            Self::Appearance { field, source } => {
                write!(f, "cannot build appearance for '{field}': {source}")
            }
        }
    }
}

impl std::error::Error for AcroFillError {}

/// Why a valid assignment did not change the document.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    /// The normalised value equals the field's current value.
    Unchanged,
    /// A pushbutton carries no value, so an empty assignment is a no-op.
    ButtonNoValue,
}

/// A field that was successfully written.
#[derive(Debug, Clone, PartialEq)]
pub struct FilledField {
    /// Fully qualified field name.
    pub name: String,
    /// The field's value before the write.
    pub old: FieldValue,
    /// The normalised value written to `/V`.
    pub new: FieldValue,
    /// Number of widget appearances regenerated.
    pub widgets: usize,
}

/// A valid assignment that changed nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkippedField {
    /// Fully qualified field name.
    pub name: String,
    /// Why the write was skipped.
    pub reason: SkipReason,
}

/// An assignment that was refused, with its typed reason.
#[derive(Debug, Clone, PartialEq)]
pub struct RejectedField {
    /// Fully qualified field name.
    pub name: String,
    /// Why the write was rejected.
    pub error: AcroFillError,
}

/// A deterministic report of all fill outcomes.
///
/// Each list is sorted by fully qualified field name, and a name appears at
/// most once per list; a later assignment to the same name replaces the
/// earlier entry in that list.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AcroFillReport {
    filled: Vec<FilledField>,
    skipped: Vec<SkippedField>,
    rejected: Vec<RejectedField>,
}

impl AcroFillReport {
    /// Returns the successful writes in name order.
    pub fn filled(&self) -> &[FilledField] {
        &self.filled
    }

    /// Returns the skipped assignments in name order.
    pub fn skipped(&self) -> &[SkippedField] {
        &self.skipped
    }

    /// Returns the rejected assignments in name order.
    pub fn rejected(&self) -> &[RejectedField] {
        &self.rejected
    }

    /// Returns `true` when no assignment produced any outcome.
    pub fn is_empty(&self) -> bool {
        self.filled.is_empty() && self.skipped.is_empty() && self.rejected.is_empty()
    }

    /// Returns the counts of filled, skipped and rejected assignments.
    pub const fn counts(&self) -> (usize, usize, usize) {
        (self.filled.len(), self.skipped.len(), self.rejected.len())
    }
}

impl fmt::Display for AcroFillReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (filled, skipped, rejected) = self.counts();
        write!(
            f,
            "fill report: {filled} filled, {skipped} skipped, {rejected} rejected"
        )
    }
}

/// Applies assignments to a concrete AcroForm dictionary and returns a report.
///
/// `acroform` must be a [`PdfValue::Dictionary`] with a `/Fields` array. Every
/// assignment is applied in order, but the report is ordered by name. An
/// invalid assignment leaves the document untouched; earlier successful writes
/// are kept.
///
/// # Example
///
/// ```text
/// use pdfxi_forms::{
///     FieldAssignment, FieldSpec, FieldValue, WidgetRect, acroform_dictionary,
///     fill_acroform,
/// };
///
/// let mut form = acroform_dictionary(&[
///     FieldSpec::text("name", WidgetRect::new(0.0, 0.0, 120.0, 20.0), 0),
/// ]).unwrap();
/// let report = fill_acroform(
///     &mut form,
///     [FieldAssignment::new("name", FieldValue::Text("Ada".into()))],
/// );
/// assert_eq!(report.counts(), (1, 0, 0));
/// ```
pub fn fill_acroform<I, A>(acroform: &mut PdfValue, assignments: I) -> AcroFillReport
where
    I: IntoIterator<Item = A>,
    A: Into<FieldAssignment>,
{
    let mut filled: BTreeMap<String, FilledField> = BTreeMap::new();
    let mut skipped: BTreeMap<String, SkippedField> = BTreeMap::new();
    let mut rejected: BTreeMap<String, RejectedField> = BTreeMap::new();

    for raw in assignments {
        let assignment: FieldAssignment = raw.into();
        match apply_one(acroform, &assignment) {
            Ok(ApplyOutcome::Filled(entry)) => {
                filled.insert(entry.name.clone(), entry);
            }
            Ok(ApplyOutcome::Skipped(entry)) => {
                skipped.insert(entry.name.clone(), entry);
            }
            Err(error) => {
                rejected.insert(
                    assignment.name().to_owned(),
                    RejectedField {
                        name: assignment.name().to_owned(),
                        error,
                    },
                );
            }
        }
    }

    AcroFillReport {
        filled: filled.into_values().collect(),
        skipped: skipped.into_values().collect(),
        rejected: rejected.into_values().collect(),
    }
}

enum ApplyOutcome {
    Filled(FilledField),
    Skipped(SkippedField),
}

fn apply_one(
    acroform: &mut PdfValue,
    assignment: &FieldAssignment,
) -> Result<ApplyOutcome, AcroFillError> {
    let resolved = find_field(acroform, assignment.name())?
        .ok_or_else(|| AcroFillError::UnknownField(assignment.name().to_owned()))?;

    if resolved.flags.is_read_only() {
        return Err(AcroFillError::ReadOnlyField(assignment.name().to_owned()));
    }
    if resolved.flags.contains(FieldFlags::from_bits(FF_REQUIRED)) && assignment.value().is_empty()
    {
        return Err(AcroFillError::RequiredField(assignment.name().to_owned()));
    }

    let validation_field = make_field(&resolved, &resolved.choices);
    let value_for_index = index_value(&validation_field, assignment.value())?;
    let appearance_labels = if resolved.labels.is_empty() {
        resolved.choices.as_slice()
    } else {
        resolved.labels.as_slice()
    };
    let normalised = normalise(
        &validation_field,
        appearance_labels,
        assignment.format(),
        &value_for_index,
    )?;

    if resolved.field_type == FieldType::Button && assignment.value().is_empty() {
        return Ok(ApplyOutcome::Skipped(SkippedField {
            name: resolved.name.clone(),
            reason: SkipReason::ButtonNoValue,
        }));
    }
    if resolved.old == normalised.value && !assignment.sets_default() {
        return Ok(ApplyOutcome::Skipped(SkippedField {
            name: resolved.name.clone(),
            reason: SkipReason::Unchanged,
        }));
    }

    let appearance_field = make_field(&resolved, appearance_labels);
    let active_states = active_states(&resolved, &normalised.value);

    // Generate every appearance before mutating, so a generation error leaves
    // the AcroForm completely unchanged.
    let mut generated: Vec<(Vec<usize>, PdfValue, Option<String>)> =
        Vec::with_capacity(resolved.widget_paths.len());
    for (index, widget) in resolved.widget_appearances.iter().enumerate() {
        let appearance = build_appearance(&appearance_field, widget, &normalised.appearance)
            .map_err(|source| AcroFillError::Appearance {
                field: resolved.name.clone(),
                source,
            })?;
        let active = active_states.get(index).cloned().flatten();
        generated.push((
            resolved.widget_paths[index].clone(),
            appearance_to_pdf(&appearance),
            active,
        ));
    }

    let field_dict =
        navigate_mut(acroform, &resolved.path).ok_or_else(|| AcroFillError::Malformed {
            context: "AcroForm /Fields",
            detail: "field path changed during fill".to_owned(),
        })?;
    field_dict.insert("V".to_owned(), normalised.pdf.clone());
    if assignment.sets_default() {
        field_dict.insert("DV".to_owned(), normalised.pdf.clone());
    }

    let widget_count = generated.len();
    for (path, ap, active) in generated {
        if let Some(widget) = navigate_mut(acroform, &path) {
            widget.insert("AP".to_owned(), ap);
            if let Some(state) = active {
                widget.insert("AS".to_owned(), PdfValue::Name(state));
            }
        }
    }

    Ok(ApplyOutcome::Filled(FilledField {
        name: resolved.name,
        old: resolved.old,
        new: normalised.value,
        widgets: widget_count,
    }))
}

#[derive(Debug, Clone, Default)]
struct Inherited {
    field_type: Option<String>,
    flags: u32,
    choices: Option<Vec<String>>,
    labels: Option<Vec<String>>,
    quadding: i64,
}

impl Inherited {
    fn merge(&self, dict: &BTreeMap<String, PdfValue>) -> Self {
        Self {
            field_type: dict
                .get("FT")
                .and_then(PdfValue::as_name)
                .map(decode_pdf_name)
                .or_else(|| self.field_type.clone()),
            flags: dict
                .get("Ff")
                .and_then(PdfValue::as_i64)
                .and_then(|value| u32::try_from(value).ok())
                .unwrap_or(self.flags),
            choices: dict
                .get("Opt")
                .and_then(crate::acroform::choices_from_opt)
                .or_else(|| self.choices.clone()),
            labels: dict
                .get("Opt")
                .and_then(choice_labels)
                .or_else(|| self.labels.clone()),
            quadding: dict
                .get("Q")
                .and_then(PdfValue::as_i64)
                .filter(|value| (0..=2).contains(value))
                .unwrap_or(self.quadding),
        }
    }
}

#[derive(Debug, Clone)]
struct Resolved {
    path: Vec<usize>,
    widget_paths: Vec<Vec<usize>>,
    name: String,
    field_type: FieldType,
    flags: FieldFlags,
    choices: Vec<String>,
    labels: Vec<String>,
    old: FieldValue,
    widget_appearances: Vec<WidgetAppearance>,
    on_states: Vec<Option<String>>,
}

fn find_field(root: &PdfValue, target: &str) -> Result<Option<Resolved>, AcroFillError> {
    let dict = root.as_dict().ok_or_else(|| AcroFillError::Malformed {
        context: "AcroForm",
        detail: "dictionary expected".to_owned(),
    })?;
    let fields = dict
        .get("Fields")
        .and_then(PdfValue::as_array)
        .ok_or_else(|| AcroFillError::Malformed {
            context: "AcroForm /Fields",
            detail: "array expected".to_owned(),
        })?;
    walk_fields(fields, &[], "", &Inherited::default(), target, false, 0)
}

fn walk_fields(
    fields: &[PdfValue],
    prefix: &[usize],
    parent_name: &str,
    inherited: &Inherited,
    target: &str,
    branch: bool,
    depth: usize,
) -> Result<Option<Resolved>, AcroFillError> {
    if depth > MAX_DEPTH {
        return Err(AcroFillError::Malformed {
            context: "AcroForm field tree",
            detail: "nesting limit exceeded".to_owned(),
        });
    }
    for (index, entry) in fields.iter().enumerate() {
        let Some(dict) = entry.as_dict() else {
            continue;
        };
        if branch && is_widget(entry) {
            continue;
        }
        let partial = dict.get("T").and_then(textish).unwrap_or_default();
        let joined = join_name(parent_name, &partial);
        let effective = inherited.merge(dict);
        let kids = dict.get("Kids").and_then(PdfValue::as_array);
        let all_widgets = kids
            .map(|items| !items.is_empty() && items.iter().all(is_widget))
            .unwrap_or(false);
        let declares_field = ["FT", "T", "V", "DV", "Ff", "Opt"]
            .iter()
            .any(|key| dict.contains_key(*key));
        let is_terminal = match kids {
            Some(items) if !items.is_empty() => all_widgets && declares_field,
            _ => true,
        };

        if is_terminal {
            if joined != target {
                continue;
            }
            let mut path = prefix.to_vec();
            path.push(index);
            let widgets: Vec<(Vec<usize>, &BTreeMap<String, PdfValue>)> = if all_widgets {
                kids.map(|items| {
                    items
                        .iter()
                        .enumerate()
                        .filter_map(|(widget_index, kid)| {
                            kid.as_dict().map(|widget_dict| {
                                let mut widget_path = path.clone();
                                widget_path.push(widget_index);
                                (widget_path, widget_dict)
                            })
                        })
                        .collect()
                })
                .unwrap_or_default()
            } else if is_widget(entry) {
                vec![(path.clone(), dict)]
            } else {
                Vec::new()
            };
            return build_resolved(&path, &joined, dict, &effective, widgets).map(Some);
        }

        let Some(items) = kids else {
            continue;
        };
        let mut child_prefix = prefix.to_vec();
        child_prefix.push(index);
        if let Some(found) = walk_fields(
            items,
            &child_prefix,
            &joined,
            &effective,
            target,
            true,
            depth + 1,
        )? {
            return Ok(Some(found));
        }
    }
    Ok(None)
}

fn build_resolved(
    path: &[usize],
    name: &str,
    dict: &BTreeMap<String, PdfValue>,
    effective: &Inherited,
    widgets: Vec<(Vec<usize>, &BTreeMap<String, PdfValue>)>,
) -> Result<Resolved, AcroFillError> {
    let field_type = classify_field_type(effective.field_type.as_deref(), effective.flags);
    let flags = FieldFlags::from_bits(effective.flags);
    let declared = effective.choices.clone().unwrap_or_default();
    let choices = if field_type == FieldType::Radio && declared.is_empty() {
        widgets
            .iter()
            .filter_map(|(_, widget)| state_names(widget).into_iter().next())
            .collect()
    } else {
        declared
    };
    let labels = effective.labels.clone().unwrap_or_default();
    let old = dict
        .get("V")
        .and_then(|value| coerce_value(value, field_type))
        .or_else(|| {
            dict.get("DV")
                .and_then(|value| coerce_value(value, field_type))
        })
        .unwrap_or(FieldValue::Empty);

    let mut widget_paths = Vec::with_capacity(widgets.len());
    let mut widget_appearances = Vec::with_capacity(widgets.len());
    let mut on_states = Vec::with_capacity(widgets.len());
    for (index, (widget_path, widget_dict)) in widgets.iter().enumerate() {
        widget_paths.push(widget_path.clone());
        widget_appearances.push(widget_appearance(widget_dict, effective.quadding, name)?);
        on_states.push(derive_on_state(widget_dict, field_type, &choices, index));
    }

    Ok(Resolved {
        path: path.to_vec(),
        widget_paths,
        name: name.to_owned(),
        field_type,
        flags,
        choices,
        labels,
        old,
        widget_appearances,
        on_states,
    })
}

fn make_field(resolved: &Resolved, choices: &[String]) -> Field {
    Field::new(resolved.name.clone(), resolved.field_type)
        .with_flags(resolved.flags)
        .with_choices(choices.iter().cloned())
}

fn join_name(parent: &str, partial: &str) -> String {
    if partial.is_empty() {
        parent.to_owned()
    } else if parent.is_empty() {
        partial.to_owned()
    } else {
        format!("{parent}.{partial}")
    }
}

fn is_widget(value: &PdfValue) -> bool {
    value
        .as_dict()
        .and_then(|dict| dict.get("Subtype"))
        .and_then(PdfValue::as_name)
        .map(decode_pdf_name)
        .as_deref()
        == Some("Widget")
}

fn choice_labels(value: &PdfValue) -> Option<Vec<String>> {
    let items = value.as_array()?;
    items
        .iter()
        .map(|item| match item {
            PdfValue::Array(pair) => pair.get(1).and_then(textish),
            other => textish(other),
        })
        .collect()
}

fn number(value: &PdfValue) -> Option<f64> {
    match value {
        PdfValue::Number(value) => Some(*value),
        _ => None,
    }
}

fn rect_from(value: &PdfValue) -> Option<WidgetRect> {
    let items = value.as_array()?;
    let values: Vec<f64> = items.iter().map(number).collect::<Option<_>>()?;
    let [x1, y1, x2, y2] = values.as_slice() else {
        return None;
    };
    Some(WidgetRect::new(
        *x1 as f32,
        *y1 as f32,
        (*x2 - *x1) as f32,
        (*y2 - *y1) as f32,
    ))
}

fn color_from(value: &PdfValue) -> Option<RgbColor> {
    let items = value.as_array()?;
    let values: Vec<f64> = items.iter().map(number).collect::<Option<_>>()?;
    let [red, green, blue] = values.as_slice() else {
        return None;
    };
    let color = RgbColor::new(*red as f32, *green as f32, *blue as f32);
    color.is_valid().then_some(color)
}

fn widget_appearance(
    dict: &BTreeMap<String, PdfValue>,
    quadding: i64,
    field_name: &str,
) -> Result<WidgetAppearance, AcroFillError> {
    let rect = dict
        .get("Rect")
        .and_then(rect_from)
        .ok_or_else(|| AcroFillError::Malformed {
            context: "widget /Rect",
            detail: "four numbers expected".to_owned(),
        })?;
    let mut appearance = WidgetAppearance::new(rect);
    if let Some(width) = dict
        .get("BS")
        .and_then(PdfValue::as_dict)
        .and_then(|bs| bs.get("W"))
        .and_then(number)
    {
        appearance.border_width = width as f32;
    }
    if let Some(mk) = dict.get("MK").and_then(PdfValue::as_dict) {
        if let Some(color) = mk.get("BC").and_then(color_from) {
            appearance.border_color = color;
        }
        if let Some(color) = mk.get("BG").and_then(color_from) {
            appearance.background = color;
        }
    }
    appearance.alignment = match quadding {
        1 => Alignment::Center,
        2 => Alignment::Right,
        _ => Alignment::Left,
    };
    if let Some(degrees) = dict.get("R").and_then(PdfValue::as_i64)
        && let Ok(degrees) = i16::try_from(degrees)
        && let Ok(rotation) = Rotation::try_from(degrees)
    {
        appearance.rotation = rotation;
    }
    appearance
        .validate()
        .map_err(|source| AcroFillError::Appearance {
            field: field_name.to_owned(),
            source,
        })?;
    Ok(appearance)
}

fn state_names(widget: &BTreeMap<String, PdfValue>) -> Vec<String> {
    widget
        .get("AP")
        .and_then(PdfValue::as_dict)
        .and_then(|ap| ap.get("N"))
        .and_then(PdfValue::as_dict)
        .map(|states| {
            states
                .keys()
                .filter(|name| name.as_str() != STATE_OFF)
                .cloned()
                .collect()
        })
        .unwrap_or_default()
}

fn derive_on_state(
    widget: &BTreeMap<String, PdfValue>,
    field_type: FieldType,
    choices: &[String],
    index: usize,
) -> Option<String> {
    if field_type != FieldType::Radio {
        return None;
    }
    let mut names = state_names(widget);
    names.sort();
    if let Some(name) = names.into_iter().next() {
        return Some(name);
    }
    if let Some(name) = widget
        .get("AS")
        .and_then(PdfValue::as_name)
        .map(decode_pdf_name)
        .filter(|name| name != STATE_OFF)
    {
        return Some(name);
    }
    choices.get(index).cloned()
}

fn appearance_to_pdf(appearance: &AppearanceDictionary) -> PdfValue {
    let normal = match &appearance.normal {
        NormalAppearance::Stream(stream) => stream_to_pdf(stream),
        NormalAppearance::States(states) => {
            let mut entries = BTreeMap::new();
            for (name, stream) in states {
                entries.insert(name.clone(), stream_to_pdf(stream));
            }
            PdfValue::Dictionary(entries)
        }
    };
    let mut dict = BTreeMap::new();
    dict.insert("N".to_owned(), normal);
    PdfValue::Dictionary(dict)
}

fn stream_to_pdf(stream: &AppearanceStream) -> PdfValue {
    let mut dict = BTreeMap::new();
    dict.insert("Type".to_owned(), PdfValue::Name("XObject".to_owned()));
    dict.insert("Subtype".to_owned(), PdfValue::Name("Form".to_owned()));
    dict.insert(
        "BBox".to_owned(),
        PdfValue::Array(vec![
            PdfValue::Number(f64::from(stream.bbox.x)),
            PdfValue::Number(f64::from(stream.bbox.y)),
            PdfValue::Number(f64::from(stream.bbox.width)),
            PdfValue::Number(f64::from(stream.bbox.height)),
        ]),
    );
    dict.insert(
        "Length".to_owned(),
        PdfValue::Number(stream.content.len() as f64),
    );
    PdfValue::Stream {
        dict,
        data: stream.content.clone(),
    }
}

fn active_states(resolved: &Resolved, value: &FieldValue) -> Vec<Option<String>> {
    let count = resolved.widget_paths.len();
    match resolved.field_type {
        FieldType::CheckBox => {
            let state = if value.as_bool().unwrap_or(false) {
                STATE_ON
            } else {
                STATE_OFF
            }
            .to_owned();
            vec![Some(state); count]
        }
        FieldType::Radio => {
            let selected = value.as_choices().and_then(|items| items.first()).cloned();
            let mut states = vec![Some(STATE_OFF.to_owned()); count];
            if let Some(selected) = selected {
                let winner = resolved
                    .on_states
                    .iter()
                    .position(|state| state.as_deref() == Some(selected.as_str()))
                    .unwrap_or(0);
                if let Some(slot) = states.get_mut(winner) {
                    *slot = Some(selected);
                }
            }
            states
        }
        _ => vec![None; count],
    }
}

fn as_dict_mut(value: &mut PdfValue) -> Option<&mut BTreeMap<String, PdfValue>> {
    match value {
        PdfValue::Dictionary(dict) => Some(dict),
        _ => None,
    }
}

fn as_array_mut(value: &mut PdfValue) -> Option<&mut Vec<PdfValue>> {
    match value {
        PdfValue::Array(items) => Some(items),
        _ => None,
    }
}

fn navigate_mut<'a>(
    root: &'a mut PdfValue,
    path: &[usize],
) -> Option<&'a mut BTreeMap<String, PdfValue>> {
    let (first, rest) = path.split_first()?;
    let fields = as_dict_mut(root)?
        .get_mut("Fields")
        .and_then(as_array_mut)?;
    let mut node = as_dict_mut(fields.get_mut(*first)?)?;
    for index in rest {
        let kids = node.get_mut("Kids").and_then(as_array_mut)?;
        node = as_dict_mut(kids.get_mut(*index)?)?;
    }
    Some(node)
}

#[derive(Debug, Clone)]
struct Normalised {
    value: FieldValue,
    appearance: FieldValue,
    pdf: PdfValue,
}

fn index_value(field: &Field, value: &FieldValue) -> Result<FieldValue, AcroFillError> {
    match (field.field_type(), value) {
        (FieldType::Radio | FieldType::Choice, FieldValue::Number(index)) => {
            let index_int = *index as i64;
            let count = field.choices().len();
            let selected = field.choices().get(index_int as usize).ok_or_else(|| {
                AcroFillError::ChoiceIndexOutOfRange {
                    field: field.name().to_owned(),
                    index: index_int,
                    count,
                }
            })?;
            Ok(FieldValue::Choice(vec![selected.clone()]))
        }
        _ => Ok(value.clone()),
    }
}

fn normalise(
    field: &Field,
    labels: &[String],
    format: Option<&Format>,
    value: &FieldValue,
) -> Result<Normalised, AcroFillError> {
    let name = field.name();
    if !field.accepts(value) {
        let expected = field
            .accepted_kinds()
            .iter()
            .map(|kind| kind.label())
            .collect::<Vec<_>>()
            .join("/");
        return Err(AcroFillError::TypeMismatch {
            field: name.to_owned(),
            expected,
            found: value.kind(),
        });
    }

    match field.field_type() {
        FieldType::Button => Ok(Normalised {
            value: FieldValue::Empty,
            appearance: FieldValue::Empty,
            pdf: PdfValue::Null,
        }),
        FieldType::CheckBox => {
            let on = value.as_bool().unwrap_or(false);
            let state = if on { STATE_ON } else { STATE_OFF }.to_owned();
            Ok(Normalised {
                value: FieldValue::Bool(on),
                appearance: FieldValue::Bool(on),
                pdf: PdfValue::Name(state),
            })
        }
        FieldType::Radio => {
            let selected = selected_indices(field, labels, value)?;
            match selected.first() {
                None => Ok(Normalised {
                    value: FieldValue::Empty,
                    appearance: FieldValue::Empty,
                    pdf: PdfValue::Null,
                }),
                Some(&index) => {
                    let export = field.choices()[index].clone();
                    Ok(Normalised {
                        value: FieldValue::Choice(vec![export.clone()]),
                        appearance: FieldValue::Choice(vec![export.clone()]),
                        pdf: PdfValue::Name(export),
                    })
                }
            }
        }
        FieldType::Choice => {
            let selected = selected_indices(field, labels, value)?;
            let exports: Vec<String> = selected
                .iter()
                .map(|&index| field.choices()[index].clone())
                .collect();
            let shown: Vec<String> = selected
                .iter()
                .map(|&index| {
                    labels
                        .get(index)
                        .cloned()
                        .unwrap_or_else(|| field.choices()[index].clone())
                })
                .collect();
            let pdf = match exports.as_slice() {
                [] => PdfValue::Null,
                [single] => PdfValue::Text(single.clone()),
                many => PdfValue::Array(many.iter().cloned().map(PdfValue::Text).collect()),
            };
            Ok(Normalised {
                value: FieldValue::Choice(exports),
                appearance: FieldValue::Choice(shown),
                pdf,
            })
        }
        FieldType::Text | FieldType::Signature | FieldType::Unknown => match format {
            Some(Format::Date(layout)) => {
                let text = value.as_text().ok_or_else(|| AcroFillError::InvalidDate {
                    field: name.to_owned(),
                    value: format!("{value:?}"),
                })?;
                let iso = normalise_date(name, text, *layout)?;
                Ok(Normalised {
                    value: FieldValue::Text(iso.clone()),
                    appearance: FieldValue::Text(iso.clone()),
                    pdf: PdfValue::Text(iso),
                })
            }
            Some(Format::Numeric) => {
                let number = numeric_value(name, value, None, false)?;
                Ok(number_normalised(number))
            }
            Some(Format::Currency(currency)) => {
                let number = numeric_value(name, value, Some(*currency), currency.grouping())?;
                Ok(number_normalised(number))
            }
            Some(Format::Pattern(_)) | None => match value {
                FieldValue::Empty => Ok(Normalised {
                    value: FieldValue::Empty,
                    appearance: FieldValue::Empty,
                    pdf: PdfValue::Null,
                }),
                FieldValue::Text(text) => Ok(Normalised {
                    value: FieldValue::Text(text.clone()),
                    appearance: FieldValue::Text(text.clone()),
                    pdf: PdfValue::Text(text.clone()),
                }),
                FieldValue::Number(number) => {
                    if !number.is_finite() {
                        return Err(AcroFillError::InvalidNumber {
                            field: name.to_owned(),
                            value: number.to_string(),
                        });
                    }
                    Ok(number_normalised(*number))
                }
                _ => Err(AcroFillError::TypeMismatch {
                    field: name.to_owned(),
                    expected: "empty/text/number".to_owned(),
                    found: value.kind(),
                }),
            },
        },
    }
}

fn number_normalised(number: f64) -> Normalised {
    Normalised {
        value: FieldValue::Number(number),
        appearance: FieldValue::Number(number),
        pdf: PdfValue::Number(number),
    }
}

fn numeric_value(
    name: &str,
    value: &FieldValue,
    currency: Option<CurrencyFormat>,
    grouping: bool,
) -> Result<f64, AcroFillError> {
    let parsed = match value {
        FieldValue::Number(number) => *number,
        FieldValue::Text(text) => {
            let mut cleaned = text.trim().to_owned();
            if let Some(format) = currency
                && let Some(symbol) = format.symbol()
            {
                cleaned = cleaned.replace(symbol, "");
            }
            if grouping {
                cleaned = cleaned.replace(',', "");
            }
            cleaned
                .trim()
                .parse::<f64>()
                .map_err(|_| AcroFillError::InvalidNumber {
                    field: name.to_owned(),
                    value: text.clone(),
                })?
        }
        other => {
            return Err(AcroFillError::TypeMismatch {
                field: name.to_owned(),
                expected: "number/text".to_owned(),
                found: other.kind(),
            });
        }
    };
    if !parsed.is_finite() {
        return Err(AcroFillError::InvalidNumber {
            field: name.to_owned(),
            value: parsed.to_string(),
        });
    }
    Ok(parsed)
}

fn selected_indices(
    field: &Field,
    labels: &[String],
    value: &FieldValue,
) -> Result<Vec<usize>, AcroFillError> {
    let choices = field.choices();
    let mut selected: Vec<String> = match value {
        FieldValue::Choice(values) => values.clone(),
        FieldValue::Text(text) => vec![text.clone()],
        FieldValue::Empty => Vec::new(),
        other => {
            return Err(AcroFillError::TypeMismatch {
                field: field.name().to_owned(),
                expected: "empty/text/choice".to_owned(),
                found: other.kind(),
            });
        }
    };
    if field.field_type() == FieldType::Radio && selected.len() > 1 {
        selected.truncate(1);
    }
    selected
        .iter()
        .map(|value| {
            choices
                .iter()
                .position(|option| option == value)
                .or_else(|| labels.iter().position(|label| label == value))
                .ok_or_else(|| AcroFillError::UnknownChoice {
                    field: field.name().to_owned(),
                    value: value.clone(),
                })
        })
        .collect()
}

fn normalise_date(name: &str, text: &str, layout: DateLayout) -> Result<String, AcroFillError> {
    let invalid = || AcroFillError::InvalidDate {
        field: name.to_owned(),
        value: text.to_owned(),
    };
    let parts: Vec<&str> = text.split(['-', '/', '.']).collect();
    if parts.len() != 3 || parts.iter().any(|part| part.is_empty()) {
        return Err(invalid());
    }
    let digits = |part: &str, width: usize| -> Option<u32> {
        if part.len() == width && part.bytes().all(|byte| byte.is_ascii_digit()) {
            part.parse::<u32>().ok()
        } else {
            None
        }
    };
    let (year, month, day) = if text.contains('-') {
        (
            digits(parts[0], 4),
            digits(parts[1], 2),
            digits(parts[2], 2),
        )
    } else {
        let separator = match layout {
            DateLayout::Iso | DateLayout::Us => '/',
            DateLayout::European => '.',
        };
        if !text.contains(separator) {
            return Err(invalid());
        }
        match layout {
            DateLayout::Iso => (
                digits(parts[0], 4),
                digits(parts[1], 2),
                digits(parts[2], 2),
            ),
            DateLayout::Us => (
                digits(parts[2], 4),
                digits(parts[0], 2),
                digits(parts[1], 2),
            ),
            DateLayout::European => (
                digits(parts[2], 4),
                digits(parts[1], 2),
                digits(parts[0], 2),
            ),
        }
    };
    let (year, month, day) = match (year, month, day) {
        (Some(year), Some(month), Some(day)) => (year, month, day),
        _ => return Err(invalid()),
    };
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let days = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return Err(invalid()),
    };
    if year == 0 || day == 0 || day > days {
        return Err(invalid());
    }
    Ok(format!("{year:04}-{month:02}-{day:02}"))
}

#[cfg(test)]
mod tests;
