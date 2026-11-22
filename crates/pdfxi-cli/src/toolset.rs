//! `pdfxi toolset`: parse, validate, inspect and run named tool aliases.
//!
//! [`pdfxi_automation::Toolset`] maps friendly aliases to saved actions and
//! parameter defaults, rejecting dangerous aliases at validation time. This
//! host reads a real toolset file, resolves relative action paths against the
//! toolset's directory, refuses dangerous step kinds at run time (mirroring the
//! API), and delegates execution to the same staging executor as
//! `pdfxi action run`.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, bail};
use pdfxi_automation::{ActionLibrary, DANGEROUS_KINDS, Tool, Toolset};
use serde::Serialize;

/// One tool row.
#[derive(Serialize)]
struct ToolRow {
    /// Friendly alias.
    alias: String,
    /// Action file as written in the toolset.
    file: String,
    /// Action name inside the file.
    action: String,
    /// Defaults in sorted key order.
    defaults: BTreeMap<String, String>,
}

/// JSON shape of `pdfxi toolset list`.
#[derive(Serialize)]
struct ListReport {
    /// Toolset file.
    file: String,
    /// Toolset name.
    name: String,
    /// Tools in alias order.
    tools: Vec<ToolRow>,
    /// Tool count.
    count: usize,
}

/// Parse a toolset file with path context.
fn load_toolset(file: &Path) -> anyhow::Result<Toolset> {
    let text = std::fs::read_to_string(file)
        .with_context(|| format!("read toolset {}", file.display()))?;
    Toolset::from_json(&text)
        .map_err(|error| anyhow::anyhow!("invalid toolset {}: {error}", file.display()))
}

/// Convert a tool into a report row.
fn row(alias: &str, tool: &Tool) -> ToolRow {
    ToolRow {
        alias: alias.to_owned(),
        file: tool.file().to_owned(),
        action: tool.action().to_owned(),
        defaults: tool.defaults().clone(),
    }
}

/// `pdfxi toolset list`.
pub(crate) fn run_list(file: &Path, json: bool) -> anyhow::Result<()> {
    let toolset = load_toolset(file)?;
    let report = ListReport {
        file: file.display().to_string(),
        name: toolset.name().to_owned(),
        tools: toolset
            .tools()
            .map(|(alias, tool)| row(alias, tool))
            .collect(),
        count: toolset.len(),
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }
    println!("file: {}", report.file);
    println!("name: {}", report.name);
    for tool in &report.tools {
        println!(
            "{}\t{}\t{}{}",
            tool.alias,
            tool.file,
            tool.action,
            if tool.defaults.is_empty() {
                String::new()
            } else {
                format!(
                    "\tdefaults: {}",
                    tool.defaults
                        .iter()
                        .map(|(key, value)| format!("{key}={value}"))
                        .collect::<Vec<_>>()
                        .join(" ")
                )
            }
        );
    }
    println!("tools: {}", report.count);
    Ok(())
}

/// `pdfxi toolset validate`.
pub(crate) fn run_validate(file: &Path, json: bool) -> anyhow::Result<()> {
    let toolset = load_toolset(file)?;
    toolset
        .validate()
        .map_err(|error| anyhow::anyhow!("toolset {}: {error}", file.display()))?;
    let payload = serde_json::json!({
        "file": file.display().to_string(),
        "name": toolset.name(),
        "tools": toolset.len(),
        "valid": true,
    });
    if json {
        println!("{}", serde_json::to_string_pretty(&payload)?);
        return Ok(());
    }
    println!("file: {}", file.display());
    println!("name: {}", toolset.name());
    println!("tools: {}", toolset.len());
    println!("valid: true");
    Ok(())
}

/// `pdfxi toolset show`.
pub(crate) fn run_show(file: &Path, alias: &str, json: bool) -> anyhow::Result<()> {
    let toolset = load_toolset(file)?;
    let tool = toolset.get(alias).ok_or_else(|| {
        let names: Vec<&str> = toolset.aliases().collect();
        anyhow::anyhow!(
            "toolset '{}' has no tool named '{alias}'; available tools: {}",
            toolset.name(),
            if names.is_empty() {
                "<none>".to_owned()
            } else {
                names.join(", ")
            }
        )
    })?;
    let report = row(alias, tool);
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }
    println!("alias: {}", report.alias);
    println!("file: {}", report.file);
    println!("action: {}", report.action);
    for (key, value) in &report.defaults {
        println!("default\t{key}\t{value}");
    }
    println!("defaults: {}", report.defaults.len());
    Ok(())
}

/// Resolve a tool's action file relative to the toolset file.
fn resolve_action_file(toolset: &Path, file: &str) -> anyhow::Result<PathBuf> {
    let candidate = Path::new(file);
    if candidate.is_absolute() {
        return Ok(candidate.to_path_buf());
    }
    for component in candidate.components() {
        if !matches!(component, Component::Normal(_) | Component::CurDir) {
            bail!("tool action file {file:?} escapes the toolset directory");
        }
    }
    let parent = toolset.parent().unwrap_or_else(|| Path::new("."));
    Ok(parent.join(candidate))
}

/// `pdfxi toolset run`.
pub(crate) fn run_tool(
    file: &Path,
    alias: &str,
    inputs: &[PathBuf],
    output_dir: Option<&Path>,
    json: bool,
) -> anyhow::Result<()> {
    let toolset = load_toolset(file)?;
    let tool = toolset.get(alias).ok_or_else(|| {
        let names: Vec<&str> = toolset.aliases().collect();
        anyhow::anyhow!(
            "toolset '{}' has no tool named '{alias}'; available tools: {}",
            toolset.name(),
            if names.is_empty() {
                "<none>".to_owned()
            } else {
                names.join(", ")
            }
        )
    })?;
    let action_file = resolve_action_file(file, tool.file())?;
    let text = std::fs::read_to_string(&action_file)
        .with_context(|| format!("read action file {}", action_file.display()))?;
    let library = ActionLibrary::from_text(&text).map_err(|error| {
        anyhow::anyhow!("invalid action file {}: {error}", action_file.display())
    })?;
    let action = library.get(tool.action()).ok_or_else(|| {
        let names: Vec<&str> = library.names().collect();
        anyhow::anyhow!(
            "no action named '{}' in '{}'; available actions: {}",
            tool.action(),
            action_file.display(),
            if names.is_empty() {
                "<none>".to_owned()
            } else {
                names.join(", ")
            }
        )
    })?;
    for step in action.steps() {
        if DANGEROUS_KINDS.contains(&step.kind()) {
            bail!(
                "tool '{alias}' aliases a dangerous operation ('{}' step '{}'); it cannot be run through a toolset",
                step.kind().as_str(),
                step.id()
            );
        }
    }
    let defaults: Vec<(String, String)> = tool
        .defaults()
        .iter()
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    crate::action::run_action_with_defaults(
        &action_file,
        tool.action(),
        &defaults,
        inputs,
        output_dir,
        json,
    )
}
