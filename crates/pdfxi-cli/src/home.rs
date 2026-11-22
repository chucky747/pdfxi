//! `pdfxi home`: recents, favorites and collections over a state file.
//!
//! [`pdfxi_app::home::HomeModel`] is the headless model: bounded recents,
//! deduplicated favorites and named collections over opaque document keys. This
//! host owns persistence in a caller-supplied state file and turns paths into
//! keys/titles. The file uses the model's newline format, so it is bounded and
//! refuses malformed input.

use std::path::Path;

use anyhow::{Context, bail};
use pdfxi_app::home::{DocumentKey, HomeModel};
use serde::Serialize;

/// JSON shape of `pdfxi home list`.
#[derive(Serialize)]
struct HomeReport {
    /// State file.
    state: String,
    /// Recent documents, most recent first.
    recents: Vec<EntryRow>,
    /// Favorite documents.
    favorites: Vec<EntryRow>,
    /// Collections and their members.
    collections: Vec<CollectionRow>,
}

/// One document row.
#[derive(Serialize)]
struct EntryRow {
    /// Normalized key.
    key: String,
    /// Display title.
    title: String,
    /// Logical recency ordinal, when present.
    #[serde(skip_serializing_if = "Option::is_none")]
    ordinal: Option<u64>,
}

/// One collection row.
#[derive(Serialize)]
struct CollectionRow {
    /// Collection name.
    name: String,
    /// Member keys in insertion order.
    members: Vec<String>,
}

/// Load the state file, or start empty when absent.
fn load(state: &Path) -> anyhow::Result<HomeModel> {
    if !state.exists() {
        return Ok(HomeModel::new());
    }
    let text = std::fs::read_to_string(state)
        .with_context(|| format!("read state {}", state.display()))?;
    HomeModel::deserialize(&text)
        .map_err(|error| anyhow::anyhow!("parse state {}: {error}", state.display()))
}

/// Write the state file.
fn save(state: &Path, model: &HomeModel) -> anyhow::Result<()> {
    if let Some(parent) = state.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    std::fs::write(state, model.serialize())
        .with_context(|| format!("write state {}", state.display()))
}

/// Turn a document path into a key and title.
fn key_and_title(path: &Path) -> anyhow::Result<(DocumentKey, String)> {
    if !path.exists() {
        bail!("document {} does not exist", path.display());
    }
    let key = DocumentKey::from_path(path);
    if key.is_empty() {
        bail!("document path {} produced an empty key", path.display());
    }
    let title = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("document")
        .to_owned();
    Ok((key, title))
}

/// Build the list report.
fn report(state: &Path, model: &HomeModel) -> HomeReport {
    HomeReport {
        state: state.display().to_string(),
        recents: model
            .recents()
            .iter()
            .map(|entry| EntryRow {
                key: entry.key().as_str().to_owned(),
                title: entry.title().to_owned(),
                ordinal: Some(entry.ordinal()),
            })
            .collect(),
        favorites: model
            .favorites()
            .iter()
            .map(|entry| EntryRow {
                key: entry.key().as_str().to_owned(),
                title: entry.title().to_owned(),
                ordinal: Some(entry.ordinal()),
            })
            .collect(),
        collections: model
            .collections()
            .map(|collection| CollectionRow {
                name: collection.name().to_owned(),
                members: collection
                    .members()
                    .map(|key| key.as_str().to_owned())
                    .collect(),
            })
            .collect(),
    }
}

/// Print the report.
fn print_report(report: &HomeReport, json: bool) -> anyhow::Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(report)?);
        return Ok(());
    }
    println!("state: {}", report.state);
    println!("recents: {}", report.recents.len());
    for row in &report.recents {
        println!("  {}\t{}", row.title, row.key);
    }
    println!("favorites: {}", report.favorites.len());
    for row in &report.favorites {
        println!("  {}\t{}", row.title, row.key);
    }
    println!("collections: {}", report.collections.len());
    for collection in &report.collections {
        println!(
            "  {}\t{} member(s)",
            collection.name,
            collection.members.len()
        );
    }
    Ok(())
}

/// `pdfxi home list`.
pub(crate) fn list(state: &Path, json: bool) -> anyhow::Result<()> {
    let model = load(state)?;
    print_report(&report(state, &model), json)
}

/// `pdfxi home add`.
pub(crate) fn add(file: &Path, state: &Path, json: bool) -> anyhow::Result<()> {
    let (key, title) = key_and_title(file)?;
    let mut model = load(state)?;
    model.record_open(key, &title);
    save(state, &model)?;
    print_report(&report(state, &model), json)
}

/// `pdfxi home favorite`.
pub(crate) fn favorite(file: &Path, state: &Path, remove: bool, json: bool) -> anyhow::Result<()> {
    let (key, title) = key_and_title(file)?;
    let mut model = load(state)?;
    if remove {
        model.remove_favorite(&key);
    } else {
        model.add_favorite(key, &title);
    }
    save(state, &model)?;
    print_report(&report(state, &model), json)
}

/// `pdfxi home collection`.
pub(crate) fn collection(
    name: &str,
    state: &Path,
    add: Option<&Path>,
    remove: Option<&Path>,
    delete: bool,
    json: bool,
) -> anyhow::Result<()> {
    if name.trim().is_empty() {
        bail!("collection name must not be empty");
    }
    let mut model = load(state)?;
    if delete {
        model.remove_collection(name);
        save(state, &model)?;
        return print_report(&report(state, &model), json);
    }
    if model.collection(name).is_none() {
        model
            .create_collection(name)
            .map_err(|error| anyhow::anyhow!("create collection {name:?}: {error}"))?;
    }
    if let Some(path) = add {
        let (key, _) = key_and_title(path)?;
        model.add_to_collection(name, key);
    }
    if let Some(path) = remove {
        let key = DocumentKey::from_path(path);
        model.remove_from_collection(name, &key);
    }
    save(state, &model)?;
    print_report(&report(state, &model), json)
}
