//! Declarative file-association manifest model (PLAT-005).
//!
//! This module models shell file associations as pure data: an extension maps
//! to a validated handler command or, in the richer descriptor form, an
//! extension/protocol maps to a [`ProgId`] and a [`CommandTemplate`] with a
//! safely quoted `%1` file placeholder. It validates manifests and descriptor
//! registries and produces deterministic [`DryRunPlan`] and
//! [`DescriptorDocument`] outputs describing what a host *would* register. It
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

/// Maximum accepted byte length of a program identifier.
pub const MAX_PROGID_BYTES: usize = 128;
/// Maximum accepted byte length of a command template.
pub const MAX_TEMPLATE_BYTES: usize = 4_096;
/// Maximum accepted byte length of a protocol scheme.
pub const MAX_SCHEME_BYTES: usize = 64;

/// A validated programmatic identifier (ProgID) used by shell descriptors.
///
/// A ProgID is a dotted ASCII identifier such as `PDFXI.Document.1`: letters,
/// digits, `_` and `.` are allowed, but a leading/trailing dot or a doubled dot
/// is not. It is pure data; no registry key is created.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProgId {
    value: String,
}

impl ProgId {
    /// Create and validate a ProgID.
    ///
    /// # Errors
    ///
    /// Returns [`AssociationError::InvalidProgId`] for an empty, oversized or
    /// malformed identifier and [`AssociationError::ControlCharacter`] for a
    /// control character.
    pub fn new(value: &str) -> Result<Self, AssociationError> {
        if value.is_empty() || value.len() > MAX_PROGID_BYTES {
            return Err(AssociationError::InvalidProgId(value.to_owned()));
        }
        if value.chars().any(|c| c.is_control()) {
            return Err(AssociationError::ControlCharacter);
        }
        let valid = value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'.')
            && !value.starts_with('.')
            && !value.ends_with('.')
            && !value.contains("..");
        if !valid {
            return Err(AssociationError::InvalidProgId(value.to_owned()));
        }
        Ok(Self {
            value: value.to_owned(),
        })
    }

    /// Return the validated identifier.
    pub fn as_str(&self) -> &str {
        &self.value
    }
}

/// A validated command template containing exactly one `%1` file placeholder.
///
/// The template is stored verbatim and is never handed to a shell. At render
/// time the placeholder is replaced by a safely quoted file argument built by
/// [`quote_argument`], so a hostile file name cannot introduce extra arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandTemplate {
    template: String,
}

impl CommandTemplate {
    /// Create and validate a command template.
    ///
    /// The template must be non-empty, bounded, free of control characters and
    /// shell metacharacters, and contain exactly one `%1` placeholder.
    ///
    /// # Errors
    ///
    /// Returns [`AssociationError::MissingPlaceholder`] when the placeholder is
    /// absent or repeated, and other typed errors for invalid content.
    pub fn new(template: &str) -> Result<Self, AssociationError> {
        if template.is_empty() || template.len() > MAX_TEMPLATE_BYTES {
            return Err(AssociationError::InvalidCommandTemplate(
                template.to_owned(),
            ));
        }
        if template.chars().any(|c| c.is_control()) {
            return Err(AssociationError::ControlCharacter);
        }
        for forbidden in [';', '|', '&', '`', '$'] {
            if template.contains(forbidden) {
                return Err(AssociationError::ForbiddenShellCharacter);
            }
        }
        if template.matches("%1").count() != 1 {
            return Err(AssociationError::MissingPlaceholder(template.to_owned()));
        }
        Ok(Self {
            template: template.to_owned(),
        })
    }

    /// Return the validated template text.
    pub fn as_str(&self) -> &str {
        &self.template
    }

    /// Render the template with a safely quoted `file` substituted for `%1`.
    ///
    /// # Errors
    ///
    /// Returns [`AssociationError::EmptyArg`] or
    /// [`AssociationError::ControlCharacter`] for an unsafe file argument.
    pub fn render(&self, file: &str) -> Result<String, AssociationError> {
        let quoted = quote_argument(file)?;
        Ok(self.template.replace("%1", &quoted))
    }
}

