//! PDF Portfolio management operations (local, deterministic).
//!
//! This module adds window-free, deterministic management operations on top of
//! the browsing model in [`crate::portfolio`]. A [`PortfolioManager`] owns an
//! insertion-independent set of entries keyed by their validated full
//! [`PortfolioPath`]. `add`, `remove`, `rename`, and `move` validate paths and
//! reject collisions, then return a [`ManagePlan`] that describes exactly what
//! changed. The host can serialize the plan with [`ManagePlan::serialize`]; no
//! file bytes are touched and no filesystem or network I/O is performed.

use std::collections::BTreeMap;

use crate::portfolio::{
    MAX_PORTFOLIO_DESCRIPTION_LEN, MAX_PORTFOLIO_ENTRIES, MAX_PORTFOLIO_NAME_LEN, PortfolioEntry,
    PortfolioError, PortfolioModel, PortfolioNode, PortfolioPath, PortfolioPathError,
    PortfolioSummary,
};

/// Typed error for a portfolio management operation.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ManageError {
    /// A supplied path, folder, or name was invalid.
    #[error("invalid portfolio path: {0}")]
    InvalidPath(#[from] PortfolioPathError),
    /// A supplied name was empty or over-long.
    #[error("invalid entry name (maximum {MAX_PORTFOLIO_NAME_LEN} characters)")]
    InvalidName,
    /// The source entry was not present in the manager.
    #[error("no portfolio entry at '{0}'")]
    NotFound(PortfolioPath),
    /// The target path was already occupied.
    #[error("portfolio entry already exists at '{0}'")]
    Duplicate(PortfolioPath),
    /// Adding an entry would exceed the portfolio entry cap.
    #[error("portfolio entry cap of {0} reached")]
    TooManyEntries(usize),
    /// The portfolio model could not be built.
    #[error(transparent)]
    Model(#[from] PortfolioError),
    /// A description exceeded the supported length.
    #[error("invalid portfolio description (maximum {MAX_PORTFOLIO_DESCRIPTION_LEN} characters)")]
    InvalidDescription,
    /// A serialized plan could not be parsed back.
    #[error("invalid management plan at line {line}: {reason}")]
    InvalidPlan {
        /// One-based line number in the serialized plan.
        line: usize,
        /// Why the line was rejected.
        reason: &'static str,
    },
}

/// One recorded step of a [`ManagePlan`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ManageOp {
    /// An entry was added at a full path.
    Add {
        /// The added entry (its name and folder are the authoritative key).
        entry: PortfolioEntry,
        /// The full path the entry now occupies.
        path: PortfolioPath,
    },
    /// An entry was removed.
    Remove {
        /// The full path that was removed.
        path: PortfolioPath,
    },
    /// An entry was renamed within its folder.
    Rename {
        /// The original full path.
        path: PortfolioPath,
        /// The new full path after renaming.
        new_path: PortfolioPath,
        /// The new leaf name.
        new_name: String,
    },
    /// An entry was moved to a different folder (name preserved).
    Move {
        /// The original full path.
        path: PortfolioPath,
        /// The new full path after moving.
        new_path: PortfolioPath,
        /// The destination folder.
        new_folder: PortfolioPath,
    },
    /// An entry's description was replaced.
    Describe {
        /// Full path of the described entry.
        path: PortfolioPath,
        /// The new description.
        description: String,
    },
}

/// A deterministic, serializable record of management changes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ManagePlan {
    ops: Vec<ManageOp>,
}

impl ManagePlan {
    /// Create an empty plan.
    pub fn new() -> Self {
        Self::default()
    }

    /// The recorded operations, in application order.
    pub fn ops(&self) -> &[ManageOp] {
        &self.ops
    }

    /// Number of recorded operations.
    pub fn len(&self) -> usize {
        self.ops.len()
    }

    /// Whether no operation was recorded.
    pub fn is_empty(&self) -> bool {
        self.ops.is_empty()
    }

    /// Render the plan as stable, escaped lines the host can persist or log.
    pub fn to_lines(&self) -> Vec<String> {
        self.ops.iter().map(op_line).collect()
    }

    /// Serialize the plan to a single deterministic string.
    pub fn serialize(&self) -> String {
        self.to_lines().join("\n")
    }

