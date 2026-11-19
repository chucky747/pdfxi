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
//! - [`load_acroform`] — loads a real PDF catalog `/AcroForm` dictionary into a
//!   [`Form`] through the dependency-free [`FormSource`] adapter trait.
//!
//! Field names are fully qualified (for example `order.address.city`). Callers
//! that build fields directly supply the dotted path; [`load_acroform`] builds
//! it from the inherited `/T` partial names of a real field tree using `.` as
//! the separator. All operations are deterministic and allocate only as needed.
//! Writing widget annotations, generating appearances, and flattening remain
//! future work.
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
mod calculation;
mod fdf;
mod field;
mod fill;
mod form;
mod validation;

pub use acroform::{
    AcroFormLoad, FormSource, LoadError, LoadedFieldInfo, LoadedForm, ObjectRef, PdfValue,
    load_acroform,
};
pub use calculation::{
    Calculation, CalculationError, CalculationOp, CalculationReport, CalculationResult,
    run_calculations,
};
pub use fdf::{FdfDocument, FdfError, FdfField, export_fdf, export_xfdf, import_fdf};
pub use field::{Field, FieldFlags, FieldType, FieldValue, TextConstraint, ValueKind};
pub use fill::{ChangedField, FillReport, FormFill};
pub use form::{Form, FormError};
pub use validation::{Validation, ValidationError};

/// Return the implementation name for diagnostics.
pub fn engine_name() -> &'static str {
    "pdfxi-forms"
}
