//! `pdfxi plugin`: plan a plugin invocation and validate typed outputs.
//!
//! `pdfxi_automation::plugin_output` builds a deterministic execution plan
//! from the manifest/invocation model and validates bounded typed results, but
//! loads no WebAssembly. This host reads a real manifest file, maps capability
//! and budget flags, and prints the plan or output verdict. No plugin code is
//! executed and no I/O beyond reading the manifest occurs.

use std::collections::BTreeSet;
use std::path::Path;

use anyhow::{Context, bail};
use pdfxi_automation::{
    Capability, CapabilityGrants, InvocationBudget, LocalPluginHost, OutputBudget, OutputType,
    PluginInvocation, PluginManifest, PluginOutput, PluginRegistry, PluginValue,
    build_execution_plan,
};
use serde::Serialize;

/// Parse an output type label.
fn parse_output_type(text: &str) -> anyhow::Result<OutputType> {
    match text.to_ascii_lowercase().as_str() {
        "null" => Ok(OutputType::Null),
        "bool" | "boolean" => Ok(OutputType::Bool),
        "int" | "integer" => Ok(OutputType::Int),
        "text" | "string" => Ok(OutputType::Text),
        "bytes" => Ok(OutputType::Bytes),
        other => bail!("unknown type {other:?}; expected null, bool, int, text or bytes"),
    }
}

/// One execution-plan step.
#[derive(Serialize)]
struct StepRow {
    /// Step kind.
    kind: &'static str,
    /// Whether the step passed.
    allowed: bool,
    /// Sorted deny codes.
    codes: Vec<String>,
}

/// JSON shape of `pdfxi plugin plan`.
#[derive(Serialize)]
struct PlanReport {
    /// Manifest file.
    file: String,
    /// Plugin id.
    plugin_id: String,
    /// Plugin display name.
    name: String,
    /// Plugin version.
    version: String,
    /// Declared ABI version.
    api_version: u64,
    /// Required capabilities.
    capabilities: Vec<String>,
    /// Planned operation.
    operation: String,
    /// Expected output type.
    expected: &'static str,
    /// Whether every step passed.
    allowed: bool,
    /// Ordered steps.
    steps: Vec<StepRow>,
    /// Stable multi-line summary.
    summary: String,
}

/// `pdfxi plugin plan`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn run_plan(
    manifest_path: &Path,
    operation: &str,
    grants: &[String],
    expected: &str,
    max_steps: u64,
    max_memory_bytes: u64,
    max_wall_clock_ms: u64,
    max_text_bytes: usize,
    max_bytes: usize,
    max_total_bytes: usize,
    json: bool,
) -> anyhow::Result<()> {
    let text = std::fs::read_to_string(manifest_path)
        .with_context(|| format!("read manifest {}", manifest_path.display()))?;
    let manifest = PluginManifest::parse_json(&text).map_err(|error| {
        anyhow::anyhow!("invalid manifest {}: {error}", manifest_path.display())
    })?;
    let mut registry = PluginRegistry::new();
    registry
        .register(manifest.clone())
        .map_err(|error| anyhow::anyhow!("register plugin: {error}"))?;

    let mut grant_set = CapabilityGrants::new();
    for name in grants {
        let capability = Capability::parse(name)
            .map_err(|error| anyhow::anyhow!("--grant {name:?}: {error}"))?;
        grant_set.grant(capability);
    }
    let budget = InvocationBudget {
        max_steps,
        max_memory_bytes,
        max_wall_clock_ms,
    };
    budget
        .validate()
        .map_err(|error| anyhow::anyhow!("invocation budget: {error}"))?;
    let host = LocalPluginHost::new()
        .with_grants(grant_set)
        .with_budget_limits(budget);
    let invocation = PluginInvocation::new(manifest.id(), operation, budget)
        .map_err(|error| anyhow::anyhow!("invocation: {error}"))?;
    let expected = parse_output_type(expected)?;
    let output_budget = OutputBudget {
        max_text_bytes,
        max_bytes,
        max_total_bytes,
    };
    let plan = build_execution_plan(&host, &registry, &invocation, expected, output_budget);
    let capabilities: BTreeSet<&str> = manifest
        .required_capabilities()
        .iter()
        .map(|capability| capability.as_str())
        .collect();
    let report = PlanReport {
        file: manifest_path.display().to_string(),
        plugin_id: manifest.id().to_owned(),
        name: manifest.name().to_owned(),
        version: manifest.version().to_owned(),
        api_version: manifest.api_version(),
        capabilities: capabilities.into_iter().map(str::to_owned).collect(),
        operation: plan.operation().to_owned(),
        expected: plan.expected_output().as_str(),
        allowed: plan.is_allowed(),
        steps: plan
            .steps()
            .iter()
            .map(|step| StepRow {
                kind: step.kind().as_str(),
                allowed: step.is_allowed(),
                codes: step.codes().to_vec(),
            })
            .collect(),
        summary: plan.summary(),
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!("file: {}", report.file);
        println!(
            "plugin: {}  {}  v{}  api {}",
            report.plugin_id, report.name, report.version, report.api_version
        );
        println!("capabilities: {}", report.capabilities.join(","));
        println!(
            "plan: {}  expect {}  {}",
            report.operation,
            report.expected,
            if report.allowed { "allowed" } else { "denied" }
        );
        for step in &report.steps {
            if step.allowed {
                println!("  {}: allow", step.kind);
            } else {
                println!("  {}: deny({})", step.kind, step.codes.join(","));
            }
        }
    }
    if !report.allowed {
        std::process::exit(2);
    }
    Ok(())
}