    /// Parse a serialized plan back into the same ordered operations.
    ///
    /// This is the exact inverse of [`Self::serialize`] for plans produced by
    /// this module: field escaping is undone, paths and numbers are revalidated,
    /// and redundant fields are checked for consistency. Malformed lines are
    /// reported as typed [`ManageError::InvalidPlan`] values with a one-based
    /// line number, so a host can reject a hostile or truncated descriptor
    /// instead of applying a partially understood plan.
    pub fn parse(serialized: &str) -> Result<Self, ManageError> {
        if serialized.is_empty() {
            return Ok(Self::new());
        }
        let mut ops = Vec::new();
        for (position, raw_line) in serialized.split('\n').enumerate() {
            let line_number = position + 1;
            let fields = split_fields(raw_line, line_number)?;
            let op = match fields.first().map(String::as_str) {
                Some("ADD") => parse_add(&fields, line_number)?,
                Some("REMOVE") => parse_remove(&fields, line_number)?,
                Some("RENAME") => parse_rename(&fields, line_number)?,
                Some("MOVE") => parse_move(&fields, line_number)?,
                Some("DESCRIBE") => parse_describe(&fields, line_number)?,
                _ => {
                    return Err(ManageError::InvalidPlan {
                        line: line_number,
                        reason: "unknown operation",
                    });
                }
            };
            ops.push(op);
        }
        Ok(Self { ops })
    }
}

fn escape_field(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '|' => out.push_str("\\|"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            _ => out.push(ch),
        }
    }
    out
}

fn path_fields(path: &PortfolioPath) -> String {
    match path.components().split_last() {
        Some((name, parents)) => {
            let folder = parents.join("/");
            format!("{}|{}", escape_field(&folder), escape_field(name))
        }
        None => "|".to_owned(),
    }
}

fn op_line(op: &ManageOp) -> String {
    match op {
        ManageOp::Add { entry, path } => {
            let modified = match entry.modified() {
                Some(value) => value.to_string(),
                None => String::new(),
            };
            format!(
                "ADD|{}|{}|{}|{}|{}|{}",
                path_fields(path),
                escape_field(entry.description()),
                entry.size(),
                escape_field(entry.mime()),
                modified,
                escape_field(entry.folder()),
            )
        }
        ManageOp::Remove { path } => format!("REMOVE|{}", path_fields(path)),
        ManageOp::Rename {
            path,
            new_path,
            new_name,
        } => format!(
            "RENAME|{}|{}|{}",
            path_fields(path),
            path_fields(new_path),
            escape_field(new_name),
        ),
        ManageOp::Move {
            path,
            new_path,
            new_folder,
        } => format!(
            "MOVE|{}|{}|{}",
            path_fields(path),
            path_fields(new_path),
            escape_field(&new_folder.as_str()),
        ),
        ManageOp::Describe { path, description } => format!(
            "DESCRIBE|{}|{}",
            path_fields(path),
            escape_field(description),
        ),
    }
}

/// Split a serialized line on unescaped `|` separators.
fn split_fields(raw_line: &str, line: usize) -> Result<Vec<String>, ManageError> {
    let mut fields = Vec::new();
    let mut current = String::new();
    let mut chars = raw_line.chars();
    while let Some(ch) = chars.next() {
        match ch {
            '\\' => {
                current.push(ch);
                let next = chars.next().ok_or(ManageError::InvalidPlan {
                    line,
                    reason: "dangling escape",
                })?;
                current.push(next);
            }
            '|' => {
                fields.push(unescape_field(&current, line)?);
                current.clear();
            }
            _ => current.push(ch),
        }
    }
    fields.push(unescape_field(&current, line)?);
    Ok(fields)
}

/// Undo the escaping performed by [`escape_field`].
fn unescape_field(value: &str, line: usize) -> Result<String, ManageError> {
    let invalid = || ManageError::InvalidPlan {
        line,
        reason: "invalid field escape",
    };
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            match chars.next() {
                Some('\\') => out.push('\\'),
                Some('|') => out.push('|'),
                Some('n') => out.push('\n'),
                Some('r') => out.push('\r'),
                _ => return Err(invalid()),
            }
        } else {
            out.push(ch);
        }
    }
    Ok(out)
}

/// Rebuild a validated full path from a folder field and a leaf name field.
fn path_from_fields(folder: &str, name: &str) -> Result<PortfolioPath, ManageError> {
    Ok(PortfolioPath::parse(folder)?.join(name)?)
}

