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
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

/// Maximum number of steps accepted by [`CalculationPlan::apply`].
pub const MAX_PLAN_STEPS: usize = 4096;

/// The arithmetic operation performed by a [`Calculation`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CalculationOp {
    /// Add all source values, starting from zero.
    Sum,
    /// Multiply all source values, starting from one.
    Product,
    /// Add all source values and divide by the source count.
    Average,
    /// Return the smallest source value.
    Min,
    /// Return the largest source value.
    Max,
}

impl CalculationOp {
    /// Returns a stable, human-readable label for this operation.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Sum => "sum",
            Self::Product => "product",
            Self::Average => "average",
            Self::Min => "min",
            Self::Max => "max",
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
    /// Two calculations in a plan target the same field.
    DuplicateTarget(String),
    /// A calculation cycle was detected; the vector names the cycle in order.
    Cycle(Vec<String>),
    /// An arithmetic expression divided by zero.
    DivisionByZero,
    /// A plan contained more steps than the supported maximum.
    TooManySteps(usize),
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
            Self::DuplicateTarget(name) => {
                write!(f, "two calculations target the same field '{name}'")
            }
            Self::Cycle(path) => write!(f, "calculation cycle: {}", path.join(" -> ")),
            Self::DivisionByZero => write!(f, "arithmetic expression divided by zero"),
            Self::TooManySteps(count) => write!(
                f,
                "calculation plan has {count} steps (maximum {MAX_PLAN_STEPS})"
            ),
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
            CalculationOp::Min => numbers.iter().copied().fold(f64::INFINITY, f64::min),
            CalculationOp::Max => numbers.iter().copied().fold(f64::NEG_INFINITY, f64::max),
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

/// A bounded arithmetic expression over named numeric fields and constants.
///
/// This is a *simplified arithmetic* interpreter: no scripts, function calls,
/// loops or string coercion are supported. The only operations are addition,
/// subtraction, multiplication, division and negation, combined with field
/// references ([`Arithmetic::Field`]) and finite constants
/// ([`Arithmetic::Const`]). Evaluation is side-effect free and deterministic.
#[derive(Debug, Clone, PartialEq)]
pub enum Arithmetic {
    /// A reference to a field's numeric value.
    Field(String),
    /// A finite numeric constant.
    Const(f64),
    /// Sum of two sub-expressions.
    Add(Box<Arithmetic>, Box<Arithmetic>),
    /// Difference of two sub-expressions.
    Sub(Box<Arithmetic>, Box<Arithmetic>),
    /// Product of two sub-expressions.
    Mul(Box<Arithmetic>, Box<Arithmetic>),
    /// Quotient of two sub-expressions.
    Div(Box<Arithmetic>, Box<Arithmetic>),
    /// Arithmetic negation of a sub-expression.
    Neg(Box<Arithmetic>),
}

impl Arithmetic {
    /// Creates a reference to the named field.
    pub fn field(name: impl Into<String>) -> Self {
        Self::Field(name.into())
    }

    /// Creates a finite constant.
    pub const fn constant(value: f64) -> Self {
        Self::Const(value)
    }

    /// Creates an addition expression.
    pub fn add_expr(left: Self, right: Self) -> Self {
        Self::Add(Box::new(left), Box::new(right))
    }

    /// Creates a subtraction expression.
    pub fn sub_expr(left: Self, right: Self) -> Self {
        Self::Sub(Box::new(left), Box::new(right))
    }

    /// Creates a multiplication expression.
    pub fn mul_expr(left: Self, right: Self) -> Self {
        Self::Mul(Box::new(left), Box::new(right))
    }

    /// Creates a division expression.
    pub fn div_expr(left: Self, right: Self) -> Self {
        Self::Div(Box::new(left), Box::new(right))
    }

    /// Creates a negation expression.
    pub fn neg_expr(value: Self) -> Self {
        Self::Neg(Box::new(value))
    }

    /// Collects the distinct field names referenced by this expression, sorted
    /// by name.
    pub fn references(&self) -> Vec<String> {
        let mut names = BTreeSet::new();
        self.collect_references(&mut names);
        names.into_iter().collect()
    }

    fn collect_references(&self, out: &mut BTreeSet<String>) {
        match self {
            Self::Field(name) => {
                out.insert(name.clone());
            }
            Self::Const(_) => {}
            Self::Add(left, right)
            | Self::Sub(left, right)
            | Self::Mul(left, right)
            | Self::Div(left, right) => {
                left.collect_references(out);
                right.collect_references(out);
            }
            Self::Neg(value) => value.collect_references(out),
        }
    }

