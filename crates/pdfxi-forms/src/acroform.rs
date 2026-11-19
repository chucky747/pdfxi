//! Loading a real PDF `/AcroForm` dictionary into the in-memory form model.
//!
//! # Why an adapter trait instead of a `pdfxi-core` dependency
//!
//! `docs/ARCHITECTURE.md` requires `pdfxi-forms` to stay dependency-free beyond
//! `thiserror` until a documented need appears, and `pdfxi-core` deliberately
//! does not expose a public resolver for arbitrary indirect references. Adding
//! a direct dependency would therefore not be enough to walk a field tree: the
//! core crate would first need a public object-graph API, which belongs to the
//! core lane and is outside this task's ownership. Instead this module defines
//! a small, stable boundary:
//!
//! - [`PdfValue`] is the minimal subset of the PDF object model the loader
//!   needs (null, booleans, numbers, strings, names, arrays, dictionaries and
//!   indirect references).
//! - [`ObjectRef`] keeps an indirect reference as `(number, generation)` so the
//!   loader can detect cycles in `/Kids` without handing out raw pointers.
//! - [`FormSource`] is implemented by a host document: it returns the catalog
//!   dictionary and resolves one indirection at a time. A future concrete
//!   adapter can be written in a crate that already owns a document object
//!   graph, leaving this crate dependency-free and directly unit-testable.
//!
//! # Loading rules
//!
//! The loader starts at the catalog's `/AcroForm`, walks `/Fields` and every
//! `/Kids` hierarchy, and builds fully qualified names by joining the
//! non-empty inherited `/T` partial names with a dot (`.`). It resolves the
//! inheritable attributes `/FT`, `/Ff`, `/V`, `/DV`, `/T`, `/TU`, `/Opt`,
//! `/MaxLen`, `/Q` and `/DA`; a value set on a child overrides the inherited
//! one. Resolved `/TU`, `/Q` and `/DA` are reported in [`LoadedFieldInfo`]
//! because the [`Field`] model has no slots for them.
//!
//! A node is a *terminal* field when it has no `/Kids`, an empty `/Kids`, or
//! only widget-annotation kids while itself declaring `/FT`, `/T`, `/V` or
//! `/DV` (the merged field/widget form used by radio groups). Otherwise its
//! kids are walked as sub-fields and widget-annotation kids are skipped. A
//! terminal field with an unknown or missing `/FT` is preserved as an opaque
//! [`FieldType::Unknown`] field rather than dropped.
//!
//! # Error and fallback policy
//!
//! - A catalog without `/AcroForm` (or with `/AcroForm null`) is not an error:
//!   [`AcroFormLoad::NoForm`] is returned.
//! - A cycle in the reference graph reached through `/Kids` is an error
//!   ([`LoadError::Cycle`]); so is a duplicate fully qualified name
//!   ([`LoadError::DuplicateFieldName`]) and an unresolvable structural
//!   reference ([`LoadError::UnresolvedReference`]).
//! - *Attribute values* that cannot be resolved or coerced are not errors. A
//!   missing attribute falls back to the inherited value, and a PDF value
//!   whose type does not map to the field's value kind falls back to
//!   [`FieldValue::Empty`]. An adapter supplies [`PdfValue::Text`] already
//!   decoded (UTF-16 with a byte-order mark, or lossy UTF-8); the loader
//!   normalises `#xx` escapes in PDF names. These rules are deliberately total
//!   so hostile input never aborts a load.

use crate::field::{Field, FieldFlags, FieldType, FieldValue, TextConstraint};
use crate::form::{Form, FormError};
use std::collections::BTreeMap;
use std::fmt;

/// Maximum number of nested field nodes accepted before treating the tree as
/// hostile. Matches the parser's defence against stack exhaustion.
const MAX_NESTING: usize = 128;

/// The button-field flag marking a radio group (PDF `/Ff` bit 16).
const BTN_RADIO: u32 = 1 << 15;
/// The button-field flag marking a pushbutton (PDF `/Ff` bit 17).
const BTN_PUSHBUTTON: u32 = 1 << 16;