/// Read a field by index with a consistent out-of-shape error.
fn field(fields: &[String], index: usize, line: usize) -> Result<&str, ManageError> {
    fields
        .get(index)
        .map(String::as_str)
        .ok_or(ManageError::InvalidPlan {
            line,
            reason: "wrong field count",
        })
}

fn parse_u64_field(value: &str, line: usize, reason: &'static str) -> Result<u64, ManageError> {
    value
        .parse::<u64>()
        .map_err(|_| ManageError::InvalidPlan { line, reason })
}

fn parse_add(fields: &[String], line: usize) -> Result<ManageOp, ManageError> {
    if fields.len() != 8 {
        return Err(ManageError::InvalidPlan {
            line,
            reason: "wrong ADD field count",
        });
    }
    let folder = field(fields, 1, line)?;
    let name = field(fields, 2, line)?;
    let description = field(fields, 3, line)?;
    let size = parse_u64_field(field(fields, 4, line)?, line, "invalid size")?;
    let mime = field(fields, 5, line)?;
    let modified_raw = field(fields, 6, line)?;
    let entry_folder = field(fields, 7, line)?;
    let modified = if modified_raw.is_empty() {
        None
    } else {
        Some(parse_u64_field(
            modified_raw,
            line,
            "invalid modified timestamp",
        )?)
    };
    if folder != entry_folder {
        return Err(ManageError::InvalidPlan {
            line,
            reason: "ADD folder mismatch",
        });
    }
    let path = path_from_fields(folder, name)?;
    let entry = PortfolioEntry::new(name, description, size, mime, modified, entry_folder);
    Ok(ManageOp::Add { entry, path })
}

fn parse_remove(fields: &[String], line: usize) -> Result<ManageOp, ManageError> {
    if fields.len() != 3 {
        return Err(ManageError::InvalidPlan {
            line,
            reason: "wrong REMOVE field count",
        });
    }
    let path = path_from_fields(field(fields, 1, line)?, field(fields, 2, line)?)?;
    Ok(ManageOp::Remove { path })
}

fn parse_rename(fields: &[String], line: usize) -> Result<ManageOp, ManageError> {
    if fields.len() != 6 {
        return Err(ManageError::InvalidPlan {
            line,
            reason: "wrong RENAME field count",
        });
    }
    let path = path_from_fields(field(fields, 1, line)?, field(fields, 2, line)?)?;
    let new_path = path_from_fields(field(fields, 3, line)?, field(fields, 4, line)?)?;
    let new_name = field(fields, 5, line)?.to_owned();
    if new_path.name() != Some(new_name.as_str()) {
        return Err(ManageError::InvalidPlan {
            line,
            reason: "RENAME name mismatch",
        });
    }
    Ok(ManageOp::Rename {
        path,
        new_path,
        new_name,
    })
}

fn parse_move(fields: &[String], line: usize) -> Result<ManageOp, ManageError> {
    if fields.len() != 6 {
        return Err(ManageError::InvalidPlan {
            line,
            reason: "wrong MOVE field count",
        });
    }
    let path = path_from_fields(field(fields, 1, line)?, field(fields, 2, line)?)?;
    let folder_field = field(fields, 3, line)?;
    let new_path = path_from_fields(folder_field, field(fields, 4, line)?)?;
    let new_folder = PortfolioPath::parse(field(fields, 5, line)?)?;
    if new_folder.as_str() != folder_field {
        return Err(ManageError::InvalidPlan {
            line,
            reason: "MOVE folder mismatch",
        });
    }
    Ok(ManageOp::Move {
        path,
        new_path,
        new_folder,
    })
}

fn parse_describe(fields: &[String], line: usize) -> Result<ManageOp, ManageError> {
    if fields.len() != 4 {
        return Err(ManageError::InvalidPlan {
            line,
            reason: "wrong DESCRIBE field count",
        });
    }
    let description = field(fields, 3, line)?.to_owned();
    if description.chars().count() > MAX_PORTFOLIO_DESCRIPTION_LEN {
        return Err(ManageError::InvalidDescription);
    }
    let path = path_from_fields(field(fields, 1, line)?, field(fields, 2, line)?)?;
    Ok(ManageOp::Describe { path, description })
}

/// A deterministic, collision-checked portfolio entry set.
#[derive(Clone, Debug, Default)]
pub struct PortfolioManager {
    entries: BTreeMap<PortfolioPath, PortfolioEntry>,
}

