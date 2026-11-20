//! Local file-drop conversion contract for Office-derived bundles (PLAT-003).
//!
//! This module is the core-side counterpart of the file-level Office adapters:
//! it models a *dropped bundle envelope* supplied entirely by the caller and
//! resolves it into a deterministic [`ConversionPlan`]. There is no ZIP
//! parsing, no COM automation, no process spawning, no filesystem access and no
//! network. The host is responsible for feeding real file bytes into the
//! existing conversion crates; this module only validates the envelope and
//! decides the stable order and naming of the planned steps.
//!
//! ADR 0008 calls for a local, file-based equivalent for retired host
//! integrations. This is that local equivalent for Office integration: a
//! declarative plan, not an operating-system hook.

use thiserror::Error;

/// Maximum number of entries in one dropped bundle.
pub const MAX_BUNDLE_ENTRIES: usize = 256;
/// Maximum byte length of a title or entry name.
pub const MAX_BUNDLE_NAME_BYTES: usize = 512;

/// The family of Office documents a bundle was derived from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OfficeKind {
    /// A Word-derived document.
    Word,
    /// An Excel-derived workbook.
    Excel,
    /// A PowerPoint-derived presentation.
    PowerPoint,
}

impl OfficeKind {
    /// Return the stable lowercase token for this kind.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Word => "word",
            Self::Excel => "excel",
            Self::PowerPoint => "powerpoint",
        }
    }

    /// Return the conventional source extension for this kind.
    pub const fn source_extension(self) -> &'static str {
        match self {
            Self::Word => "docx",
            Self::Excel => "xlsx",
            Self::PowerPoint => "pptx",
        }
    }

    /// Parse a kind token (case-insensitive).
    ///
    /// # Errors
    ///
    /// Returns [`OfficeDropError::UnsupportedKind`] for an unknown token.
    pub fn parse(token: &str) -> Result<Self, OfficeDropError> {
        match token.to_ascii_lowercase().as_str() {
            "word" | "docx" | "doc" => Ok(Self::Word),
            "excel" | "xlsx" | "xls" => Ok(Self::Excel),
            "powerpoint" | "pptx" | "ppt" => Ok(Self::PowerPoint),
            other => Err(OfficeDropError::UnsupportedKind(other.to_owned())),
        }
    }
}

/// The caller's description of the content in one bundle entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentHint {
    /// Plain UTF-8 text.
    Text,
    /// A saved HTML fragment.
    HtmlFragment,
    /// A spreadsheet-like table.
    Table,
    /// A presentation slide.
    Slide,
}

impl ContentHint {
    /// Return the stable lowercase token for this hint.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::HtmlFragment => "html-fragment",
            Self::Table => "table",
            Self::Slide => "slide",
        }
    }

    /// Parse a content-hint token (case-insensitive).
    ///
    /// # Errors
    ///
    /// Returns [`OfficeDropError::UnsupportedHint`] for an unknown token.
    pub fn parse(token: &str) -> Result<Self, OfficeDropError> {
        match token.to_ascii_lowercase().as_str() {
            "text" => Ok(Self::Text),
            "html-fragment" => Ok(Self::HtmlFragment),
            "table" => Ok(Self::Table),
            "slide" => Ok(Self::Slide),
            other => Err(OfficeDropError::UnsupportedHint(other.to_owned())),
        }
    }
}

/// One entry in a dropped Office-derived bundle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BundleEntry {
    name: String,
    order: u32,
    hint: ContentHint,
}

impl BundleEntry {
    /// Create and validate a bundle entry.
    ///
    /// `name` must be a safe, traversal-free relative path (see
    /// [`validate_relative_name`]). `order` is a caller-supplied stable sort
    /// key; ties are broken by name so output is always deterministic.
    ///
    /// # Errors
    ///
    /// Returns a typed [`OfficeDropError`] for an invalid name or active
    /// content marker.
    pub fn new(name: &str, order: u32, hint: ContentHint) -> Result<Self, OfficeDropError> {
        validate_relative_name(name)?;
        reject_active_content(name)?;
        Ok(Self {
            name: name.to_owned(),
            order,
            hint,
        })
    }

    /// Return the validated relative name of this entry.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Return the caller's stable order key.
    pub const fn order(&self) -> u32 {
        self.order
    }

