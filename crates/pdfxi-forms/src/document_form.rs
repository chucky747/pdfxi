//! Form filling on a real `pdfxi_core::Document`.
//!
//! This module is the bridge between a parsed PDF and the crate's
//! dependency-free field/appearance model. It reads the catalog `/AcroForm`,
//! resolves inherited field attributes and fully qualified names, fills values,
//! regenerates `/AP /N` appearance streams, exports and imports XFDF, and
//! flattens widgets into page content.
//!
//! # Snapshot semantics
//!
//! [`read_acroform`] reads the *source* bytes of a
//! [`pdfxi_core::Document`]. `pdfxi-core`
//! keeps staged mutations (from [`pdfxi_core::Document::set_object`]) separate
//! from the parsed source and does not expose them through
//! [`pdfxi_core::Document::object`]. A caller that mutates and then needs the
//! new values re-opens the saved bytes (the session contract of ADR 0010:
//! append, save, reload). Setters therefore locate fields from the source and
//! write independent changes per field, which makes repeated fills of different
//! fields in one session safe.
//!
//! # Scope
//!
//! - Supported field types: text, checkbox, radio, choice, signature
//!   (read-only metadata) and pushbutton (no value).
//! - Appearance generation uses the crate's [`crate::build_appearance`] with a
//!   base-14 Helvetica fallback. The field's `/DA` font resource and size are
//!   honoured when present; an embedded font program is not rasterised.
//! - XFA, JavaScript calculations and validation scripts are detected and
//!   reported ([`AcroFormModel::contains_xfa`],
//!   [`AcroFormModel::has_javascript`]) but never executed.
//! - Inline field or widget dictionaries (not indirect references) are visible
//!   in the model but cannot be mutated; setters return
//!   [`AcroFormError::UnaddressableField`].

use std::collections::HashMap;

use pdfxi_core::{Dictionary, Document, Object, ObjectId};

use crate::appearance::{
    Alignment, AppearanceError, FontSpec, NormalAppearance, RgbColor, TextEncoder,
    WidgetAppearance, WidgetRect, build_appearance_with_encoder, text_extent,
};
use crate::field::{Field, FieldFlags, FieldType, FieldValue};
use crate::matrix::Matrix;

mod font;

use font::{FieldFont, FontChoice, WinAnsiEncoder, resolve_font, sanitise_name};

/// The kind of a terminal AcroForm field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FormFieldKind {
    /// A text field (`/FT /Tx`).
    Text,
    /// A checkbox (`/FT /Btn` without the radio or pushbutton flag).
    Checkbox,
    /// A radio button group (`/FT /Btn` with the radio flag).
    Radio,
    /// A choice field (`/FT /Ch`).
    Choice,
    /// A signature field (`/FT /Sig`).
    Signature,
    /// A pushbutton (`/FT /Btn` with the pushbutton flag).
    Button,
}

impl FormFieldKind {
    /// Returns a stable, human-readable label.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Checkbox => "checkbox",
            Self::Radio => "radio",
            Self::Choice => "choice",
            Self::Signature => "signature",
            Self::Button => "button",
        }
    }

    fn from_pdf(ft: &str, flags: u32) -> Option<Self> {
        match ft {
            "Tx" => Some(Self::Text),
            "Ch" => Some(Self::Choice),
            "Sig" => Some(Self::Signature),
            "Btn" if flags & (1 << 15) != 0 => Some(Self::Radio),
            "Btn" if flags & (1 << 16) != 0 => Some(Self::Button),
            "Btn" => Some(Self::Checkbox),
            _ => None,
        }
    }
}

impl std::fmt::Display for FormFieldKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A widget rectangle in PDF user space (points).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FormRect {
    /// X coordinate of the left edge.
    pub left: f64,
    /// Y coordinate of the bottom edge.
    pub bottom: f64,
    /// X coordinate of the right edge.
    pub right: f64,
    /// Y coordinate of the top edge.
    pub top: f64,
}

impl FormRect {
    /// Creates a rectangle from its four edges.
    pub const fn new(left: f64, bottom: f64, right: f64, top: f64) -> Self {
        Self {
            left,
            bottom,
            right,
            top,
        }
    }

    /// Returns the width, never negative.
    pub fn width(self) -> f64 {
        (self.right - self.left).abs()
    }

    /// Returns the height, never negative.
    pub fn height(self) -> f64 {
        (self.top - self.bottom).abs()
    }

    /// Returns `true` when the rectangle has no area.
    pub fn is_empty(self) -> bool {
        self.width() <= 0.0 || self.height() <= 0.0
    }

    fn from_object(object: &Object) -> Option<Self> {
        let items = object.as_array()?;
        if items.len() < 4 {
            return None;
        }
        let mut values = [0.0_f64; 4];
        for (slot, item) in values.iter_mut().zip(items.iter()) {
            *slot = item.as_f64()?;
        }
        Some(Self::new(
            values[0].min(values[2]),
            values[1].min(values[3]),
            values[0].max(values[2]),
            values[1].max(values[3]),
        ))
    }
}

/// One widget annotation belonging to a terminal field.
#[derive(Debug, Clone, PartialEq)]
pub struct FormWidget {
    /// Indirect object identifier when the widget is an indirect object.
    pub object: Option<ObjectId>,
    /// The widget `/Rect` in page points.
    pub rect: FormRect,
    /// Zero-based page index resolved from `/P` or the page `/Annots` arrays.
    pub page: Option<usize>,
    /// Named on-states from `/AP /N` (excluding `Off`).
    pub on_states: Vec<String>,
    /// Whether the widget already carries an `/AP` normal appearance.
    pub has_appearance: bool,
}

impl FormWidget {
    /// Returns the first named on-state, if the widget declares one.
    pub fn on_state(&self) -> Option<&str> {
        self.on_states.first().map(String::as_str)
    }
}

/// One terminal AcroForm field.
#[derive(Debug, Clone, PartialEq)]
pub struct FormField {
    /// Fully qualified name (`parent.child`).
    pub name: String,
    /// Field kind.
    pub kind: FormFieldKind,
    /// Current `/V` value.
    pub value: FieldValue,
    /// Default `/DV` value.
    pub default_value: FieldValue,
    /// `/Opt` entries, using the export value of `[export display]` pairs.
    pub options: Vec<String>,
    /// Raw `/Ff` flags.
    pub flags: FieldFlags,
    /// `true` when the read-only flag (bit 2) is set.
    pub read_only: bool,
    /// `true` when the required flag (bit 1) is set.
    pub required: bool,
    /// `/Q` quadding (`0` left, `1` centre, `2` right).
    pub alignment: i64,
    /// Effective `/DA` default appearance string, if any.
    pub default_appearance: Option<String>,
    /// Widgets belonging to this field.
    pub widgets: Vec<FormWidget>,
    /// Indirect identifier of this field (may equal a widget identifier).
    pub field_object: Option<ObjectId>,
}

impl FormField {
    /// Returns the first widget rectangle, if any.
    pub fn primary_rect(&self) -> Option<FormRect> {
        self.widgets.first().map(|widget| widget.rect)
    }

    /// Returns the page of the first widget, if resolved.
    pub fn primary_page(&self) -> Option<usize> {
        self.widgets.first().and_then(|widget| widget.page)
    }
}

/// The AcroForm of a document, as read from the source bytes.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AcroFormModel {
    /// Whether the catalog contains an `/AcroForm` dictionary.
    pub present: bool,
    /// Catalog `/AcroForm /NeedAppearances` (default `false`).
    pub need_appearances: bool,
    /// `true` when the AcroForm carries an `/XFA` entry.
    pub contains_xfa: bool,
    /// `true` when field or catalog actions use JavaScript.
    pub has_javascript: bool,
    /// Terminal fields in document order.
    pub fields: Vec<FormField>,
    /// Indirect identifier of the AcroForm dictionary, when indirect.
    pub acroform_object: Option<ObjectId>,
}

impl AcroFormModel {
    /// Returns the terminal fields in document order.
    pub fn fields(&self) -> &[FormField] {
        &self.fields
    }

    /// Returns the field with the exact fully qualified `name`.
    pub fn field(&self, name: &str) -> Option<&FormField> {
        self.fields.iter().find(|field| field.name == name)
    }

    /// Returns the number of terminal fields.
    pub fn len(&self) -> usize {
        self.fields.len()
    }

    /// Returns `true` when the model has no terminal fields.
    pub fn is_empty(&self) -> bool {
        self.fields.is_empty()
    }
}