impl PortfolioManager {
    /// Create an empty manager.
    pub fn new() -> Self {
        Self::default()
    }

    /// Seed a manager from the accepted entries of a browsing model.
    pub fn from_model(model: &PortfolioModel) -> Self {
        let mut manager = Self::new();
        collect_files(model.tree().root(), &mut manager.entries);
        manager
    }

    /// Seed a manager directly from a caller-supplied summary.
    pub fn from_summary(summary: &PortfolioSummary) -> Result<Self, ManageError> {
        let model = PortfolioModel::from_summary(summary)?;
        Ok(Self::from_model(&model))
    }

    /// The managed entries keyed by full path.
    pub fn entries(&self) -> &BTreeMap<PortfolioPath, PortfolioEntry> {
        &self.entries
    }

    /// Number of managed entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the manager holds no entries.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Whether an entry exists at `path`.
    pub fn contains(&self, path: &PortfolioPath) -> bool {
        self.entries.contains_key(path)
    }

    /// Validate an entry's fields and compute its full path.
    fn validated_path(entry: &PortfolioEntry) -> Result<PortfolioPath, ManageError> {
        if entry.name().is_empty() || entry.name().chars().count() > MAX_PORTFOLIO_NAME_LEN {
            return Err(ManageError::InvalidName);
        }
        PortfolioPath::validate_component(entry.name())?;
        let folder = PortfolioPath::parse(entry.folder())?;
        Ok(folder.join(entry.name())?)
    }

    /// Add an entry, rejecting invalid paths and collisions.
    pub fn add(&mut self, entry: PortfolioEntry) -> Result<ManagePlan, ManageError> {
        let path = Self::validated_path(&entry)?;
        if self.entries.len() >= MAX_PORTFOLIO_ENTRIES {
            return Err(ManageError::TooManyEntries(MAX_PORTFOLIO_ENTRIES));
        }
        if self.entries.contains_key(&path) {
            return Err(ManageError::Duplicate(path));
        }
        let mut plan = ManagePlan::new();
        plan.ops.push(ManageOp::Add {
            entry: entry.clone(),
            path: path.clone(),
        });
        self.entries.insert(path, entry);
        Ok(plan)
    }

    /// Remove the entry at `path`.
    pub fn remove(&mut self, path: &PortfolioPath) -> Result<ManagePlan, ManageError> {
        if path.is_root() {
            return Err(ManageError::InvalidPath(PortfolioPathError::EmptyComponent));
        }
        if self.entries.remove(path).is_none() {
            return Err(ManageError::NotFound(path.clone()));
        }
        let mut plan = ManagePlan::new();
        plan.ops.push(ManageOp::Remove { path: path.clone() });
        Ok(plan)
    }

    /// Rename the entry at `path` to `new_name`, staying in its folder.
    pub fn rename(
        &mut self,
        path: &PortfolioPath,
        new_name: &str,
    ) -> Result<ManagePlan, ManageError> {
        if new_name.is_empty() || new_name.chars().count() > MAX_PORTFOLIO_NAME_LEN {
            return Err(ManageError::InvalidName);
        }
        PortfolioPath::validate_component(new_name)?;
        let entry = self
            .entries
            .get(path)
            .cloned()
            .ok_or_else(|| ManageError::NotFound(path.clone()))?;
        let new_path = path
            .parent()
            .unwrap_or_else(PortfolioPath::root)
            .join(new_name)?;
        self.entries.remove(path);
        if self.entries.contains_key(&new_path) {
            self.entries.insert(path.clone(), entry);
            return Err(ManageError::Duplicate(new_path));
        }
        let folder = new_path
            .parent()
            .unwrap_or_else(PortfolioPath::root)
            .as_str();
        let renamed = PortfolioEntry::new(
            new_name,
            entry.description(),
            entry.size(),
            entry.mime(),
            entry.modified(),
            folder,
        );
        self.entries.insert(new_path.clone(), renamed);
        let mut plan = ManagePlan::new();
        plan.ops.push(ManageOp::Rename {
            path: path.clone(),
            new_path,
            new_name: new_name.to_owned(),
        });
        Ok(plan)
    }

