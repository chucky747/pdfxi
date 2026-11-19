//! Typed field creation and emission of AcroForm dictionaries and widget
//! annotations.
//!
//! [`FieldSpec`] is a builder for a single field of any supported
//! [`FieldType`]. It carries the typed properties a form author needs — fully
//! qualified name, tooltip, current and default values, flags, choice options,
//! maximum text length, alignment, border colour/style and a widget rectangle
//! with its owning page — and converts them into both the in-memory [`Field`]
//! model ([`FieldSpec::to_field`]) and the PDF object model
//! ([`FieldSpec::field_dictionary`], [`FieldSpec::widget_annotation`]).
//!
//! [`acroform_dictionary`] assembles a complete `/AcroForm` dictionary from a
//! slice of specs. The emitted values use [`crate::acroform::PdfValue`], the
//! crate's dependency-free object-model boundary, so a host with a concrete
//! writer can serialise them without this crate depending on `pdfxi-core`.
//!
//! All emission is deterministic: dictionary keys are stored in a `BTreeMap`
//! and arrays keep the order of the supplied specs. Geometry and colour are
//! validated before emission; nothing here panics on hostile input.

use crate::acroform::{ObjectRef, PdfValue};
use crate::appearance::{Alignment, BorderStyle, RgbColor, WidgetRect};
use crate::field::{Field, FieldFlags, FieldType, FieldValue, TextConstraint};
use std::collections::BTreeMap;
use std::fmt;

/// Maximum number of choice/radio options accepted by [`FieldSpec`].
pub const MAX_OPTIONS: usize = 4096;

/// The PDF `/Ff` bit that marks a button field as a radio group.
const BTN_RADIO: u32 = 1 << 15;
/// The PDF `/Ff` bit that marks a button field as a pushbutton.
const BTN_PUSHBUTTON: u32 = 1 << 16;

/// An error produced while creating or emitting a field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CreationError {
    /// The field name was empty.
    EmptyName,
    /// The widget rectangle contained a non-finite coordinate.
    NonFiniteRect,
    /// The widget rectangle had a zero or negative extent.
    ZeroSizedRect,
    /// The border width was non-finite or negative.
    InvalidBorderWidth,
    /// A colour had a component outside `0.0..=1.0` or non-finite.
    InvalidColor(&'static str),
    /// Too many choice options were supplied.
    TooManyOptions {
        /// The number of options supplied.
        count: usize,
        /// The maximum number accepted.
        max: usize,
    },
    /// A numeric current or default value was not finite.
    NonFiniteValue(&'static str),
}

impl fmt::Display for CreationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyName => write!(f, "field name must not be empty"),
            Self::NonFiniteRect => write!(f, "widget rectangle has a non-finite coordinate"),
            Self::ZeroSizedRect => write!(f, "widget rectangle has a zero or negative extent"),
            Self::InvalidBorderWidth => write!(f, "border width must be finite and non-negative"),
            Self::InvalidColor(what) => write!(f, "{what} colour component is out of range"),
            Self::TooManyOptions { count, max } => {
                write!(f, "{count} options exceed the maximum of {max}")
            }
            Self::NonFiniteValue(what) => write!(f, "{what} value is not finite"),
        }
    }
}

impl std::error::Error for CreationError {}

/// A typed description of one AcroForm field and its widget.
///
/// Use the type-specific constructors ([`FieldSpec::text`],
/// [`FieldSpec::checkbox`], [`FieldSpec::radio_group`], [`FieldSpec::choice`],
/// [`FieldSpec::push_button`], [`FieldSpec::signature`]) and then the `with_*`
/// builder methods to set optional properties.
#[derive(Debug, Clone, PartialEq)]
pub struct FieldSpec {
    name: String,
    field_type: FieldType,
    flags: FieldFlags,
    value: FieldValue,
    default_value: FieldValue,
    choices: Vec<String>,
    max_len: Option<usize>,
    tooltip: Option<String>,
    alignment: Alignment,
    rect: WidgetRect,
    border_style: BorderStyle,
    border_width: f32,
    border_color: RgbColor,
    background: RgbColor,
    page: u32,
}

impl FieldSpec {
    /// Creates a text field on `page` at `rect`.
    pub fn text(name: impl Into<String>, rect: WidgetRect, page: u32) -> Self {
        Self::base(name, FieldType::Text, rect, page)
    }