/// A typed error produced by form reading, filling or flattening.
#[derive(Debug, Clone, thiserror::Error)]
pub enum AcroFormError {
    /// The catalog or AcroForm dictionary was structurally unusable.
    #[error("malformed AcroForm: {0}")]
    Malformed(&'static str),
    /// No field with the exact fully qualified name exists.
    #[error("no field named '{0}'")]
    UnknownField(String),
    /// A setter was used with the wrong field kind.
    #[error("field '{field}' is a {actual} field; expected {expected}")]
    WrongFieldKind {
        /// Fully qualified field name.
        field: String,
        /// Kind required by the setter.
        expected: &'static str,
        /// Kind found in the document.
        actual: &'static str,
    },
    /// The field is read-only.
    #[error("field '{0}' is read-only")]
    ReadOnlyField(String),
    /// A choice or radio value is not one of the declared options.
    #[error("value '{value}' is not an option of field '{field}'")]
    ValueNotAllowed {
        /// Fully qualified field name.
        field: String,
        /// Rejected value.
        value: String,
    },
    /// The field or widget is an inline object that cannot be replaced.
    #[error("field '{0}' has no indirect object and cannot be mutated")]
    UnaddressableField(String),
    /// The appearance layout generator rejected the geometry or style.
    #[error("appearance generation failed: {0}")]
    Appearance(#[from] AppearanceError),
    /// The XFDF text could not be parsed.
    #[error("XFDF error: {0}")]
    Xfdf(#[from] crate::xfdf::XfdfError),
    /// The underlying PDF reader or writer failed.
    #[error("PDF error: {0}")]
    Core(#[from] pdfxi_core::Error),
}

/// A summary of a completed flatten operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FlattenFormReport {
    /// Number of terminal fields removed from the catalog.
    pub fields: usize,
    /// Number of widgets painted into page content.
    pub widgets: usize,
    /// Number of pages whose content changed.
    pub pages: usize,
}

/// The font actually used to write a text appearance.
///
/// [`AppearanceFont::FieldFont`] means the value was encoded with the font
/// resource named by the field `/DA` and resolved through the page or AcroForm
/// resources. [`AppearanceFont::Fallback`] means the declared font could not be
/// used; the appearance was written with the base-14 Helvetica path and the
/// reason records why, so callers never have to guess.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppearanceFont {
    /// The document's own font resource was used.
    FieldFont {
        /// `/DA` resource name written before `Tf`.
        resource: String,
        /// Whether the resolved font has an embedded program.
        embedded: bool,
    },
    /// The base-14 Helvetica path was used instead.
    Fallback {
        /// The `/DA` resource name that was requested.
        requested: String,
        /// Why the document font could not be used.
        reason: FallbackReason,
    },
}

/// Why a text appearance could not use the field's declared font.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FallbackReason {
    /// The `/DA` name is in neither the page `/Resources /Font` nor
    /// `/AcroForm /DR /Font`.
    MissingResource,
    /// The font dictionary is missing, malformed, or uses an encoding this
    /// build cannot write (for example a named non-identity CMap).
    UnsupportedFont,
    /// The font cannot represent at least one character of the value.
    CannotEncode,
}

impl FallbackReason {
    /// Returns a stable ASCII label for diagnostics and XFDF status output.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::MissingResource => "missing_resource",
            Self::UnsupportedFont => "unsupported_font",
            Self::CannotEncode => "cannot_encode",
        }
    }
}

/// Returns the stable ASCII label of a [`FallbackReason`].
pub const fn fallback_reason_name(reason: FallbackReason) -> &'static str {
    reason.as_str()
}

/// Per-widget appearance report produced while filling or regenerating.
#[derive(Debug, Clone, PartialEq)]
pub struct WidgetAppearanceReport {
    /// Widget object identifier, when the widget is an indirect object.
    pub widget: Option<ObjectId>,
    /// Font decision for text-like widgets; `None` for buttons.
    pub font: Option<AppearanceFont>,
    /// `/MK /R` rotation applied to the appearance, normalized to `0`, `90`,
    /// `180` or `270` degrees counter-clockwise.
    pub rotation: i32,
    /// The `/Matrix` written to the appearance stream (`identity` when upright).
    pub matrix: Matrix,
    /// Appearance-space `/BBox` (`(0, 0, width, height)` before the matrix).
    pub bbox: FormRect,
    /// Union of the laid-out text line boxes in appearance space, when the
    /// widget draws text.
    pub text_rect: Option<FormRect>,
}

/// Appearance decisions for one fill or regeneration call.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AppearanceReport {
    /// One entry per widget whose appearance was regenerated.
    pub widgets: Vec<WidgetAppearanceReport>,
}

impl AppearanceReport {
    /// Returns the font used by the first text widget, if any.
    pub fn font(&self) -> Option<&AppearanceFont> {
        self.widgets.iter().find_map(|widget| widget.font.as_ref())
    }

    /// Returns `true` when every text widget used its document font.
    pub fn all_field_fonts(&self) -> bool {
        self.widgets
            .iter()
            .filter_map(|widget| widget.font.as_ref())
            .all(|font| matches!(font, AppearanceFont::FieldFont { .. }))
    }
}

#[derive(Clone, Default)]
struct Inherited {
    ft: Option<String>,
    flags: Option<u32>,
    value: Option<Object>,
    default_value: Option<Object>,
    default_appearance: Option<String>,
    alignment: Option<i64>,
    options: Option<Vec<Object>>,
}

impl Inherited {
    fn merged_with(&self, dictionary: &Dictionary) -> Self {
        let mut merged = self.clone();
        if let Some(ft) = dictionary.get("FT").and_then(value_text) {
            merged.ft = Some(ft);
        }
        if let Some(flags) = dictionary
            .get("Ff")
            .and_then(Object::as_i64)
            .and_then(|value| u32::try_from(value).ok())
        {
            merged.flags = Some(flags);
        }
        if let Some(value) = dictionary.get("V") {
            merged.value = Some(value.clone());
        }
        if let Some(value) = dictionary.get("DV") {
            merged.default_value = Some(value.clone());
        }
        if let Some(da) = dictionary.get("DA").and_then(value_text) {
            merged.default_appearance = Some(da);
        }
        if let Some(q) = dictionary.get("Q").and_then(Object::as_i64) {
            merged.alignment = Some(q);
        }
        if let Some(opt) = dictionary.get("Opt").and_then(Object::as_array) {
            merged.options = Some(opt.to_vec());
        }
        merged
    }
}

#[derive(Default)]
struct PageIndex {
    pages: HashMap<ObjectId, usize>,
    annots: HashMap<ObjectId, usize>,
}

/// Read the catalog `/AcroForm` into a typed model.
///
/// A document without an AcroForm returns an empty model with
/// [`AcroFormModel::present`] set to `false`, so callers can treat "no form"
/// as a normal state instead of an error.
///
/// # Errors
///
/// Returns [`AcroFormError::Core`] for a damaged document and
/// [`AcroFormError::Malformed`] when the AcroForm, `/Fields`, a field node or a
/// widget has the wrong type.
pub fn read_acroform(doc: &Document) -> Result<AcroFormModel, AcroFormError> {
    let root_object = doc.object(doc.root()?)?;
    let root = root_object
        .as_dictionary()
        .ok_or(AcroFormError::Malformed("catalog is not a dictionary"))?;

    let acroform_object = root.get("AcroForm").and_then(Object::as_reference);
    let Some(acroform_value) = root.get("AcroForm") else {
        return Ok(AcroFormModel::default());
    };
    let acroform = dictionary_of(doc, acroform_value)?;

    let pages = page_index(doc)?;
    let acroform_da = acroform.get("DA").and_then(value_text);
    let mut model = AcroFormModel {
        present: true,
        need_appearances: acroform
            .get("NeedAppearances")
            .and_then(|value| match value {
                Object::Boolean(value) => Some(*value),
                _ => None,
            })
            .unwrap_or(false),
        contains_xfa: acroform.contains_key("XFA"),
        has_javascript: false,
        fields: Vec::new(),
        acroform_object,
    };

    model.has_javascript =
        catalog_has_javascript(doc, root) || dictionary_has_javascript(doc, &acroform);

    if let Some(fields) = acroform.get("Fields").and_then(Object::as_array) {
        let mut context = ReadContext {
            doc,
            pages: &pages,
            acroform_da: acroform_da.as_deref(),
            fields: Vec::new(),
            has_javascript: model.has_javascript,
        };
        context.walk(fields, "", &Inherited::default())?;
        model.fields = context.fields;
        model.has_javascript = context.has_javascript;
    }

    Ok(model)
}

struct ReadContext<'a> {
    doc: &'a Document,
    pages: &'a PageIndex,
    acroform_da: Option<&'a str>,
    fields: Vec<FormField>,
    has_javascript: bool,
}