    /// Move the entry at `path` into `new_folder`, preserving its name.
    pub fn move_entry(
        &mut self,
        path: &PortfolioPath,
        new_folder: &PortfolioPath,
    ) -> Result<ManagePlan, ManageError> {
        let entry = self
            .entries
            .get(path)
            .cloned()
            .ok_or_else(|| ManageError::NotFound(path.clone()))?;
        let name = match path.name() {
            Some(name) => name,
            None => return Err(ManageError::InvalidPath(PortfolioPathError::EmptyComponent)),
        };
        let new_path = new_folder.join(name)?;
        self.entries.remove(path);
        if self.entries.contains_key(&new_path) {
            self.entries.insert(path.clone(), entry);
            return Err(ManageError::Duplicate(new_path));
        }
        let moved = PortfolioEntry::new(
            name,
            entry.description(),
            entry.size(),
            entry.mime(),
            entry.modified(),
            new_folder.as_str(),
        );
        self.entries.insert(new_path.clone(), moved);
        let mut plan = ManagePlan::new();
        plan.ops.push(ManageOp::Move {
            path: path.clone(),
            new_path,
            new_folder: new_folder.clone(),
        });
        Ok(plan)
    }

    /// Replace the description of the entry at `path`, preserving all other
    /// fields. A missing source or an over-long description is a typed error.
    pub fn describe(
        &mut self,
        path: &PortfolioPath,
        description: impl Into<String>,
    ) -> Result<ManagePlan, ManageError> {
        let description = description.into();
        if description.chars().count() > MAX_PORTFOLIO_DESCRIPTION_LEN {
            return Err(ManageError::InvalidDescription);
        }
        let entry = self
            .entries
            .get(path)
            .cloned()
            .ok_or_else(|| ManageError::NotFound(path.clone()))?;
        let updated = PortfolioEntry::new(
            entry.name(),
            description.clone(),
            entry.size(),
            entry.mime(),
            entry.modified(),
            entry.folder(),
        );
        self.entries.insert(path.clone(), updated);
        let mut plan = ManagePlan::new();
        plan.ops.push(ManageOp::Describe {
            path: path.clone(),
            description,
        });
        Ok(plan)
    }
}

