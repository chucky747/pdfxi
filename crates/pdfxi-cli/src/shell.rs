//! `pdfxi shell`: file associations and the `pdfxi://` protocol handler.
//!
//! [`pdfxi_core::association`] validates descriptor registries (extension ->
//! ProgID -> command template and scheme -> ProgID -> template) and exports a
//! deterministic descriptor document. This host turns that model into real
//! per-user Windows registry keys under a caller-chosen root (default
//! `HKCU\Software\Classes`), reports their status, and removes them. It never
//! writes machine-wide (`HKLM`) state and never touches a key outside the root.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, bail};
use pdfxi_core::association::{
    CommandTemplate, DescriptorRegistry, ProgId, ProtocolDescriptor, ShellDescriptor,
    export_descriptors,
};
use serde::Serialize;

/// ProgID used for PDF documents.
const PDF_PROGID: &str = "PDFXI.pdf";
/// ProgID used for the `pdfxi://` protocol.
const PROTOCOL_PROGID: &str = "PDFXI.protocol";
/// Extension associated with the viewer.
const EXTENSION: &str = ".pdf";
/// URL scheme handled by the viewer.
const SCHEME: &str = "pdfxi";

/// A real per-user registry plan/report.
#[derive(Serialize)]
struct ShellReport {
    /// Registry root used.
    root: String,
    /// Executable registered.
    exe: String,
    /// Extension.
    extension: String,
    /// Extension ProgID.
    progid: String,
    /// Protocol scheme.
    scheme: String,
    /// Protocol ProgID.
    protocol_progid: String,
    /// Whether the extension resolves to the ProgID.
    association_installed: bool,
    /// Whether the ProgID command exists.
    association_command_installed: bool,
    /// Whether the protocol handler exists.
    protocol_installed: bool,
    /// Descriptor document lines.
    descriptors: Vec<String>,
}

/// Resolve the executable to register.
fn executable(exe: Option<&Path>) -> anyhow::Result<PathBuf> {
    let path = match exe {
        Some(path) => path.to_path_buf(),
        None => std::env::current_exe().context("resolve the current executable")?,
    };
    if !path.exists() {
        bail!("executable {} does not exist", path.display());
    }
    Ok(path)
}

/// Validate the registry root.
fn validate_root(root: &str) -> anyhow::Result<()> {
    let lowered = root.to_ascii_lowercase();
    if !lowered.starts_with("hkcu\\software\\") {
        bail!(
            "registry root {root:?} must be a per-user path under \
             HKCU\\Software\\ (machine-wide HKLM is never written)"
        );
    }
    if root.contains('"') || root.contains('\'') {
        bail!("registry root must not contain quotes");
    }
    Ok(())
}

/// Build the descriptor registry for one executable.
fn registry(exe: &Path) -> anyhow::Result<DescriptorRegistry> {
    let program = exe.display().to_string();
    let template = format!("\"{program}\" \"%1\"");
    let command = CommandTemplate::new(&template)
        .map_err(|error| anyhow::anyhow!("command template: {error}"))?;
    let association = ShellDescriptor::new(
        EXTENSION,
        ProgId::new(PDF_PROGID).map_err(|error| anyhow::anyhow!("ProgID: {error}"))?,
        command,
        "PDF document",
    )
    .map_err(|error| anyhow::anyhow!("shell descriptor: {error}"))?;
    let protocol_template = format!("\"{program}\" \"%1\"");
    let protocol_command = CommandTemplate::new(&protocol_template)
        .map_err(|error| anyhow::anyhow!("protocol template: {error}"))?;
    let protocol = ProtocolDescriptor::new(
        SCHEME,
        ProgId::new(PROTOCOL_PROGID).map_err(|error| anyhow::anyhow!("ProgID: {error}"))?,
        protocol_command,
        "PDFXI handoff URL",
    )
    .map_err(|error| anyhow::anyhow!("protocol descriptor: {error}"))?;
    DescriptorRegistry::new(vec![association], vec![protocol])
        .map_err(|error| anyhow::anyhow!("descriptor registry: {error}"))
}