    /// Return the content hint.
    pub const fn hint(&self) -> ContentHint {
        self.hint
    }
}

/// A caller-supplied Office-derived bundle envelope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OfficeDropEnvelope {
    kind: OfficeKind,
    title: String,
    entries: Vec<BundleEntry>,
}

impl OfficeDropEnvelope {
    /// Create and validate a bundle envelope.
    ///
    /// The title must be non-empty, bounded and free of active-content
    /// markers. At least one and at most [`MAX_BUNDLE_ENTRIES`] entries are
    /// required, and entry names must be unique case-insensitively.
    ///
    /// # Errors
    ///
    /// Returns a typed [`OfficeDropError`] describing the first violation.
    pub fn new(
        kind: OfficeKind,
        title: &str,
        entries: Vec<BundleEntry>,
    ) -> Result<Self, OfficeDropError> {
        if title.is_empty() {
            return Err(OfficeDropError::EmptyTitle);
        }
        if title.len() > MAX_BUNDLE_NAME_BYTES {
            return Err(OfficeDropError::NameTooLong(title.len()));
        }
        reject_active_content(title)?;
        if entries.is_empty() {
            return Err(OfficeDropError::EmptyBundle);
        }
        if entries.len() > MAX_BUNDLE_ENTRIES {
            return Err(OfficeDropError::TooManyEntries(entries.len()));
        }
        let mut seen = std::collections::BTreeSet::new();
        for entry in &entries {
            let key = entry.name().to_ascii_lowercase();
            if !seen.insert(key.clone()) {
                return Err(OfficeDropError::DuplicateEntry(key));
            }
        }
        Ok(Self {
            kind,
            title: title.to_owned(),
            entries,
        })
    }

    /// Return the Office family of this bundle.
    pub const fn kind(&self) -> OfficeKind {
        self.kind
    }

    /// Return the validated title.
    pub fn title(&self) -> &str {
        &self.title
    }

    /// Return the entries in their supplied order.
    pub fn entries(&self) -> &[BundleEntry] {
        &self.entries
    }
}

/// One resolved step of a conversion plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversionStep {
    source_name: String,
    target_name: String,
    order: u32,
    hint: ContentHint,
}

impl ConversionStep {
    /// Return the validated source entry name.
    pub fn source_name(&self) -> &str {
        &self.source_name
    }

    /// Return the deterministic target name, always ending in `.pdf`.
    pub fn target_name(&self) -> &str {
        &self.target_name
    }

    /// Return the stable order key.
    pub const fn order(&self) -> u32 {
        self.order
    }

    /// Return the content hint carried over from the entry.
    pub const fn hint(&self) -> ContentHint {
        self.hint
    }
}

/// A deterministic conversion plan produced from an [`OfficeDropEnvelope`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversionPlan {
    kind: OfficeKind,
    steps: Vec<ConversionStep>,
    summary: String,
}

impl ConversionPlan {
    /// Return the Office family this plan was built for.
    pub const fn kind(&self) -> OfficeKind {
        self.kind
    }

    /// Return the ordered conversion steps.
    pub fn steps(&self) -> &[ConversionStep] {
        &self.steps
    }

    /// Return the byte-for-byte deterministic summary string.
    pub fn summary(&self) -> &str {
        &self.summary
    }
}

/// Typed errors produced by Office drop-envelope validation.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum OfficeDropError {
    /// The kind token was not recognized.
    #[error("unsupported Office bundle kind: {0}")]
    UnsupportedKind(String),
    /// The content-hint token was not recognized.
    #[error("unsupported content hint: {0}")]
    UnsupportedHint(String),
    /// The bundle title was empty.
    #[error("Office bundle title must not be empty")]
    EmptyTitle,
    /// The bundle had no entries.
    #[error("Office bundle must contain at least one entry")]
    EmptyBundle,
    /// The bundle had too many entries.
    #[error("Office bundle has {0} entries (limit 256)")]
    TooManyEntries(usize),
    /// A name exceeded the documented byte limit.
    #[error("Office bundle name is {0} bytes (limit 512)")]
    NameTooLong(usize),
    /// A name was empty, absolute, traversed, or held a forbidden character.
    #[error("unsafe Office bundle relative name: {0}")]
    UnsafeName(String),
    /// Two entries shared the same case-insensitive name.
    #[error("duplicate Office bundle entry name: {0}")]
    DuplicateEntry(String),
    /// Active content was detected in a name or title.
    #[error("active content marker is not allowed: {0}")]
    ActiveContent(String),
}

