//! Heuristic recognition of static form areas from caller-supplied hints.
//!
//! This module does **not** inspect page content, images, or fonts and performs
//! no OCR. The caller supplies a bounded list of [`FormHint`]s — rectangles that
//! have already been classified as a label, input box, checkbox, radio group, or
//! choice list by some upstream process (for example a PDF content-stream
//! analyser or a human authoring tool). Recognition here is a deterministic
//! pairing pass: each control hint is associated with at most one nearby label
//! hint on the same page, assigned a stable generated name, and given a
//! confidence in `0.0..=1.0`.
//!
//! The result is a [`RecognitionPlan`] whose [`RecognitionPlan::conversion_plan`]
//! produces a typed [`ConversionPlan`] of [`FieldSpec`] values. Those specs can
//! be turned into in-memory [`Field`]s or an `/AcroForm` dictionary model with
//! the existing [`crate::build`] helpers. Labels are never converted into
//! fields; unmatched label texts are reported as orphans so a host can decide
//! whether to attach them manually.
//!
//! Both control processing and output ordering use an intrinsic geometric key,
//! so the result is independent of the order in which hints are supplied. All
//! inputs are bounded and invalid geometry or over-long text yields a typed
//! [`RecognitionError`] rather than a panic.

use crate::acroform::PdfValue;
use crate::appearance::WidgetRect;
use crate::build::{CreationError, FieldSpec, acroform_dictionary};
use crate::field::FieldType;
use crate::form::{Form, FormError};
use std::cmp::Ordering;
use std::fmt;

/// Maximum number of hints accepted by [`recognize`].
pub const MAX_HINTS: usize = 4096;

/// Maximum number of characters in a hint's label or name text.
pub const RECOGNITION_MAX_TEXT_LEN: usize = 256;

/// Maximum perpendicular gap, in points, within which a label may attach to a
/// control. This is a heuristic tolerating typical form padding and leading.
pub const LABEL_PROXIMITY: f32 = 24.0;

/// The caller-supplied classification of a static form area.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HintKind {
    /// A text label that may name a nearby control.
    Label,
    /// A single-line or multiline text input box.
    InputBox,
    /// A checkbox with an on/off state.
    CheckBox,
    /// A radio group (one option selected).
    RadioGroup,
    /// A choice list or combo box.
    Choice,
}

impl HintKind {
    /// Returns `true` when this hint describes a control that can become a
    /// field (that is, anything other than [`HintKind::Label`]).
    pub const fn is_control(self) -> bool {
        !matches!(self, Self::Label)
    }

    /// Returns a stable lowercase label for diagnostics and bundle metadata.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Label => "label",
            Self::InputBox => "input",
            Self::CheckBox => "checkbox",
            Self::RadioGroup => "radio",
            Self::Choice => "choice",
        }
    }
}

impl fmt::Display for HintKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// One caller-supplied hint: a rectangle, a classification, and optional text.
///
/// Build one with [`FormHint::new`] and enrich it with [`FormHint::with_text`]
/// (the visible label/prompt, used for naming and as a tooltip) and
/// [`FormHint::with_name`] (an explicit fully qualified field name). All text is
/// bounded by [`RECOGNITION_MAX_TEXT_LEN`].
#[derive(Debug, Clone, PartialEq)]
pub struct FormHint {
    kind: HintKind,
    rect: WidgetRect,
    page: u32,
    text: Option<String>,
    name: Option<String>,
}

impl FormHint {
    /// Creates a hint of `kind` on zero-based `page` at `rect`.
    pub const fn new(kind: HintKind, rect: WidgetRect, page: u32) -> Self {
        Self {
            kind,
            rect,
            page,
            text: None,
            name: None,
        }
    }

    /// Attaches caller-supplied text (a visible label or prompt).
    #[must_use]
    pub fn with_text(mut self, text: impl Into<String>) -> Self {
        self.text = Some(text.into());
        self
    }

    /// Attaches an explicit fully qualified field name.
    #[must_use]
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Returns the hint classification.
    pub const fn kind(&self) -> HintKind {
        self.kind
    }

    /// Returns the hint rectangle.
    pub const fn rect(&self) -> WidgetRect {
        self.rect
    }

    /// Returns the zero-based page index.
    pub const fn page(&self) -> u32 {
        self.page
    }

    /// Returns the caller-supplied text, if any.
    pub fn text(&self) -> Option<&str> {
        self.text.as_deref()
    }

    /// Returns the explicit field name, if any.
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }
}