/// Identifier of an indirect PDF object.
///
/// Object and generation numbers are kept verbatim; they are used only as an
/// identity key for cycle detection, never as a memory address.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ObjectRef {
    /// The indirect object number.
    pub number: u32,
    /// The indirect object generation number.
    pub generation: u16,
}

impl ObjectRef {
    /// Creates an indirect reference from its object and generation numbers.
    pub fn new(number: u32, generation: u16) -> Self {
        Self { number, generation }
    }
}

impl fmt::Display for ObjectRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {} R", self.number, self.generation)
    }
}

/// The minimal PDF object values needed to walk an `/AcroForm` field tree.
///
/// A [`FormSource`] adapter converts its host object model into this enum.
/// References stay as [`PdfValue::Reference`] so the loader can detect cycles
/// instead of eagerly dereferencing them.
#[derive(Debug, Clone, PartialEq)]
pub enum PdfValue {
    /// The PDF `null` object.
    Null,
    /// A Boolean object.
    Bool(bool),
    /// A numeric object. Integers and reals share this variant.
    Number(f64),
    /// A PDF string decoded to text (UTF-16 with BOM, or lossy UTF-8).
    Text(String),
    /// A PDF name, stored without its leading slash and with `#xx` decoded.
    Name(String),
    /// An ordered array of objects.
    Array(Vec<PdfValue>),
    /// A dictionary mapping names (without the leading slash) to objects.
    Dictionary(BTreeMap<String, PdfValue>),
    /// An indirect reference to another object.
    Reference(ObjectRef),
}

impl PdfValue {
    /// Returns the dictionary entries when this is a [`PdfValue::Dictionary`].
    pub fn as_dict(&self) -> Option<&BTreeMap<String, PdfValue>> {
        match self {
            Self::Dictionary(entries) => Some(entries),
            _ => None,
        }
    }

    /// Returns the items when this is a [`PdfValue::Array`].
    pub fn as_array(&self) -> Option<&[PdfValue]> {
        match self {
            Self::Array(items) => Some(items.as_slice()),
            _ => None,
        }
    }

    /// Returns the name when this is a [`PdfValue::Name`].
    pub fn as_name(&self) -> Option<&str> {
        match self {
            Self::Name(name) => Some(name.as_str()),
            _ => None,
        }
    }

    /// Returns the text when this is a [`PdfValue::Text`].
    pub fn as_text(&self) -> Option<&str> {
        match self {
            Self::Text(text) => Some(text.as_str()),
            _ => None,
        }
    }

    /// Returns the Boolean when this is a [`PdfValue::Bool`].
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Bool(value) => Some(*value),
            _ => None,
        }
    }

    /// Returns the reference identifier when this is a [`PdfValue::Reference`].
    pub fn as_reference(&self) -> Option<ObjectRef> {
        match self {
            Self::Reference(id) => Some(*id),
            _ => None,
        }
    }

    /// Returns the value as an `i64` when it is a finite, in-range number.
    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Self::Number(value)
                if value.is_finite() && *value >= i64::MIN as f64 && *value <= i64::MAX as f64 =>
            {
                Some(*value as i64)
            }
            _ => None,
        }
    }
}

/// The document-side adapter used by [`load_acroform`].
///
/// Implementations expose the document catalog and resolve indirect
/// references one step at a time. Direct values must be returned unchanged.
/// Returning `None` for an unresolved reference produces
/// [`LoadError::UnresolvedReference`] for structural entries and a documented
/// fallback for attribute values.
pub trait FormSource {
    /// Returns the document catalog dictionary as a direct value.
    ///
    /// Entries may be indirect references; the loader resolves them through
    /// [`FormSource::resolve`].
    fn catalog(&self) -> PdfValue;

    /// Resolves `value` by following a single indirect reference.
    ///
    /// For a [`PdfValue::Reference`], returns the referenced object (which may
    /// itself be another reference). For every other variant, returns a clone
    /// of the value. Returns `None` only when a reference cannot be resolved.
    fn resolve(&self, value: &PdfValue) -> Option<PdfValue>;
}