/// Quote one literal argument for a Windows command line.
///
/// The result is deterministic: a bare token is returned unchanged, while an
/// argument containing whitespace or a double quote is wrapped in quotes with
/// embedded quotes and trailing backslashes escaped using the standard
/// `CommandLineToArgvW` rules. Control characters (including NUL) are rejected.
///
/// # Errors
///
/// Returns [`AssociationError::EmptyArg`] for an empty argument and
/// [`AssociationError::ControlCharacter`] for a control character.
pub fn quote_argument(argument: &str) -> Result<String, AssociationError> {
    if argument.is_empty() {
        return Err(AssociationError::EmptyArg);
    }
    if argument.chars().any(|c| c.is_control()) {
        return Err(AssociationError::ControlCharacter);
    }
    let needs_quotes = argument
        .bytes()
        .any(|b| b.is_ascii_whitespace() || b == b'"');
    if !needs_quotes {
        return Ok(argument.to_owned());
    }
    let mut out = String::with_capacity(argument.len().saturating_add(2));
    out.push('"');
    let mut backslashes = 0usize;
    for c in argument.chars() {
        if c == '\\' {
            backslashes = backslashes.saturating_add(1);
        } else if c == '"' {
            for _ in 0..backslashes.saturating_mul(2).saturating_add(1) {
                out.push('\\');
            }
            out.push('"');
            backslashes = 0;
        } else {
            for _ in 0..backslashes {
                out.push('\\');
            }
            out.push(c);
            backslashes = 0;
        }
    }
    for _ in 0..backslashes.saturating_mul(2) {
        out.push('\\');
    }
    out.push('"');
    Ok(out)
}

/// A validated file-association descriptor: extension -> ProgID -> template.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellDescriptor {
    extension: String,
    progid: ProgId,
    command: CommandTemplate,
    description: String,
}

impl ShellDescriptor {
    /// Create and validate a shell descriptor.
    ///
    /// # Errors
    ///
    /// Returns a typed [`AssociationError`] for an invalid extension, ProgID,
    /// template or description.
    pub fn new(
        extension: &str,
        progid: ProgId,
        command: CommandTemplate,
        description: &str,
    ) -> Result<Self, AssociationError> {
        let extension = normalize_extension(extension)?;
        validate_description(description)?;
        Ok(Self {
            extension,
            progid,
            command,
            description: description.to_owned(),
        })
    }

    /// Return the normalized lowercase extension (no leading dot).
    pub fn extension(&self) -> &str {
        &self.extension
    }

    /// Return the associated ProgID.
    pub const fn progid(&self) -> &ProgId {
        &self.progid
    }

    /// Return the command template.
    pub const fn command(&self) -> &CommandTemplate {
        &self.command
    }

    /// Return the human-readable description.
    pub fn description(&self) -> &str {
        &self.description
    }
}

/// A validated protocol-handler descriptor: scheme -> ProgID -> template.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProtocolDescriptor {
    scheme: String,
    progid: ProgId,
    command: CommandTemplate,
    description: String,
}

impl ProtocolDescriptor {
    /// Create and validate a protocol descriptor.
    ///
    /// The scheme must start with an ASCII letter and contain only letters,
    /// digits, `+`, `-` and `.`, within [`MAX_SCHEME_BYTES`].
    ///
    /// # Errors
    ///
    /// Returns a typed [`AssociationError`] for an invalid scheme, ProgID,
    /// template or description.
    pub fn new(
        scheme: &str,
        progid: ProgId,
        command: CommandTemplate,
        description: &str,
    ) -> Result<Self, AssociationError> {
        validate_scheme(scheme)?;
        validate_description(description)?;
        Ok(Self {
            scheme: scheme.to_ascii_lowercase(),
            progid,
            command,
            description: description.to_owned(),
        })
    }

    /// Return the normalized lowercase scheme.
    pub fn scheme(&self) -> &str {
        &self.scheme
    }

    /// Return the associated ProgID.
    pub const fn progid(&self) -> &ProgId {
        &self.progid
    }