impl ReadContext<'_> {
    fn walk(
        &mut self,
        fields: &[Object],
        parent: &str,
        inherited: &Inherited,
    ) -> Result<(), AcroFormError> {
        let doc = self.doc;
        for item in fields {
            let object_id = item.as_reference();
            let dictionary = dictionary_of(doc, item)?;
            self.has_javascript |= dictionary_has_javascript(doc, &dictionary);
            let merged = inherited.merged_with(&dictionary);

            let partial = dictionary.get("T").and_then(value_text);
            let name = match partial {
                Some(partial) if !partial.is_empty() => {
                    if parent.is_empty() {
                        partial
                    } else {
                        format!("{parent}.{partial}")
                    }
                }
                _ => parent.to_owned(),
            };

            let kids: Vec<Object> = dictionary
                .get("Kids")
                .and_then(Object::as_array)
                .map(<[Object]>::to_vec)
                .unwrap_or_default();
            let kids_are_widgets = !kids.is_empty() && kids.iter().all(|kid| is_widget(doc, kid));

            let is_terminal = kids_are_widgets
                || (kids.is_empty()
                    && (dictionary
                        .get("Subtype")
                        .and_then(Object::as_name)
                        .is_some_and(|name| name == "Widget")
                        || dictionary.contains_key("Rect")));

            if is_terminal {
                let Some(ft) = merged.ft.as_deref() else {
                    continue;
                };
                let Some(kind) = FormFieldKind::from_pdf(ft, merged.flags.unwrap_or(0)) else {
                    continue;
                };
                if name.is_empty() {
                    continue;
                }
                let options = read_options(merged.options.as_deref());
                let value = read_value(kind, merged.value.as_ref(), &options);
                let default_value = read_value(kind, merged.default_value.as_ref(), &options);
                let flags = FieldFlags::from_bits(merged.flags.unwrap_or(0));
                let widgets = collect_widgets(
                    doc,
                    &kids,
                    object_id,
                    &dictionary,
                    self.pages,
                    kids_are_widgets,
                )?;

                self.fields.push(FormField {
                    name,
                    kind,
                    value,
                    default_value,
                    options,
                    flags,
                    read_only: flags.is_read_only(),
                    required: flags.is_required(),
                    alignment: merged.alignment.unwrap_or(0),
                    default_appearance: merged
                        .default_appearance
                        .clone()
                        .or_else(|| self.acroform_da.map(str::to_owned)),
                    widgets,
                    field_object: object_id,
                });
            } else if !kids.is_empty() {
                self.walk(&kids, &name, &merged)?;
            }
        }
        Ok(())
    }
}

fn collect_widgets(
    doc: &Document,
    kids: &[Object],
    own_id: Option<ObjectId>,
    own_dictionary: &Dictionary,
    pages: &PageIndex,
    kids_are_widgets: bool,
) -> Result<Vec<FormWidget>, AcroFormError> {
    if kids_are_widgets {
        let mut widgets = Vec::with_capacity(kids.len());
        for kid in kids {
            let id = kid.as_reference();
            let dictionary = dictionary_of(doc, kid)?;
            if let Some(widget) = read_widget(id, &dictionary, pages) {
                widgets.push(widget);
            }
        }
        Ok(widgets)
    } else if let Some(widget) = read_widget(own_id, own_dictionary, pages) {
        Ok(vec![widget])
    } else {
        Ok(Vec::new())
    }
}

fn read_widget(
    object: Option<ObjectId>,
    dictionary: &Dictionary,
    pages: &PageIndex,
) -> Option<FormWidget> {
    let rect = dictionary.get("Rect").and_then(FormRect::from_object)?;
    let page = dictionary
        .get("P")
        .and_then(Object::as_reference)
        .and_then(|id| pages.pages.get(&id).copied())
        .or_else(|| object.and_then(|id| pages.annots.get(&id).copied()));
    let normal = dictionary
        .get("AP")
        .and_then(Object::as_dictionary)
        .and_then(|ap| ap.get("N"));
    let has_appearance = normal.is_some();
    let mut on_states = Vec::new();
    if let Some(Object::Dictionary(states)) = normal {
        for name in states.keys() {
            if name != "Off" {
                on_states.push(name.clone());
            }
        }
    }
    on_states.sort();
    on_states.dedup();
    Some(FormWidget {
        object,
        rect,
        page,
        on_states,
        has_appearance,
    })
}

fn read_options(options: Option<&[Object]>) -> Vec<String> {
    let Some(options) = options else {
        return Vec::new();
    };
    options
        .iter()
        .filter_map(|option| match option {
            Object::Array(pair) => pair.first().and_then(value_text),
            other => value_text(other),
        })
        .collect()
}

fn read_value(kind: FormFieldKind, value: Option<&Object>, options: &[String]) -> FieldValue {
    let Some(value) = value else {
        return FieldValue::Empty;
    };
    match kind {
        FormFieldKind::Text | FormFieldKind::Signature => match value {
            Object::String(_) | Object::Name(_) => {
                value_text(value).map(FieldValue::Text).unwrap_or_default()
            }
            _ => FieldValue::Empty,
        },
        FormFieldKind::Choice => match value {
            Object::String(_) | Object::Name(_) => value_text(value)
                .map(|text| FieldValue::Choice(vec![text]))
                .unwrap_or_default(),
            Object::Array(items) => {
                let selected: Vec<String> = items.iter().filter_map(value_text).collect();
                if selected.is_empty() {
                    FieldValue::Empty
                } else {
                    FieldValue::Choice(selected)
                }
            }
            _ => FieldValue::Empty,
        },
        FormFieldKind::Checkbox => {
            let state = value_text(value).unwrap_or_default();
            FieldValue::Bool(!state.is_empty() && state != "Off" && state != "No")
        }
        FormFieldKind::Radio => {
            let state = value_text(value).unwrap_or_default();
            if state.is_empty() || state == "Off" {
                if options.is_empty() {
                    FieldValue::Empty
                } else {
                    FieldValue::Choice(vec!["Off".to_owned()])
                }
            } else {
                FieldValue::Choice(vec![state])
            }
        }
        FormFieldKind::Button => FieldValue::Empty,
    }
}

fn page_index(doc: &Document) -> Result<PageIndex, AcroFormError> {
    let pages = doc.pages()?;
    let mut index = PageIndex::default();
    for page in &pages {
        index.pages.insert(page.id, page.index);
        let dictionary = doc
            .object(page.id)?
            .as_dictionary()
            .cloned()
            .ok_or(AcroFormError::Malformed("page is not a dictionary"))?;
        let annots = match dictionary.get("Annots") {
            Some(Object::Reference(id)) => Some(doc.object(*id)?),
            Some(value) => Some(value.clone()),
            None => None,
        };
        if let Some(Object::Array(items)) = annots {
            for item in items {
                if let Some(id) = item.as_reference() {
                    index.annots.entry(id).or_insert(page.index);
                }
            }
        }
    }
    Ok(index)
}

fn catalog_has_javascript(doc: &Document, root: &Dictionary) -> bool {
    if let Some(action) = root.get("OpenAction")
        && action_is_javascript(doc, action)
    {
        return true;
    }
    if let Some(names) = root.get("Names").and_then(Object::as_dictionary)
        && names.contains_key("JavaScript")
    {
        return true;
    }
    false
}

fn dictionary_has_javascript(doc: &Document, dictionary: &Dictionary) -> bool {
    if let Some(action) = dictionary.get("A")
        && action_is_javascript(doc, action)
    {
        return true;
    }
    if let Some(actions) = dictionary.get("AA").and_then(Object::as_dictionary) {
        for action in actions.values() {
            if action_is_javascript(doc, action) {
                return true;
            }
        }
    }
    false
}

fn action_is_javascript(doc: &Document, action: &Object) -> bool {
    let dictionary = match action {
        Object::Reference(id) => doc.object(*id).ok(),
        other => Some(other.clone()),
    };
    dictionary
        .as_ref()
        .and_then(Object::as_dictionary)
        .and_then(|dictionary| dictionary.get("S"))
        .and_then(Object::as_name)
        .is_some_and(|name| name == "JavaScript")
}

fn dictionary_of(doc: &Document, object: &Object) -> Result<Dictionary, AcroFormError> {
    let resolved = match object {
        Object::Reference(id) => doc.object(*id)?,
        other => other.clone(),
    };
    resolved
        .as_dictionary()
        .cloned()
        .ok_or(AcroFormError::Malformed("expected a dictionary"))
}

fn is_widget(doc: &Document, object: &Object) -> bool {
    let dictionary = match object {
        Object::Reference(id) => doc.object(*id).ok(),
        other => Some(other.clone()),
    };
    dictionary
        .and_then(|value| value.as_dictionary().cloned())
        .and_then(|dictionary| {
            dictionary
                .get("Subtype")
                .and_then(Object::as_name)
                .map(str::to_owned)
        })
        .is_some_and(|subtype| subtype == "Widget")
}