fn collect_files(
    folder: &crate::portfolio::PortfolioFolder,
    out: &mut BTreeMap<PortfolioPath, PortfolioEntry>,
) {
    for child in folder.children() {
        match child {
            PortfolioNode::Folder(nested) => collect_files(nested, out),
            PortfolioNode::File(file) => {
                out.insert(file.path().clone(), file.entry().clone());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, folder: &str) -> PortfolioEntry {
        PortfolioEntry::new(name, "desc", 7, "txt", Some(42), folder)
    }

    fn path(value: &str) -> PortfolioPath {
        PortfolioPath::parse(value).unwrap_or_else(|_| panic!("test path"))
    }

    #[test]
    fn add_produces_plan_and_updates_manager() {
        let mut manager = PortfolioManager::new();
        let plan = manager.add(entry("a.txt", "docs"));
        assert!(plan.is_ok());
        let plan = plan.unwrap_or_else(|_| panic!("add"));
        assert_eq!(plan.len(), 1);
        assert_eq!(manager.len(), 1);
        assert!(manager.contains(&path("docs/a.txt")));
        assert!(matches!(&plan.ops()[0], ManageOp::Add { .. }));
    }

    #[test]
    fn add_duplicate_is_rejected() {
        let mut manager = PortfolioManager::new();
        manager
            .add(entry("a.txt", "docs"))
            .unwrap_or_else(|_| panic!("add"));
        match manager.add(entry("a.txt", "docs")) {
            Err(ManageError::Duplicate(p)) => assert_eq!(p, path("docs/a.txt")),
            _ => panic!("expected duplicate"),
        }
        assert_eq!(manager.len(), 1);
    }

    #[test]
    fn add_invalid_path_is_rejected() {
        let mut manager = PortfolioManager::new();
        match manager.add(entry("bad.txt", "../escape")) {
            Err(ManageError::InvalidPath(_)) => {}
            _ => panic!("expected invalid path"),
        }
        assert!(manager.is_empty());
    }

    #[test]
    fn rename_plan_repaths_entry() {
        let mut manager = PortfolioManager::new();
        manager
            .add(entry("old.txt", "docs"))
            .unwrap_or_else(|_| panic!("add"));
        let plan = manager.rename(&path("docs/old.txt"), "new.txt");
        assert!(plan.is_ok());
        let plan = plan.unwrap_or_else(|_| panic!("rename"));
        assert!(matches!(&plan.ops()[0], ManageOp::Rename { .. }));
        assert!(!manager.contains(&path("docs/old.txt")));
        assert!(manager.contains(&path("docs/new.txt")));
        let renamed = manager.entries().get(&path("docs/new.txt"));
        assert_eq!(renamed.map(PortfolioEntry::name), Some("new.txt"));
    }

    #[test]
    fn rename_to_existing_name_is_rejected_without_losing_source() {
        let mut manager = PortfolioManager::new();
        manager
            .add(entry("a.txt", "docs"))
            .unwrap_or_else(|_| panic!("add a"));
        manager
            .add(entry("b.txt", "docs"))
            .unwrap_or_else(|_| panic!("add b"));
        match manager.rename(&path("docs/a.txt"), "b.txt") {
            Err(ManageError::Duplicate(p)) => assert_eq!(p, path("docs/b.txt")),
            _ => panic!("expected duplicate"),
        }
        assert!(manager.contains(&path("docs/a.txt")));
        assert_eq!(manager.len(), 2);
    }

    #[test]
    fn move_plan_repaths_entry() {
        let mut manager = PortfolioManager::new();
        manager
            .add(entry("a.txt", "in"))
            .unwrap_or_else(|_| panic!("add"));
        let plan = manager.move_entry(&path("in/a.txt"), &path("out"));
        assert!(plan.is_ok());
        let plan = plan.unwrap_or_else(|_| panic!("move"));
        assert!(matches!(&plan.ops()[0], ManageOp::Move { .. }));
        assert!(!manager.contains(&path("in/a.txt")));
        assert!(manager.contains(&path("out/a.txt")));
    }

    #[test]
    fn remove_plan_drops_entry() {
        let mut manager = PortfolioManager::new();
        manager
            .add(entry("a.txt", "in"))
            .unwrap_or_else(|_| panic!("add"));
        let plan = manager.remove(&path("in/a.txt"));
        assert!(plan.is_ok());
        let plan = plan.unwrap_or_else(|_| panic!("remove"));
        assert!(matches!(&plan.ops()[0], ManageOp::Remove { .. }));
        assert!(manager.is_empty());
        match manager.remove(&path("in/a.txt")) {
            Err(ManageError::NotFound(_)) => {}
            _ => panic!("expected not found"),
        }
    }

    #[test]
    fn plan_serializes_deterministically_and_escapes_fields() {
        let mut manager = PortfolioManager::new();
        manager
            .add(PortfolioEntry::new(
                "a.txt",
                "line|one\ntwo",
                3,
                "txt",
                None,
                "docs",
            ))
            .unwrap_or_else(|_| panic!("add"));
        let plan = manager
            .rename(&path("docs/a.txt"), "b.txt")
            .unwrap_or_else(|_| panic!("rename"));
        let first = plan.serialize();
        assert_eq!(first, plan.serialize());
        assert_eq!(first, plan.to_lines().join("\n"));
        assert!(first.starts_with("RENAME|"));

        let mut other = PortfolioManager::new();
        other
            .add(PortfolioEntry::new(
                "a.txt",
                "line|one\ntwo",
                3,
                "txt",
                None,
                "docs",
            ))
            .unwrap_or_else(|_| panic!("add other"));
        let other_plan = other
            .rename(&path("docs/a.txt"), "b.txt")
            .unwrap_or_else(|_| panic!("rename other"));
        assert_eq!(first, other_plan.serialize());
        assert!(!first.contains('\n') || first.lines().count() == 1);
    }

    #[test]
    fn from_model_seeds_managed_entries() {
        let summary = PortfolioSummary::new()
            .with_entry(entry("a.txt", "docs"))
            .with_entry(entry("b.txt", ""));
        let model = PortfolioModel::from_summary(&summary).unwrap_or_else(|_| panic!("model"));
        let manager = PortfolioManager::from_model(&model);
        assert_eq!(manager.len(), 2);
        assert!(manager.contains(&path("docs/a.txt")));
        assert!(manager.contains(&path("b.txt")));
    }

    fn assert_round_trip(plan: &ManagePlan) {
        let serialized = plan.serialize();
        let parsed = ManagePlan::parse(&serialized).unwrap_or_else(|_| panic!("parse plan"));
        assert_eq!(&parsed, plan);
        assert_eq!(parsed.serialize(), serialized);
    }

    #[test]
    fn describe_updates_entry_and_emits_plan() {
        let mut manager = PortfolioManager::new();
        manager
            .add(entry("a.txt", "docs"))
            .unwrap_or_else(|_| panic!("add"));
        let plan = manager
            .describe(&path("docs/a.txt"), "a longer description")
            .unwrap_or_else(|_| panic!("describe"));
        assert!(matches!(&plan.ops()[0], ManageOp::Describe { .. }));
        let described = manager
            .entries()
            .get(&path("docs/a.txt"))
            .unwrap_or_else(|| panic!("described entry"));
        assert_eq!(described.description(), "a longer description");
        assert_eq!(described.name(), "a.txt");
        assert_eq!(described.size(), 7);
        assert_round_trip(&plan);
    }

    #[test]
    fn describe_missing_entry_and_overlong_description_are_typed_errors() {
        let mut manager = PortfolioManager::new();
        assert_eq!(
            manager.describe(&path("missing.txt"), "x"),
            Err(ManageError::NotFound(path("missing.txt")))
        );
        manager
            .add(entry("a.txt", ""))
            .unwrap_or_else(|_| panic!("add"));
        let too_long = "x".repeat(MAX_PORTFOLIO_DESCRIPTION_LEN + 1);
        assert_eq!(
            manager.describe(&path("a.txt"), too_long),
            Err(ManageError::InvalidDescription)
        );
    }

    #[test]
    fn plan_round_trips_all_operation_kinds() {
        let mut manager = PortfolioManager::new();
        let add = manager
            .add(PortfolioEntry::new(
                "a.txt",
                "desc|line\nnext\\end",
                42,
                "application/pdf",
                Some(99),
                "docs",
            ))
            .unwrap_or_else(|_| panic!("add"));
        let describe = manager
            .describe(&path("docs/a.txt"), "changed|desc\nline")
            .unwrap_or_else(|_| panic!("describe"));
        let rename = manager
            .rename(&path("docs/a.txt"), "b.txt")
            .unwrap_or_else(|_| panic!("rename"));
        let move_plan = manager
            .move_entry(&path("docs/b.txt"), &path("archive"))
            .unwrap_or_else(|_| panic!("move"));
        let remove = manager
            .remove(&path("archive/b.txt"))
            .unwrap_or_else(|_| panic!("remove"));
        for plan in [&add, &describe, &rename, &move_plan, &remove] {
            assert_round_trip(plan);
        }
        // A multi-operation plan keeps its application order.
        let mut combined = ManagePlan::new();
        combined.ops.extend(add.ops().iter().cloned());
        combined.ops.extend(describe.ops().iter().cloned());
        combined.ops.extend(rename.ops().iter().cloned());
        combined.ops.extend(move_plan.ops().iter().cloned());
        combined.ops.extend(remove.ops().iter().cloned());
        assert_eq!(combined.len(), 5);
        assert_round_trip(&combined);
    }

    #[test]
    fn empty_plan_round_trips_as_empty_string() {
        let plan = ManagePlan::new();
        assert_eq!(plan.serialize(), "");
        assert_round_trip(&plan);
    }

    #[test]
    fn parse_rejects_malformed_lines_with_typed_errors() {
        assert_eq!(
            ManagePlan::parse("UNKNOWN|x"),
            Err(ManageError::InvalidPlan {
                line: 1,
                reason: "unknown operation"
            })
        );
        assert_eq!(
            ManagePlan::parse("REMOVE|docs"),
            Err(ManageError::InvalidPlan {
                line: 1,
                reason: "wrong REMOVE field count"
            })
        );
        assert_eq!(
            ManagePlan::parse("REMOVE|docs\\q|a.txt"),
            Err(ManageError::InvalidPlan {
                line: 1,
                reason: "invalid field escape"
            })
        );
        assert_eq!(
            ManagePlan::parse("REMOVE|docs|a.txt\nREMOVE|docs"),
            Err(ManageError::InvalidPlan {
                line: 2,
                reason: "wrong REMOVE field count"
            })
        );
        assert_eq!(
            ManagePlan::parse("REMOVE|../escape|a.txt"),
            Err(ManageError::InvalidPath(PortfolioPathError::Traversal))
        );
    }
}