    /// Return the command template.
    pub const fn command(&self) -> &CommandTemplate {
        &self.command
    }

    /// Return the human-readable description.
    pub fn description(&self) -> &str {
        &self.description
    }
}

/// A validated registry of shell and protocol descriptors.
///
/// Extensions and schemes are each unique case-insensitively; the registry is
/// non-empty and bounded. It holds only data and never touches the OS.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DescriptorRegistry {
    associations: Vec<ShellDescriptor>,
    protocols: Vec<ProtocolDescriptor>,
}

impl DescriptorRegistry {
    /// Create and validate a descriptor registry.
    ///
    /// # Errors
    ///
    /// Returns [`AssociationError::EmptyRegistry`],
    /// [`AssociationError::TooManyDescriptors`] or a duplicate error for an
    /// invalid combination.
    pub fn new(
        associations: Vec<ShellDescriptor>,
        protocols: Vec<ProtocolDescriptor>,
    ) -> Result<Self, AssociationError> {
        let total = associations.len().saturating_add(protocols.len());
        if total == 0 {
            return Err(AssociationError::EmptyRegistry);
        }
        if total > MAX_ASSOCIATIONS {
            return Err(AssociationError::TooManyDescriptors(total));
        }
        let mut extensions = std::collections::BTreeSet::new();
        for entry in &associations {
            if !extensions.insert(entry.extension().to_owned()) {
                return Err(AssociationError::DuplicateExtension(
                    entry.extension().to_owned(),
                ));
            }
        }
        let mut schemes = std::collections::BTreeSet::new();
        for entry in &protocols {
            if !schemes.insert(entry.scheme().to_owned()) {
                return Err(AssociationError::DuplicateScheme(entry.scheme().to_owned()));
            }
        }
        Ok(Self {
            associations,
            protocols,
        })
    }

    /// Return the shell descriptors in their supplied order.
    pub fn associations(&self) -> &[ShellDescriptor] {
        &self.associations
    }

    /// Return the protocol descriptors in their supplied order.
    pub fn protocols(&self) -> &[ProtocolDescriptor] {
        &self.protocols
    }
}

/// A deterministic, pure-data descriptor document produced by export.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DescriptorDocument {
    lines: Vec<String>,
    summary: String,
}

impl DescriptorDocument {
    /// Return the ordered descriptor lines.
    pub fn lines(&self) -> &[String] {
        &self.lines
    }

    /// Return the byte-for-byte deterministic summary string.
    pub fn summary(&self) -> &str {
        &self.summary
    }
}

/// Validate a protocol scheme name.
fn validate_scheme(scheme: &str) -> Result<(), AssociationError> {
    let valid = !scheme.is_empty()
        && scheme.len() <= MAX_SCHEME_BYTES
        && scheme
            .bytes()
            .next()
            .is_some_and(|first| first.is_ascii_alphabetic())
        && scheme
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'-' | b'.'));
    if valid {
        Ok(())
    } else {
        Err(AssociationError::InvalidScheme(scheme.to_owned()))
    }
}

/// Validate a shared human-readable description field.
fn validate_description(description: &str) -> Result<(), AssociationError> {
    if description.is_empty() {
        return Err(AssociationError::EmptyDescription);
    }
    if description.len() > MAX_PROGRAM_BYTES {
        return Err(AssociationError::NameTooLong(description.len()));
    }
    if description.chars().any(|c| c.is_control()) {
        return Err(AssociationError::ControlCharacter);
    }
    Ok(())
}

