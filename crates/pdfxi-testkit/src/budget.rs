//! Parser and page-count scaler for the committed performance budget descriptor.
//!
//! [`crate::perf-budgets.toml`](../../perf-budgets.toml) records the five logical
//! operations from AC-05 (open, parse, render, text-extraction and search). This
//! module parses that file without adding a TOML dependency, validates its
//! shape, and exposes [`BudgetFile::budget_ms`], which reproduces the scaling
//! formula used by the `cargo xtask bench` harness so documentation and gate
//! cannot silently diverge.
//!
//! No wall-clock measurement happens here. This module only handles the
//! *declared* budgets; measured timings come from `cargo xtask bench` and are
//! archived in `docs/evidence/qa/T-0144.md`.

use std::fmt;

/// Descriptor schema version.
pub const BUDGET_VERSION: u32 = 1;

/// Page count at which the base budgets are defined (the 500-page reference).
pub const REFERENCE_PAGES: usize = 500;

/// Minimum per-operation budget in milliseconds (fixed-overhead allowance).
pub const MIN_BUDGET_MS: f64 = 5.0;

/// Extra budget headroom for host variance (25%), matching the xtask harness.
pub const BUDGET_TOLERANCE: f64 = 0.25;

/// The five logical operations required by the budget descriptor.
pub const REQUIRED_OPERATIONS: [&str; 5] = ["open", "parse", "render", "text_extraction", "search"];

/// One parsed `[[budget]]` row.
#[derive(Debug, Clone, PartialEq)]
pub struct BudgetEntry {
    /// Logical operation name from AC-05's source table.
    pub operation: String,
    /// Concrete metric emitted by `cargo xtask bench`.
    pub harness_metric: String,
    /// Budget in milliseconds at [`REFERENCE_PAGES`] pages.
    pub base_ms: f64,
    /// Human-readable justification for the budget.
    pub doc: String,
}

/// A validated budget descriptor.
#[derive(Debug, Clone, PartialEq)]
pub struct BudgetFile {
    /// Descriptor schema version.
    pub version: u32,
    /// Page count the base budgets refer to.
    pub reference_pages: usize,
    /// Budget rows in descriptor order.
    pub entries: Vec<BudgetEntry>,
}

/// Why a budget descriptor was rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BudgetError {
    /// A required field is missing or malformed.
    Malformed(String),
    /// An unsupported schema version was declared.
    UnsupportedVersion(u32),
    /// An operation is listed more than once.
    DuplicateOperation(String),
    /// A required operation is absent.
    MissingOperation(String),
    /// A base budget is not a finite, positive number.
    InvalidBaseMs(String),
}

impl fmt::Display for BudgetError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Malformed(why) => write!(formatter, "malformed budget file: {why}"),
            Self::UnsupportedVersion(version) => {
                write!(formatter, "unsupported budget version: {version}")
            }
            Self::DuplicateOperation(operation) => {
                write!(formatter, "duplicate budget operation: {operation}")
            }
            Self::MissingOperation(operation) => {
                write!(formatter, "missing budget operation: {operation}")
            }
            Self::InvalidBaseMs(operation) => {
                write!(formatter, "invalid base_ms for {operation}")
            }
        }
    }
}

impl std::error::Error for BudgetError {}

/// Strip surrounding double quotes from a TOML basic-string scalar.
fn unquote(value: &str) -> String {
    let value = value.trim();
    if value.len() >= 2 && value.starts_with('"') && value.ends_with('"') {
        value[1..value.len() - 1].to_string()
    } else {
        value.to_string()
    }
}

/// Return the committed budget descriptor text, embedded at compile time.
pub fn committed_text() -> &'static str {
    include_str!("../perf-budgets.toml")
}

/// Return the parsed and validated committed budget descriptor.
///
/// Panics only if the checked-in descriptor is invalid, which is a build-time
/// invariant covered by the unit tests in this module.
pub fn committed_budgets() -> BudgetFile {
    parse_budgets(committed_text()).expect("committed perf-budgets.toml is valid")
}