/// Returns `value` as UTF-16BE or Latin-1 text, decoding a UTF-16BE BOM.
fn decode_pdf_text(bytes: &[u8]) -> String {
    if bytes.starts_with(&[0xFE, 0xFF]) {
        let units: Vec<u16> = bytes[2..]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| u16::from_be_bytes(*pair))
            .collect();
        String::from_utf16_lossy(&units)
    } else {
        bytes.iter().map(|byte| char::from(*byte)).collect()
    }
}

/// Encodes text for a PDF text string (`/V`): ASCII verbatim, otherwise UTF-16BE.
fn encode_pdf_text(text: &str) -> Vec<u8> {
    if text.is_ascii() {
        return text.as_bytes().to_vec();
    }
    let mut bytes = vec![0xFE, 0xFF];
    for unit in text.encode_utf16() {
        bytes.extend_from_slice(&unit.to_be_bytes());
    }
    bytes
}

fn value_text(object: &Object) -> Option<String> {
    match object {
        Object::String(bytes) => Some(decode_pdf_text(bytes)),
        Object::Name(name) => Some(name.clone()),
        _ => None,
    }
}

fn locate(doc: &Document, name: &str) -> Result<(AcroFormModel, usize), AcroFormError> {
    let model = read_acroform(doc)?;
    let index = model
        .fields
        .iter()
        .position(|field| field.name == name)
        .ok_or_else(|| AcroFormError::UnknownField(name.to_owned()))?;
    Ok((model, index))
}

fn ensure_kind(field: &FormField, expected: FormFieldKind) -> Result<(), AcroFormError> {
    if field.kind == expected {
        Ok(())
    } else {
        Err(AcroFormError::WrongFieldKind {
            field: field.name.clone(),
            expected: expected.as_str(),
            actual: field.kind.as_str(),
        })
    }
}

fn ensure_mutable(field: &FormField) -> Result<(), AcroFormError> {
    if field.read_only {
        Err(AcroFormError::ReadOnlyField(field.name.clone()))
    } else {
        Ok(())
    }
}

struct Allocator {
    font: u32,
    base: u32,
    stride: u32,
}

impl Allocator {
    fn new(doc: &Document, model: &AcroFormModel) -> Result<Self, AcroFormError> {
        let declared = doc.trailer_size()?.unwrap_or(0);
        let base = declared.saturating_add(1).max(1);
        let max_widgets = model
            .fields
            .iter()
            .map(|field| field.widgets.len())
            .max()
            .unwrap_or(0)
            .max(1);
        let stride = u32::try_from(max_widgets)
            .unwrap_or(u32::MAX / 4)
            .saturating_mul(2)
            .saturating_add(1)
            .max(3);
        Ok(Self {
            font: base,
            base,
            stride,
        })
    }

    fn stream(&self, field_index: usize, widget_index: usize, state_index: usize) -> u32 {
        let base = self.base.saturating_add(1);
        let field = u32::try_from(field_index).unwrap_or(u32::MAX / 8);
        let widget = u32::try_from(widget_index).unwrap_or(u32::MAX / 8);
        let state = u32::try_from(state_index).unwrap_or(0);
        base.saturating_add(field.saturating_mul(self.stride))
            .saturating_add(widget.saturating_mul(2))
            .saturating_add(state)
    }
}

struct AppearanceUpdate {
    ap: Dictionary,
    active_state: Option<String>,
}

/// Set a text field value and regenerate its widget appearances.
///
/// # Errors
///
/// Returns [`AcroFormError::UnknownField`], [`AcroFormError::WrongFieldKind`],
/// [`AcroFormError::ReadOnlyField`], [`AcroFormError::UnaddressableField`], or
/// a reader/writer error.
pub fn set_text(doc: &mut Document, name: &str, value: &str) -> Result<(), AcroFormError> {
    set_text_reported(doc, name, value).map(|_| ())
}

/// Set a text field value and report the appearance decisions.
///
/// Behaves exactly like [`set_text`] but returns an [`AppearanceReport`] that
/// names the font used for every regenerated widget and whether it was the
/// field's own `/DA` font or the base-14 fallback.
///
/// # Errors
///
/// Same as [`set_text`].
pub fn set_text_reported(
    doc: &mut Document,
    name: &str,
    value: &str,
) -> Result<AppearanceReport, AcroFormError> {
    let (model, index) = locate(doc, name)?;
    {
        let field = &model.fields[index];
        ensure_kind(field, FormFieldKind::Text)?;
        ensure_mutable(field)?;
    }
    apply_value(
        doc,
        &model,
        index,
        &FieldValue::Text(value.to_owned()),
        true,
    )
}

/// Set a choice field value, or clear it with `None`.
///
/// A value outside the declared `/Opt` options is rejected unless the field
/// carries the editable-combo flag.
///
/// # Errors
///
/// Returns [`AcroFormError::WrongFieldKind`],
/// [`AcroFormError::ReadOnlyField`] or [`AcroFormError::ValueNotAllowed`] for
/// invalid requests, plus reader/writer errors.
pub fn set_choice(
    doc: &mut Document,
    name: &str,
    value: Option<&str>,
) -> Result<(), AcroFormError> {
    set_choice_reported(doc, name, value).map(|_| ())
}

/// Set a choice field value and report the appearance decisions.
///
/// Behaves exactly like [`set_choice`] but returns an [`AppearanceReport`].
///
/// # Errors
///
/// Same as [`set_choice`].
pub fn set_choice_reported(
    doc: &mut Document,
    name: &str,
    value: Option<&str>,
) -> Result<AppearanceReport, AcroFormError> {
    let (model, index) = locate(doc, name)?;
    let next = {
        let field = &model.fields[index];
        ensure_kind(field, FormFieldKind::Choice)?;
        ensure_mutable(field)?;
        match value {
            Some(value) => {
                let editable = field.flags.contains(FieldFlags::CHOICE_EDIT);
                if !field.options.is_empty()
                    && !editable
                    && !field.options.iter().any(|option| option == value)
                {
                    return Err(AcroFormError::ValueNotAllowed {
                        field: name.to_owned(),
                        value: value.to_owned(),
                    });
                }
                FieldValue::Choice(vec![value.to_owned()])
            }
            None => FieldValue::Empty,
        }
    };
    apply_value(doc, &model, index, &next, true)
}

/// Set a checkbox on or off and regenerate its appearance.
///
/// # Errors
///
/// Returns [`AcroFormError::WrongFieldKind`],
/// [`AcroFormError::ReadOnlyField`] or [`AcroFormError::UnaddressableField`],
/// plus reader/writer errors.
pub fn set_checkbox(doc: &mut Document, name: &str, checked: bool) -> Result<(), AcroFormError> {
    set_checkbox_reported(doc, name, checked).map(|_| ())
}

/// Set a checkbox on or off and report the appearance decisions.
///
/// Behaves exactly like [`set_checkbox`] but returns an [`AppearanceReport`].
///
/// # Errors
///
/// Same as [`set_checkbox`].
pub fn set_checkbox_reported(
    doc: &mut Document,
    name: &str,
    checked: bool,
) -> Result<AppearanceReport, AcroFormError> {
    let (model, index) = locate(doc, name)?;
    {
        let field = &model.fields[index];
        ensure_kind(field, FormFieldKind::Checkbox)?;
        ensure_mutable(field)?;
    }
    apply_value(doc, &model, index, &FieldValue::Bool(checked), true)
}

/// Select one radio button by its on-state name.
///
/// # Errors
///
/// Returns [`AcroFormError::WrongFieldKind`],
/// [`AcroFormError::ReadOnlyField`] or [`AcroFormError::ValueNotAllowed`] when
/// the name is not an on-state of any widget, plus reader/writer errors.
pub fn set_radio(doc: &mut Document, name: &str, value: &str) -> Result<(), AcroFormError> {
    set_radio_reported(doc, name, value).map(|_| ())
}

/// Select one radio button and report the appearance decisions.
///
/// Behaves exactly like [`set_radio`] but returns an [`AppearanceReport`].
///
/// # Errors
///
/// Same as [`set_radio`].
pub fn set_radio_reported(
    doc: &mut Document,
    name: &str,
    value: &str,
) -> Result<AppearanceReport, AcroFormError> {
    let (model, index) = locate(doc, name)?;
    {
        let field = &model.fields[index];
        ensure_kind(field, FormFieldKind::Radio)?;
        ensure_mutable(field)?;
        let allowed = field
            .widgets
            .iter()
            .any(|widget| widget.on_states.iter().any(|state| state == value));
        if !allowed {
            return Err(AcroFormError::ValueNotAllowed {
                field: name.to_owned(),
                value: value.to_owned(),
            });
        }
    }
    apply_value(
        doc,
        &model,
        index,
        &FieldValue::Choice(vec![value.to_owned()]),
        true,
    )
}

