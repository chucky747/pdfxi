//! `pdfxi policy`: deployment preferences and customization enforcement.
//!
//! [`pdfxi_core::preferences`] models a validated, bounded deployment document
//! (base preferences, named feature toggles, allowed local paths and an update
//! channel) with deterministic serialization and merge semantics. This host
//! reads and writes that document and checks a requested runtime setting
//! against it. No registry, MSI transform or policy engine is involved; the host
//! is the deployment tool boundary the model documents.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, bail};
use pdfxi_core::preferences::{
    DeploymentPreferences, Preferences, SCHEMA_VERSION, Theme, UpdateChannel, parse_preferences,
    serialize_preferences,
};
use serde::Serialize;

/// JSON shape of an exported deployment policy.
#[derive(Serialize)]
struct PolicyJson {
    /// Base theme.
    theme: String,
    /// Base locale.
    locale: String,
    /// Base recent-document count.
    recent_count: u32,
    /// Base toolbar visibility.
    show_toolbar: bool,
    /// Base default zoom percent.
    default_zoom_percent: u16,
    /// Base autosave interval in seconds.
    autosave_seconds: u32,
    /// Feature toggles.
    toggles: BTreeMap<String, bool>,
    /// Allowed local path prefixes.
    allowed_paths: Vec<String>,
    /// Update channel.
    channel: String,
    /// Serialized document, when requested.
    #[serde(skip_serializing_if = "Option::is_none")]
    document: Option<String>,
}

/// Read a deployment document.
fn load(path: &Path) -> anyhow::Result<DeploymentPreferences> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("read policy {}", path.display()))?;
    DeploymentPreferences::parse(&text)
        .map_err(|error| anyhow::anyhow!("parse policy {}: {error}", path.display()))
}

/// Build the JSON view.
fn json(policy: &DeploymentPreferences, document: Option<String>) -> PolicyJson {
    let base = policy.base();
    PolicyJson {
        theme: base.theme().as_str().to_owned(),
        locale: base.locale().to_owned(),
        recent_count: base.recent_count(),
        show_toolbar: base.show_toolbar(),
        default_zoom_percent: base.default_zoom_percent(),
        autosave_seconds: base.autosave_seconds(),
        toggles: policy.toggles().clone(),
        allowed_paths: policy.allowed_paths().to_vec(),
        channel: policy.update_channel().as_str().to_owned(),
        document,
    }
}

/// Print the text view.
fn print(policy: &DeploymentPreferences) {
    let base = policy.base();
    println!(
        "theme: {}  locale: {}  channel: {}",
        base.theme().as_str(),
        base.locale(),
        policy.update_channel().as_str()
    );
    println!(
        "recents: {}  toolbar: {}  zoom: {}%  autosave: {}s",
        base.recent_count(),
        base.show_toolbar(),
        base.default_zoom_percent(),
        base.autosave_seconds()
    );
    for (name, enabled) in policy.toggles() {
        println!("toggle: {name}={enabled}");
    }
    for path in policy.allowed_paths() {
        println!("allowed-path: {path}");
    }
}

/// Parse a `NAME=true|false` toggle.
fn parse_toggle(value: &str) -> anyhow::Result<(String, bool)> {
    let (name, enabled) = value
        .split_once('=')
        .with_context(|| format!("toggle {value:?} must be NAME=true|false"))?;
    let enabled = match enabled {
        "true" => true,
        "false" => false,
        other => bail!("toggle {name:?} has invalid boolean {other:?}; use true or false"),
    };
    Ok((name.to_owned(), enabled))
}

/// `pdfxi policy show`.
pub(crate) fn show(file: &Path, json_output: bool) -> anyhow::Result<()> {
    let policy = load(file)?;
    let document = std::fs::read_to_string(file).ok();
    if json_output {
        println!(
            "{}",
            serde_json::to_string_pretty(&json(&policy, document))?
        );
        return Ok(());
    }
    print(&policy);
    Ok(())
}