/// Parse and validate a budget descriptor.
pub fn parse_budgets(text: &str) -> Result<BudgetFile, BudgetError> {
    let mut version: Option<u32> = None;
    let mut reference_pages: Option<usize> = None;
    let mut entries = Vec::new();
    let mut current: Option<(String, String, Option<f64>, String)> = None;

    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(value) = line.strip_prefix("version") {
            let (_, value) = value
                .split_once('=')
                .ok_or_else(|| BudgetError::Malformed("version has no '='".to_owned()))?;
            version = Some(
                value
                    .trim()
                    .parse()
                    .map_err(|_| BudgetError::Malformed("version is not numeric".to_owned()))?,
            );
            continue;
        }
        if let Some(value) = line.strip_prefix("reference_pages") {
            let (_, value) = value
                .split_once('=')
                .ok_or_else(|| BudgetError::Malformed("reference_pages has no '='".to_owned()))?;
            reference_pages = Some(value.trim().parse().map_err(|_| {
                BudgetError::Malformed("reference_pages is not numeric".to_owned())
            })?);
            continue;
        }
        if line == "[[budget]]" {
            if let Some((operation, harness_metric, base_ms, doc)) = current.take() {
                entries.push(finish_entry(operation, harness_metric, base_ms, doc)?);
            }
            current = Some((String::new(), String::new(), None, String::new()));
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let Some(entry) = current.as_mut() else {
            continue;
        };
        match key.trim() {
            "operation" => entry.0 = unquote(value),
            "harness_metric" => entry.1 = unquote(value),
            "base_ms" => {
                entry.2 =
                    Some(value.trim().parse().map_err(|_| {
                        BudgetError::Malformed("base_ms is not numeric".to_owned())
                    })?);
            }
            "doc" => entry.3 = unquote(value),
            _ => {}
        }
    }
    if let Some((operation, harness_metric, base_ms, doc)) = current.take() {
        entries.push(finish_entry(operation, harness_metric, base_ms, doc)?);
    }

    let version = version.ok_or_else(|| BudgetError::Malformed("no version field".to_owned()))?;
    if version != BUDGET_VERSION {
        return Err(BudgetError::UnsupportedVersion(version));
    }
    let reference_pages = reference_pages
        .ok_or_else(|| BudgetError::Malformed("no reference_pages field".to_owned()))?;
    if reference_pages == 0 {
        return Err(BudgetError::Malformed(
            "reference_pages must be positive".to_owned(),
        ));
    }

    let mut seen = Vec::new();
    for entry in &entries {
        if seen.contains(&entry.operation) {
            return Err(BudgetError::DuplicateOperation(entry.operation.clone()));
        }
        seen.push(entry.operation.clone());
    }
    for required in REQUIRED_OPERATIONS {
        if !seen.iter().any(|operation| operation == required) {
            return Err(BudgetError::MissingOperation(required.to_owned()));
        }
    }

    Ok(BudgetFile {
        version,
        reference_pages,
        entries,
    })
}

fn finish_entry(
    operation: String,
    harness_metric: String,
    base_ms: Option<f64>,
    doc: String,
) -> Result<BudgetEntry, BudgetError> {
    if operation.is_empty() || harness_metric.is_empty() || doc.is_empty() {
        return Err(BudgetError::Malformed(
            "budget row missing a required field".to_owned(),
        ));
    }
    let base_ms = base_ms
        .ok_or_else(|| BudgetError::Malformed(format!("budget {operation} has no base_ms")))?;
    if !base_ms.is_finite() || base_ms <= 0.0 {
        return Err(BudgetError::InvalidBaseMs(operation));
    }
    Ok(BudgetEntry {
        operation,
        harness_metric,
        base_ms,
        doc,
    })
}

impl BudgetFile {
    /// Look up an entry by logical operation name.
    pub fn entry(&self, operation: &str) -> Option<&BudgetEntry> {
        self.entries
            .iter()
            .find(|entry| entry.operation == operation)
    }