/// JSON shape of `pdfxi plugin output`.
#[derive(Serialize)]
struct OutputReport {
    /// Producing plugin id.
    plugin_id: String,
    /// Operation name.
    operation: String,
    /// Value type.
    ty: &'static str,
    /// Expected type.
    expected: &'static str,
    /// Encoded length used by budget accounting.
    encoded_len: usize,
    /// Rendered value (`bytes(N)` for opaque bytes).
    value: String,
    /// Stable summary.
    summary: String,
    /// Always `true` on success.
    valid: bool,
}

/// Build the typed value from CLI fields.
fn build_value(
    ty: OutputType,
    text: Option<&str>,
    text_len: Option<usize>,
    int: Option<i64>,
    bool_value: Option<bool>,
    bytes_len: Option<usize>,
) -> anyhow::Result<PluginValue> {
    Ok(match ty {
        OutputType::Null => PluginValue::Null,
        OutputType::Bool => PluginValue::Bool(
            bool_value.ok_or_else(|| anyhow::anyhow!("--type bool needs --bool true|false"))?,
        ),
        OutputType::Int => {
            PluginValue::Int(int.ok_or_else(|| anyhow::anyhow!("--type int needs --int N"))?)
        }
        OutputType::Text => {
            let value = match (text, text_len) {
                (Some(text), _) => text.to_owned(),
                (None, Some(len)) => "x".repeat(len),
                (None, None) => bail!("--type text needs --text TEXT or --text-len N"),
            };
            PluginValue::Text(value)
        }
        OutputType::Bytes => {
            let len =
                bytes_len.ok_or_else(|| anyhow::anyhow!("--type bytes needs --bytes-len N"))?;
            PluginValue::Bytes(vec![0u8; len])
        }
    })
}

/// Render a plugin value for the report.
fn render_value(value: &PluginValue) -> String {
    match value {
        PluginValue::Null => "null".to_owned(),
        PluginValue::Bool(flag) => flag.to_string(),
        PluginValue::Int(number) => number.to_string(),
        PluginValue::Text(text) => text.clone(),
        PluginValue::Bytes(bytes) => format!("bytes({})", bytes.len()),
    }
}

/// `pdfxi plugin output`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn run_output(
    plugin_id: &str,
    operation: &str,
    ty: &str,
    text: Option<&str>,
    text_len: Option<usize>,
    int: Option<i64>,
    bool_value: Option<bool>,
    bytes_len: Option<usize>,
    expected: &str,
    max_text_bytes: usize,
    max_bytes: usize,
    max_total_bytes: usize,
    json: bool,
) -> anyhow::Result<()> {
    let ty = parse_output_type(ty)?;
    let expected = parse_output_type(expected)?;
    if plugin_id.trim().is_empty() {
        bail!("--plugin-id must not be empty");
    }
    if operation.trim().is_empty() {
        bail!("--operation must not be empty");
    }
    let value = build_value(ty, text, text_len, int, bool_value, bytes_len)?;
    let output = PluginOutput::new(plugin_id, operation, value)
        .map_err(|error| anyhow::anyhow!("plugin output: {error}"))?;
    let budget = OutputBudget {
        max_text_bytes,
        max_bytes,
        max_total_bytes,
    };
    output
        .validate_expected(expected, &budget)
        .map_err(|error| anyhow::anyhow!("plugin output: {error}"))?;
    let report = OutputReport {
        plugin_id: output.plugin_id().to_owned(),
        operation: output.operation().to_owned(),
        ty: output.value().output_type().as_str(),
        expected: expected.as_str(),
        encoded_len: output.value().encoded_len(),
        value: render_value(output.value()),
        summary: output.summary(),
        valid: true,
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }
    println!("plugin: {}", report.plugin_id);
    println!("operation: {}", report.operation);
    println!("type: {}\texpected: {}", report.ty, report.expected);
    println!("encoded_len: {}", report.encoded_len);
    println!("value: {}", report.value);
    println!("valid: true");
    Ok(())
}