/// Export a registry into a deterministic [`DescriptorDocument`].
///
/// Shell descriptors are sorted by extension, protocol descriptors by scheme,
/// so the document is independent of construction order. Each line is a stable
/// `key=value; ...` sequence and the summary prefixes the counts. No file is
/// written and no OS association is applied.
pub fn export_descriptors(registry: &DescriptorRegistry) -> DescriptorDocument {
    let mut associations: Vec<&ShellDescriptor> = registry.associations().iter().collect();
    associations.sort_by(|left, right| left.extension().cmp(right.extension()));
    let mut protocols: Vec<&ProtocolDescriptor> = registry.protocols().iter().collect();
    protocols.sort_by(|left, right| left.scheme().cmp(right.scheme()));

    let mut lines = Vec::with_capacity(associations.len().saturating_add(protocols.len()));
    for entry in associations {
        lines.push(format!(
            "assoc.ext={}; progid={}; command={}; description={}",
            entry.extension(),
            entry.progid().as_str(),
            entry.command().as_str(),
            entry.description()
        ));
    }
    for entry in protocols {
        lines.push(format!(
            "proto.scheme={}; progid={}; command={}; description={}",
            entry.scheme(),
            entry.progid().as_str(),
            entry.command().as_str(),
            entry.description()
        ));
    }
    let summary = format!(
        "associations={}; protocols={}; {}",
        registry.associations().len(),
        registry.protocols().len(),
        lines.join(" | ")
    );
    DescriptorDocument { lines, summary }
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
    /// The ProgID was empty, oversized or malformed.
    #[error("invalid ProgID: {0}")]
    InvalidProgId(String),
    /// The protocol scheme was empty or malformed.
    #[error("invalid protocol scheme: {0}")]
    InvalidScheme(String),
    /// The command template was empty or oversized.
    #[error("invalid command template: {0}")]
    InvalidCommandTemplate(String),
    /// The command template did not contain exactly one `%1` placeholder.
    #[error("command template must contain exactly one %1 placeholder: {0}")]
    MissingPlaceholder(String),
    /// A rendered file argument was empty or unsafe.
    #[error("unsafe command argument: {0}")]
    UnsafeArgument(String),
    /// Two protocol descriptors declared the same scheme.
    #[error("duplicate protocol scheme: {0}")]
    DuplicateScheme(String),
    /// The descriptor registry held no entries.
    #[error("descriptor registry must not be empty")]
    EmptyRegistry,
    /// The descriptor registry exceeded the documented entry limit.
    #[error("descriptor registry has {0} entries (limit 512)")]
    TooManyDescriptors(usize),
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

    // ---- T-0181 PLAT-005 descriptors, quoting and deterministic export ----

    fn progid() -> ProgId {
        ProgId::new("PDFXI.Document.1").expect("valid progid")
    }

    fn template() -> CommandTemplate {
        CommandTemplate::new(r#"C:\bin\viewer.exe "%1""#).expect("valid template")
    }

    #[test]
    fn progid_and_scheme_validation_has_clear_boundaries() {
        assert_eq!(progid().as_str(), "PDFXI.Document.1");
        assert!(matches!(
            ProgId::new(""),
            Err(AssociationError::InvalidProgId(_))
        ));
        assert!(matches!(
            ProgId::new(".Leading"),
            Err(AssociationError::InvalidProgId(_))
        ));
        assert!(matches!(
            ProgId::new("double..dot"),
            Err(AssociationError::InvalidProgId(_))
        ));
        assert!(matches!(
            ProgId::new("bad name"),
            Err(AssociationError::InvalidProgId(_))
        ));
        assert_eq!(
            ProtocolDescriptor::new("PDFXI", progid(), template(), "PDFXI protocol")
                .expect("protocol")
                .scheme(),
            "pdfxi"
        );
        assert!(matches!(
            ProtocolDescriptor::new("", progid(), template(), "desc"),
            Err(AssociationError::InvalidScheme(_))
        ));
        assert!(matches!(
            ProtocolDescriptor::new("1bad", progid(), template(), "desc"),
            Err(AssociationError::InvalidScheme(_))
        ));
    }

    #[test]
    fn command_template_validation_requires_exactly_one_placeholder() {
        assert_eq!(template().as_str(), r#"C:\bin\viewer.exe "%1""#);
        assert!(matches!(
            CommandTemplate::new("viewer.exe"),
            Err(AssociationError::MissingPlaceholder(_))
        ));
        assert!(matches!(
            CommandTemplate::new("viewer.exe %1 %1"),
            Err(AssociationError::MissingPlaceholder(_))
        ));
        assert!(matches!(
            CommandTemplate::new("viewer.exe %1; calc"),
            Err(AssociationError::ForbiddenShellCharacter)
        ));
        assert!(matches!(
            CommandTemplate::new("viewer.exe %1\n"),
            Err(AssociationError::ControlCharacter)
        ));
    }

    #[test]
    fn quote_argument_escapes_spaces_quotes_and_trailing_backslashes() {
        assert_eq!(quote_argument("plain").expect("plain"), "plain");
        assert_eq!(
            quote_argument(r"C:\Program Files\a b.pdf").expect("space"),
            r#""C:\Program Files\a b.pdf""#
        );
        assert_eq!(quote_argument(r#"a"b"#).expect("quote"), r#""a\"b""#);
        assert_eq!(quote_argument("trail\\").expect("trail"), "trail\\");
        assert_eq!(
            quote_argument("has space and trail\\").expect("space trail"),
            r#""has space and trail\\""#
        );
        assert!(matches!(
            quote_argument(""),
            Err(AssociationError::EmptyArg)
        ));
        assert!(matches!(
            quote_argument("bad\0arg"),
            Err(AssociationError::ControlCharacter)
        ));
    }

    #[test]
    fn descriptor_registry_export_is_deterministic_regardless_of_order() {
        let pdf = ShellDescriptor::new("PDF", progid(), template(), "PDF document")
            .expect("pdf descriptor");
        let txt = ShellDescriptor::new(".txt", progid(), template(), "Text file")
            .expect("txt descriptor");
        let proto = ProtocolDescriptor::new("pdfxi", progid(), template(), "PDFXI protocol")
            .expect("protocol");
        let first = DescriptorRegistry::new(vec![txt.clone(), pdf.clone()], vec![proto.clone()])
            .expect("first registry");
        let second = DescriptorRegistry::new(vec![pdf, txt], vec![proto]).expect("second registry");
        let exported = export_descriptors(&first);
        assert_eq!(exported, export_descriptors(&second));
        assert_eq!(exported.lines().len(), 3);
        assert!(exported.lines()[0].starts_with("assoc.ext=pdf;"));
        assert!(exported.lines()[1].starts_with("assoc.ext=txt;"));
        assert!(exported.lines()[2].starts_with("proto.scheme=pdfxi;"));
        assert!(
            exported
                .summary()
                .starts_with("associations=2; protocols=1; ")
        );
        assert_eq!(exported.summary(), export_descriptors(&second).summary());
    }

    #[test]
    fn descriptor_registry_rejects_duplicates_and_empty_input() {
        let pdf = ShellDescriptor::new("pdf", progid(), template(), "PDF document")
            .expect("pdf descriptor");
        let other = ShellDescriptor::new(".PDF", progid(), template(), "duplicate")
            .expect("duplicate descriptor");
        assert!(matches!(
            DescriptorRegistry::new(vec![pdf, other], vec![]),
            Err(AssociationError::DuplicateExtension(_))
        ));
        assert!(matches!(
            DescriptorRegistry::new(vec![], vec![]),
            Err(AssociationError::EmptyRegistry)
        ));
        let proto = ProtocolDescriptor::new("pdfxi", progid(), template(), "PDFXI protocol")
            .expect("protocol");
        let duplicate = ProtocolDescriptor::new("PDFXI", progid(), template(), "duplicate")
            .expect("duplicate protocol");
        assert!(matches!(
            DescriptorRegistry::new(vec![], vec![proto, duplicate]),
            Err(AssociationError::DuplicateScheme(_))
        ));
    }

    #[test]
    fn command_template_render_substitutes_a_quoted_argument() {
        let template = CommandTemplate::new(r"viewer.exe --open %1").expect("template");
        assert_eq!(
            template.render(r"C:\a b.pdf").expect("render"),
            r#"viewer.exe --open "C:\a b.pdf""#
        );
        assert_eq!(
            template.render(r"C:\a b.pdf").expect("render again"),
            template.render(r"C:\a b.pdf").expect("render third")
        );
        assert!(matches!(
            template.render(""),
            Err(AssociationError::EmptyArg)
        ));
    }
}