/// An error produced while loading an `/AcroForm` dictionary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoadError {
    /// A structural indirect reference could not be resolved.
    UnresolvedReference(ObjectRef),
    /// A reference cycle was detected while walking the field tree.
    Cycle {
        /// The reference that closed the cycle.
        id: ObjectRef,
        /// The chain of references from the root to the repeated reference.
        path: Vec<ObjectRef>,
    },
    /// The field tree was nested deeper than the fixed limit of 128 nodes.
    NestingTooDeep(usize),
    /// Two terminal fields resolved to the same fully qualified name.
    DuplicateFieldName(String),
    /// A structural value had the wrong shape.
    Malformed {
        /// The PDF entry being read, for example `"AcroForm /Fields"`.
        context: &'static str,
        /// A human-readable description of the problem.
        detail: String,
    },
}

impl fmt::Display for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnresolvedReference(id) => write!(f, "unresolved PDF reference {id}"),
            Self::Cycle { id, path } => {
                let chain = path
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(" -> ");
                write!(f, "reference cycle in /Kids at {id} (path: {chain})")
            }
            Self::NestingTooDeep(depth) => {
                write!(
                    f,
                    "field tree nesting limit {MAX_NESTING} exceeded at {depth}"
                )
            }
            Self::DuplicateFieldName(name) => {
                write!(f, "duplicate fully qualified field name '{name}'")
            }
            Self::Malformed { context, detail } => {
                write!(f, "malformed {context}: {detail}")
            }
        }
    }
}

impl std::error::Error for LoadError {}

/// Resolved, non-value metadata for one loaded terminal field.
///
/// The [`Field`] model stores names, types, flags, values and constraints but
/// has no slots for tooltips, quadding or the default appearance string. This
/// record keeps those inherited attributes observable to callers.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LoadedFieldInfo {
    /// Fully qualified name of the field, identical to [`Field::name`].
    pub name: String,
    /// The field's own `/T` partial name, when present.
    pub partial_name: Option<String>,
    /// The resolved `/TU` tooltip (user name).
    pub tooltip: Option<String>,
    /// The resolved `/Q` quadding code, when present and in range.
    pub quadding: Option<i64>,
    /// The resolved `/DA` default appearance string.
    pub default_appearance: Option<String>,
}

/// A form loaded from an `/AcroForm` dictionary.
#[derive(Debug, Clone, PartialEq)]
pub struct LoadedForm {
    /// The populated form model, ready for [`crate::FormFill`].
    pub form: Form,
    /// Resolved metadata for each terminal field, in the same order as
    /// [`Form::fields`].
    pub fields: Vec<LoadedFieldInfo>,
}

/// The result of attempting to load an `/AcroForm`.
#[derive(Debug, Clone, PartialEq)]
pub enum AcroFormLoad {
    /// The document catalog has no `/AcroForm` entry, or it is `null`.
    NoForm,
    /// A form was loaded successfully.
    Loaded(LoadedForm),
}

impl AcroFormLoad {
    /// Returns the loaded form, or `None` when the document has no form.
    pub fn form(&self) -> Option<&Form> {
        match self {
            Self::NoForm => None,
            Self::Loaded(loaded) => Some(&loaded.form),
        }
    }

    /// Consumes the result and returns the loaded form, if any.
    pub fn into_form(self) -> Option<Form> {
        match self {
            Self::NoForm => None,
            Self::Loaded(loaded) => Some(loaded.form),
        }
    }

    /// Consumes the result and returns the resolved field metadata, if any.
    pub fn into_info(self) -> Vec<LoadedFieldInfo> {
        match self {
            Self::NoForm => Vec::new(),
            Self::Loaded(loaded) => loaded.fields,
        }
    }

    /// Returns `true` when the document catalog has no form dictionary.
    pub fn is_no_form(&self) -> bool {
        matches!(self, Self::NoForm)
    }
}