/// Validate a caller-supplied relative name for traversal and control characters.
///
/// A valid name is non-empty, at most [`MAX_BUNDLE_NAME_BYTES`] bytes, uses
/// forward slashes only, is neither absolute nor drive-qualified, and contains
/// no empty, `.` or `..` path component and no NUL/control character.
///
/// # Errors
///
/// Returns [`OfficeDropError::UnsafeName`] or
/// [`OfficeDropError::NameTooLong`] for a rejected name.
pub fn validate_relative_name(name: &str) -> Result<(), OfficeDropError> {
    if name.is_empty() || name.len() > MAX_BUNDLE_NAME_BYTES {
        return Err(if name.is_empty() {
            OfficeDropError::UnsafeName(name.to_owned())
        } else {
            OfficeDropError::NameTooLong(name.len())
        });
    }
    if name.starts_with('/') || name.contains('\\') || has_drive_prefix(name) {
        return Err(OfficeDropError::UnsafeName(name.to_owned()));
    }
    for component in name.split('/') {
        if component.is_empty() || component == "." || component == ".." {
            return Err(OfficeDropError::UnsafeName(name.to_owned()));
        }
    }
    if name.chars().any(|c| c.is_control()) {
        return Err(OfficeDropError::UnsafeName(name.to_owned()));
    }
    Ok(())
}

/// Return whether a name begins with a Windows drive or UNC-style prefix.
fn has_drive_prefix(name: &str) -> bool {
    let mut chars = name.chars();
    let first = match chars.next() {
        Some(value) => value,
        None => return false,
    };
    if first.is_ascii_alphabetic() {
        return matches!(chars.next(), Some(':'));
    }
    false
}

/// Reject text that looks like active web content.
fn reject_active_content(value: &str) -> Result<(), OfficeDropError> {
    let lowered = value.to_ascii_lowercase();
    for marker in [
        "<script",
        "javascript:",
        "<iframe",
        "<object",
        "<embed",
        "onerror=",
        "onload=",
    ] {
        if lowered.contains(marker) {
            return Err(OfficeDropError::ActiveContent(marker.to_owned()));
        }
    }
    Ok(())
}