/// Run `reg.exe` and return stdout.
fn reg(args: &[String]) -> anyhow::Result<String> {
    let output = Command::new("reg")
        .args(args)
        .output()
        .context("run reg.exe")?;
    if !output.status.success() {
        bail!(
            "reg {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Write one default registry string value.
fn set_default(path: &str, value: &str) -> anyhow::Result<()> {
    reg(&[
        "add".to_owned(),
        path.to_owned(),
        "/ve".to_owned(),
        "/t".to_owned(),
        "REG_SZ".to_owned(),
        "/d".to_owned(),
        value.to_owned(),
        "/f".to_owned(),
    ])?;
    Ok(())
}

/// Read one default registry string value.
fn get_default(path: &str) -> Option<String> {
    let output = Command::new("reg")
        .args(["query", path, "/ve"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let line = text.lines().find(|line| line.contains("REG_SZ"))?;
    let value = line.split("REG_SZ").nth(1)?.trim();
    Some(value.to_owned())
}

/// Remove a registry key tree.
fn delete_tree(path: &str) -> anyhow::Result<()> {
    let output = Command::new("reg")
        .args(["delete", path, "/f"])
        .output()
        .context("run reg delete")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if !stderr.contains("unable to find") {
            bail!("reg delete {path} failed: {}", stderr.trim());
        }
    }
    Ok(())
}

/// Build the status report.
fn status(root: &str, exe: Option<&Path>) -> anyhow::Result<ShellReport> {
    let exe = executable(exe)?;
    let registry = registry(&exe)?;
    let document = export_descriptors(&registry);
    let extension_key = format!("{root}\\{EXTENSION}");
    let progid_key = format!("{root}\\{PDF_PROGID}");
    let command_key = format!("{progid_key}\\shell\\open\\command");
    let protocol_key = format!("{root}\\{SCHEME}");
    let extension_value = get_default(&extension_key);
    let command_value = get_default(&command_key);
    Ok(ShellReport {
        root: root.to_owned(),
        exe: exe.display().to_string(),
        extension: EXTENSION.to_owned(),
        progid: PDF_PROGID.to_owned(),
        scheme: SCHEME.to_owned(),
        protocol_progid: PROTOCOL_PROGID.to_owned(),
        association_installed: extension_value.as_deref() == Some(PDF_PROGID),
        association_command_installed: command_value.is_some(),
        protocol_installed: get_default(&protocol_key).is_some(),
        descriptors: document.lines().to_vec(),
    })
}

/// Print a status report.
fn print(report: &ShellReport) {
    println!("root: {}  exe: {}", report.root, report.exe);
    println!(
        ".{} -> {}: {}  command: {}",
        report.extension,
        report.progid,
        report.association_installed,
        report.association_command_installed
    );
    println!(
        "{}:// -> {}: {}",
        report.scheme, report.protocol_progid, report.protocol_installed
    );
}

/// `pdfxi shell plan`.
pub(crate) fn plan(exe: Option<&Path>, json: bool) -> anyhow::Result<()> {
    let exe = executable(exe)?;
    let registry = registry(&exe)?;
    let document = export_descriptors(&registry);
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "exe": exe.display().to_string(),
                "lines": document.lines(),
                "summary": document.summary(),
            }))?
        );
        return Ok(());
    }
    for line in document.lines() {
        println!("{line}");
    }
    println!("summary: {}", document.summary());
    Ok(())
}

/// `pdfxi shell register`.
pub(crate) fn register(root: &str, exe: Option<&Path>, json: bool) -> anyhow::Result<()> {
    validate_root(root)?;
    let exe = executable(exe)?;
    let registry = registry(&exe)?;
    let Some(association) = registry.associations().first() else {
        bail!("descriptor registry has no association");
    };
    let Some(protocol) = registry.protocols().first() else {
        bail!("descriptor registry has no protocol");
    };
    let progid_key = format!("{root}\\{PDF_PROGID}");
    set_default(&format!("{root}\\{EXTENSION}"), PDF_PROGID)?;
    set_default(&progid_key, "PDF document")?;
    set_default(
        &format!("{progid_key}\\shell\\open\\command"),
        association.command().as_str(),
    )?;
    let protocol_key = format!("{root}\\{SCHEME}");
    set_default(&protocol_key, "URL:PDFXI Protocol")?;
    reg(&[
        "add".to_owned(),
        protocol_key.clone(),
        "/v".to_owned(),
        "URL Protocol".to_owned(),
        "/t".to_owned(),
        "REG_SZ".to_owned(),
        "/f".to_owned(),
    ])?;
    set_default(
        &format!("{protocol_key}\\shell\\open\\command"),
        protocol.command().as_str(),
    )?;
    let report = status(root, Some(&exe))?;
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print(&report);
        println!("registered: {}", root);
    }
    Ok(())
}

/// `pdfxi shell unregister`.
pub(crate) fn unregister(root: &str, exe: Option<&Path>, json: bool) -> anyhow::Result<()> {
    validate_root(root)?;
    let progid_key = format!("{root}\\{PDF_PROGID}");
    let protocol_key = format!("{root}\\{SCHEME}");
    let extension_key = format!("{root}\\{EXTENSION}");
    if get_default(&extension_key).as_deref() == Some(PDF_PROGID) {
        delete_tree(&extension_key)?;
    }
    delete_tree(&progid_key)?;
    delete_tree(&protocol_key)?;
    let report = status(root, exe)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print(&report);
        println!("unregistered: {}", root);
    }
    Ok(())
}

/// `pdfxi shell status`.
pub(crate) fn show(root: &str, exe: Option<&Path>, json: bool) -> anyhow::Result<()> {
    validate_root(root)?;
    let report = status(root, exe)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print(&report);
    }
    Ok(())
}
