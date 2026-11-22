//! `pdfxi ops`: enumerate and resolve scriptable automation operations.
//!
//! [`pdfxi_automation::OperationRegistry`] models the "CLI for every scriptable
//! operation" contract: every supported command, its typed parameters, and a
//! validated call plan. This host is the CLI adapter the model intentionally
//! left out. Listing is deterministic (alphabetical); resolution is pure and
//! never executes anything.

use anyhow::bail;
use pdfxi_automation::{OperationRegistry, ParamType, TypedValue};
use serde::Serialize;

/// One parameter row.
#[derive(Serialize)]
struct ParamRow {
    /// Parameter name used in `name=value`.
    name: &'static str,
    /// Stable type label.
    ty: &'static str,
    /// Whether the parameter must be supplied.
    required: bool,
}

/// One operation row.
#[derive(Serialize)]
struct OperationRow {
    /// Stable operation name.
    name: &'static str,
    /// Typed parameters in declaration order.
    params: Vec<ParamRow>,
}

/// One resolved argument.
#[derive(Serialize)]
struct ArgumentRow {
    /// Parameter name.
    name: String,
    /// Stable type label.
    ty: &'static str,
    /// Rendered value.
    value: String,
}

/// JSON shape of `pdfxi ops resolve`.
#[derive(Serialize)]
struct ResolveReport {
    /// Resolved operation.
    operation: String,
    /// Arguments in alphabetical key order.
    arguments: Vec<ArgumentRow>,
    /// Stable one-line rendering.
    summary: String,
}

/// `pdfxi ops list`.
pub(crate) fn run_list(json: bool) -> anyhow::Result<()> {
    let registry = OperationRegistry::with_defaults();
    let rows: Vec<OperationRow> = registry
        .operations()
        .map(|operation| OperationRow {
            name: operation.name,
            params: operation
                .params
                .iter()
                .map(|param| ParamRow {
                    name: param.name,
                    ty: param.ty.as_str(),
                    required: param.required,
                })
                .collect(),
        })
        .collect();
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "operations": rows,
                "count": rows.len(),
            }))?
        );
        return Ok(());
    }
    for operation in &rows {
        let params: Vec<String> = operation
            .params
            .iter()
            .map(|param| {
                format!(
                    "{}{}:{}",
                    param.name,
                    if param.required { "" } else { "?" },
                    param.ty
                )
            })
            .collect();
        if params.is_empty() {
            println!("{}", operation.name);
        } else {
            println!("{}\t{}", operation.name, params.join(","));
        }
    }
    println!("operations: {}", rows.len());
    Ok(())
}

/// Return the stable type label for a resolved value.
fn value_type(value: &TypedValue) -> &'static str {
    match value {
        TypedValue::String(_) => ParamType::String.as_str(),
        TypedValue::Bool(_) => ParamType::Bool.as_str(),
        TypedValue::Integer(_) => ParamType::Integer.as_str(),
        TypedValue::Path(_) => ParamType::Path.as_str(),
    }
}

/// `pdfxi ops resolve`.
pub(crate) fn run_resolve(name: &str, assignments: &[String], json: bool) -> anyhow::Result<()> {
    if name.trim().is_empty() {
        bail!("the operation name must not be empty");
    }
    let registry = OperationRegistry::with_defaults();
    let mut args = Vec::with_capacity(assignments.len() + 1);
    args.push(name.to_owned());
    args.extend(assignments.iter().cloned());
    let plan = registry
        .resolve(&args)
        .map_err(|error| anyhow::anyhow!("{error}"))?;
    let report = ResolveReport {
        operation: plan.operation().to_owned(),
        arguments: plan
            .arguments()
            .map(|(name, value)| ArgumentRow {
                name: name.to_owned(),
                ty: value_type(value),
                value: value.to_string(),
            })
            .collect(),
        summary: plan.summary(),
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }
    println!("operation: {}", report.operation);
    for argument in &report.arguments {
        println!("{}\t{}\t{}", argument.name, argument.ty, argument.value);
    }
    println!("arguments: {}", report.arguments.len());
    println!("summary: {}", report.summary);
    Ok(())
}