/// `pdfxi policy write`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn write(
    out: &Path,
    theme: &str,
    locale: &str,
    recent_count: u32,
    show_toolbar: bool,
    zoom: u16,
    autosave: u32,
    channel: &str,
    toggles: &[String],
    allowed_paths: &[String],
    json_output: bool,
) -> anyhow::Result<()> {
    let theme = Theme::parse(theme).map_err(|error| anyhow::anyhow!("{error}"))?;
    let channel = UpdateChannel::parse(channel).map_err(|error| anyhow::anyhow!("{error}"))?;
    let base = Preferences::new(
        SCHEMA_VERSION,
        theme,
        locale,
        recent_count,
        show_toolbar,
        zoom,
        autosave,
    )
    .map_err(|error| anyhow::anyhow!("preferences: {error}"))?;
    let mut map = BTreeMap::new();
    for value in toggles {
        let (name, enabled) = parse_toggle(value)?;
        map.insert(name, enabled);
    }
    let policy = DeploymentPreferences::new(base, map, allowed_paths.to_vec(), channel)
        .map_err(|error| anyhow::anyhow!("deployment preferences: {error}"))?;
    let document = policy.serialize();
    if let Some(parent) = out.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    std::fs::write(out, &document).with_context(|| format!("write {}", out.display()))?;
    if json_output {
        println!(
            "{}",
            serde_json::to_string_pretty(&json(&policy, Some(document)))?
        );
        return Ok(());
    }
    print(&policy);
    println!("written: {}", out.display());
    Ok(())
}

/// `pdfxi policy merge`.
pub(crate) fn merge(
    base: &Path,
    overlay: &Path,
    out: Option<&Path>,
    json_output: bool,
) -> anyhow::Result<()> {
    let base = load(base)?;
    let overlay = load(overlay)?;
    let merged = base.merge(&overlay);
    let document = merged.serialize();
    if let Some(path) = out {
        std::fs::write(path, &document).with_context(|| format!("write {}", path.display()))?;
    }
    if json_output {
        println!(
            "{}",
            serde_json::to_string_pretty(&json(&merged, Some(document)))?
        );
        return Ok(());
    }
    print(&merged);
    if let Some(path) = out {
        println!("written: {}", path.display());
    }
    Ok(())
}

/// Whether `path` is inside one of the allowed prefixes.
fn path_allowed(path: &str, allowed: &[String]) -> bool {
    allowed.iter().any(|prefix| {
        let prefix = prefix.trim_end_matches(['/', '\\']);
        path == prefix
            || path
                .strip_prefix(prefix)
                .is_some_and(|rest| rest.starts_with('/') || rest.starts_with('\\'))
    })
}

/// `pdfxi policy check`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn check(
    file: &Path,
    theme: Option<&str>,
    channel: Option<&str>,
    toggles: &[String],
    paths: &[String],
    json_output: bool,
) -> anyhow::Result<()> {
    let policy = load(file)?;
    let mut failures: Vec<String> = Vec::new();
    if let Some(theme) = theme {
        let requested = Theme::parse(theme).map_err(|error| anyhow::anyhow!("{error}"))?;
        if requested != policy.base().theme() {
            failures.push(format!(
                "theme {requested:?} is not the deployment theme {:?}",
                policy.base().theme()
            ));
        }
    }
    if let Some(channel) = channel {
        let requested =
            UpdateChannel::parse(channel).map_err(|error| anyhow::anyhow!("{error}"))?;
        if requested != policy.update_channel() {
            failures.push(format!(
                "channel {} is not the deployment channel {}",
                requested.as_str(),
                policy.update_channel().as_str()
            ));
        }
    }
    for name in toggles {
        if !policy.is_enabled(name) {
            failures.push(format!("feature toggle {name:?} is not enabled by policy"));
        }
    }
    for path in paths {
        if !policy.allowed_paths().is_empty() && !path_allowed(path, policy.allowed_paths()) {
            failures.push(format!("path {path:?} is outside every allowed path"));
        }
    }
    let allowed = failures.is_empty();
    if json_output {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "allowed": allowed,
                "failures": failures,
            }))?
        );
    } else {
        println!("allowed: {allowed}");
        for failure in &failures {
            println!("  {failure}");
        }
    }
    if !allowed {
        bail!("{} deployment-policy check(s) failed", failures.len());
    }
    Ok(())
}

/// `pdfxi policy preferences`: parse a plain preferences document.
pub(crate) fn preferences(file: &Path, json_output: bool) -> anyhow::Result<()> {
    let text = std::fs::read_to_string(file)
        .with_context(|| format!("read preferences {}", file.display()))?;
    let prefs = parse_preferences(&text)
        .map_err(|error| anyhow::anyhow!("parse preferences {}: {error}", file.display()))?;
    if json_output {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "theme": prefs.theme().as_str(),
                "locale": prefs.locale(),
                "recent_count": prefs.recent_count(),
                "show_toolbar": prefs.show_toolbar(),
                "default_zoom_percent": prefs.default_zoom_percent(),
                "autosave_seconds": prefs.autosave_seconds(),
                "document": serialize_preferences(&prefs),
            }))?
        );
        return Ok(());
    }
    print!("{}", serialize_preferences(&prefs));
    Ok(())
}