    /// Creates a checkbox field on `page` at `rect`.
    pub fn checkbox(name: impl Into<String>, rect: WidgetRect, page: u32) -> Self {
        Self::base(name, FieldType::CheckBox, rect, page)
    }

    /// Creates a radio group on `page` at `rect` with the given option names.
    ///
    /// The PDF radio bit is set automatically in the emitted `/Ff` flags.
    pub fn radio_group<I, S>(
        name: impl Into<String>,
        rect: WidgetRect,
        page: u32,
        options: I,
    ) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let mut spec = Self::base(name, FieldType::Radio, rect, page);
        spec.flags = spec.flags.union(FieldFlags::from_bits(BTN_RADIO));
        spec.choices = options.into_iter().map(Into::into).collect();
        spec
    }

    /// Creates a choice field on `page` at `rect`.
    ///
    /// Set `combo` to `true` for a combo box; otherwise a list box is emitted.
    pub fn choice<I, S>(
        name: impl Into<String>,
        rect: WidgetRect,
        page: u32,
        options: I,
        combo: bool,
    ) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let mut spec = Self::base(name, FieldType::Choice, rect, page);
        if combo {
            spec.flags = spec.flags.union(FieldFlags::COMBO);
        }
        spec.choices = options.into_iter().map(Into::into).collect();
        spec
    }

    /// Creates a pushbutton on `page` at `rect`.
    ///
    /// Pushbuttons carry no value; the PDF pushbutton flag is set
    /// automatically.
    pub fn push_button(name: impl Into<String>, rect: WidgetRect, page: u32) -> Self {
        let mut spec = Self::base(name, FieldType::Button, rect, page);
        spec.flags = spec.flags.union(FieldFlags::from_bits(BTN_PUSHBUTTON));
        spec
    }

    /// Creates a signature field on `page` at `rect`.
    pub fn signature(name: impl Into<String>, rect: WidgetRect, page: u32) -> Self {
        Self::base(name, FieldType::Signature, rect, page)
    }

    fn base(name: impl Into<String>, field_type: FieldType, rect: WidgetRect, page: u32) -> Self {
        Self {
            name: name.into(),
            field_type,
            flags: FieldFlags::NONE,
            value: FieldValue::Empty,
            default_value: FieldValue::Empty,
            choices: Vec::new(),
            max_len: None,
            tooltip: None,
            alignment: Alignment::Left,
            rect,
            border_style: BorderStyle::Solid,
            border_width: 1.0,
            border_color: RgbColor::BLACK,
            background: RgbColor::WHITE,
            page,
        }
    }

    /// Sets the field flags, replacing any previously configured flags.
    #[must_use]
    pub fn with_flags(mut self, flags: FieldFlags) -> Self {
        self.flags = flags;
        self
    }

    /// Adds flags to the currently configured set.
    #[must_use]
    pub fn with_added_flags(mut self, flags: FieldFlags) -> Self {
        self.flags = self.flags.union(flags);
        self
    }

    /// Sets the current value.
    #[must_use]
    pub fn with_value(mut self, value: FieldValue) -> Self {
        self.value = value;
        self
    }

    /// Sets the default value.
    #[must_use]
    pub fn with_default(mut self, value: FieldValue) -> Self {
        self.default_value = value;
        self
    }

    /// Sets or replaces the choice/radio options.
    #[must_use]
    pub fn with_options<I, S>(mut self, options: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.choices = options.into_iter().map(Into::into).collect();
        self
    }

    /// Sets the maximum text length (emitted as PDF `/MaxLen`).
    #[must_use]
    pub const fn with_max_len(mut self, max_len: usize) -> Self {
        self.max_len = Some(max_len);
        self
    }

    /// Sets the `/TU` tooltip (user name).
    #[must_use]
    pub fn with_tooltip(mut self, tooltip: impl Into<String>) -> Self {
        self.tooltip = Some(tooltip.into());
        self
    }

    /// Sets the text alignment (emitted as PDF `/Q`).
    #[must_use]
    pub const fn with_alignment(mut self, alignment: Alignment) -> Self {
        self.alignment = alignment;
        self
    }

    /// Sets the border style.
    #[must_use]
    pub const fn with_border_style(mut self, style: BorderStyle) -> Self {
        self.border_style = style;
        self
    }

    /// Sets the border width in points.
    #[must_use]
    pub const fn with_border_width(mut self, width: f32) -> Self {
        self.border_width = width;
        self
    }

    /// Sets the border colour.
    #[must_use]
    pub const fn with_border_color(mut self, color: RgbColor) -> Self {
        self.border_color = color;
        self
    }

    /// Sets the widget background colour.
    #[must_use]
    pub const fn with_background(mut self, color: RgbColor) -> Self {
        self.background = color;
        self
    }

    /// Returns the fully qualified field name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the field type.
    pub const fn field_type(&self) -> FieldType {
        self.field_type
    }

    /// Returns the configured flags, including the automatic radio/pushbutton bits.
    pub const fn flags(&self) -> FieldFlags {
        self.flags
    }

    /// Returns the current value.
    pub fn value(&self) -> &FieldValue {
        &self.value
    }

    /// Returns the default value.
    pub fn default_value(&self) -> &FieldValue {
        &self.default_value
    }

    /// Returns the choice/radio options.
    pub fn options(&self) -> &[String] {
        &self.choices
    }

    /// Returns the maximum text length, if set.
    pub const fn max_len(&self) -> Option<usize> {
        self.max_len
    }

    /// Returns the tooltip, if set.
    pub fn tooltip(&self) -> Option<&str> {
        self.tooltip.as_deref()
    }

    /// Returns the alignment.
    pub const fn alignment(&self) -> Alignment {
        self.alignment
    }

    /// Returns the widget rectangle.
    pub const fn rect(&self) -> WidgetRect {
        self.rect
    }

    /// Returns the zero-based index of the page owning the widget.
    pub const fn page(&self) -> u32 {
        self.page
    }

    /// Validates the typed properties before they are used for emission.
    ///
    /// # Errors
    ///
    /// Returns [`CreationError`] for an empty name, invalid geometry or colour,
    /// too many options, or a non-finite numeric value.
    pub fn validate(&self) -> Result<(), CreationError> {
        if self.name.is_empty() {
            return Err(CreationError::EmptyName);
        }
        if !self.rect.is_finite() {
            return Err(CreationError::NonFiniteRect);
        }
        if self.rect.is_zero_sized() {
            return Err(CreationError::ZeroSizedRect);
        }
        if !self.border_width.is_finite() || self.border_width < 0.0 {
            return Err(CreationError::InvalidBorderWidth);
        }
        if !self.border_color.is_valid() {
            return Err(CreationError::InvalidColor("border"));
        }
        if !self.background.is_valid() {
            return Err(CreationError::InvalidColor("background"));
        }
        if self.choices.len() > MAX_OPTIONS {
            return Err(CreationError::TooManyOptions {
                count: self.choices.len(),
                max: MAX_OPTIONS,
            });
        }
        if let FieldValue::Number(number) = self.value
            && !number.is_finite()
        {
            return Err(CreationError::NonFiniteValue("current"));
        }
        if let FieldValue::Number(number) = self.default_value
            && !number.is_finite()
        {
            return Err(CreationError::NonFiniteValue("default"));
        }
        Ok(())
    }

    /// Builds the in-memory [`Field`] described by this spec.
    ///
    /// # Errors
    ///
    /// Returns [`CreationError`] when [`FieldSpec::validate`] fails.
    pub fn to_field(&self) -> Result<Field, CreationError> {
        self.validate()?;
        let mut field = Field::new(self.name.clone(), self.field_type)
            .with_flags(self.flags)
            .with_choices(self.choices.iter().cloned());
        // A PDF field with no explicit /V presents its /DV, so only override
        // the default when a non-empty current value was supplied.
        if !self.default_value.is_empty() {
            field = field.with_default(self.default_value.clone());
        }
        if !self.value.is_empty() {
            field = field.with_value(self.value.clone());
        }
        if let Some(max_len) = self.max_len {
            field = field.with_constraint(TextConstraint::new().with_max_len(max_len));
        }
        Ok(field)
    }

    /// Emits the field-node dictionary for this field.
    ///
    /// The dictionary contains `/T`, `/FT`, `/Ff`, `/V`, `/DV` (when non-empty),
    /// `/TU` (when set), `/Opt`, `/MaxLen`, `/Q`, and a `/Kids` array holding
    /// the single widget annotation from [`FieldSpec::widget_annotation`].
    ///
    /// # Errors
    ///
    /// Returns [`CreationError`] when [`FieldSpec::validate`] fails.
    pub fn field_dictionary(&self) -> Result<PdfValue, CreationError> {
        self.validate()?;
        let mut dict = BTreeMap::new();
        dict.insert("Type".to_owned(), PdfValue::Name("Annot".to_owned()));
        dict.insert("T".to_owned(), PdfValue::Text(self.name.clone()));
        dict.insert(
            "FT".to_owned(),
            PdfValue::Name(field_type_pdf_name(self.field_type).to_owned()),
        );
        dict.insert(
            "Ff".to_owned(),
            PdfValue::Number(f64::from(self.flags.bits())),
        );
        dict.insert("V".to_owned(), value_to_pdf(&self.value));
        if !self.default_value.is_empty() {
            dict.insert("DV".to_owned(), value_to_pdf(&self.default_value));
        }
        if let Some(tooltip) = &self.tooltip {
            dict.insert("TU".to_owned(), PdfValue::Text(tooltip.clone()));
        }
        if !self.choices.is_empty() {
            dict.insert(
                "Opt".to_owned(),
                PdfValue::Array(self.choices.iter().cloned().map(PdfValue::Text).collect()),
            );
        }
        if let Some(max_len) = self.max_len {
            dict.insert("MaxLen".to_owned(), PdfValue::Number(max_len as f64));
        }
        dict.insert(
            "Q".to_owned(),
            PdfValue::Number(self.alignment.quadding() as f64),
        );
        dict.insert(
            "Kids".to_owned(),
            PdfValue::Array(vec![self.widget_annotation_unchecked()]),
        );
        Ok(PdfValue::Dictionary(dict))
    }

    /// Emits the widget annotation dictionary for this field.
    ///
    /// Contains `/Type /Annot`, `/Subtype /Widget`, `/Rect`, `/F 4`, `/BS`,
    /// `/MK`, and `/P` (an indirect reference whose object number is the
    /// one-based page index derived from [`FieldSpec::page`]).
    ///
    /// # Errors
    ///
    /// Returns [`CreationError`] when [`FieldSpec::validate`] fails.
    pub fn widget_annotation(&self) -> Result<PdfValue, CreationError> {
        self.validate()?;
        Ok(self.widget_annotation_unchecked())
    }

    fn widget_annotation_unchecked(&self) -> PdfValue {
        let mut dict = BTreeMap::new();
        dict.insert("Type".to_owned(), PdfValue::Name("Annot".to_owned()));
        dict.insert("Subtype".to_owned(), PdfValue::Name("Widget".to_owned()));
        dict.insert(
            "Rect".to_owned(),
            PdfValue::Array(vec![
                PdfValue::Number(f64::from(self.rect.x)),
                PdfValue::Number(f64::from(self.rect.y)),
                PdfValue::Number(f64::from(self.rect.right())),
                PdfValue::Number(f64::from(self.rect.top())),
            ]),
        );
        dict.insert("F".to_owned(), PdfValue::Number(4.0));
        let mut bs = BTreeMap::new();
        bs.insert(
            "W".to_owned(),
            PdfValue::Number(f64::from(self.border_width)),
        );
        bs.insert(
            "S".to_owned(),
            PdfValue::Name(self.border_style.pdf_name().to_owned()),
        );
        dict.insert("BS".to_owned(), PdfValue::Dictionary(bs));
        let mut mk = BTreeMap::new();
        mk.insert("BC".to_owned(), color_array(self.border_color));
        mk.insert("BG".to_owned(), color_array(self.background));
        dict.insert("MK".to_owned(), PdfValue::Dictionary(mk));
        // A zero-based page index maps to the one-based PDF page object number.
        dict.insert(
            "P".to_owned(),
            PdfValue::Reference(ObjectRef::new(self.page + 1, 0)),
        );
        if let Some(tooltip) = &self.tooltip {
            dict.insert("TU".to_owned(), PdfValue::Text(tooltip.clone()));
        }
        PdfValue::Dictionary(dict)
    }
}

