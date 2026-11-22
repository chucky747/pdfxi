//! `pdfxi script`: run a script in the deterministic Acrobat-compatible sandbox.
//!
//! [`pdfxi_automation::Script`] parses and interprets a dependency-free
//! JavaScript subset with explicit CPU, call, loop, string, array, object and
//! nesting budgets. This host reads a script file, seeds an in-memory
//! [`AcrobatContext`] from `--field`/`--event-*`, runs the script with the
//! documented limits, and reports the return value plus the final field store.
//!
//! Nothing in the sandbox can read a file, touch the network, start a process,
//! read the clock, or use randomness. The context is the only way a script can
//! observe or change form-field values, and the store is in memory only.

use std::path::Path;

use anyhow::{Context, bail};
use pdfxi_automation::{AcrobatContext, CancelToken, Limits, MAX_SCRIPT_BYTES, run_acrobat_script};
use serde::Serialize;

/// One final form field.
#[derive(Serialize)]
struct FieldRow {
    /// Field name.
    name: String,
    /// Field value.
    value: String,
}

/// Event state reported after the run.
#[derive(Serialize)]
struct EventReport {
    /// Event key, when seeded.
    key: String,
    /// Event value, when seeded.
    value: String,
    /// Event return code.
    rc: bool,
}

/// JSON shape of `pdfxi script run`.
#[derive(Serialize)]
struct ScriptReport {
    /// Script file.
    script: String,
    /// Script bytes.
    bytes: usize,
    /// CPU-step budget.
    max_steps: u64,
    /// Maximum nested call depth.
    max_call_depth: usize,
    /// Parsed return value rendered as JavaScript text.
    result: String,
    /// JavaScript type of the return value.
    result_type: String,
    /// Final form-field store in name order.
    fields: Vec<FieldRow>,
    /// Event state.
    event: EventReport,
}

/// Split `name=value`, rejecting an empty name.
fn parse_field(text: &str) -> anyhow::Result<(String, String)> {
    let (name, value) = text
        .split_once('=')
        .with_context(|| format!("field {text:?} must be NAME=VALUE"))?;
    if name.is_empty() {
        bail!("field {text:?} has an empty name");
    }
    Ok((name.to_owned(), value.to_owned()))
}

/// `pdfxi script run`.
pub(crate) fn run_script_file(
    script: &Path,
    fields: &[String],
    event_key: Option<&str>,
    event_value: Option<&str>,
    max_steps: Option<u64>,
    json: bool,
) -> anyhow::Result<()> {
    let source = std::fs::read_to_string(script)
        .with_context(|| format!("read script {}", script.display()))?;
    if source.len() > MAX_SCRIPT_BYTES {
        bail!(
            "script {} is {} bytes; the limit is {MAX_SCRIPT_BYTES}",
            script.display(),
            source.len()
        );
    }
    let mut context = AcrobatContext::new();
    for text in fields {
        let (name, value) = parse_field(text)?;
        context = context.with_field(name, value);
    }
    if let Some(key) = event_key {
        context = context.with_event_key(key);
    }
    if let Some(value) = event_value {
        context = context.with_event_value(value);
    }
    let mut limits = Limits::default();
    if let Some(steps) = max_steps {
        limits = limits.with_max_steps(steps);
    }
    let result = run_acrobat_script(&source, &limits, &CancelToken::new(), &context)
        .map_err(|error| anyhow::anyhow!("script: {error}"))?;
    let fields = context
        .try_field_snapshot()
        .map_err(|error| anyhow::anyhow!("field snapshot: {error}"))?
        .into_iter()
        .map(|(name, value)| FieldRow { name, value })
        .collect();
    let report = ScriptReport {
        script: script.display().to_string(),
        bytes: source.len(),
        max_steps: limits.max_steps,
        max_call_depth: limits.max_call_depth,
        result: result.display(),
        result_type: result.type_name().to_owned(),
        fields,
        event: EventReport {
            key: context.event_key(),
            value: context.event_value(),
            rc: context.event_rc(),
        },
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }
    println!("script: {}", report.script);
    println!(
        "result: {} ({})  bytes: {}  max_steps: {}",
        report.result, report.result_type, report.bytes, report.max_steps
    );
    for field in &report.fields {
        println!("field: {} = {}", field.name, field.value);
    }
    println!(
        "event: key={} value={} rc={}",
        report.event.key, report.event.value, report.event.rc
    );
    Ok(())
}