fn apply_value(
    doc: &mut Document,
    model: &AcroFormModel,
    field_index: usize,
    value: &FieldValue,
    write_value: bool,
) -> Result<AppearanceReport, AcroFormError> {
    let field = model
        .fields
        .get(field_index)
        .ok_or(AcroFormError::Malformed("field index out of range"))?;
    let field_id = field
        .field_object
        .ok_or_else(|| AcroFormError::UnaddressableField(field.name.clone()))?;
    let acroform = acroform_dictionary(doc, model)?;
    let allocator = Allocator::new(doc, model)?;
    let mut report = AppearanceReport::default();

    let mut field_dictionary = dictionary_of(doc, &Object::Reference(field_id))?;
    let mut field_dirty = write_value;

    if write_value {
        match field.kind {
            FormFieldKind::Text => {
                if let FieldValue::Text(text) = value {
                    field_dictionary.insert("V".to_owned(), Object::String(encode_pdf_text(text)));
                } else {
                    field_dictionary.remove("V");
                }
            }
            FormFieldKind::Choice => {
                let selected = value
                    .as_choices()
                    .and_then(|values| values.first())
                    .cloned();
                if let Some(selected) = selected {
                    field_dictionary
                        .insert("V".to_owned(), Object::String(encode_pdf_text(&selected)));
                } else {
                    field_dictionary.remove("V");
                }
            }
            FormFieldKind::Checkbox => {
                let on = value.as_bool().unwrap_or(false);
                let on_state = checkbox_on_state(field);
                field_dictionary.insert(
                    "V".to_owned(),
                    Object::Name(if on { on_state } else { "Off".to_owned() }),
                );
            }
            FormFieldKind::Radio => {
                let selected = value
                    .as_choices()
                    .and_then(|values| values.first())
                    .cloned()
                    .unwrap_or_else(|| "Off".to_owned());
                field_dictionary.insert("V".to_owned(), Object::Name(selected));
            }
            FormFieldKind::Signature | FormFieldKind::Button => {
                field_dirty = false;
            }
        }
    }

    for (widget_index, widget) in field.widgets.iter().enumerate() {
        let widget_id = match widget.object {
            Some(id) => id,
            None => continue,
        };
        let mut dictionary = if widget_id == field_id {
            field_dictionary.clone()
        } else {
            dictionary_of(doc, &Object::Reference(widget_id))?
        };
        if let Some((update, widget_report)) = build_widget_appearance(
            doc,
            &acroform,
            &allocator,
            model,
            field_index,
            widget_index,
            value,
        )? {
            dictionary.insert("AP".to_owned(), Object::Dictionary(update.ap));
            match update.active_state {
                Some(state) => {
                    dictionary.insert("AS".to_owned(), Object::Name(state));
                }
                None => {
                    dictionary.remove("AS");
                }
            }
            report.widgets.push(widget_report);
            if widget_id == field_id {
                field_dictionary = dictionary;
                field_dirty = true;
            } else {
                doc.set_object(widget_id, Object::Dictionary(dictionary))?;
            }
        }
    }

    if field_dirty {
        doc.set_object(field_id, Object::Dictionary(field_dictionary))?;
    }
    Ok(report)
}

fn checkbox_on_state(field: &FormField) -> String {
    field
        .widgets
        .iter()
        .find_map(FormWidget::on_state)
        .map(str::to_owned)
        .or_else(|| field.options.first().cloned())
        .unwrap_or_else(|| "Yes".to_owned())
}

fn build_widget_appearance(
    doc: &mut Document,
    acroform: &Dictionary,
    allocator: &Allocator,
    model: &AcroFormModel,
    field_index: usize,
    widget_index: usize,
    value: &FieldValue,
) -> Result<Option<(AppearanceUpdate, WidgetAppearanceReport)>, AcroFormError> {
    let field = &model.fields[field_index];
    let widget = match field.widgets.get(widget_index) {
        Some(widget) => widget,
        None => return Ok(None),
    };
    if widget.rect.is_empty() {
        return Ok(None);
    }
    let width = widget.rect.width();
    let height = widget.rect.height();
    let rotation = widget_rotation(doc, widget.object);
    let (local_width, local_height) = if matches!(rotation, 90 | 270) {
        (height, width)
    } else {
        (width, height)
    };

    let mut appearance = Field::new(field.name.clone(), field_type(field.kind))
        .with_flags(field.flags)
        .with_value(value.clone());
    if field.kind == FormFieldKind::Choice {
        appearance.set_choices(field.options.iter().cloned());
    }
    let on_state = widget
        .on_state()
        .map(str::to_owned)
        .or_else(|| field.options.first().cloned())
        .unwrap_or_else(|| "Yes".to_owned());
    if field.kind == FormFieldKind::Radio {
        appearance.set_choices([on_state.clone()]);
    }

    let da = parse_default_appearance(field.default_appearance.as_deref().unwrap_or(""));
    let font_size = if da.size > 0.0 {
        da.size
    } else {
        auto_font_size(local_height as f32)
    };
    let mut widget_appearance = WidgetAppearance::new(WidgetRect::new(
        0.0,
        0.0,
        local_width as f32,
        local_height as f32,
    ));
    widget_appearance.alignment = match field.alignment {
        1 => Alignment::Center,
        2 => Alignment::Right,
        _ => Alignment::Left,
    };
    widget_appearance.font = FontSpec::new("Helvetica", font_size, da.color);

    if field.kind == FormFieldKind::Choice {
        appearance.set_flags(field.flags.union(FieldFlags::COMBO));
    }

    let matrix = Matrix::rotation_into_box(
        i64::from(rotation),
        widget_appearance.rect.width.into(),
        widget_appearance.rect.height.into(),
    )
    .unwrap_or(Matrix::IDENTITY);
    let text_rect = text_extent(&appearance, &widget_appearance, value).map(|rect| {
        FormRect::new(
            f64::from(rect.x),
            f64::from(rect.y),
            f64::from(rect.right()),
            f64::from(rect.top()),
        )
    });
    let mut report = WidgetAppearanceReport {
        widget: widget.object,
        font: None,
        rotation,
        matrix,
        bbox: FormRect::new(0.0, 0.0, local_width, local_height),
        text_rect,
    };

    let generated = if matches!(field.kind, FormFieldKind::Text | FormFieldKind::Choice) {
        let (field_font, font_report) = choose_font(doc, acroform, field, value, &da.font);
        report.font = Some(font_report);
        let fallback = WinAnsiEncoder;
        match &field_font {
            Some(font) => {
                build_appearance_with_encoder(&appearance, &widget_appearance, value, font)?
            }
            None if fallback.encode(&appearance_text(value)).is_some() => {
                build_appearance_with_encoder(&appearance, &widget_appearance, value, &fallback)?
            }
            None => crate::appearance::build_appearance(&appearance, &widget_appearance, value)?,
        }
    } else {
        crate::appearance::build_appearance(&appearance, &widget_appearance, value)?
    };

    match generated.normal {
        NormalAppearance::Stream(stream) => {
            let (font_name, font_object) = match &report.font {
                Some(AppearanceFont::FieldFont { resource, .. }) => {
                    let object = font_object_for(doc, acroform, field, resource);
                    (resource.clone(), object)
                }
                Some(AppearanceFont::Fallback { .. }) => {
                    fallback_font(doc, &da.font, allocator.font)?
                }
                None => ("Helv".to_owned(), Object::Null),
            };
            let content = if matches!(field.kind, FormFieldKind::Choice | FormFieldKind::Text) {
                replace_token(
                    &stream.content,
                    b"/F1 ",
                    format!("/{font_name} ").as_bytes(),
                )
            } else {
                stream.content
            };
            let font_resource = match font_object {
                Object::Null => None,
                object => Some((font_name.as_str(), object)),
            };
            let id = allocator.stream(field_index, widget_index, 0);
            let reference = write_stream(
                doc,
                id,
                (local_width, local_height),
                content,
                font_resource,
                matrix,
            )?;
            Ok(Some((
                AppearanceUpdate {
                    ap: single_normal(Object::Reference(reference)),
                    active_state: None,
                },
                report,
            )))
        }
        NormalAppearance::States(states) => {
            let mut named = Dictionary::new();
            let mut active = None;
            for (state_index, (state, stream)) in states.iter().enumerate() {
                let state_name = if field.kind == FormFieldKind::Checkbox && state == "On" {
                    on_state.clone()
                } else {
                    state.clone()
                };
                let id = allocator.stream(field_index, widget_index, state_index);
                let reference = write_stream(
                    doc,
                    id,
                    (local_width, local_height),
                    stream.content.clone(),
                    None,
                    matrix,
                )?;
                named.insert(state_name.clone(), Object::Reference(reference));
                if field.kind == FormFieldKind::Checkbox {
                    let checked = value.as_bool().unwrap_or(false);
                    active = Some(if checked {
                        on_state.clone()
                    } else {
                        "Off".to_owned()
                    });
                } else {
                    let selected = value
                        .as_choices()
                        .and_then(|values| values.first())
                        .map(String::as_str)
                        .unwrap_or("Off");
                    active = Some(if selected == on_state {
                        on_state.clone()
                    } else {
                        "Off".to_owned()
                    });
                }
            }
            Ok(Some((
                AppearanceUpdate {
                    ap: single_normal(Object::Dictionary(named)),
                    active_state: active,
                },
                report,
            )))
        }
    }
}

