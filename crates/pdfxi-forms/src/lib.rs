//! AcroForm/XFA model, fields, calculations, validation, FDF/XFDF, and flattening.
//!
//! This crate provides a dependency-free, in-memory model of an interactive PDF
//! form. The core abstractions are:
//!
//! - [`Field`] — a single widget with a type, flags, current value, default
//!   value, choice options, and optional text constraints.
//! - [`Form`] — an ordered, uniquely named collection of fields with lookup,
//!   value mutation, reset, and validation.
//! - [`Validation`] / [`ValidationError`] — the side-effect-free result of
//!   checking every field, including missing-required, invalid-choice,
//!   invalid-value-type, and invalid-text problems.
//! - [`FormFill`] / [`FillReport`] — a filling session that records net field
//!   changes in deterministic, name-sorted order.
//! - [`Calculation`] / [`CalculationOp`] — sum, product, and average of named
//!   numeric fields with deterministic rounding.
//! - [`export_fdf`] / [`import_fdf`] — round-trippable FDF 1.7 field data, plus
//!   a small [`export_xfdf`] XML subset.
//! - [`parse_xfdf`] / [`import_xfdf`] — a bounded XML reader for XFDF field data
//!   that maps names and values onto the target [`Form`]'s field types.
//! - [`load_acroform`] — loads a real PDF catalog `/AcroForm` dictionary into a
//!   [`Form`] through the dependency-free [`FormSource`] adapter trait.
//! - [`build_appearance`] — deterministically generates a widget's appearance
//!   content stream and `/AP` dictionary model from a [`Field`], a
//!   [`WidgetAppearance`] and the current [`FieldValue`].
//! - [`flatten_page`] / [`flatten_document`] — bake widget appearances into
//!   page content, remove widget annotations, and preserve everything else.
//!
//! Field names are fully qualified (for example `order.address.city`). Callers
//! that build fields directly supply the dotted path; [`load_acroform`] builds
//! it from the inherited `/T` partial names of a real field tree using `.` as
//! the separator. All operations are deterministic and allocate only as needed.
//!
//! # Example
//!
//! ```text
//! use pdfxi_forms::{Calculation, CalculationOp, Field, FieldValue, Form, FormFill, FieldType};
//!
//! let mut form = Form::from_fields([
//!     Field::new("a", FieldType::Text).with_value(FieldValue::number(2.0)),
//!     Field::new("b", FieldType::Text).with_value(FieldValue::number(3.0)),
//!     Field::new("total", FieldType::Text),
//! ]).unwrap();
//! Calculation::new("total", CalculationOp::Sum, ["a", "b"])
//!     .apply(&mut form).unwrap();
//! assert_eq!(form.value("total"), Ok(&FieldValue::number(5.0)));
//!
//! let mut fill = FormFill::new(form);
//! fill.set("a", FieldValue::number(10.0)).unwrap();
//! assert_eq!(fill.changes().len(), 1);
//! ```

#![deny(missing_docs)]
#![forbid(unsafe_code)]

mod acroform;
mod appearance;
mod calculation;
mod fdf;
mod field;
mod fill;
mod flatten;
mod form;
mod validation;
mod xfdf;

pub use acroform::{
    AcroFormLoad, FormSource, LoadError, LoadedFieldInfo, LoadedForm, ObjectRef, PdfValue,
    load_acroform,
};
pub use appearance::{
    AVERAGE_WIDTH, Alignment, AppearanceDictionary, AppearanceError, AppearanceStream, BorderStyle,
    DEFAULT_FONT_RESOURCE, FontSpec, HORIZONTAL_PADDING, LINE_HEIGHT_FACTOR, NARROW_WIDTH,
    NormalAppearance, RgbColor, STATE_OFF, STATE_ON, TextLayout, TextLine, VERTICAL_PADDING,
    WIDE_WIDTH, WidgetAppearance, WidgetRect, build_appearance, button_state_names,
    check_mark_content, escape_literal_string, format_number, layout_text,
};
pub use calculation::{
    Calculation, CalculationError, CalculationOp, CalculationReport, CalculationResult,
    run_calculations,
};
pub use fdf::{FdfDocument, FdfError, FdfField, export_fdf, export_xfdf, import_fdf};
pub use field::{Field, FieldFlags, FieldType, FieldValue, TextConstraint, ValueKind};
pub use fill::{ChangedField, FillReport, FormFill};
pub use flatten::{
    AppearancePolicy, FlatDocument, FlatPage, FlattenError, FlattenedDocument, FlattenedPage,
    PageAnnotation, StoredAppearance, flatten_document, flatten_page,
};
pub use form::{Form, FormError};
pub use validation::{Validation, ValidationError};
pub use xfdf::{XFDF_NAMESPACE, XfdfDocument, XfdfError, XfdfField, import_xfdf, parse_xfdf};

/// Return the implementation name for diagnostics.
pub fn engine_name() -> &'static str {
    "pdfxi-forms"
}
