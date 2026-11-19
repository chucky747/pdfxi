//! Deterministic field calculations: sum, product, and average.
//!
//! A [`Calculation`] names a target field, an operation, and a list of source
//! field names. [`Calculation::evaluate`] reads [`FieldValue::Number`] values
//! from a [`Form`] and returns a rounded numeric value; it never mutates the
//! form. [`Calculation::apply`] writes that value into the target field through
//! [`Form::set_value`], inheriting read-only and type checks.
//!
//! # Non-numeric input
//!
//! Calculation sources must be numeric. A missing field yields
//! [`CalculationError::FieldNotFound`] and a present field holding any other
//! value kind (including text that merely looks like a number) yields
//! [`CalculationError::NonNumeric`]. There is no implicit string-to-number
//! coercion and no JavaScript evaluation.
//!
//! # Rounding
//!
//! Results are rounded to a fixed number of decimal places (default
//! [`Calculation::DEFAULT_DECIMALS`]) with `f64::round`, i.e. ties go *away from
//! zero*, so the same inputs always produce the same output. Because the
//! accumulator is an `f64`, extremely large or long-running sums can still show
//! ordinary floating-point error; results must be finite or
//! [`CalculationError::NonFiniteResult`] is returned.

use crate::field::FieldValue;
use crate::form::{Form, FormError};
use std::fmt;

/// The arithmetic operation performed by a [`Calculation`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CalculationOp {
    /// Add all source values, starting from zero.
    Sum,
    /// Multiply all source values, starting from one.
    Product,
    /// Add all source values and divide by the source count.
    Average,
}

impl CalculationOp {
    /// Returns a stable, human-readable label for this operation.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Sum => "sum",
            Self::Product => "product",
            Self::Average => "average",
        }
    }
}

impl fmt::Display for CalculationOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// An error produced while evaluating or applying a [`Calculation`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CalculationError {
    /// A named source field does not exist in the form.
    FieldNotFound(String),
    /// A named source field exists but does not hold a numeric value.
    NonNumeric {
        /// Fully qualified name of the offending field.
        field: String,
        /// Debug representation of the value that was found.
        found: String,
    },
    /// An average or other operation was given no source fields.
    EmptyOperands,
    /// The computed result was infinite or NaN.
    NonFiniteResult,
    /// More than 12 decimal places were requested.
    InvalidDecimals(u8),
    /// Writing the result into the target field was rejected by the form.
    SetFailed(FormError),
}

impl fmt::Display for CalculationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::FieldNotFound(name) => write!(f, "calculation source '{name}' does not exist"),
            Self::NonNumeric { field, found } => {
                write!(f, "calculation source '{field}' is not numeric: {found}")
            }
            Self::EmptyOperands => write!(f, "calculation has no source fields"),
            Self::NonFiniteResult => write!(f, "calculation produced a non-finite result"),
            Self::InvalidDecimals(value) => {
                write!(
                    f,
                    "calculation requested {value} decimal places (maximum 12)"
                )
            }
            Self::SetFailed(error) => write!(f, "calculation could not write its result: {error}"),
        }
    }
}

impl std::error::Error for CalculationError {}

/// A single sum/product/average calculation over named numeric fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Calculation {
    target: String,
    op: CalculationOp,
    sources: Vec<String>,
    decimals: u8,
}

impl Calculation {
    /// Rounds to six decimal places by default.
    pub const DEFAULT_DECIMALS: u8 = 6;

    /// Builds a calculation for `target` over `sources`.
    pub fn new<S, I>(target: S, op: CalculationOp, sources: I) -> Self
    where
        S: Into<String>,
        I: IntoIterator,
        I::Item: Into<String>,
    {
        Self {
            target: target.into(),
            op,
            sources: sources.into_iter().map(Into::into).collect(),
            decimals: Self::DEFAULT_DECIMALS,
        }
    }

    /// Overrides the number of decimal places (maximum 12).
    #[must_use]
    pub const fn with_decimals(mut self, decimals: u8) -> Self {
        self.decimals = decimals;
        self
    }

    /// Returns the target field's fully qualified name.
    pub fn target(&self) -> &str {
        &self.target
    }

    /// Returns the arithmetic operation.
    pub const fn op(&self) -> CalculationOp {
        self.op
    }

    /// Returns the source field names in evaluation order.
    pub fn sources(&self) -> &[String] {
        &self.sources
    }