/// Loads the catalog's `/AcroForm` into the forms model.
///
/// See the module-level documentation for inheritance, naming and fallback
/// rules. A missing form is reported as [`AcroFormLoad::NoForm`], not as an
/// error.
///
/// # Errors
///
/// Returns [`LoadError`] when a structural reference cannot be resolved, when
/// `/Kids` contains a reference cycle, when the tree is nested too deeply, or
/// when two terminal fields share a fully qualified name.
pub fn load_acroform<S>(source: &S) -> Result<AcroFormLoad, LoadError>
where
    S: FormSource + ?Sized,
{
    let catalog = source.catalog();
    let catalog_dict = catalog.as_dict().ok_or_else(|| LoadError::Malformed {
        context: "catalog",
        detail: "catalog is not a dictionary".to_owned(),
    })?;

    let Some(acroform_ref) = catalog_dict.get("AcroForm") else {
        return Ok(AcroFormLoad::NoForm);
    };
    if matches!(acroform_ref, PdfValue::Null) {
        return Ok(AcroFormLoad::NoForm);
    }

    let acroform = resolve_value(source, acroform_ref, &[])?;
    let acroform_dict = acroform.as_dict().ok_or_else(|| LoadError::Malformed {
        context: "AcroForm",
        detail: "/AcroForm is not a dictionary".to_owned(),
    })?;

    let inherited = Inherited::from_acroform(acroform_dict);
    if let Some(fields_ref) = acroform_dict.get("Fields") {
        let fields = resolve_value(source, fields_ref, &[])?;
        let entries = fields.as_array().ok_or_else(|| LoadError::Malformed {
            context: "AcroForm /Fields",
            detail: "/Fields is not an array".to_owned(),
        })?;
        let mut loader = Loader {
            source,
            fields: Vec::new(),
            infos: Vec::new(),
            unnamed: 0,
        };
        for entry in entries {
            loader.walk(entry, &inherited, 0, &[])?;
        }
        let form = Form::from_fields(loader.fields).map_err(map_form_error)?;
        Ok(AcroFormLoad::Loaded(LoadedForm {
            form,
            fields: loader.infos,
        }))
    } else {
        Ok(AcroFormLoad::Loaded(LoadedForm {
            form: Form::new(),
            fields: Vec::new(),
        }))
    }
}

fn map_form_error(error: FormError) -> LoadError {
    match error {
        FormError::DuplicateFieldName(name) => LoadError::DuplicateFieldName(name),
        other => LoadError::Malformed {
            context: "AcroForm field",
            detail: other.to_string(),
        },
    }
}

/// Follows a chain of indirect references, rejecting loops against both the
/// ancestor path and the local chain.
fn resolve_value<S>(
    source: &S,
    value: &PdfValue,
    ancestors: &[ObjectRef],
) -> Result<PdfValue, LoadError>
where
    S: FormSource + ?Sized,
{
    let mut current = value.clone();
    let mut local: Vec<ObjectRef> = Vec::new();
    loop {
        match &current {
            PdfValue::Reference(id) => {
                if ancestors.contains(id) || local.contains(id) {
                    return Err(LoadError::Cycle {
                        id: *id,
                        path: ancestors.iter().chain(local.iter()).copied().collect(),
                    });
                }
                local.push(*id);
                current = source
                    .resolve(&current)
                    .ok_or(LoadError::UnresolvedReference(*id))?;
            }
            _ => return Ok(current),
        }
    }
}

/// Resolves an attribute, treating failures as "absent" per the fallback
/// policy documented on the module.
fn resolve_attr<S>(source: &S, value: &PdfValue, ancestors: &[ObjectRef]) -> Option<PdfValue>
where
    S: FormSource + ?Sized,
{
    resolve_value(source, value, ancestors).ok()
}

/// The inheritable field attributes collected while descending the tree.
#[derive(Debug, Clone, Default)]
struct Inherited {
    field_type: Option<String>,
    flags: Option<u32>,
    value: Option<PdfValue>,
    default_value: Option<PdfValue>,
    tooltip: Option<String>,
    options: Option<Vec<String>>,
    max_len: Option<usize>,
    quadding: Option<i64>,
    appearance: Option<String>,
    names: Vec<String>,
}

impl Inherited {
    /// Seeds document-level defaults from the `/AcroForm` dictionary.
    fn from_acroform(dict: &BTreeMap<String, PdfValue>) -> Self {
        Self {
            quadding: dict
                .get("Q")
                .and_then(PdfValue::as_i64)
                .filter(|value| (0..=2).contains(value)),
            appearance: dict
                .get("DA")
                .and_then(PdfValue::as_text)
                .map(ToOwned::to_owned),
            ..Self::default()
        }
    }

