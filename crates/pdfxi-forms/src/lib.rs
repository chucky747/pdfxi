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
//!   checking every field, including missing-required, invalid-choice, and
//!   invalid-text problems.
//!
//! Field names are treated as already fully qualified (for example
//! `order.address.city`); the crate does not parse hierarchical name trees.
//! All operations are deterministic and allocate only as needed. Future
//! milestones will add calculations, FDF/XFDF, and flattening on top of this
//! model without changing its public contracts.

#![deny(missing_docs)]
#![forbid(unsafe_code)]

mod field;
mod form;
mod validation;

pub use field::{Field, FieldFlags, FieldType, FieldValue, TextConstraint};
pub use form::{Form, FormError};
pub use validation::{Validation, ValidationError};

/// Return the implementation name for diagnostics.
pub fn engine_name() -> &'static str {
    "pdfxi-forms"
}