    /// Returns the configured number of decimal places.
    pub const fn decimals(&self) -> u8 {
        self.decimals
    }

    /// Evaluates the calculation against `form` without changing it.
    ///
    /// Non-numeric sources are rejected and results are rounded as described on
    /// [`Calculation`]. The returned value is always [`FieldValue::Number`].
    pub fn evaluate(&self, form: &Form) -> Result<FieldValue, CalculationError> {
        if self.decimals > 12 {
            return Err(CalculationError::InvalidDecimals(self.decimals));
        }
        if self.sources.is_empty() {
            return Err(CalculationError::EmptyOperands);
        }

        let mut numbers = Vec::with_capacity(self.sources.len());
        for name in &self.sources {
            let value = form
                .value(name)
                .map_err(|_| CalculationError::FieldNotFound(name.clone()))?;
            let number = value
                .as_number()
                .ok_or_else(|| CalculationError::NonNumeric {
                    field: name.clone(),
                    found: format!("{value:?}"),
                })?;
            if !number.is_finite() {
                return Err(CalculationError::NonNumeric {
                    field: name.clone(),
                    found: format!("{number:?}"),
                });
            }
            numbers.push(number);
        }

        let result = match self.op {
            CalculationOp::Sum => numbers.iter().sum(),
            CalculationOp::Product => numbers.iter().product(),
            CalculationOp::Average => numbers.iter().sum::<f64>() / numbers.len() as f64,
        };
        if !result.is_finite() {
            return Err(CalculationError::NonFiniteResult);
        }
        Ok(FieldValue::number(round_to(result, self.decimals)))
    }

    /// Evaluates the calculation and writes the result into its target field.
    ///
    /// Read-only targets and value-kind mismatches surface as
    /// [`CalculationError::SetFailed`].
    pub fn apply(&self, form: &mut Form) -> Result<(), CalculationError> {
        let value = self.evaluate(form)?;
        form.set_value(&self.target, value)
            .map_err(CalculationError::SetFailed)
    }
}

/// The value written by one applied calculation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CalculationResult {
    target: String,
    value: FieldValue,
}

impl CalculationResult {
    /// Builds a result record.
    pub fn new(target: impl Into<String>, value: FieldValue) -> Self {
        Self {
            target: target.into(),
            value,
        }
    }

    /// Returns the target field name.
    pub fn target(&self) -> &str {
        &self.target
    }

    /// Returns the computed value.
    pub fn value(&self) -> &FieldValue {
        &self.value
    }
}

/// The outcomes of a batch of applied calculations, in application order.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CalculationReport {
    results: Vec<CalculationResult>,
}

impl CalculationReport {
    /// Builds a report from result records.
    pub fn new(results: Vec<CalculationResult>) -> Self {
        Self { results }
    }

    /// Returns the results in application order.
    pub fn results(&self) -> &[CalculationResult] {
        &self.results
    }

    /// Returns the number of results.
    pub fn len(&self) -> usize {
        self.results.len()
    }

    /// Returns `true` when no calculations were applied.
    pub fn is_empty(&self) -> bool {
        self.results.is_empty()
    }
}

/// Applies calculations to a form in the given order.
///
/// Because calculations run sequentially, a later calculation may read a field
/// written by an earlier one. The first failure is returned immediately; earlier
/// writes remain in the form and are recorded in the error path only up to the
/// caller's own bookkeeping.
pub fn run_calculations(
    form: &mut Form,
    calculations: &[Calculation],
) -> Result<CalculationReport, CalculationError> {
    let mut results = Vec::with_capacity(calculations.len());
    for calculation in calculations {
        calculation.apply(form)?;
        let value = form
            .value(calculation.target())
            .map_err(CalculationError::SetFailed)?;
        results.push(CalculationResult::new(calculation.target(), value.clone()));
    }
    Ok(CalculationReport::new(results))
}