/// A control that was recognized, with its derived name and confidence.
#[derive(Debug, Clone, PartialEq)]
pub struct RecognizedField {
    name: String,
    field_type: FieldType,
    rect: WidgetRect,
    page: u32,
    label: Option<String>,
    confidence: f32,
    rank: usize,
}

impl RecognizedField {
    /// Returns the stable generated field name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the target AcroForm field type.
    pub const fn field_type(&self) -> FieldType {
        self.field_type
    }

    /// Returns the widget rectangle taken from the control hint.
    pub const fn rect(&self) -> WidgetRect {
        self.rect
    }

    /// Returns the zero-based page index.
    pub const fn page(&self) -> u32 {
        self.page
    }

    /// Returns the label text paired with (or supplied on) this control.
    pub fn label(&self) -> Option<&str> {
        self.label.as_deref()
    }

    /// Returns the heuristic confidence in `0.0..=1.0`.
    pub const fn confidence(&self) -> f32 {
        self.confidence
    }

    /// Returns the field's zero-based position in recognition (reading) order.
    pub const fn rank(&self) -> usize {
        self.rank
    }
}

/// The deterministic result of [`recognize`].
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RecognitionPlan {
    fields: Vec<RecognizedField>,
    orphan_labels: Vec<String>,
}

impl RecognitionPlan {
    /// Creates an empty plan.
    pub fn empty() -> Self {
        Self::default()
    }

    /// Returns the recognized controls in reading order.
    pub fn fields(&self) -> &[RecognizedField] {
        &self.fields
    }

    /// Returns the texts of label hints that matched no control, sorted.
    pub fn orphan_labels(&self) -> &[String] {
        &self.orphan_labels
    }

    /// Builds a typed conversion plan of [`FieldSpec`] values.
    ///
    /// # Errors
    ///
    /// Returns [`RecognitionError`] if any recognized field cannot be modelled
    /// as a valid AcroForm specification (for example an unmappable page).
    pub fn conversion_plan(&self) -> Result<ConversionPlan, RecognitionError> {
        let mut specs = Vec::with_capacity(self.fields.len());
        for field in &self.fields {
            let mut spec = match field.field_type {
                FieldType::Text => FieldSpec::text(field.name.clone(), field.rect, field.page),
                FieldType::CheckBox => {
                    FieldSpec::checkbox(field.name.clone(), field.rect, field.page)
                }
                FieldType::Radio => {
                    FieldSpec::radio_group(field.name.clone(), field.rect, field.page, ["On"])
                }
                FieldType::Choice => FieldSpec::choice(
                    field.name.clone(),
                    field.rect,
                    field.page,
                    Vec::<String>::new(),
                    false,
                ),
                FieldType::Button | FieldType::Signature | FieldType::Unknown => {
                    FieldSpec::text(field.name.clone(), field.rect, field.page)
                }
            };
            if let Some(label) = &field.label {
                spec = spec.with_tooltip(label.clone());
            }
            specs.push(spec);
        }
        Ok(ConversionPlan { specs })
    }
}

/// A typed plan describing how recognized areas become AcroForm fields.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ConversionPlan {
    specs: Vec<FieldSpec>,
}

impl ConversionPlan {
    /// Returns the field specifications in reading order.
    pub fn specs(&self) -> &[FieldSpec] {
        &self.specs
    }

    /// Builds an in-memory [`Form`] from the plan.
    ///
    /// # Errors
    ///
    /// Returns [`RecognitionError`] for invalid field specifications or
    /// duplicate generated names.
    pub fn to_form(&self) -> Result<Form, RecognitionError> {
        let mut fields = Vec::with_capacity(self.specs.len());
        for spec in &self.specs {
            fields.push(spec.to_field()?);
        }
        Ok(Form::from_fields(fields)?)
    }

    /// Emits an `/AcroForm` dictionary model from the plan.
    ///
    /// # Errors
    ///
    /// Returns [`RecognitionError`] when any specification fails validation.
    pub fn acroform_dictionary(&self) -> Result<PdfValue, RecognitionError> {
        Ok(acroform_dictionary(&self.specs)?)
    }
}