    /// Return the budget in milliseconds for `operation` at `pages` pages.
    ///
    /// The formula mirrors `xtask/src/bench.rs`: scale the base budget linearly
    /// with page count, floor it at [`MIN_BUDGET_MS`], then add the documented
    /// [`BUDGET_TOLERANCE`] headroom for host variance.
    pub fn budget_ms(&self, operation: &str, pages: usize) -> Option<f64> {
        let entry = self.entry(operation)?;
        let scaled = entry.base_ms * (pages as f64) / (self.reference_pages as f64);
        Some(scaled.max(MIN_BUDGET_MS) * (1.0 + BUDGET_TOLERANCE))
    }

    /// Return the harness metric mapped to a logical operation.
    pub fn harness_metric(&self, operation: &str) -> Option<&str> {
        self.entry(operation)
            .map(|entry| entry.harness_metric.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn committed_descriptor_parses_with_all_required_operations() {
        let file = committed_budgets();
        assert_eq!(file.version, BUDGET_VERSION);
        assert_eq!(file.reference_pages, REFERENCE_PAGES);
        assert_eq!(file.entries.len(), REQUIRED_OPERATIONS.len());
        for operation in REQUIRED_OPERATIONS {
            assert!(
                file.entry(operation).is_some(),
                "missing required operation {operation}"
            );
        }
    }

    #[test]
    fn budget_scaling_matches_the_xtask_formula() {
        let file = committed_budgets();
        let open = file
            .budget_ms("open", REFERENCE_PAGES)
            .expect("open budget");
        assert!((open - 1875.0).abs() < 1e-9, "open @500 = {open}");
        let open_100 = file.budget_ms("open", 100).expect("open budget");
        assert!((open_100 - 375.0).abs() < 1e-9, "open @100 = {open_100}");
        let open_1 = file.budget_ms("open", 1).expect("open budget");
        assert!((open_1 - 6.25).abs() < 1e-9, "open @1 = {open_1}");
        let render = file.budget_ms("render", REFERENCE_PAGES).expect("render");
        assert!((render - 187.5).abs() < 1e-9, "render @500 = {render}");
    }

    #[test]
    fn harness_metrics_are_stable() {
        let file = committed_budgets();
        assert_eq!(file.harness_metric("open"), Some("open"));
        assert_eq!(file.harness_metric("parse"), Some("open"));
        assert_eq!(file.harness_metric("render"), Some("first_page_render"));
        assert_eq!(file.harness_metric("text_extraction"), Some("text_extract"));
        assert_eq!(file.harness_metric("search"), Some("search_index_build"));
    }

    #[test]
    fn parser_rejects_duplicate_operations() {
        let text = committed_text().replace("operation = \"parse\"", "operation = \"open\"");
        assert!(matches!(
            parse_budgets(&text),
            Err(BudgetError::DuplicateOperation(_))
        ));
    }

    #[test]
    fn parser_rejects_non_positive_base_ms() {
        let text = committed_text().replace("base_ms = 1500.0", "base_ms = -1.0");
        assert!(matches!(
            parse_budgets(&text),
            Err(BudgetError::InvalidBaseMs(_))
        ));
    }

    #[test]
    fn parser_rejects_missing_required_operation() {
        let text = committed_text().replace("operation = \"search\"", "operation = \"other\"");
        assert!(matches!(
            parse_budgets(&text),
            Err(BudgetError::MissingOperation(_))
        ));
    }

    #[test]
    fn parser_rejects_unsupported_version() {
        let text = committed_text().replace("version = 1", "version = 99");
        assert!(matches!(
            parse_budgets(&text),
            Err(BudgetError::UnsupportedVersion(99))
        ));
    }

    #[test]
    fn parser_rejects_malformed_descriptor() {
        assert!(matches!(
            parse_budgets("version = 1\n"),
            Err(BudgetError::Malformed(_))
        ));
    }
}