/// Builds a complete `/AcroForm` dictionary from a slice of field specs.
///
/// The returned value is a [`PdfValue::Dictionary`] with `/Fields` (one entry
/// per spec, in the given order), `/NeedAppearances false` and a default
/// appearance string.
///
/// # Errors
///
/// Returns the first [`CreationError`] from any spec that fails validation.
pub fn acroform_dictionary(specs: &[FieldSpec]) -> Result<PdfValue, CreationError> {
    let mut fields = Vec::with_capacity(specs.len());
    for spec in specs {
        fields.push(spec.field_dictionary()?);
    }
    let mut dict = BTreeMap::new();
    dict.insert("Fields".to_owned(), PdfValue::Array(fields));
    dict.insert("NeedAppearances".to_owned(), PdfValue::Bool(false));
    dict.insert("DA".to_owned(), PdfValue::Text("Helv 0 Tf 0 g".to_owned()));
    Ok(PdfValue::Dictionary(dict))
}

/// Returns the PDF field-type name for a [`FieldType`].
fn field_type_pdf_name(field_type: FieldType) -> &'static str {
    match field_type {
        FieldType::Text => "Tx",
        FieldType::CheckBox | FieldType::Radio | FieldType::Button => "Btn",
        FieldType::Choice => "Ch",
        FieldType::Signature => "Sig",
        FieldType::Unknown => "Tx",
    }
}