    /// Merges this node's own attributes over the inherited ones.
    fn merge<S>(
        &self,
        node: &BTreeMap<String, PdfValue>,
        source: &S,
        ancestors: &[ObjectRef],
    ) -> Self
    where
        S: FormSource + ?Sized,
    {
        let get = |key: &str| -> Option<PdfValue> {
            node.get(key)
                .and_then(|value| resolve_attr(source, value, ancestors))
        };
        let mut names = self.names.clone();
        if let Some(partial) = get("T")
            .as_ref()
            .and_then(textish)
            .filter(|name| !name.is_empty())
        {
            names.push(partial);
        }
        Self {
            field_type: get("FT")
                .as_ref()
                .and_then(PdfValue::as_name)
                .map(decode_pdf_name)
                .or_else(|| self.field_type.clone()),
            flags: get("Ff")
                .as_ref()
                .and_then(PdfValue::as_i64)
                .and_then(|value| u32::try_from(value).ok())
                .or(self.flags),
            value: get("V").or_else(|| self.value.clone()),
            default_value: get("DV").or_else(|| self.default_value.clone()),
            tooltip: get("TU")
                .as_ref()
                .and_then(PdfValue::as_text)
                .map(ToOwned::to_owned)
                .or_else(|| self.tooltip.clone()),
            options: get("Opt")
                .as_ref()
                .and_then(choices_from_opt)
                .or_else(|| self.options.clone()),
            max_len: get("MaxLen")
                .as_ref()
                .and_then(PdfValue::as_i64)
                .and_then(|value| usize::try_from(value).ok())
                .or(self.max_len),
            quadding: get("Q")
                .as_ref()
                .and_then(PdfValue::as_i64)
                .filter(|value| (0..=2).contains(value))
                .or(self.quadding),
            appearance: get("DA")
                .as_ref()
                .and_then(PdfValue::as_text)
                .map(ToOwned::to_owned)
                .or_else(|| self.appearance.clone()),
            names,
        }
    }
}

/// Whether a field-tree node is a terminal field or a branch of sub-fields.
enum NodeKind {
    /// A field to materialise; the list holds extra radio option names.
    Terminal(Vec<String>),
    /// A non-terminal node whose kids are sub-fields.
    Branch,
}

struct Loader<'a, S: ?Sized> {
    source: &'a S,
    fields: Vec<Field>,
    infos: Vec<LoadedFieldInfo>,
    unnamed: usize,
}

