//! `pdfxi handoff`: hand a local export to the browser.
//!
//! [`pdfxi_core::browser_handoff`] validates a local path or `file:` URL,
//! rejects traversal and unsupported schemes/MIME types, and resolves a
//! deterministic open/download plan with a stable `file:` URL. This host
//! exposes that plan, builds the HTML URL with an optional fragment, and can
//! optionally launch the platform browser for a validated local file. No
//! network access happens unless the launched browser makes it.

use std::path::Path;
use std::process::Command;

use anyhow::{Context, bail};
use pdfxi_core::browser_handoff::{BrowserHandoff, HandoffPlan, HandoffRequest, resolve_request};
use serde::Serialize;

/// JSON shape of one handoff plan.
#[derive(Serialize)]
struct HandoffReport {
    /// Requested input.
    input: String,
    /// Resolved local path.
    path: String,
    /// Deterministic `file:` URL.
    file_url: String,
    /// `open` or `download`.
    decision: &'static str,
    /// Declared MIME type, when any.
    #[serde(skip_serializing_if = "Option::is_none")]
    mime: Option<String>,
    /// Stable summary.
    summary: String,
    /// Whether the browser was launched.
    launched: bool,
}

/// Make a caller-supplied path absolute so the model's rooted-path rule is met.
///
/// Already-rooted paths are passed through unchanged so a `..` component is
/// still visible to (and rejected by) the handoff model; relative paths are
/// rejected when they contain traversal before being made absolute.
fn absolute(input: &str) -> anyhow::Result<String> {
    if input
        .split(['/', '\\'])
        .any(|component| component == ".." || component == ".")
    {
        bail!("path {input:?} contains traversal");
    }
    let path = Path::new(input);
    if path.is_absolute() {
        return Ok(input.to_owned());
    }
    let path = std::path::absolute(path)
        .with_context(|| format!("resolve absolute path for {input:?}"))?;
    Ok(path.display().to_string())
}

/// Build the report for one resolved plan.
fn report(input: &str, plan: &HandoffPlan, launched: bool) -> HandoffReport {
    HandoffReport {
        input: input.to_owned(),
        path: plan.path().to_owned(),
        file_url: plan.file_url().to_owned(),
        decision: plan.decision().as_str(),
        mime: plan.mime().map(|mime| mime.as_str().to_owned()),
        summary: plan.summary().to_owned(),
        launched,
    }
}

/// Print one report.
fn print(report: &HandoffReport) {
    println!("input: {}", report.input);
    println!(
        "path: {}  decision: {}  mime: {}",
        report.path,
        report.decision,
        report.mime.as_deref().unwrap_or("-")
    );
    println!("url: {}", report.file_url);
    if report.launched {
        println!("launched: yes");
    }
}

/// Launch the platform browser for a validated `file:` URL.
fn launch(url: &str) -> anyhow::Result<()> {
    let mut command = if cfg!(windows) {
        let mut command = Command::new("cmd");
        command.args(["/C", "start", ""]).arg(url);
        command
    } else if cfg!(target_os = "macos") {
        let mut command = Command::new("open");
        command.arg(url);
        command
    } else {
        let mut command = Command::new("xdg-open");
        command.arg(url);
        command
    };
    command
        .spawn()
        .with_context(|| format!("launch the browser for {url}"))?;
    Ok(())
}

/// `pdfxi handoff open`.
pub(crate) fn open(
    input: &str,
    mime: Option<&str>,
    launch_browser: bool,
    json: bool,
) -> anyhow::Result<()> {
    let input = absolute(input)?;
    if !Path::new(&input).exists() {
        bail!("file {input} does not exist");
    }
    let mut request = HandoffRequest::from_path(&input)
        .map_err(|error| anyhow::anyhow!("handoff request: {error}"))?;
    if let Some(mime) = mime {
        request = request
            .with_mime(mime)
            .map_err(|error| anyhow::anyhow!("handoff MIME: {error}"))?;
    }
    let plan =
        resolve_request(&request).map_err(|error| anyhow::anyhow!("resolve handoff: {error}"))?;
    if launch_browser {
        launch(plan.file_url())?;
    }
    let report = report(&input, &plan, launch_browser);
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }
    print(&report);
    Ok(())
}

/// `pdfxi handoff parse`.
pub(crate) fn parse(input: &str, json: bool) -> anyhow::Result<()> {
    let resolved = if input.contains("://") || input.to_ascii_lowercase().starts_with("file:") {
        input.to_owned()
    } else {
        absolute(input)?
    };
    if !resolved.contains("://") && !Path::new(&resolved).exists() {
        bail!("file {resolved} does not exist");
    }
    let request = HandoffRequest::parse(&resolved)
        .map_err(|error| anyhow::anyhow!("handoff request: {error}"))?;
    let plan =
        resolve_request(&request).map_err(|error| anyhow::anyhow!("resolve handoff: {error}"))?;
    if !Path::new(plan.path()).exists() {
        bail!("file {} does not exist", plan.path());
    }
    let report = report(&resolved, &plan, false);
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }
    print(&report);
    Ok(())
}

/// `pdfxi handoff link`.
pub(crate) fn link(file: &Path, fragment: Option<&str>, json: bool) -> anyhow::Result<()> {
    let input = absolute(&file.display().to_string())?;
    if !file.exists() {
        bail!("HTML file {} does not exist", file.display());
    }
    let mut handoff =
        BrowserHandoff::new(&input).map_err(|error| anyhow::anyhow!("HTML handoff: {error}"))?;
    if let Some(fragment) = fragment {
        handoff = handoff
            .with_fragment(fragment)
            .map_err(|error| anyhow::anyhow!("handoff fragment: {error}"))?;
    }
    let plan = handoff.resolve_plan();
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "input": input,
                "file_url": plan.file_url(),
                "summary": plan.summary(),
            }))?
        );
        return Ok(());
    }
    println!("url: {}", plan.file_url());
    println!("summary: {}", plan.summary());
    Ok(())
}
