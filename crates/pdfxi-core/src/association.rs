//! Declarative file-association manifest model (PLAT-005).
//!
//! This module models shell file associations as pure data: an extension maps
//! to a validated handler command. It validates the manifest and produces a
//! deterministic [`DryRunPlan`] describing what a host *would* register. It
//! deliberately does **not** write registry keys, spawn processes, or touch the
//! filesystem — that remains the host's responsibility and is out of scope for
//! the headless core. ADR 0008 requires a local, declarative equivalent, which
//! this module provides.

use thiserror::Error;

/// Maximum length of a file extension, excluding the leading dot.
pub const MAX_EXTENSION_LEN: usize = 16;
/// Maximum byte length of a handler program path.
pub const MAX_PROGRAM_BYTES: usize = 4_096;
/// Maximum number of arguments on one handler command.
pub const MAX_HANDLER_ARGS: usize = 32;
/// Maximum number of associations in one manifest.
pub const MAX_ASSOCIATIONS: usize = 512;

/// A validated handler command: a program plus literal arguments.
///
/// Arguments are stored verbatim and are never joined into a shell string, so
/// the plan cannot introduce shell interpretation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HandlerCommand {
    program: String,
    args: Vec<String>,
}

impl HandlerCommand {
    /// Create and validate a handler command.
    ///
    /// The program must be non-empty, bounded and free of control characters
    /// and shell metacharacters. Each argument must be non-empty and bounded.
    ///
    /// # Errors
    ///
    /// Returns a typed [`AssociationError`] for a rejected program or argument.
    pub fn new(program: &str, args: Vec<String>) -> Result<Self, AssociationError> {
        validate_program(program)?;
        if args.len() > MAX_HANDLER_ARGS {
            return Err(AssociationError::TooManyArgs(args.len()));
        }
        for arg in &args {
            if arg.is_empty() {
                return Err(AssociationError::EmptyArg);
            }
            if arg.len() > MAX_PROGRAM_BYTES {
                return Err(AssociationError::NameTooLong(arg.len()));
            }
            if arg.chars().any(|c| c.is_control()) {
                return Err(AssociationError::ControlCharacter);
            }
        }
        Ok(Self {
            program: program.to_owned(),
            args,
        })
    }

    /// Return the validated program path or name.
    pub fn program(&self) -> &str {
        &self.program
    }

    /// Return the literal arguments.
    pub fn args(&self) -> &[String] {
        &self.args
    }
}

/// One validated extension-to-handler association.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssociationEntry {
    extension: String,
    handler: HandlerCommand,
    description: String,
}

impl AssociationEntry {
    /// Create and validate one association.
    ///
    /// `extension` may be written with or without a leading dot; it is
    /// normalized to lowercase without the dot. The description must be
    /// non-empty, bounded and free of control characters.
    ///
    /// # Errors
    ///
    /// Returns a typed [`AssociationError`] for an invalid extension,
    /// handler or description.
    pub fn new(
        extension: &str,
        handler: HandlerCommand,
        description: &str,
    ) -> Result<Self, AssociationError> {
        let extension = normalize_extension(extension)?;
        if description.is_empty() {
            return Err(AssociationError::EmptyDescription);
        }
        if description.len() > MAX_PROGRAM_BYTES {
            return Err(AssociationError::NameTooLong(description.len()));
        }
        if description.chars().any(|c| c.is_control()) {
            return Err(AssociationError::ControlCharacter);
        }
        Ok(Self {
            extension,
            handler,
            description: description.to_owned(),
        })
    }

    /// Return the normalized lowercase extension (no leading dot).
    pub fn extension(&self) -> &str {
        &self.extension
    }

    /// Return the handler command.
    pub fn handler(&self) -> &HandlerCommand {
        &self.handler
    }

    /// Return the human-readable description.
    pub fn description(&self) -> &str {
        &self.description
    }
}

/// A validated, declarative association manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssociationManifest {
    entries: Vec<AssociationEntry>,
}