impl<'a, S> Loader<'a, S>
where
    S: FormSource + ?Sized,
{
    fn walk(
        &mut self,
        value: &PdfValue,
        inherited: &Inherited,
        depth: usize,
        ancestors: &[ObjectRef],
    ) -> Result<(), LoadError> {
        if depth > MAX_NESTING {
            return Err(LoadError::NestingTooDeep(depth));
        }
        let node_value = resolve_value(self.source, value, ancestors)?;
        let Some(node) = node_value.as_dict() else {
            // A non-dictionary /Fields or /Kids entry is ignored rather than
            // aborting the whole load (hostile-input fallback).
            return Ok(());
        };

        let mut here: Vec<ObjectRef> = ancestors.to_vec();
        if let Some(id) = value.as_reference() {
            if here.contains(&id) {
                return Err(LoadError::Cycle {
                    id,
                    path: here.clone(),
                });
            }
            here.push(id);
        }
        let effective = inherited.merge(node, self.source, &here);

        let kids = node
            .get("Kids")
            .and_then(|kids_ref| resolve_attr(self.source, kids_ref, &here))
            .and_then(|kids| kids.as_array().map(ToOwned::to_owned));

        let kind = self.classify(node, kids.as_deref(), &here)?;
        match kind {
            NodeKind::Branch => {
                if let Some(kids) = kids {
                    for kid in &kids {
                        // Widget annotations that are not merged with a field
                        // are not sub-fields and must not become entries.
                        let resolved = resolve_attr(self.source, kid, &here);
                        if is_widget(resolved.as_ref()) {
                            continue;
                        }
                        // `walk` pushes the kid's own reference onto the path,
                        // so pass only the ancestor chain here.
                        self.walk(kid, &effective, depth + 1, &here)?;
                    }
                }
            }
            NodeKind::Terminal(radio_states) => {
                let field = build_field(&effective, &radio_states, &mut self.unnamed);
                self.infos.push(LoadedFieldInfo {
                    name: field.name().to_owned(),
                    partial_name: effective.names.last().cloned(),
                    tooltip: effective.tooltip.clone(),
                    quadding: effective.quadding,
                    default_appearance: effective.appearance.clone(),
                });
                self.fields.push(field);
            }
        }
        Ok(())
    }

    fn classify(
        &self,
        node: &BTreeMap<String, PdfValue>,
        kids: Option<&[PdfValue]>,
        ancestors: &[ObjectRef],
    ) -> Result<NodeKind, LoadError> {
        let Some(kids) = kids.filter(|kids| !kids.is_empty()) else {
            return Ok(NodeKind::Terminal(radio_options_from_ap(
                self.source,
                node,
                ancestors,
            )));
        };
        let mut all_widgets = true;
        for kid in kids {
            let resolved = resolve_value(self.source, kid, ancestors)?;
            if !is_widget(Some(&resolved)) {
                all_widgets = false;
                break;
            }
        }
        let parent_declares_field = node.contains_key("FT")
            || node.contains_key("T")
            || node.contains_key("V")
            || node.contains_key("DV");
        if all_widgets && parent_declares_field {
            Ok(NodeKind::Terminal(radio_options_from_ap(
                self.source,
                node,
                ancestors,
            )))
        } else {
            Ok(NodeKind::Branch)
        }
    }
}

fn is_widget(value: Option<&PdfValue>) -> bool {
    value
        .and_then(PdfValue::as_dict)
        .and_then(|dict| dict.get("Subtype"))
        .and_then(PdfValue::as_name)
        .map(decode_pdf_name)
        .as_deref()
        == Some("Widget")
}

/// Extracts normal-state names from a node's `/AP /N` dictionary for radio
/// groups. State names are sorted because the source is a `BTreeMap`.
fn radio_options_from_ap<S>(
    source: &S,
    node: &BTreeMap<String, PdfValue>,
    ancestors: &[ObjectRef],
) -> Vec<String>
where
    S: FormSource + ?Sized,
{
    let Some(ap) = node
        .get("AP")
        .and_then(|v| resolve_attr(source, v, ancestors))
    else {
        return Vec::new();
    };
    let Some(normal) = ap
        .as_dict()
        .and_then(|d| d.get("N"))
        .and_then(|v| resolve_attr(source, v, ancestors))
    else {
        return Vec::new();
    };
    let Some(states) = normal.as_dict() else {
        return Vec::new();
    };
    states
        .keys()
        .map(|name| decode_pdf_name(name))
        .filter(|name| name != "Off")
        .collect()
}

/// Classifies a terminal field using the inherited `/FT` name and button
/// flags. Missing or unrecognised types become [`FieldType::Unknown`].
fn classify_field_type(field_type: Option<&str>, flags: u32) -> FieldType {
    match field_type {
        Some("Tx") => FieldType::Text,
        Some("Ch") => FieldType::Choice,
        Some("Sig") => FieldType::Signature,
        Some("Btn") => {
            if flags & BTN_PUSHBUTTON != 0 {
                FieldType::Button
            } else if flags & BTN_RADIO != 0 {
                FieldType::Radio
            } else {
                FieldType::CheckBox
            }
        }
        _ => FieldType::Unknown,
    }
}