    /// Evaluates the expression against `form` without mutating it.
    ///
    /// # Errors
    ///
    /// Returns [`CalculationError::FieldNotFound`] or
    /// [`CalculationError::NonNumeric`] for bad references,
    /// [`CalculationError::DivisionByZero`] for a zero divisor, and
    /// [`CalculationError::NonFiniteResult`] for a non-finite intermediate or
    /// final value.
    pub fn evaluate(&self, form: &Form, decimals: u8) -> Result<FieldValue, CalculationError> {
        if decimals > 12 {
            return Err(CalculationError::InvalidDecimals(decimals));
        }
        let result = self.eval_number(form)?;
        if !result.is_finite() {
            return Err(CalculationError::NonFiniteResult);
        }
        Ok(FieldValue::number(round_to(result, decimals)))
    }

    fn eval_number(&self, form: &Form) -> Result<f64, CalculationError> {
        match self {
            Self::Field(name) => {
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
                Ok(number)
            }
            Self::Const(value) => {
                if value.is_finite() {
                    Ok(*value)
                } else {
                    Err(CalculationError::NonFiniteResult)
                }
            }
            Self::Add(left, right) => Ok(left.eval_number(form)? + right.eval_number(form)?),
            Self::Sub(left, right) => Ok(left.eval_number(form)? - right.eval_number(form)?),
            Self::Mul(left, right) => Ok(left.eval_number(form)? * right.eval_number(form)?),
            Self::Div(left, right) => {
                let numerator = left.eval_number(form)?;
                let denominator = right.eval_number(form)?;
                if denominator == 0.0 {
                    return Err(CalculationError::DivisionByZero);
                }
                Ok(numerator / denominator)
            }
            Self::Neg(value) => Ok(-value.eval_number(form)?),
        }
    }
}

/// An arithmetic expression assigned to a target field.
#[derive(Debug, Clone, PartialEq)]
pub struct ArithmeticCalculation {
    target: String,
    expression: Arithmetic,
    decimals: u8,
}

impl ArithmeticCalculation {
    /// Creates a calculation writing `expression` into `target`.
    pub fn new(target: impl Into<String>, expression: Arithmetic) -> Self {
        Self {
            target: target.into(),
            expression,
            decimals: Calculation::DEFAULT_DECIMALS,
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

    /// Returns the arithmetic expression.
    pub fn expression(&self) -> &Arithmetic {
        &self.expression
    }

    /// Returns the configured number of decimal places.
    pub const fn decimals(&self) -> u8 {
        self.decimals
    }

    /// Evaluates the expression without mutating the form.
    pub fn evaluate(&self, form: &Form) -> Result<FieldValue, CalculationError> {
        self.expression.evaluate(form, self.decimals)
    }

    /// Evaluates the expression and writes the result into the target field.
    pub fn apply(&self, form: &mut Form) -> Result<(), CalculationError> {
        let value = self.evaluate(form)?;
        form.set_value(&self.target, value)
            .map_err(CalculationError::SetFailed)
    }
}

/// One step of a [`CalculationPlan`].
#[derive(Debug, Clone, PartialEq)]
pub enum CalculationStep {
    /// A named sum/product/average/min/max calculation.
    Standard(Calculation),
    /// A simplified arithmetic expression calculation.
    Arithmetic(ArithmeticCalculation),
}

impl CalculationStep {
    /// Returns the target field name of this step.
    pub fn target(&self) -> &str {
        match self {
            Self::Standard(calculation) => calculation.target(),
            Self::Arithmetic(calculation) => calculation.target(),
        }
    }

    /// Returns the distinct field names this step reads, sorted by name.
    pub fn dependencies(&self) -> Vec<String> {
        match self {
            Self::Standard(calculation) => {
                let names: BTreeSet<String> = calculation.sources().iter().cloned().collect();
                names.into_iter().collect()
            }
            Self::Arithmetic(calculation) => calculation.expression().references(),
        }
    }

    /// Applies this step to `form`.
    pub fn apply(&self, form: &mut Form) -> Result<(), CalculationError> {
        match self {
            Self::Standard(calculation) => calculation.apply(form),
            Self::Arithmetic(calculation) => calculation.apply(form),
        }
    }
}

/// A set of calculations evaluated in dependency order.
///
/// The plan builds a graph whose nodes are calculation targets. If step A
/// reads a field written by step B, B is evaluated before A. Independent steps
/// are ordered by target name, and the final report follows that evaluation
/// order, so supplying the same steps in a different order yields identical
/// results. Cycles (including a step reading its own target) are reported as
/// [`CalculationError::Cycle`] before any step is applied.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CalculationPlan {
    steps: Vec<CalculationStep>,
}

impl CalculationPlan {
    /// Creates a plan from the given steps.
    pub fn new(steps: Vec<CalculationStep>) -> Self {
        Self { steps }
    }

    /// Appends a step to the plan.
    #[must_use]
    pub fn with_step(mut self, step: CalculationStep) -> Self {
        self.steps.push(step);
        self
    }