/// Returns the widget `/MK /R` rotation normalized to `0`, `90`, `180` or
/// `270` degrees counter-clockwise.
///
/// A missing, malformed or non-right-angle value is treated as upright; the
/// widget is still filled and its appearance remains inside the rectangle.
fn widget_rotation(doc: &Document, object: Option<ObjectId>) -> i32 {
    let Some(object) = object else {
        return 0;
    };
    let Ok(value) = doc.object(object) else {
        return 0;
    };
    let Some(dictionary) = value.as_dictionary() else {
        return 0;
    };
    let Some(rotation) = dictionary
        .get("MK")
        .and_then(Object::as_dictionary)
        .and_then(|mk| mk.get("R"))
        .and_then(Object::as_i64)
    else {
        return 0;
    };
    let normalized = rotation.rem_euclid(360);
    if matches!(normalized, 90 | 180 | 270) {
        i32::try_from(normalized).unwrap_or(0)
    } else {
        0
    }
}

/// Returns the standard automatic font size for a field `height`.
///
/// PDF uses `/DA` size `0` for "auto": the size is the largest that fits the
/// field, capped at 12 points. The 4-point inset matches the generator's
/// vertical padding and border.
fn auto_font_size(height: f32) -> f32 {
    (height - 4.0).clamp(4.0, 12.0)
}

/// The text drawn by a text-like field value, for the encoding test.
fn appearance_text(value: &FieldValue) -> String {
    match value {
        FieldValue::Text(text) => text.clone(),
        FieldValue::Choice(values) => values.first().cloned().unwrap_or_default(),
        FieldValue::Number(number) => number.to_string(),
        FieldValue::Bool(flag) => if *flag { "On" } else { "Off" }.to_owned(),
        FieldValue::Empty => String::new(),
    }
}

/// Chooses the text encoder for a field.
///
/// Returns the encoder plus its report value. A `None` encoder means the caller
/// must build the appearance with the default Helvetica path; the report still
/// explains why.
fn choose_font(
    doc: &Document,
    acroform: &Dictionary,
    field: &FormField,
    value: &FieldValue,
    requested: &str,
) -> (Option<FieldFont>, AppearanceFont) {
    let text = appearance_text(value);
    match resolve_font(doc, acroform, field, requested) {
        FontChoice::Field(font) => {
            if font.encode(&text).is_some() {
                let report = font.report();
                (Some(font), report)
            } else {
                (
                    None,
                    AppearanceFont::Fallback {
                        requested: sanitise_name(requested),
                        reason: FallbackReason::CannotEncode,
                    },
                )
            }
        }
        FontChoice::Fallback(reason) => (
            None,
            AppearanceFont::Fallback {
                requested: sanitise_name(requested),
                reason,
            },
        ),
    }
}

/// Returns the object for a resolved `/DA` font resource.
///
/// The lookup is repeated rather than carried through the report so the report
/// stays a plain value; the font was resolved from the same document moments
/// earlier, so a miss means the document changed under the fill.
fn font_object_for(
    doc: &Document,
    acroform: &Dictionary,
    field: &FormField,
    resource: &str,
) -> Object {
    match resolve_font(doc, acroform, field, resource) {
        FontChoice::Field(font) => font.object,
        FontChoice::Fallback(_) => Object::Null,
    }
}

fn single_normal(normal: Object) -> Dictionary {
    let mut ap = Dictionary::new();
    ap.insert("N".to_owned(), normal);
    ap
}

fn write_stream(
    doc: &mut Document,
    id: u32,
    extents: (f64, f64),
    content: Vec<u8>,
    font: Option<(&str, Object)>,
    matrix: Matrix,
) -> Result<ObjectId, AcroFormError> {
    let (width, height) = extents;
    let mut dictionary = Dictionary::new();
    dictionary.insert("Type".to_owned(), Object::Name("XObject".to_owned()));
    dictionary.insert("Subtype".to_owned(), Object::Name("Form".to_owned()));
    dictionary.insert("FormType".to_owned(), Object::Integer(1));
    dictionary.insert(
        "BBox".to_owned(),
        Object::Array(vec![
            Object::Integer(0),
            Object::Integer(0),
            number(width),
            number(height),
        ]),
    );
    if !matrix.is_identity() {
        dictionary.insert(
            "Matrix".to_owned(),
            Object::Array(matrix.to_array().into_iter().map(number).collect()),
        );
    }
    if let Some((name, object)) = font {
        let mut fonts = Dictionary::new();
        fonts.insert(name.to_owned(), object);
        let mut resources = Dictionary::new();
        resources.insert("Font".to_owned(), Object::Dictionary(fonts));
        dictionary.insert("Resources".to_owned(), Object::Dictionary(resources));
    }
    let identifier = ObjectId::new(id, 0);
    doc.set_object(
        identifier,
        Object::Stream {
            dictionary,
            data: content,
        },
    )?;
    Ok(identifier)
}

/// Creates the base-14 Helvetica font used by the fallback appearance path.
///
/// The returned resource name is the sanitised `/DA` name, so the emitted `Tf`
/// operator still refers to the name the document asked for even though the
/// glyphs come from Helvetica. The appearance stream carries its own
/// `/Resources /Font` mapping, which shadows any page or AcroForm entry of the
/// same name for this stream only.
fn fallback_font(
    doc: &mut Document,
    requested: &str,
    font_id: u32,
) -> Result<(String, Object), AcroFormError> {
    let name = sanitise_name(requested);
    let mut font = Dictionary::new();
    font.insert("Type".to_owned(), Object::Name("Font".to_owned()));
    font.insert("Subtype".to_owned(), Object::Name("Type1".to_owned()));
    font.insert("BaseFont".to_owned(), Object::Name("Helvetica".to_owned()));
    font.insert(
        "Encoding".to_owned(),
        Object::Name("WinAnsiEncoding".to_owned()),
    );
    let identifier = ObjectId::new(font_id, 0);
    doc.set_object(identifier, Object::Dictionary(font))?;
    Ok((name, Object::Reference(identifier)))
}

struct DefaultAppearance {
    font: String,
    size: f32,
    color: RgbColor,
}

fn parse_default_appearance(text: &str) -> DefaultAppearance {
    let tokens: Vec<&str> = text.split_whitespace().collect();
    let mut font = "Helv".to_owned();
    let mut size = 0.0_f32;
    let mut color = RgbColor::BLACK;
    for (index, token) in tokens.iter().enumerate() {
        match *token {
            "Tf" if index >= 2 => {
                font = tokens[index - 2].trim_start_matches('/').to_owned();
                size = tokens[index - 1].parse::<f32>().unwrap_or(0.0);
            }
            "g" if index >= 1 => {
                if let Ok(grey) = tokens[index - 1].parse::<f32>() {
                    color = RgbColor::new(grey, grey, grey);
                }
            }
            "rg" if index >= 3 => {
                let red = tokens[index - 3].parse::<f32>().unwrap_or(0.0);
                let green = tokens[index - 2].parse::<f32>().unwrap_or(0.0);
                let blue = tokens[index - 1].parse::<f32>().unwrap_or(0.0);
                color = RgbColor::new(red, green, blue);
            }
            _ => {}
        }
    }
    DefaultAppearance { font, size, color }
}

fn replace_token(data: &[u8], from: &[u8], to: &[u8]) -> Vec<u8> {
    if from.is_empty() {
        return data.to_vec();
    }
    let mut out = Vec::with_capacity(data.len());
    let mut index = 0;
    while index < data.len() {
        if data[index..].starts_with(from) {
            out.extend_from_slice(to);
            index += from.len();
        } else {
            out.push(data[index]);
            index += 1;
        }
    }
    out
}

fn number(value: f64) -> Object {
    if value.fract() == 0.0 {
        Object::Integer(value as i64)
    } else {
        Object::Real(value)
    }
}

fn field_type(kind: FormFieldKind) -> FieldType {
    match kind {
        FormFieldKind::Text => FieldType::Text,
        FormFieldKind::Checkbox => FieldType::CheckBox,
        FormFieldKind::Radio => FieldType::Radio,
        FormFieldKind::Choice => FieldType::Choice,
        FormFieldKind::Signature => FieldType::Signature,
        FormFieldKind::Button => FieldType::Button,
    }
}