/// Resolve a deterministic conversion plan from a validated envelope.
///
/// Steps are ordered by ascending `order`, then by ascending source name, so
/// input order cannot change the result. Each target name is the bundle title
/// with the entry's base name appended and a `.pdf` suffix; the source
/// extension comes from the bundle kind. The summary is a stable sequence of
/// `kind`, `title` and ordered `source>target(hint)` tokens.
pub fn plan_conversion(envelope: &OfficeDropEnvelope) -> ConversionPlan {
    let mut steps: Vec<ConversionStep> = envelope
        .entries()
        .iter()
        .map(|entry| {
            let target_name = format!("{}-{}.pdf", envelope.title(), entry.name());
            ConversionStep {
                source_name: entry.name().to_owned(),
                target_name,
                order: entry.order(),
                hint: entry.hint(),
            }
        })
        .collect();
    steps.sort_by(|left, right| {
        left.order
            .cmp(&right.order)
            .then_with(|| left.source_name.cmp(&right.source_name))
    });
    let mut summary = format!(
        "kind={}; title={}; source_ext={}",
        envelope.kind().as_str(),
        envelope.title(),
        envelope.kind().source_extension()
    );
    for step in &steps {
        summary.push_str(&format!(
            "; {}>{}({})",
            step.source_name,
            step.target_name,
            step.hint.as_str()
        ));
    }
    ConversionPlan {
        kind: envelope.kind(),
        steps,
        summary,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, order: u32, hint: ContentHint) -> BundleEntry {
        BundleEntry::new(name, order, hint).expect("valid entry")
    }

    #[test]
    fn office_envelope_validation_accepts_a_well_formed_bundle() {
        let envelope = OfficeDropEnvelope::new(
            OfficeKind::Word,
            "Quarterly report",
            vec![
                entry("body.html", 2, ContentHint::HtmlFragment),
                entry("notes.txt", 1, ContentHint::Text),
            ],
        )
        .expect("valid envelope");
        assert_eq!(envelope.kind(), OfficeKind::Word);
        assert_eq!(envelope.title(), "Quarterly report");
        assert_eq!(envelope.entries().len(), 2);
    }

    #[test]
    fn office_envelope_validation_rejects_bad_titles_and_duplicates() {
        assert!(matches!(
            OfficeDropEnvelope::new(
                OfficeKind::Word,
                "",
                vec![entry("a.txt", 0, ContentHint::Text)]
            ),
            Err(OfficeDropError::EmptyTitle)
        ));
        assert!(matches!(
            OfficeDropEnvelope::new(OfficeKind::Word, "T", vec![]),
            Err(OfficeDropError::EmptyBundle)
        ));
        assert!(matches!(
            OfficeDropEnvelope::new(
                OfficeKind::Word,
                "T",
                vec![
                    entry("A.txt", 0, ContentHint::Text),
                    entry("a.txt", 1, ContentHint::Text),
                ],
            ),
            Err(OfficeDropError::DuplicateEntry(_))
        ));
        assert!(matches!(
            OfficeDropEnvelope::new(
                OfficeKind::Word,
                "<script>alert(1)</script>",
                vec![entry("a.txt", 0, ContentHint::Text)],
            ),
            Err(OfficeDropError::ActiveContent(_))
        ));
    }

    #[test]
    fn conversion_plan_is_deterministic_and_ordered() {
        let first = OfficeDropEnvelope::new(
            OfficeKind::Excel,
            "Budget",
            vec![
                entry("sheet2.html", 9, ContentHint::HtmlFragment),
                entry("sheet1.txt", 1, ContentHint::Text),
            ],
        )
        .expect("first");
        let second = OfficeDropEnvelope::new(
            OfficeKind::Excel,
            "Budget",
            vec![
                entry("sheet1.txt", 1, ContentHint::Text),
                entry("sheet2.html", 9, ContentHint::HtmlFragment),
            ],
        )
        .expect("second");
        let plan_a = plan_conversion(&first);
        let plan_b = plan_conversion(&second);
        assert_eq!(plan_a, plan_b);
        assert_eq!(plan_a.summary(), plan_b.summary());
        assert_eq!(plan_a.steps()[0].source_name(), "sheet1.txt");
        assert_eq!(plan_a.steps()[1].source_name(), "sheet2.html");
        assert!(plan_a.steps()[0].target_name().ends_with(".pdf"));
        assert_eq!(plan_a.kind().source_extension(), "xlsx");
    }

    #[test]
    fn traversal_and_absolute_entry_names_are_rejected() {
        for bad in [
            "../evil.txt",
            "a/../b",
            "/abs.txt",
            "C:/abs.txt",
            "a\\b",
            "a//b",
            ".",
        ] {
            assert!(
                matches!(
                    BundleEntry::new(bad, 0, ContentHint::Text),
                    Err(OfficeDropError::UnsafeName(_))
                ),
                "name must be rejected: {bad}"
            );
        }
    }

    #[test]
    fn office_kind_and_hint_tokens_are_stable() {
        assert_eq!(
            OfficeKind::parse("PPTX").expect("kind"),
            OfficeKind::PowerPoint
        );
        assert_eq!(
            ContentHint::parse("html-fragment").expect("hint"),
            ContentHint::HtmlFragment
        );
        assert!(matches!(
            OfficeKind::parse("pdf"),
            Err(OfficeDropError::UnsupportedKind(_))
        ));
        assert!(matches!(
            ContentHint::parse("script"),
            Err(OfficeDropError::UnsupportedHint(_))
        ));
    }

    #[test]
    fn office_drop_typed_errors_are_distinct() {
        let a = OfficeDropError::EmptyBundle;
        let b = OfficeDropError::TooManyEntries(300);
        assert_ne!(a, b);
        assert!(b.to_string().contains("300"));
        assert!(
            OfficeDropError::NameTooLong(600)
                .to_string()
                .contains("600")
        );
    }
}