    /// Returns the steps in their original (unoptimised) order.
    pub fn steps(&self) -> &[CalculationStep] {
        &self.steps
    }

    /// Returns `true` when the plan has no steps.
    pub fn is_empty(&self) -> bool {
        self.steps.is_empty()
    }

    /// Returns the number of steps.
    pub fn len(&self) -> usize {
        self.steps.len()
    }

    /// Returns the step indices in the order they will be evaluated.
    ///
    /// # Errors
    ///
    /// Returns [`CalculationError::TooManySteps`] for oversized plans,
    /// [`CalculationError::DuplicateTarget`] for repeated target names, or
    /// [`CalculationError::Cycle`] when the dependency graph has a cycle.
    pub fn evaluation_order(&self) -> Result<Vec<usize>, CalculationError> {
        if self.steps.len() > MAX_PLAN_STEPS {
            return Err(CalculationError::TooManySteps(self.steps.len()));
        }
        let mut targets: BTreeMap<&str, usize> = BTreeMap::new();
        for (index, step) in self.steps.iter().enumerate() {
            if targets.insert(step.target(), index).is_some() {
                return Err(CalculationError::DuplicateTarget(step.target().to_owned()));
            }
        }
        let count = self.steps.len();
        let mut depends_on: Vec<Vec<usize>> = vec![Vec::new(); count];
        let mut dependents: Vec<Vec<usize>> = vec![Vec::new(); count];
        let mut indegree = vec![0usize; count];
        for (index, step) in self.steps.iter().enumerate() {
            for name in step.dependencies() {
                if let Some(&source) = targets.get(name.as_str()) {
                    if source == index {
                        return Err(CalculationError::Cycle(vec![step.target().to_owned()]));
                    }
                    if !depends_on[index].contains(&source) {
                        depends_on[index].push(source);
                        dependents[source].push(index);
                        indegree[index] += 1;
                    }
                }
            }
        }

        // Ready nodes are keyed by target name so independent steps are
        // processed in a deterministic, insertion-order-independent sequence.
        let mut ready: BTreeMap<String, usize> = BTreeMap::new();
        for (index, step) in self.steps.iter().enumerate() {
            if indegree[index] == 0 {
                ready.insert(step.target().to_owned(), index);
            }
        }
        let mut order = Vec::with_capacity(count);
        while let Some((_, index)) = ready.pop_first() {
            order.push(index);
            for &dependent in &dependents[index] {
                indegree[dependent] -= 1;
                if indegree[dependent] == 0 {
                    ready.insert(self.steps[dependent].target().to_owned(), dependent);
                }
            }
        }
        if order.len() != count {
            let remaining: Vec<usize> = (0..count).filter(|&index| indegree[index] > 0).collect();
            return Err(CalculationError::Cycle(find_cycle(
                &depends_on,
                &self.steps,
                &remaining,
            )));
        }
        Ok(order)
    }