fn acroform_dictionary(doc: &Document, model: &AcroFormModel) -> Result<Dictionary, AcroFormError> {
    let Some(id) = model.acroform_object else {
        return Ok(Dictionary::new());
    };
    doc.object(id)?
        .as_dictionary()
        .cloned()
        .ok_or(AcroFormError::Malformed("AcroForm is not a dictionary"))
}

/// Returns `true` when the AcroForm requests appearance generation.
///
/// # Errors
///
/// Returns a reader error when the document body cannot be parsed.
pub fn needs_appearances(doc: &Document) -> Result<bool, AcroFormError> {
    Ok(read_acroform(doc)?.need_appearances)
}

/// Regenerate every field appearance from the values read from the source.
///
/// This is the `NeedAppearances` repair path: it rewrites `/AP /N` for every
/// addressable field and clears the flag. Fields without an indirect object are
/// skipped. As with the setters, values come from the document snapshot; save
/// and reopen first when earlier in-memory fills must be included.
///
/// # Errors
///
/// Returns [`AcroFormError::Core`] or [`AcroFormError::Malformed`] when the
/// document or AcroForm cannot be read or written.
pub fn regenerate_appearances(doc: &mut Document) -> Result<usize, AcroFormError> {
    regenerate_appearances_reported(doc).map(|(count, _)| count)
}

/// Regenerate every field appearance and report the appearance decisions.
///
/// Behaves exactly like [`regenerate_appearances`] but returns the number of
/// regenerated fields together with the per-widget [`AppearanceReport`].
///
/// # Errors
///
/// Same as [`regenerate_appearances`].
pub fn regenerate_appearances_reported(
    doc: &mut Document,
) -> Result<(usize, AppearanceReport), AcroFormError> {
    let model = read_acroform(doc)?;
    if !model.present {
        return Ok((0, AppearanceReport::default()));
    }
    let mut regenerated = 0;
    let mut report = AppearanceReport::default();
    for index in 0..model.fields.len() {
        let field = &model.fields[index];
        if field.field_object.is_none() {
            continue;
        }
        let value = field.value.clone();
        let field_report = apply_value(doc, &model, index, &value, false)?;
        report.widgets.extend(field_report.widgets);
        regenerated += 1;
    }

    if let Some(id) = model.acroform_object {
        let mut dictionary = doc
            .object(id)?
            .as_dictionary()
            .cloned()
            .ok_or(AcroFormError::Malformed("AcroForm is not a dictionary"))?;
        dictionary.insert("NeedAppearances".to_owned(), Object::Boolean(false));
        doc.set_object(id, Object::Dictionary(dictionary))?;
    }
    Ok((regenerated, report))
}

/// Flatten every widget into page content and remove the catalog AcroForm.
///
/// Each widget's `/AP /N` stream (or its `/AS` state stream) is added to the
/// page `/Resources /XObject`, painted with a placement matrix derived from the
/// widget `/Rect` and the stream `/BBox`, and the widget annotation is removed.
/// Original page content is preserved, so pre-existing page text is unchanged.
///
/// # Errors
///
/// Returns [`AcroFormError::Core`] or [`AcroFormError::Malformed`] for a
/// damaged document, and [`AcroFormError::Malformed`] for a widget with an
/// unusable `/Contents` or `/AP` shape.
pub fn flatten(doc: &mut Document) -> Result<FlattenFormReport, AcroFormError> {
    let model = read_acroform(doc)?;
    if !model.present {
        return Ok(FlattenFormReport::default());
    }

    let widget_ids: Vec<(usize, ObjectId, FormRect)> = model
        .fields
        .iter()
        .flat_map(|field| {
            field
                .widgets
                .iter()
                .filter_map(|widget| Some((widget.page?, widget.object?, widget.rect)))
        })
        .collect();
    let widget_set: std::collections::BTreeSet<ObjectId> = model
        .fields
        .iter()
        .flat_map(|field| field.widgets.iter().filter_map(|widget| widget.object))
        .collect();

    let mut next_id = doc.trailer_size()?.unwrap_or(0).saturating_add(1).max(1);
    let mut report = FlattenFormReport {
        fields: model.fields.len(),
        ..FlattenFormReport::default()
    };

    let page_count = doc.page_count()?;
    for page_index in 0..page_count {
        let page = doc.page(page_index)?;
        let mut page_dictionary = doc
            .object(page.id)?
            .as_dictionary()
            .cloned()
            .ok_or(AcroFormError::Malformed("page is not a dictionary"))?;

        let paints: Vec<(ObjectId, FormRect)> = widget_ids
            .iter()
            .filter(|(widget_page, _, _)| *widget_page == page_index)
            .map(|(_, id, rect)| (*id, *rect))
            .collect();
        if paints.is_empty() && !page_has_widget(&page_dictionary, &widget_set)? {
            continue;
        }

        let mut resources = match page_dictionary.get("Resources") {
            Some(Object::Reference(id)) => {
                doc.object(*id)?
                    .as_dictionary()
                    .cloned()
                    .ok_or(AcroFormError::Malformed(
                        "page Resources is not a dictionary",
                    ))?
            }
            Some(value) => value
                .as_dictionary()
                .cloned()
                .ok_or(AcroFormError::Malformed(
                    "page Resources is not a dictionary",
                ))?,
            None => Dictionary::new(),
        };
        let mut xobjects = match resources.get("XObject") {
            Some(Object::Reference(id)) => {
                doc.object(*id)?
                    .as_dictionary()
                    .cloned()
                    .ok_or(AcroFormError::Malformed(
                        "XObject resources are not a dictionary",
                    ))?
            }
            Some(value) => value
                .as_dictionary()
                .cloned()
                .ok_or(AcroFormError::Malformed(
                    "XObject resources are not a dictionary",
                ))?,
            None => Dictionary::new(),
        };
        let mut paint_content = String::new();
        let mut painted = 0usize;

        for (widget_id, rect) in paints {
            let widget = doc
                .object(widget_id)?
                .as_dictionary()
                .cloned()
                .ok_or(AcroFormError::Malformed("widget is not a dictionary"))?;
            let Some((stream_id, bbox, appearance_matrix)) = normal_appearance(doc, &widget)?
            else {
                continue;
            };
            let Some(placement) = Matrix::fit_bbox(
                appearance_matrix,
                (bbox.left, bbox.bottom, bbox.right, bbox.top),
                rect.left,
                rect.bottom,
                rect.width(),
                rect.height(),
            ) else {
                continue;
            };
            let name = unique_xobject_name(&xobjects);
            xobjects.insert(name.clone(), Object::Reference(stream_id));
            paint_content.push_str(&format!(
                "q\n{} {} {} {} {} {} cm\n/{} Do\nQ\n",
                fmt(placement.a),
                fmt(placement.b),
                fmt(placement.c),
                fmt(placement.d),
                fmt(placement.e),
                fmt(placement.f),
                name
            ));
            painted += 1;
        }

        if painted > 0 {
            let content_id = ObjectId::new(next_id, 0);
            next_id = next_id.saturating_add(1);
            doc.set_object(
                content_id,
                Object::Stream {
                    dictionary: Dictionary::new(),
                    data: paint_content.into_bytes(),
                },
            )?;
            let contents = append_content(doc, &page_dictionary, content_id, &mut next_id)?;
            page_dictionary.insert("Contents".to_owned(), contents);
        }

        if !xobjects.is_empty() {
            resources.insert("XObject".to_owned(), Object::Dictionary(xobjects));
            page_dictionary.insert("Resources".to_owned(), Object::Dictionary(resources));
        }

        let annots = filter_widget_annots(doc, &page_dictionary, &widget_set)?;
        if let Some(annots) = annots {
            page_dictionary.insert("Annots".to_owned(), annots);
        }

        doc.set_object(page.id, Object::Dictionary(page_dictionary))?;
        report.widgets += painted;
        if painted > 0 {
            report.pages += 1;
        }
    }

    // Remove the catalog AcroForm. The field dictionaries become unreachable
    // with it, which is the PDF meaning of flattening.
    let root_id = doc.root()?;
    let mut root = doc
        .object(root_id)?
        .as_dictionary()
        .cloned()
        .ok_or(AcroFormError::Malformed("catalog is not a dictionary"))?;
    root.remove("AcroForm");
    doc.set_object(root_id, Object::Dictionary(root))?;

    Ok(report)
}

fn page_has_widget(
    dictionary: &Dictionary,
    widgets: &std::collections::BTreeSet<ObjectId>,
) -> Result<bool, AcroFormError> {
    let Some(annots) = dictionary.get("Annots") else {
        return Ok(false);
    };
    let items = match annots {
        Object::Array(items) => items.as_slice(),
        _ => return Ok(false),
    };
    Ok(items
        .iter()
        .filter_map(Object::as_reference)
        .any(|id| widgets.contains(&id)))
}