/// Converts a [`FieldValue`] into the corresponding PDF object.
fn value_to_pdf(value: &FieldValue) -> PdfValue {
    match value {
        FieldValue::Empty => PdfValue::Null,
        FieldValue::Text(text) => PdfValue::Text(text.clone()),
        FieldValue::Bool(flag) => PdfValue::Name(if *flag { "On" } else { "Off" }.to_owned()),
        FieldValue::Number(number) => PdfValue::Number(*number),
        FieldValue::Choice(values) => match values.as_slice() {
            [single] => PdfValue::Text(single.clone()),
            many => PdfValue::Array(many.iter().cloned().map(PdfValue::Text).collect()),
        },
    }
}

/// Converts an [`RgbColor`] into a PDF colour array.
fn color_array(color: RgbColor) -> PdfValue {
    PdfValue::Array(vec![
        PdfValue::Number(f64::from(color.red)),
        PdfValue::Number(f64::from(color.green)),
        PdfValue::Number(f64::from(color.blue)),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::field::FieldType;

    fn rect() -> WidgetRect {
        WidgetRect::new(10.0, 20.0, 100.0, 24.0)
    }

    #[test]
    fn create_text_field_with_typed_properties() {
        let spec = FieldSpec::text("order.name", rect(), 1)
            .with_tooltip("Customer name")
            .with_max_len(40)
            .with_default(FieldValue::Text("Ada".into()))
            .with_alignment(Alignment::Center)
            .with_flags(FieldFlags::REQUIRED);
        let field = spec.to_field().unwrap();

        assert_eq!(field.name(), "order.name");
        assert_eq!(field.field_type(), FieldType::Text);
        assert_eq!(field.flags(), FieldFlags::REQUIRED);
        assert_eq!(field.constraint().max_len(), Some(40));
        assert_eq!(field.value(), &FieldValue::Text("Ada".into()));
        assert_eq!(spec.tooltip(), Some("Customer name"));

        let dict = spec.field_dictionary().unwrap();
        let entries = dict.as_dict().unwrap();
        assert_eq!(entries.get("FT"), Some(&PdfValue::Name("Tx".into())));
        assert_eq!(
            entries.get("TU"),
            Some(&PdfValue::Text("Customer name".into()))
        );
        assert_eq!(entries.get("MaxLen"), Some(&PdfValue::Number(40.0)));
        assert_eq!(entries.get("Q"), Some(&PdfValue::Number(1.0)));
    }

    #[test]
    fn create_checkbox_field() {
        let spec = FieldSpec::checkbox("agree", rect(), 0)
            .with_value(FieldValue::Bool(true))
            .with_default(FieldValue::Bool(false));
        let field = spec.to_field().unwrap();

        assert_eq!(field.field_type(), FieldType::CheckBox);
        assert_eq!(field.value(), &FieldValue::Bool(true));
        let entries = spec.field_dictionary().unwrap();
        let entries = entries.as_dict().unwrap();
        assert_eq!(entries.get("FT"), Some(&PdfValue::Name("Btn".into())));
        assert_eq!(entries.get("V"), Some(&PdfValue::Name("On".into())));
    }

    #[test]
    fn create_radio_group_with_options_and_flag() {
        let spec = FieldSpec::radio_group("size", rect(), 2, ["S", "M", "L"]);
        let field = spec.to_field().unwrap();

        assert_eq!(field.field_type(), FieldType::Radio);
        assert_eq!(field.choices(), ["S", "M", "L"]);
        assert!(spec.flags().contains(FieldFlags::from_bits(BTN_RADIO)));
        let entries = spec.field_dictionary().unwrap();
        let entries = entries.as_dict().unwrap();
        assert!(matches!(entries.get("Opt"), Some(PdfValue::Array(items)) if items.len() == 3));
    }

    #[test]
    fn create_choice_combo_and_list() {
        let combo = FieldSpec::choice("color", rect(), 0, ["red", "green"], true);
        let list = FieldSpec::choice("country", rect(), 0, ["US", "CA"], false);

        assert!(combo.flags().contains(FieldFlags::COMBO));
        assert!(!list.flags().contains(FieldFlags::COMBO));
        assert_eq!(combo.to_field().unwrap().field_type(), FieldType::Choice);
        assert_eq!(list.options(), ["US", "CA"]);
    }

    #[test]
    fn create_push_button_and_signature() {
        let button = FieldSpec::push_button("submit", rect(), 3);
        let signature = FieldSpec::signature("sig", rect(), 3);

        assert_eq!(button.to_field().unwrap().field_type(), FieldType::Button);
        assert!(
            button
                .flags()
                .contains(FieldFlags::from_bits(BTN_PUSHBUTTON))
        );
        assert_eq!(
            signature.to_field().unwrap().field_type(),
            FieldType::Signature
        );
        let entries = signature.field_dictionary().unwrap();
        let entries = entries.as_dict().unwrap();
        assert_eq!(entries.get("FT"), Some(&PdfValue::Name("Sig".into())));
    }

    #[test]
    fn widget_rect_and_page_association_are_emitted() {
        let spec = FieldSpec::text("field", WidgetRect::new(5.0, 7.0, 30.0, 12.0), 4)
            .with_border_style(BorderStyle::Dashed)
            .with_border_width(2.0)
            .with_border_color(RgbColor::new(0.1, 0.2, 0.3));
        let widget = spec.widget_annotation().unwrap();
        let entries = widget.as_dict().unwrap();

        assert_eq!(
            entries.get("Rect"),
            Some(&PdfValue::Array(vec![
                PdfValue::Number(5.0),
                PdfValue::Number(7.0),
                PdfValue::Number(35.0),
                PdfValue::Number(19.0),
            ]))
        );
        assert_eq!(
            entries.get("P"),
            Some(&PdfValue::Reference(ObjectRef::new(5, 0)))
        );
        assert!(
            matches!(entries.get("BS"), Some(PdfValue::Dictionary(bs)) if bs.get("S") == Some(&PdfValue::Name("D".into())))
        );
        assert!(
            matches!(entries.get("MK"), Some(PdfValue::Dictionary(mk)) if mk.contains_key("BC"))
        );
    }

    #[test]
    fn acroform_dictionary_lists_fields_in_order() {
        let specs = [
            FieldSpec::text("a", rect(), 0),
            FieldSpec::checkbox("b", rect(), 0),
        ];
        let form = acroform_dictionary(&specs).unwrap();
        let entries = form.as_dict().unwrap();
        let fields = entries.get("Fields").and_then(PdfValue::as_array).unwrap();

        assert_eq!(fields.len(), 2);
        assert_eq!(entries.get("NeedAppearances"), Some(&PdfValue::Bool(false)));
        let first = fields[0].as_dict().unwrap();
        assert_eq!(first.get("T"), Some(&PdfValue::Text("a".into())));
    }

    #[test]
    fn invalid_geometry_and_values_are_rejected() {
        let empty = FieldSpec::text("", rect(), 0);
        assert_eq!(empty.to_field(), Err(CreationError::EmptyName));

        let zero = FieldSpec::text("f", WidgetRect::new(0.0, 0.0, 0.0, 10.0), 0);
        assert_eq!(zero.to_field(), Err(CreationError::ZeroSizedRect));

        let nan = FieldSpec::text("f", WidgetRect::new(f32::NAN, 0.0, 10.0, 10.0), 0);
        assert_eq!(nan.to_field(), Err(CreationError::NonFiniteRect));

        let bad_color =
            FieldSpec::text("f", rect(), 0).with_border_color(RgbColor::new(2.0, 0.0, 0.0));
        assert_eq!(
            bad_color.to_field(),
            Err(CreationError::InvalidColor("border"))
        );

        let nan_value = FieldSpec::text("f", rect(), 0).with_value(FieldValue::number(f64::NAN));
        assert_eq!(
            nan_value.to_field(),
            Err(CreationError::NonFiniteValue("current"))
        );
    }
}