impl AssociationManifest {
    /// Create and validate a manifest.
    ///
    /// Extensions must be unique case-insensitively; the manifest must be
    /// non-empty and not exceed [`MAX_ASSOCIATIONS`] entries.
    ///
    /// # Errors
    ///
    /// Returns a typed [`AssociationError`] for an empty, oversized or
    /// duplicate-containing manifest.
    pub fn new(entries: Vec<AssociationEntry>) -> Result<Self, AssociationError> {
        if entries.is_empty() {
            return Err(AssociationError::EmptyManifest);
        }
        if entries.len() > MAX_ASSOCIATIONS {
            return Err(AssociationError::TooManyAssociations(entries.len()));
        }
        let mut seen = std::collections::BTreeSet::new();
        for entry in &entries {
            if !seen.insert(entry.extension().to_owned()) {
                return Err(AssociationError::DuplicateExtension(
                    entry.extension().to_owned(),
                ));
            }
        }
        Ok(Self { entries })
    }

    /// Return the entries in their supplied order.
    pub fn entries(&self) -> &[AssociationEntry] {
        &self.entries
    }
}

/// A deterministic dry-run plan.
///
/// The plan contains textual lines only. Producing it never writes registry
/// keys or creates associations; a host may log or display it, and only an
/// explicit host action would apply it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DryRunPlan {
    lines: Vec<String>,
    summary: String,
}

impl DryRunPlan {
    /// Return the ordered dry-run lines.
    pub fn lines(&self) -> &[String] {
        &self.lines
    }

    /// Return the byte-for-byte deterministic summary string.
    pub fn summary(&self) -> &str {
        &self.summary
    }
}

/// Typed errors produced by association-manifest validation.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum AssociationError {
    /// The extension was empty or held invalid characters.
    #[error("invalid file extension: {0}")]
    InvalidExtension(String),
    /// The handler program was empty or invalid.
    #[error("invalid handler program: {0}")]
    InvalidProgram(String),
    /// A handler argument was empty.
    #[error("handler argument must not be empty")]
    EmptyArg,
    /// The description was empty.
    #[error("association description must not be empty")]
    EmptyDescription,
    /// The manifest contained no entries.
    #[error("association manifest must not be empty")]
    EmptyManifest,
    /// A string exceeded the documented byte limit.
    #[error("association string is {0} bytes (limit 4096)")]
    NameTooLong(usize),
    /// A handler had too many arguments.
    #[error("handler has {0} arguments (limit 32)")]
    TooManyArgs(usize),
    /// The manifest had too many entries.
    #[error("association manifest has {0} entries (limit 512)")]
    TooManyAssociations(usize),
    /// A shell metacharacter was found in the program.
    #[error("handler program contains a forbidden shell character")]
    ForbiddenShellCharacter,
    /// A control character was found in a string.
    #[error("association string contains a control character")]
    ControlCharacter,
    /// Two entries declared the same extension.
    #[error("duplicate file extension: {0}")]
    DuplicateExtension(String),
}

/// Normalize and validate a file extension.
fn normalize_extension(extension: &str) -> Result<String, AssociationError> {
    let trimmed = extension.strip_prefix('.').unwrap_or(extension);
    if trimmed.is_empty() || trimmed.len() > MAX_EXTENSION_LEN {
        return Err(AssociationError::InvalidExtension(extension.to_owned()));
    }
    if !trimmed.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return Err(AssociationError::InvalidExtension(extension.to_owned()));
    }
    Ok(trimmed.to_ascii_lowercase())
}

/// Validate a handler program name or path.
fn validate_program(program: &str) -> Result<(), AssociationError> {
    if program.is_empty() || program.len() > MAX_PROGRAM_BYTES {
        return Err(AssociationError::InvalidProgram(program.to_owned()));
    }
    if program.chars().any(|c| c.is_control()) {
        return Err(AssociationError::ControlCharacter);
    }
    for forbidden in [';', '|', '&', '`', '$', '\n', '\r'] {
        if program.contains(forbidden) {
            return Err(AssociationError::ForbiddenShellCharacter);
        }
    }
    Ok(())
}