    /// Applies every step in dependency order and returns a deterministic
    /// [`CalculationReport`].
    ///
    /// No step runs if the plan is invalid (oversized, duplicated targets or
    /// cyclic). A runtime error in an individual step stops the batch; steps
    /// applied before it remain in the form.
    pub fn apply(&self, form: &mut Form) -> Result<CalculationReport, CalculationError> {
        let order = self.evaluation_order()?;
        let mut results = Vec::with_capacity(order.len());
        for index in order {
            let step = &self.steps[index];
            step.apply(form)?;
            let value = form
                .value(step.target())
                .map_err(CalculationError::SetFailed)?;
            results.push(CalculationResult::new(step.target(), value.clone()));
        }
        Ok(CalculationReport::new(results))
    }
}

/// Finds a deterministic cycle path among the unprocessed graph nodes.
fn find_cycle(
    depends_on: &[Vec<usize>],
    steps: &[CalculationStep],
    remaining: &[usize],
) -> Vec<String> {
    let mut state = vec![0u8; steps.len()];
    let mut path: Vec<usize> = Vec::new();
    for &start in remaining {
        if let Some(cycle) = find_cycle_from(start, depends_on, steps, &mut state, &mut path) {
            return cycle;
        }
    }
    match remaining.first() {
        Some(&index) => vec![steps[index].target().to_owned()],
        None => Vec::new(),
    }
}

fn find_cycle_from(
    node: usize,
    depends_on: &[Vec<usize>],
    steps: &[CalculationStep],
    state: &mut [u8],
    path: &mut Vec<usize>,
) -> Option<Vec<String>> {
    if state[node] == 1
        && let Some(start) = path.iter().position(|&index| index == node)
    {
        return Some(
            path[start..]
                .iter()
                .map(|&index| steps[index].target().to_owned())
                .collect(),
        );
    }
    if state[node] != 0 {
        return None;
    }
    state[node] = 1;
    path.push(node);
    for &dependency in &depends_on[node] {
        if let Some(cycle) = find_cycle_from(dependency, depends_on, steps, state, path) {
            return Some(cycle);
        }
    }
    path.pop();
    state[node] = 2;
    None
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

    #[test]
    fn min_and_max_are_deterministic() {
        let form = numeric_form();
        let min = Calculation::new("result", CalculationOp::Min, ["a", "b", "c"]);
        let max = Calculation::new("result", CalculationOp::Max, ["a", "b", "c"]);

        assert_eq!(min.evaluate(&form), Ok(FieldValue::number(-2.0)));
        assert_eq!(max.evaluate(&form), Ok(FieldValue::number(20.25)));
    }

    #[test]
    fn arithmetic_expression_evaluates_and_rounds() {
        let form = numeric_form();
        // (a + b) / c - 2 * 3, with a=10.5, b=20.25, c=-2 -> -21.375
        let expression = Arithmetic::sub_expr(
            Arithmetic::div_expr(
                Arithmetic::add_expr(Arithmetic::field("a"), Arithmetic::field("b")),
                Arithmetic::field("c"),
            ),
            Arithmetic::mul_expr(Arithmetic::constant(2.0), Arithmetic::constant(3.0)),
        );
        let calculation = ArithmeticCalculation::new("result", expression);

        assert_eq!(calculation.evaluate(&form), Ok(FieldValue::number(-21.375)));
        assert_eq!(calculation.expression().references(), ["a", "b", "c"]);
    }

    #[test]
    fn arithmetic_reports_division_by_zero_and_unknown_fields() {
        let form = numeric_form();
        let div_zero = Arithmetic::div_expr(Arithmetic::field("a"), Arithmetic::constant(0.0));
        assert_eq!(
            ArithmeticCalculation::new("result", div_zero).evaluate(&form),
            Err(CalculationError::DivisionByZero)
        );
        let unknown = Arithmetic::neg_expr(Arithmetic::field("absent"));
        assert_eq!(
            ArithmeticCalculation::new("result", unknown).evaluate(&form),
            Err(CalculationError::FieldNotFound("absent".to_owned()))
        );
    }

    #[test]
    fn plan_is_order_independent_and_respects_dependencies() {
        let make_plan = |steps: Vec<CalculationStep>| CalculationPlan::new(steps);
        let first =
            CalculationStep::Standard(Calculation::new("result", CalculationOp::Sum, ["a", "c"]));
        let second = CalculationStep::Standard(Calculation::new(
            "b",
            CalculationOp::Product,
            ["result", "c"],
        ));

        let forward = make_plan(vec![first.clone(), second.clone()]);
        let reverse = make_plan(vec![second, first]);
        let mut form_a = numeric_form();
        let mut form_b = form_a.clone();
        let report_a = forward.apply(&mut form_a).unwrap();
        let report_b = reverse.apply(&mut form_b).unwrap();

        assert_eq!(form_a, form_b);
        assert_eq!(report_a, report_b);
        assert_eq!(report_a.len(), 2);
        assert_eq!(report_a.results()[0].target(), "result");
        assert_eq!(report_a.results()[1].target(), "b");
        assert_eq!(form_a.value("result"), Ok(&FieldValue::number(8.5)));
        assert_eq!(form_a.value("b"), Ok(&FieldValue::number(-17.0)));
    }

    #[test]
    fn plan_detects_dependency_cycles_before_applying() {
        let plan = CalculationPlan::new(vec![
            CalculationStep::Standard(Calculation::new("a", CalculationOp::Sum, ["b"])),
            CalculationStep::Standard(Calculation::new("b", CalculationOp::Sum, ["a"])),
        ]);
        let mut form = numeric_form();
        let before = form.clone();
        let error = plan.apply(&mut form).unwrap_err();

        assert_eq!(form, before, "a cyclic plan must not mutate the form");
        assert!(matches!(error, CalculationError::Cycle(path) if path == ["a", "b"]));
    }

    #[test]
    fn plan_rejects_self_references_and_duplicate_targets() {
        let self_ref = CalculationPlan::new(vec![CalculationStep::Arithmetic(
            ArithmeticCalculation::new("a", Arithmetic::field("a")),
        )]);
        assert_eq!(
            self_ref.evaluation_order(),
            Err(CalculationError::Cycle(vec!["a".to_owned()]))
        );

        let duplicate = CalculationPlan::new(vec![
            CalculationStep::Standard(Calculation::new("x", CalculationOp::Sum, ["a"])),
            CalculationStep::Standard(Calculation::new("x", CalculationOp::Sum, ["b"])),
        ]);
        assert_eq!(
            duplicate.evaluation_order(),
            Err(CalculationError::DuplicateTarget("x".to_owned()))
        );
    }
}