/// Rounds `value` to `decimals` places, ties away from zero.
fn round_to(value: f64, decimals: u8) -> f64 {
    if decimals == 0 {
        return value.round();
    }
    let factor = 10_f64.powi(i32::from(decimals));
    (value * factor).round() / factor
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::field::{Field, FieldType};

    fn numeric_form() -> Form {
        Form::from_fields([
            Field::new("a", FieldType::Text).with_value(FieldValue::number(10.5)),
            Field::new("b", FieldType::Text).with_value(FieldValue::number(20.25)),
            Field::new("c", FieldType::Text).with_value(FieldValue::number(-2.0)),
            Field::new("text", FieldType::Text).with_value(FieldValue::Text("3".into())),
            Field::new("result", FieldType::Text),
        ])
        .unwrap()
    }

    #[test]
    fn calculation_sum_is_deterministic() {
        let form = numeric_form();
        let sum = Calculation::new("result", CalculationOp::Sum, ["a", "b", "c"]);
        assert_eq!(sum.evaluate(&form), Ok(FieldValue::number(28.75)));
    }

    #[test]
    fn calculation_product_is_deterministic() {
        let form = numeric_form();
        let product = Calculation::new("result", CalculationOp::Product, ["a", "b", "c"]);
        assert_eq!(product.evaluate(&form), Ok(FieldValue::number(-425.25)));
    }

    #[test]
    fn calculation_average_is_deterministic() {
        let form = numeric_form();
        let average = Calculation::new("result", CalculationOp::Average, ["a", "b", "c"]);
        assert_eq!(average.evaluate(&form), Ok(FieldValue::number(9.583333)));
    }

    #[test]
    fn calculation_rounding_is_half_away_from_zero() {
        let form = Form::from_fields([
            Field::new("up", FieldType::Text).with_value(FieldValue::number(1.5)),
            Field::new("down", FieldType::Text).with_value(FieldValue::number(-1.5)),
        ])
        .unwrap();
        let up = Calculation::new("ignored", CalculationOp::Sum, ["up"]).with_decimals(0);
        let down = Calculation::new("ignored", CalculationOp::Sum, ["down"]).with_decimals(0);

        assert_eq!(up.evaluate(&form), Ok(FieldValue::number(2.0)));
        assert_eq!(down.evaluate(&form), Ok(FieldValue::number(-2.0)));
    }

    #[test]
    fn calculation_fails_on_non_numeric_input() {
        let form = numeric_form();
        let calculation = Calculation::new("result", CalculationOp::Sum, ["a", "text"]);
        let error = calculation.evaluate(&form).unwrap_err();

        assert!(matches!(
            error,
            CalculationError::NonNumeric { ref field, .. } if field == "text"
        ));
    }

    #[test]
    fn calculation_fails_on_missing_or_empty_sources() {
        let form = numeric_form();
        let missing = Calculation::new("result", CalculationOp::Sum, ["absent"]);
        assert_eq!(
            missing.evaluate(&form),
            Err(CalculationError::FieldNotFound("absent".to_owned()))
        );

        let empty = Calculation::new("result", CalculationOp::Average, Vec::<String>::new());
        assert_eq!(empty.evaluate(&form), Err(CalculationError::EmptyOperands));
    }

    #[test]
    fn calculation_rejects_unsupported_decimal_counts() {
        let form = numeric_form();
        let calculation = Calculation::new("result", CalculationOp::Sum, ["a"]).with_decimals(13);
        assert_eq!(
            calculation.evaluate(&form),
            Err(CalculationError::InvalidDecimals(13))
        );
    }

    #[test]
    fn calculation_apply_writes_the_target_field() {
        let mut form = numeric_form();
        let sum = Calculation::new("result", CalculationOp::Sum, ["a", "b"]);
        sum.apply(&mut form).unwrap();

        assert_eq!(form.value("result"), Ok(&FieldValue::number(30.75)));
    }

    #[test]
    fn run_calculations_applies_in_order_and_reports_results() {
        let mut form = numeric_form();
        let calculations = [
            Calculation::new("result", CalculationOp::Sum, ["a", "b"]),
            Calculation::new("result", CalculationOp::Product, ["result", "c"]),
        ];
        let report = run_calculations(&mut form, &calculations).unwrap();

        assert_eq!(report.len(), 2);
        assert_eq!(report.results()[1].target(), "result");
        assert_eq!(report.results()[1].value(), &FieldValue::number(-61.5));
    }

    #[test]
    fn calculation_does_not_mutate_during_evaluate() {
        let form = numeric_form();
        let before = form.clone();
        let calculation = Calculation::new("result", CalculationOp::Sum, ["a", "b"]);
        let _ = calculation.evaluate(&form).unwrap();
        assert_eq!(form, before);
    }
}