/// An error produced while recognizing or converting static form hints.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum RecognitionError {
    /// More than [`MAX_HINTS`] hints were supplied.
    #[error("too many hints: {count} exceeds maximum {max}")]
    TooManyHints {
        /// The number of hints supplied.
        count: usize,
        /// The maximum accepted.
        max: usize,
    },
    /// A hint rectangle contained a non-finite coordinate.
    #[error("hint {index} has a non-finite widget rectangle")]
    NonFiniteRect {
        /// The zero-based hint index.
        index: usize,
    },
    /// A hint rectangle had a zero or negative extent.
    #[error("hint {index} has a zero-sized widget rectangle")]
    ZeroSizedRect {
        /// The zero-based hint index.
        index: usize,
    },
    /// A hint page index could not be mapped to a PDF object number.
    #[error("hint {index} has a page index that cannot be mapped to a PDF object")]
    PageOverflow {
        /// The zero-based hint index.
        index: usize,
    },
    /// A hint's label/prompt text exceeded [`RECOGNITION_MAX_TEXT_LEN`].
    #[error("hint {index} text is too long ({len} > {max})")]
    TextTooLong {
        /// The zero-based hint index.
        index: usize,
        /// The observed length.
        len: usize,
        /// The maximum accepted.
        max: usize,
    },
    /// A hint's explicit name exceeded [`RECOGNITION_MAX_TEXT_LEN`].
    #[error("hint {index} name is too long ({len} > {max})")]
    NameTooLong {
        /// The zero-based hint index.
        index: usize,
        /// The observed length.
        len: usize,
        /// The maximum accepted.
        max: usize,
    },
    /// An AcroForm field specification failed validation.
    #[error("field creation failed: {0}")]
    Creation(#[from] CreationError),
    /// The converted fields could not form a valid [`Form`].
    #[error("form construction failed: {0}")]
    Form(#[from] FormError),
}

/// Recognizes controls from caller-supplied hints and pairs them with labels.
///
/// Each control is bound to at most one label on the same page. A label is
/// considered nearby when it lies to the left of (or directly above) the
/// control and the perpendicular box gap is at most [`LABEL_PROXIMITY`] points.
/// Among candidates the closest centre is chosen. Labels are consumed once, so
/// two controls never claim the same label.
///
/// Controls and labels are visited in an intrinsic reading order derived from
/// page, top edge, left edge, kind and text — never from the caller's ordering —
/// so permuting the input slice does not change the result.
///
/// # Errors
///
/// Returns [`RecognitionError`] for too many hints, invalid geometry, an
/// unmappable page index, or over-long text. An empty slice yields an empty,
/// default [`RecognitionPlan`].
pub fn recognize(hints: &[FormHint]) -> Result<RecognitionPlan, RecognitionError> {
    if hints.len() > MAX_HINTS {
        return Err(RecognitionError::TooManyHints {
            count: hints.len(),
            max: MAX_HINTS,
        });
    }
    for (index, hint) in hints.iter().enumerate() {
        if !hint.rect.is_finite() {
            return Err(RecognitionError::NonFiniteRect { index });
        }
        if hint.rect.is_zero_sized() {
            return Err(RecognitionError::ZeroSizedRect { index });
        }
        if hint.page.checked_add(1).is_none() {
            return Err(RecognitionError::PageOverflow { index });
        }
        if let Some(text) = &hint.text
            && text.chars().count() > RECOGNITION_MAX_TEXT_LEN
        {
            return Err(RecognitionError::TextTooLong {
                index,
                len: text.chars().count(),
                max: RECOGNITION_MAX_TEXT_LEN,
            });
        }
        if let Some(name) = &hint.name
            && name.chars().count() > RECOGNITION_MAX_TEXT_LEN
        {
            return Err(RecognitionError::NameTooLong {
                index,
                len: name.chars().count(),
                max: RECOGNITION_MAX_TEXT_LEN,
            });
        }
    }

    let mut controls: Vec<usize> = (0..hints.len())
        .filter(|&index| hints[index].kind.is_control())
        .collect();
    let mut labels: Vec<usize> = (0..hints.len())
        .filter(|&index| hints[index].kind == HintKind::Label)
        .collect();
    controls.sort_by(|&a, &b| intrinsic_order(&hints[a], &hints[b]));
    labels.sort_by(|&a, &b| intrinsic_order(&hints[a], &hints[b]));

    let mut consumed = vec![false; labels.len()];
    let mut fields = Vec::with_capacity(controls.len());
    let mut used_names: Vec<String> = Vec::with_capacity(controls.len());

    for (rank, &control_index) in controls.iter().enumerate() {
        let control = &hints[control_index];
        let label_position = match_label(hints, &labels, control, &consumed);
        if let Some(position) = label_position {
            consumed[position] = true;
        }
        let paired_label =
            label_position.map(|position| hints[labels[position]].text().unwrap_or("").to_owned());
        let has_pair = paired_label.as_ref().is_some_and(|text| !text.is_empty());
        // A control's own text is a secondary naming source when no separate
        // label hint was paired with it.
        let label = paired_label.or_else(|| control.text().map(str::to_owned));
        let name = choose_name(
            control.name().unwrap_or(""),
            label.as_deref(),
            rank,
            &used_names,
        );
        used_names.push(name.clone());

        let field_type = match control.kind {
            HintKind::InputBox => FieldType::Text,
            HintKind::CheckBox => FieldType::CheckBox,
            HintKind::RadioGroup => FieldType::Radio,
            HintKind::Choice => FieldType::Choice,
            HintKind::Label => FieldType::Unknown,
        };
        let has_explicit_name = control.name().is_some_and(|name| !name.is_empty());
        let confidence = confidence_for(control.kind, has_pair, has_explicit_name);
        fields.push(RecognizedField {
            name,
            field_type,
            rect: control.rect,
            page: control.page,
            label,
            confidence,
            rank,
        });
    }

    let mut orphan_labels: Vec<String> = labels
        .iter()
        .enumerate()
        .filter(|(position, _)| !consumed[*position])
        .map(|(_, &index)| hints[index].text().unwrap_or("").to_owned())
        .collect();
    orphan_labels.sort();
    Ok(RecognitionPlan {
        fields,
        orphan_labels,
    })
}

/// Intrinsic, input-order-independent ordering of hints.
fn intrinsic_order(a: &FormHint, b: &FormHint) -> Ordering {
    a.page
        .cmp(&b.page)
        .then_with(|| b.rect.top().total_cmp(&a.rect.top()))
        .then_with(|| a.rect.x.total_cmp(&b.rect.x))
        .then_with(|| a.kind.label().cmp(b.kind.label()))
        .then_with(|| a.name().unwrap_or("").cmp(b.name().unwrap_or("")))
        .then_with(|| a.text().unwrap_or("").cmp(b.text().unwrap_or("")))
        .then_with(|| a.rect.width.total_cmp(&b.rect.width))
        .then_with(|| a.rect.height.total_cmp(&b.rect.height))
}

/// Returns the position in `labels` of the best unconsumed label for `control`.
fn match_label(
    hints: &[FormHint],
    labels: &[usize],
    control: &FormHint,
    consumed: &[bool],
) -> Option<usize> {
    let mut best: Option<(u32, usize)> = None;
    for (position, &label_index) in labels.iter().enumerate() {
        if consumed[position] {
            continue;
        }
        let candidate = &hints[label_index];
        if candidate.page != control.page || !label_is_near(candidate.rect, control.rect) {
            continue;
        }
        let key = centre_distance(candidate.rect, control.rect);
        if best.is_none_or(|(current, _)| key < current) {
            best = Some((key, position));
        }
    }
    best.map(|(_, position)| position)
}

/// Returns `true` when `label` lies left of or above `control` within the
/// proximity tolerance on the perpendicular axis.
fn label_is_near(label: WidgetRect, control: WidgetRect) -> bool {
    let left = label.right() <= control.x + LABEL_PROXIMITY;
    let above = label.y >= control.top() - LABEL_PROXIMITY;
    let horizontal_gap = axis_gap(label.x, label.right(), control.x, control.right());
    let vertical_gap = axis_gap(label.y, label.top(), control.y, control.top());
    (left && vertical_gap <= LABEL_PROXIMITY) || (above && horizontal_gap <= LABEL_PROXIMITY)
}

/// Returns the non-overlapping gap between two intervals, or zero when they
/// overlap. Inputs are validated finite before this helper is called.
fn axis_gap(a0: f32, a1: f32, b0: f32, b1: f32) -> f32 {
    if a1 < b0 {
        b0 - a1
    } else if b1 < a0 {
        a0 - b1
    } else {
        0.0
    }
}

/// Returns the squared distance between two rectangle centres as ordered bits.
fn centre_distance(a: WidgetRect, b: WidgetRect) -> u32 {
    let ax = a.x + a.width / 2.0;
    let ay = a.y + a.height / 2.0;
    let bx = b.x + b.width / 2.0;
    let by = b.y + b.height / 2.0;
    let dx = ax - bx;
    let dy = ay - by;
    (dx * dx + dy * dy).to_bits()
}

/// Chooses a unique field name from an explicit name, a sanitized label, or a
/// positional fallback. A numeric suffix keeps generated names unique.
fn choose_name(explicit: &str, label: Option<&str>, rank: usize, used: &[String]) -> String {
    let derived = if !explicit.is_empty() {
        sanitize_name(explicit)
    } else {
        label.and_then(sanitize_name)
    };
    let base = derived.unwrap_or_else(|| format!("field_{}", rank + 1));
    if !used.iter().any(|name| name == &base) {
        return base;
    }
    let mut suffix = 2usize;
    loop {
        let candidate = format!("{base}_{suffix}");
        if !used.iter().any(|name| name == &candidate) {
            return candidate;
        }
        suffix += 1;
    }
}

/// Converts text to a lowercase ASCII identifier, or `None` when no character
/// is usable. Non-ASCII and punctuation map to underscores which are trimmed
/// and collapsed.
fn sanitize_name(text: &str) -> Option<String> {
    let mut out = String::new();
    let mut previous_underscore = false;
    for ch in text.chars() {
        let mapped = if ch.is_ascii_alphanumeric() {
            ch.to_ascii_lowercase()
        } else {
            '_'
        };
        if mapped == '_' {
            if previous_underscore {
                continue;
            }
            previous_underscore = true;
        } else {
            previous_underscore = false;
        }
        out.push(mapped);
    }
    let trimmed = out.trim_matches('_');
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_owned())
    }
}