fn normal_appearance(
    doc: &Document,
    widget: &Dictionary,
) -> Result<Option<(ObjectId, FormRect, Matrix)>, AcroFormError> {
    let normal = widget
        .get("AP")
        .and_then(Object::as_dictionary)
        .and_then(|ap| ap.get("N"));
    let Some(normal) = normal else {
        return Ok(None);
    };
    let states = match normal {
        Object::Reference(id) => match doc.object(*id)? {
            Object::Stream { .. } => return appearance_bbox(doc, *id),
            Object::Dictionary(states) => Some(states),
            _ => None,
        },
        Object::Dictionary(states) => Some(states.clone()),
        _ => None,
    };
    let selected = states.and_then(|states| {
        let preferred = widget
            .get("AS")
            .and_then(Object::as_name)
            .map(str::to_owned)
            .or_else(|| states.keys().find(|name| name.as_str() != "Off").cloned())
            .or_else(|| states.keys().next().cloned());
        preferred
            .and_then(|name| states.get(&name))
            .and_then(Object::as_reference)
    });
    let Some(stream_id) = selected else {
        return Ok(None);
    };
    appearance_bbox(doc, stream_id)
}

fn appearance_bbox(
    doc: &Document,
    stream_id: ObjectId,
) -> Result<Option<(ObjectId, FormRect, Matrix)>, AcroFormError> {
    let stream = doc.object(stream_id)?;
    let dictionary = stream.as_dictionary().ok_or(AcroFormError::Malformed(
        "appearance stream is not a stream",
    ))?;
    let bbox = dictionary
        .get("BBox")
        .and_then(FormRect::from_object)
        .unwrap_or_else(|| FormRect::new(0.0, 0.0, 1.0, 1.0));
    let matrix = dictionary
        .get("Matrix")
        .and_then(Object::as_array)
        .and_then(|values| {
            if values.len() < 6 {
                return None;
            }
            let mut coefficients = [0.0_f64; 6];
            for (slot, value) in coefficients.iter_mut().zip(values.iter()) {
                *slot = value.as_f64()?;
            }
            Some(Matrix::from_array(coefficients))
        })
        .filter(|matrix| matrix.is_finite())
        .unwrap_or(Matrix::IDENTITY);
    Ok(Some((stream_id, bbox, matrix)))
}

fn unique_xobject_name(xobjects: &Dictionary) -> String {
    let mut index = 0usize;
    loop {
        let name = format!("Fm{index}");
        if !xobjects.contains_key(&name) {
            return name;
        }
        index += 1;
    }
}

fn append_content(
    doc: &mut Document,
    page: &Dictionary,
    new_id: ObjectId,
    next_id: &mut u32,
) -> Result<Object, AcroFormError> {
    match page.get("Contents") {
        None => Ok(Object::Reference(new_id)),
        Some(Object::Reference(id)) => Ok(Object::Array(vec![
            Object::Reference(*id),
            Object::Reference(new_id),
        ])),
        Some(Object::Array(items)) => {
            let mut items = items.clone();
            items.push(Object::Reference(new_id));
            Ok(Object::Array(items))
        }
        Some(Object::Stream { dictionary, data }) => {
            let copy_id = ObjectId::new(*next_id, 0);
            *next_id = next_id.saturating_add(1);
            doc.set_object(
                copy_id,
                Object::Stream {
                    dictionary: dictionary.clone(),
                    data: data.clone(),
                },
            )?;
            Ok(Object::Array(vec![
                Object::Reference(copy_id),
                Object::Reference(new_id),
            ]))
        }
        Some(_) => Err(AcroFormError::Malformed(
            "page Contents is neither a stream nor an array",
        )),
    }
}

fn filter_widget_annots(
    doc: &Document,
    page: &Dictionary,
    widgets: &std::collections::BTreeSet<ObjectId>,
) -> Result<Option<Object>, AcroFormError> {
    let Some(annots) = page.get("Annots") else {
        return Ok(None);
    };
    let items = match annots {
        Object::Reference(id) => match doc.object(*id)? {
            Object::Array(items) => items,
            _ => return Ok(None),
        },
        Object::Array(items) => items.clone(),
        _ => return Ok(None),
    };
    let kept: Vec<Object> = items
        .into_iter()
        .filter(|item| match item.as_reference() {
            Some(id) => !widgets.contains(&id),
            None => true,
        })
        .collect();
    Ok(Some(Object::Array(kept)))
}

fn fmt(value: f64) -> String {
    if !value.is_finite() {
        return "0".to_owned();
    }
    let rounded = (value * 10_000.0).round() / 10_000.0;
    if rounded == 0.0 {
        return "0".to_owned();
    }
    let text = format!("{rounded:.4}");
    let text = text.trim_end_matches('0');
    let text = text.trim_end_matches('.');
    text.to_owned()
}

/// Export the model's field values as XFDF.
///
/// Fields are written in name-sorted order. Checkbox values are exported as
/// `On`/`Off`, radio and choice fields as their selected names, and text fields
/// verbatim. The output is the subset accepted by [`import_xfdf`] and by the
/// crate's existing XFDF reader.
pub fn export_xfdf(model: &AcroFormModel) -> String {
    let mut fields: Vec<&FormField> = model
        .fields
        .iter()
        .filter(|field| !field.flags.is_no_export())
        .collect();
    fields.sort_by(|a, b| a.name.cmp(&b.name));

    let mut out = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <xfdf xmlns=\"http://ns.adobe.com/xfdf/\">\n<fields>\n",
    );
    for field in fields {
        out.push_str("<field name=\"");
        write_xml_escaped(&mut out, &field.name, true);
        out.push_str("\">\n");
        match &field.value {
            FieldValue::Empty => {}
            FieldValue::Text(text) => write_xml_value(&mut out, text),
            FieldValue::Bool(on) => write_xml_value(&mut out, if *on { "On" } else { "Off" }),
            FieldValue::Choice(values) => {
                for value in values {
                    write_xml_value(&mut out, value);
                }
            }
            FieldValue::Number(number) => write_xml_value(&mut out, &number.to_string()),
        }
        out.push_str("</field>\n");
    }
    out.push_str("</fields>\n</xfdf>\n");
    out
}

fn write_xml_value(out: &mut String, value: &str) {
    out.push_str("<value>");
    write_xml_escaped(out, value, false);
    out.push_str("</value>\n");
}

fn write_xml_escaped(out: &mut String, text: &str, attribute: bool) {
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' if attribute => out.push_str("&quot;"),
            '\'' if attribute => out.push_str("&apos;"),
            ch if ch.is_control() => out.push_str(&format!("&#x{:X};", u32::from(ch))),
            ch => out.push(ch),
        }
    }
}

/// Import XFDF field values into `doc`, returning the number of fields applied.
///
/// Field kinds come from the document. Unknown names are rejected with
/// [`AcroFormError::UnknownField`]; signature and pushbutton values are ignored
/// because they carry no user data.
///
/// # Errors
///
/// Returns [`AcroFormError::Xfdf`] for malformed XML,
/// [`AcroFormError::UnknownField`] for an unknown name, and any setter error.
pub fn import_xfdf(doc: &mut Document, input: &str) -> Result<usize, AcroFormError> {
    let document = crate::xfdf::parse_xfdf(input)?;
    let model = read_acroform(doc)?;
    let mut applied = 0;

    for raw in document.fields() {
        let field = model
            .fields
            .iter()
            .find(|field| field.name == raw.name())
            .ok_or_else(|| AcroFormError::UnknownField(raw.name().to_owned()))?;
        let first = raw.values().first().cloned();
        match field.kind {
            FormFieldKind::Text => {
                set_text(doc, &field.name, first.as_deref().unwrap_or(""))?;
                applied += 1;
            }
            FormFieldKind::Choice => {
                set_choice(doc, &field.name, first.as_deref())?;
                applied += 1;
            }
            FormFieldKind::Checkbox => {
                let on_state = checkbox_on_state(field);
                let checked = first.as_deref().is_some_and(|value| {
                    value.eq_ignore_ascii_case("on")
                        || value.eq_ignore_ascii_case("yes")
                        || value.eq_ignore_ascii_case("true")
                        || value == "1"
                        || value == on_state
                });
                set_checkbox(doc, &field.name, checked)?;
                applied += 1;
            }
            FormFieldKind::Radio => {
                if let Some(value) = first.as_deref().filter(|value| !value.is_empty()) {
                    set_radio(doc, &field.name, value)?;
                    applied += 1;
                }
            }
            FormFieldKind::Signature | FormFieldKind::Button => {}
        }
    }
    Ok(applied)
}

#[cfg(test)]
mod tests;