/// Decodes the `#xx` hexadecimal escapes used by PDF names.
fn decode_pdf_name(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'#'
            && index + 2 < bytes.len()
            && let Some(high) = hex_digit(bytes[index + 1])
            && let Some(low) = hex_digit(bytes[index + 2])
        {
            decoded.push((high << 4) | low);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8_lossy(&decoded).into_owned()
}

fn hex_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn textish(value: &PdfValue) -> Option<String> {
    match value {
        PdfValue::Text(text) => Some(text.clone()),
        PdfValue::Name(name) => Some(decode_pdf_name(name)),
        _ => None,
    }
}

fn choices_from_opt(value: &PdfValue) -> Option<Vec<String>> {
    let items = value.as_array()?;
    let mut choices = Vec::with_capacity(items.len());
    for item in items {
        match item {
            // An /Opt entry may be a two-element [export, display] array.
            PdfValue::Array(pair) => choices.push(pair.iter().find_map(textish)?),
            other => choices.push(textish(other)?),
        }
    }
    Some(choices)
}

fn build_field(inherited: &Inherited, radio_states: &[String], unnamed: &mut usize) -> Field {
    let field_type = classify_field_type(
        inherited.field_type.as_deref(),
        inherited.flags.unwrap_or(0),
    );
    let flags = FieldFlags::from_bits(inherited.flags.unwrap_or(0));
    let joined = inherited.names.join(".");
    let name = if joined.is_empty() {
        *unnamed += 1;
        format!("<unnamed-{}>", *unnamed)
    } else {
        joined
    };

    let mut choices = inherited.options.clone().unwrap_or_default();
    if field_type == FieldType::Radio && choices.is_empty() {
        choices = radio_states.to_vec();
    }

    let mut field = Field::new(name, field_type)
        .with_flags(flags)
        .with_choices(choices);
    if field_type == FieldType::Text
        && let Some(max_len) = inherited.max_len
    {
        field = field.with_constraint(TextConstraint::new().with_max_len(max_len));
    }

    let default = inherited
        .default_value
        .as_ref()
        .and_then(|value| coerce_value(value, field_type));
    let current = inherited
        .value
        .as_ref()
        .and_then(|value| coerce_value(value, field_type))
        .or_else(|| default.clone());
    if let Some(default) = default {
        field = field.with_default(default);
    }
    if let Some(current) = current {
        field = field.with_value(current);
    }
    field
}

/// Maps a PDF value onto the [`FieldValue`] shape of a field type.
///
/// Returns `None` when the PDF type has no meaningful representation for the
/// field; `build_field` turns that into the documented [`FieldValue::Empty`]
/// fallback.
fn coerce_value(value: &PdfValue, field_type: FieldType) -> Option<FieldValue> {
    match field_type {
        FieldType::Button => Some(FieldValue::Empty),
        FieldType::CheckBox => match value {
            PdfValue::Bool(flag) => Some(FieldValue::Bool(*flag)),
            PdfValue::Number(number) => Some(FieldValue::Bool(*number != 0.0)),
            PdfValue::Text(text) | PdfValue::Name(text) => match text.as_str() {
                "On" | "Yes" | "True" | "true" => Some(FieldValue::Bool(true)),
                "Off" | "No" | "False" | "false" => Some(FieldValue::Bool(false)),
                _ => None,
            },
            _ => None,
        },
        FieldType::Text => match value {
            PdfValue::Text(text) | PdfValue::Name(text) => Some(FieldValue::Text(text.clone())),
            PdfValue::Number(number) => Some(FieldValue::Number(*number)),
            PdfValue::Bool(flag) => Some(FieldValue::Text(flag.to_string())),
            _ => None,
        },
        FieldType::Choice | FieldType::Radio => match value {
            PdfValue::Array(items) => {
                let mut selected = Vec::with_capacity(items.len());
                for item in items {
                    selected.push(textish(item)?);
                }
                Some(FieldValue::Choice(selected))
            }
            other => textish(other).map(|text| FieldValue::Choice(vec![text])),
        },
        FieldType::Signature => textish(value).map(FieldValue::Text),
        FieldType::Unknown => match value {
            PdfValue::Bool(flag) => Some(FieldValue::Bool(*flag)),
            PdfValue::Number(number) => Some(FieldValue::Number(*number)),
            PdfValue::Array(items) => {
                let mut selected = Vec::with_capacity(items.len());
                for item in items {
                    selected.push(textish(item)?);
                }
                Some(FieldValue::Choice(selected))
            }
            other => textish(other).map(FieldValue::Text),
        },
    }
}

#[cfg(test)]
mod tests;