/// Computes a heuristic confidence for a recognized control.
fn confidence_for(kind: HintKind, has_label_pair: bool, has_explicit_name: bool) -> f32 {
    let mut value: f32 = match kind {
        HintKind::InputBox => 0.55,
        HintKind::CheckBox => 0.65,
        HintKind::RadioGroup => 0.60,
        HintKind::Choice => 0.60,
        HintKind::Label => 0.0,
    };
    if has_label_pair {
        value += 0.30;
    }
    if has_explicit_name {
        value += 0.10;
    }
    value.clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_controls_with_confidence_and_reading_order() {
        let plan = recognize(&[
            FormHint::new(HintKind::Label, WidgetRect::new(20.0, 100.0, 80.0, 20.0), 0)
                .with_text("Full Name"),
            FormHint::new(
                HintKind::InputBox,
                WidgetRect::new(200.0, 100.0, 60.0, 20.0),
                0,
            )
            .with_name("name"),
            FormHint::new(
                HintKind::CheckBox,
                WidgetRect::new(200.0, 60.0, 20.0, 20.0),
                0,
            )
            .with_text("Agree"),
            FormHint::new(
                HintKind::InputBox,
                WidgetRect::new(140.0, 100.0, 60.0, 20.0),
                1,
            ),
        ])
        .unwrap();

        assert_eq!(plan.fields().len(), 3);
        // Reading order is top-to-bottom: the named input (top 120) precedes
        // the checkbox (top 80) on page 0; page 1 comes last.
        assert_eq!(plan.fields()[0].name(), "name");
        assert_eq!(plan.fields()[1].name(), "agree");
        assert_eq!(plan.fields()[2].name(), "field_3");
        assert_eq!(plan.fields()[0].label(), Some("Full Name"));
        assert!(plan.fields()[0].confidence() > plan.fields()[1].confidence());
        assert!(plan.fields().iter().all(|f| f.confidence() <= 1.0));
        assert_eq!(plan.fields()[0].rank(), 0);
        assert_eq!(plan.fields()[2].rank(), 2);
    }

    #[test]
    fn derives_names_from_labels_and_disambiguates_duplicates() {
        let plan = recognize(&[
            FormHint::new(HintKind::Label, WidgetRect::new(10.0, 100.0, 50.0, 20.0), 0)
                .with_text("Customer Name"),
            FormHint::new(
                HintKind::InputBox,
                WidgetRect::new(200.0, 100.0, 60.0, 20.0),
                0,
            ),
            FormHint::new(HintKind::Label, WidgetRect::new(10.0, 60.0, 50.0, 20.0), 0)
                .with_text("Customer Name!"),
            FormHint::new(
                HintKind::InputBox,
                WidgetRect::new(200.0, 60.0, 60.0, 20.0),
                0,
            ),
        ])
        .unwrap();

        let mut names: Vec<&str> = plan.fields().iter().map(|f| f.name()).collect();
        names.sort_unstable();
        assert_eq!(names, ["customer_name", "customer_name_2"]);
    }

    #[test]
    fn conversion_plan_builds_form_and_acroform_dictionary() {
        let plan = recognize(&[
            FormHint::new(HintKind::Label, WidgetRect::new(10.0, 100.0, 50.0, 20.0), 0)
                .with_text("Full Name"),
            FormHint::new(
                HintKind::InputBox,
                WidgetRect::new(200.0, 100.0, 60.0, 20.0),
                0,
            ),
            FormHint::new(
                HintKind::CheckBox,
                WidgetRect::new(200.0, 60.0, 20.0, 20.0),
                0,
            )
            .with_text("Agree"),
        ])
        .unwrap();
        let conversion = plan.conversion_plan().unwrap();
        assert_eq!(conversion.specs().len(), 2);
        assert_eq!(conversion.specs()[0].field_type(), FieldType::Text);
        assert_eq!(conversion.specs()[1].field_type(), FieldType::CheckBox);
        assert_eq!(conversion.specs()[0].tooltip(), Some("Full Name"));

        let form = conversion.to_form().unwrap();
        assert_eq!(form.len(), 2);
        assert!(form.contains("full_name"));
        assert!(matches!(
            conversion.acroform_dictionary().unwrap(),
            PdfValue::Dictionary(_)
        ));
    }

    #[test]
    fn empty_hints_yield_empty_plan() {
        let plan = recognize(&[]).unwrap();
        assert!(plan.fields().is_empty());
        assert!(plan.orphan_labels().is_empty());
        assert!(
            plan.conversion_plan()
                .unwrap()
                .to_form()
                .unwrap()
                .is_empty()
        );
        assert_eq!(plan, RecognitionPlan::empty());
    }

    #[test]
    fn orphan_labels_are_reported() {
        let plan = recognize(&[
            FormHint::new(
                HintKind::Label,
                WidgetRect::new(900.0, 900.0, 50.0, 20.0),
                0,
            )
            .with_text("Orphan"),
            FormHint::new(
                HintKind::InputBox,
                WidgetRect::new(20.0, 20.0, 60.0, 20.0),
                0,
            ),
        ])
        .unwrap();
        assert_eq!(plan.orphan_labels(), ["Orphan"]);
    }

    #[test]
    fn recognition_is_deterministic_under_input_reordering() {
        let a = FormHint::new(HintKind::Label, WidgetRect::new(10.0, 100.0, 50.0, 20.0), 0)
            .with_text("Name");
        let b = FormHint::new(
            HintKind::InputBox,
            WidgetRect::new(200.0, 100.0, 60.0, 20.0),
            0,
        );
        let c = FormHint::new(
            HintKind::CheckBox,
            WidgetRect::new(200.0, 60.0, 20.0, 20.0),
            0,
        );
        let first = recognize(&[a.clone(), b.clone(), c.clone()]).unwrap();
        let second = recognize(&[c.clone(), a.clone(), b.clone()]).unwrap();
        assert_eq!(first, second);
    }

    #[test]
    fn invalid_inputs_are_typed_errors() {
        let nan = FormHint::new(
            HintKind::InputBox,
            WidgetRect::new(f32::NAN, 0.0, 10.0, 10.0),
            0,
        );
        assert_eq!(
            recognize(&[nan]),
            Err(RecognitionError::NonFiniteRect { index: 0 })
        );

        let zero = FormHint::new(HintKind::InputBox, WidgetRect::new(0.0, 0.0, 0.0, 10.0), 0);
        assert_eq!(
            recognize(&[zero]),
            Err(RecognitionError::ZeroSizedRect { index: 0 })
        );

        let page = FormHint::new(
            HintKind::InputBox,
            WidgetRect::new(0.0, 0.0, 10.0, 10.0),
            u32::MAX,
        );
        assert_eq!(
            recognize(&[page]),
            Err(RecognitionError::PageOverflow { index: 0 })
        );

        let long = "x".repeat(RECOGNITION_MAX_TEXT_LEN + 1);
        let text = FormHint::new(HintKind::Label, WidgetRect::new(0.0, 0.0, 10.0, 10.0), 0)
            .with_text(long);
        assert!(matches!(
            recognize(&[text]),
            Err(RecognitionError::TextTooLong { index: 0, .. })
        ));
    }

    #[test]
    fn too_many_hints_is_a_typed_error() {
        let hint = FormHint::new(HintKind::InputBox, WidgetRect::new(0.0, 0.0, 10.0, 10.0), 0);
        let many = vec![hint; MAX_HINTS + 1];
        assert!(matches!(
            recognize(&many),
            Err(RecognitionError::TooManyHints { .. })
        ));
    }
}