/// Build the deterministic dry-run plan for a validated manifest.
///
/// Entries are ordered by ascending normalized extension. Each line has the
/// form `.ext -> program arg1 arg2: description`. The summary is the lines
/// joined with `; ` and prefixed with `associations=<count>`.
pub fn dry_run_plan(manifest: &AssociationManifest) -> DryRunPlan {
    let mut entries: Vec<&AssociationEntry> = manifest.entries().iter().collect();
    entries.sort_by(|left, right| left.extension().cmp(right.extension()));
    let mut lines = Vec::with_capacity(entries.len());
    for entry in entries {
        let mut line = format!(".{} -> {}", entry.extension(), entry.handler().program());
        for arg in entry.handler().args() {
            line.push(' ');
            line.push_str(arg);
        }
        line.push_str(": ");
        line.push_str(entry.description());
        lines.push(line);
    }
    let summary = format!("associations={}; {}", lines.len(), lines.join("; "));
    DryRunPlan { lines, summary }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn handler(program: &str) -> HandlerCommand {
        HandlerCommand::new(program, vec!["--open".to_owned()]).expect("valid handler")
    }

    fn entry(ext: &str, desc: &str) -> AssociationEntry {
        AssociationEntry::new(ext, handler(r"C:\bin\viewer.exe"), desc).expect("valid entry")
    }

    #[test]
    fn association_manifest_validation_accepts_a_valid_manifest() {
        let manifest = AssociationManifest::new(vec![
            entry("PDF", "PDF document"),
            entry(".txt", "Text file"),
        ])
        .expect("valid manifest");
        assert_eq!(manifest.entries().len(), 2);
        assert_eq!(manifest.entries()[0].extension(), "pdf");
        assert_eq!(manifest.entries()[1].extension(), "txt");
        assert_eq!(manifest.entries()[0].handler().args(), &["--open"]);
    }

    #[test]
    fn association_manifest_rejects_invalid_extensions_and_duplicates() {
        assert!(matches!(
            AssociationEntry::new("a/b", handler("viewer"), "desc"),
            Err(AssociationError::InvalidExtension(_))
        ));
        assert!(matches!(
            AssociationEntry::new("", handler("viewer"), "desc"),
            Err(AssociationError::InvalidExtension(_))
        ));
        assert!(matches!(
            AssociationManifest::new(vec![entry("pdf", "a"), entry(".PDF", "b")]),
            Err(AssociationError::DuplicateExtension(_))
        ));
        assert!(matches!(
            AssociationManifest::new(vec![]),
            Err(AssociationError::EmptyManifest)
        ));
    }

    #[test]
    fn association_manifest_rejects_dangerous_handlers() {
        assert!(matches!(
            HandlerCommand::new("viewer; calc", vec![]),
            Err(AssociationError::ForbiddenShellCharacter)
        ));
        assert!(matches!(
            HandlerCommand::new("", vec![]),
            Err(AssociationError::InvalidProgram(_))
        ));
        assert!(matches!(
            HandlerCommand::new("viewer", vec![String::new()]),
            Err(AssociationError::EmptyArg)
        ));
        assert!(matches!(
            AssociationEntry::new("pdf", handler("viewer"), ""),
            Err(AssociationError::EmptyDescription)
        ));
    }

    #[test]
    fn dry_run_plan_is_deterministic_regardless_of_input_order() {
        let first = AssociationManifest::new(vec![entry("txt", "Text"), entry("pdf", "PDF")])
            .expect("first");
        let second = AssociationManifest::new(vec![entry("pdf", "PDF"), entry("txt", "Text")])
            .expect("second");
        let plan_a = dry_run_plan(&first);
        let plan_b = dry_run_plan(&second);
        assert_eq!(plan_a, plan_b);
        assert_eq!(plan_a.summary(), plan_b.summary());
        assert_eq!(plan_a.lines().len(), 2);
        assert!(plan_a.lines()[0].starts_with(".pdf -> "));
        assert!(plan_a.lines()[1].starts_with(".txt -> "));
        assert!(plan_a.summary().starts_with("associations=2; "));
    }

    #[test]
    fn dry_run_plan_does_not_spawn_or_write() {
        // The plan is pure data: building it has no observable side effect and
        // repeats identically. (No registry, process, or filesystem API exists
        // in this module.)
        let manifest = AssociationManifest::new(vec![entry("pdf", "PDF")]).expect("manifest");
        let a = dry_run_plan(&manifest);
        let b = dry_run_plan(&manifest);
        assert_eq!(a, b);
        assert_eq!(a.lines(), b.lines());
    }

    #[test]
    fn association_typed_errors_are_distinct() {
        let a = AssociationError::EmptyManifest;
        let b = AssociationError::DuplicateExtension("pdf".to_owned());
        assert_ne!(a, b);
        assert!(b.to_string().contains("pdf"));
        assert!(
            AssociationError::TooManyAssociations(600)
                .to_string()
                .contains("600")
        );
    }
}
