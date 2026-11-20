//! Deterministic, library-level standards conformance planning over core facts.
//!
//! `pdfxi-core` must not depend on `pdfxi-standards` (see
//! `docs/ARCHITECTURE.md`), so this module exposes a small self-contained
//! contract for the standards rows that can be decided from facts a core caller
//! already has. It is a **subset preflight**, not certification: every plan
//! declares its [`CheckScope`] and lists the operations that are deliberately
//! left to a renderer, an accessibility checker, a security crate or an
//! external validator such as veraPDF. No rule fires on a fact the caller did
//! not supply; unknown facts produce an informational manual-check finding
//! instead of a false pass or a false failure.
//!
//! Reports are deterministic: findings are sorted by severity, code, message and
//! location, and every derived index is assigned in depth-first order.

use crate::Document;
use crate::compatibility::PdfVersion;
use crate::object::Object;
use crate::transparency::BlendMode;
use std::fmt;

/// Maximum number of caller-supplied names in a [`DocumentFacts`] snapshot.
pub const MAX_FACT_NAMES: usize = 256;
/// Maximum length of a single caller-supplied fact name.
pub const MAX_NAME_LEN: usize = 64;
/// Maximum page count accepted in a [`DocumentFacts`] snapshot.
pub const MAX_FACT_PAGES: usize = 1_000_000;

/// A typed reason a conformance snapshot or operation was rejected.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConformanceError {
    /// A code did not name a known standards row.
    #[error("unknown standard row: {0}")]
    UnknownRow(String),
    /// A caller-supplied name was empty, over-long or contained white space.
    #[error("invalid fact name: {0}")]
    InvalidFactName(String),
    /// Too many names were supplied for validation to stay bounded.
    #[error("snapshot has {count} names, above the {limit} limit")]
    TooManyFacts {
        /// Names supplied.
        count: usize,
        /// Maximum accepted names.
        limit: usize,
    },
    /// The page count exceeded the bounded range.
    #[error("snapshot reports {count} pages, above the {limit} limit")]
    TooManyPages {
        /// Pages reported.
        count: usize,
        /// Maximum accepted pages.
        limit: usize,
    },
    /// The requested operation has no library implementation.
    #[error("unsupported conformance operation: {0}")]
    UnsupportedOperation(&'static str),
}

/// The six standards rows closed at library level by this contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StandardRow {
    /// Transparency and blend modes (STD-005).
    TransparencyBlend,
    /// Tagged PDF and the structure tree (STD-006).
    TaggedStructure,
    /// PDF/A conformance (STD-007).
    PdfA,
    /// PDF/X conformance (STD-008).
    PdfX,
    /// PDF/UA conformance (STD-009).
    PdfUa,
    /// Incremental update and digital preservation (STD-010).
    IncrementalPreservation,
}

impl StandardRow {
    /// All rows in canonical code order.
    #[must_use]
    pub const fn all() -> &'static [StandardRow] {
        &[
            Self::TransparencyBlend,
            Self::TaggedStructure,
            Self::PdfA,
            Self::PdfX,
            Self::PdfUa,
            Self::IncrementalPreservation,
        ]
    }

    /// Return the stable feature-matrix code, for example `STD-007`.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::TransparencyBlend => "STD-005",
            Self::TaggedStructure => "STD-006",
            Self::PdfA => "STD-007",
            Self::PdfX => "STD-008",
            Self::PdfUa => "STD-009",
            Self::IncrementalPreservation => "STD-010",
        }
    }

    /// Return a short canonical label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::TransparencyBlend => "Transparency and blend modes",
            Self::TaggedStructure => "Tagged PDF and structure tree",
            Self::PdfA => "PDF/A conformance",
            Self::PdfX => "PDF/X conformance",
            Self::PdfUa => "PDF/UA conformance",
            Self::IncrementalPreservation => "Incremental preservation",
        }
    }

    /// Return the finding-code prefix used by this row's checks.
    #[must_use]
    pub const fn prefix(self) -> &'static str {
        match self {
            Self::TransparencyBlend => "STD005",
            Self::TaggedStructure => "STD006",
            Self::PdfA => "STD007",
            Self::PdfX => "STD008",
            Self::PdfUa => "STD009",
            Self::IncrementalPreservation => "STD010",
        }
    }

    /// Resolve a stable code or label case-insensitively.
    ///
    /// # Errors
    ///
    /// Returns [`ConformanceError::UnknownRow`] when no row matches.
    pub fn from_code(code: &str) -> Result<Self, ConformanceError> {
        let needle = code.trim();
        for row in Self::all() {
            if needle.eq_ignore_ascii_case(row.code()) || needle.eq_ignore_ascii_case(row.label()) {
                return Ok(*row);
            }
        }
        Err(ConformanceError::UnknownRow(code.to_owned()))
    }
}

impl fmt::Display for StandardRow {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {}", self.code(), self.label())
    }
}

/// How much of a row this library can honestly decide.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckScope {
    /// Only a deterministic subset is decidable here; the rest is external.
    DeterministicSubset,
    /// No part of the row is decidable without an external validator.
    ExternalValidator,
}

/// Severity of a conformance finding.
///
/// The declaration order is significant: it is also the deterministic sort
/// order, with [`Self::Error`] first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum FindingSeverity {
    /// A rule that fails the row was violated.
    Error,
    /// A noteworthy condition that does not by itself fail the row.
    Warning,
    /// A fact was unavailable and needs manual verification.
    Info,
}

impl fmt::Display for FindingSeverity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Error => f.write_str("error"),
            Self::Warning => f.write_str("warning"),
            Self::Info => f.write_str("info"),
        }
    }
}

/// One stable-coded conformance finding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConformanceFinding {
    code: String,
    severity: FindingSeverity,
    message: String,
    location: Option<String>,
}

impl ConformanceFinding {
    /// Create a finding with a stable code, severity and message.
    #[must_use]
    pub fn new(
        severity: FindingSeverity,
        code: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            code: code.into(),
            severity,
            message: message.into(),
            location: None,
        }
    }

    /// Attach a human-readable location such as `header` or a fact name.
    #[must_use]
    pub fn with_location(mut self, location: impl Into<String>) -> Self {
        self.location = Some(location.into());
        self
    }

    /// Return the stable finding code.
    #[must_use]
    pub fn code(&self) -> &str {
        &self.code
    }

    /// Return the finding severity.
    #[must_use]
    pub const fn severity(&self) -> FindingSeverity {
        self.severity
    }

    /// Return the actionable message.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }

    /// Return the attached location, if any.
    #[must_use]
    pub fn location(&self) -> Option<&str> {
        self.location.as_deref()
    }

    fn sort_key(&self) -> (FindingSeverity, &str, &str, Option<&str>) {
        (
            self.severity,
            self.code.as_str(),
            self.message.as_str(),
            self.location.as_deref(),
        )
    }
}

/// The verdict of a deterministic subset check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConformanceVerdict {
    /// No error-severity finding fired in the decidable subset.
    Conforming,
    /// At least one error-severity finding fired.
    NonConforming,
    /// No error fired, but a required fact was unavailable to the checker.
    Incomplete,
}

impl fmt::Display for ConformanceVerdict {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Conforming => f.write_str("conforming"),
            Self::NonConforming => f.write_str("non-conforming"),
            Self::Incomplete => f.write_str("incomplete"),
        }
    }
}

/// A deterministic account of one subset conformance check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConformanceReport {
    row: StandardRow,
    scope: CheckScope,
    verdict: ConformanceVerdict,
    findings: Vec<ConformanceFinding>,
}

impl ConformanceReport {
    /// Return the row that was checked.
    #[must_use]
    pub const fn row(&self) -> StandardRow {
        self.row
    }

    /// Return the honest scope of the check.
    #[must_use]
    pub const fn scope(&self) -> CheckScope {
        self.scope
    }

    /// Return the verdict over the decidable subset.
    #[must_use]
    pub const fn verdict(&self) -> ConformanceVerdict {
        self.verdict
    }

    /// Return findings in canonical order.
    #[must_use]
    pub fn findings(&self) -> &[ConformanceFinding] {
        &self.findings
    }

    /// Return `true` when no error-severity finding fired.
    #[must_use]
    pub const fn is_conforming(&self) -> bool {
        matches!(self.verdict, ConformanceVerdict::Conforming)
    }

    /// Count error-severity findings.
    #[must_use]
    pub fn error_count(&self) -> usize {
        self.findings
            .iter()
            .filter(|finding| finding.severity == FindingSeverity::Error)
            .count()
    }

    /// Count warning-severity findings.
    #[must_use]
    pub fn warning_count(&self) -> usize {
        self.findings
            .iter()
            .filter(|finding| finding.severity == FindingSeverity::Warning)
            .count()
    }

    /// Return a stable one-line summary suitable for logs and CLI output.
    #[must_use]
    pub fn summary(&self) -> String {
        format!(
            "{}: {} ({}, scope={:?})",
            self.row.code(),
            self.verdict,
            self.counts(),
            self.scope
        )
    }

    fn counts(&self) -> String {
        let errors = self.error_count();
        let warnings = self.warning_count();
        let info = self.findings.len() - errors - warnings;
        format!("{errors} errors, {warnings} warnings, {info} info")
    }
}

/// A bounded, plain-data snapshot of core-observable document facts.
///
/// `None` means "the caller does not know"; checks neither pass nor fail such a
/// fact but record an informational manual-check finding instead. Collections
/// are bounded by [`MAX_FACT_NAMES`] and every name by [`MAX_NAME_LEN`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocumentFacts {
    /// Effective PDF version from the header or catalog override.
    pub version: PdfVersion,
    /// Number of leaf pages in the page tree.
    pub page_count: usize,
    /// Whether the document is encrypted.
    pub is_encrypted: Option<bool>,
    /// Whether `/MarkInfo /Marked` is true.
    pub marked: Option<bool>,
    /// Whether the catalog has a `/StructTreeRoot`.
    pub has_struct_tree_root: Option<bool>,
    /// Whether the structure tree passed structural validation.
    pub struct_tree_valid: Option<bool>,
    /// Whether the catalog has a `/Lang` entry.
    pub has_lang: Option<bool>,
    /// Whether any transparency construct is used.
    pub uses_transparency: Option<bool>,
    /// Canonical blend-mode names observed in the document.
    pub transparency_modes: Vec<String>,
    /// Whether a `GTS_PDFA1` output intent is present.
    pub has_output_intent_pdfa: Option<bool>,
    /// Whether a `GTS_PDFX` output intent is present.
    pub has_output_intent_pdfx: Option<bool>,
    /// Whether an XMP `/Metadata` stream is present.
    pub has_xmp_metadata: Option<bool>,
    /// Whether every font resource embeds its font program.
    pub all_fonts_embedded: Option<bool>,
    /// Whether an appended incremental revision exists.
    pub is_incrementally_updated: Option<bool>,
    /// Whether preservation validation of the appended revision succeeded.
    pub preservation_valid: Option<bool>,
    /// Interesting catalog entries present in the document.
    pub catalog_entries: Vec<String>,
}

impl Default for DocumentFacts {
    fn default() -> Self {
        Self {
            version: PdfVersion::new(0, 0),
            page_count: 0,
            is_encrypted: None,
            marked: None,
            has_struct_tree_root: None,
            struct_tree_valid: None,
            has_lang: None,
            uses_transparency: None,
            transparency_modes: Vec::new(),
            has_output_intent_pdfa: None,
            has_output_intent_pdfx: None,
            has_xmp_metadata: None,
            all_fonts_embedded: None,
            is_incrementally_updated: None,
            preservation_valid: None,
            catalog_entries: Vec::new(),
        }
    }
}

impl DocumentFacts {
    /// Validate the snapshot's own bounds before any check runs.
    ///
    /// # Errors
    ///
    /// Returns [`ConformanceError::TooManyFacts`],
    /// [`ConformanceError::InvalidFactName`] or
    /// [`ConformanceError::TooManyPages`] for out-of-range inputs.
    pub fn validate(&self) -> Result<(), ConformanceError> {
        let count = self
            .transparency_modes
            .len()
            .saturating_add(self.catalog_entries.len());
        if count > MAX_FACT_NAMES {
            return Err(ConformanceError::TooManyFacts {
                count,
                limit: MAX_FACT_NAMES,
            });
        }
        for name in self
            .transparency_modes
            .iter()
            .chain(self.catalog_entries.iter())
        {
            if name.is_empty() || name.len() > MAX_NAME_LEN || name.chars().any(char::is_whitespace)
            {
                return Err(ConformanceError::InvalidFactName(name.clone()));
            }
        }
        if self.page_count > MAX_FACT_PAGES {
            return Err(ConformanceError::TooManyPages {
                count: self.page_count,
                limit: MAX_FACT_PAGES,
            });
        }
        Ok(())
    }

    /// Derive the reliably observable facts from an already-opened document.
    ///
    /// Version, page count and catalog flags are read through the public core
    /// API. Facts that need a renderer, a structure reader, a font scanner or a
    /// preservation check are left as `None` so callers must supply them
    /// explicitly instead of getting an accidental pass.
    ///
    /// # Errors
    ///
    /// Returns any bound violation from [`Self::validate`].
    pub fn from_document(document: &Document) -> Result<Self, ConformanceError> {
        let version = document.effective_version();
        let page_count = document.page_count().unwrap_or(0);
        let mut facts = Self {
            version,
            page_count,
            ..Self::default()
        };

        let catalog = document
            .root()
            .ok()
            .and_then(|id| document.object(id).ok())
            .and_then(|object| object.as_dictionary().cloned());
        if let Some(catalog) = catalog {
            facts.marked = catalog.get("MarkInfo").map(|value| {
                value
                    .as_dictionary()
                    .and_then(|dictionary| dictionary.get("Marked"))
                    == Some(&Object::Boolean(true))
            });
            facts.has_struct_tree_root = Some(catalog.contains_key("StructTreeRoot"));
            facts.has_lang = Some(catalog.contains_key("Lang"));
            facts.has_xmp_metadata = Some(catalog.contains_key("Metadata"));
            let intents = catalog
                .get("OutputIntents")
                .and_then(Object::as_array)
                .unwrap_or(&[]);
            for intent in intents {
                if let Some(name) = intent
                    .as_dictionary()
                    .and_then(|dictionary| dictionary.get("S"))
                    .and_then(Object::as_name)
                {
                    let name = name.strip_prefix('/').unwrap_or(name);
                    if name.eq_ignore_ascii_case("GTS_PDFA1") {
                        facts.has_output_intent_pdfa = Some(true);
                    }
                    if name.eq_ignore_ascii_case("GTS_PDFX") {
                        facts.has_output_intent_pdfx = Some(true);
                    }
                }
            }
            for key in [
                "JavaScript",
                "EmbeddedFiles",
                "AA",
                "OpenAction",
                "RichMedia",
            ] {
                if catalog.contains_key(key) {
                    facts.catalog_entries.push(key.to_owned());
                }
            }
        }

        let startxref = b"startxref";
        let revisions = document
            .bytes()
            .windows(startxref.len())
            .filter(|window| *window == startxref)
            .count();
        facts.is_incrementally_updated = Some(revisions > 1);

        facts.validate()?;
        Ok(facts)
    }
}

/// A deterministic, bounded conformance plan for one standards row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConformancePlan {
    row: StandardRow,
    scope: CheckScope,
}

impl ConformancePlan {
    /// Create the honest library plan for `row`.
    #[must_use]
    pub const fn for_row(row: StandardRow) -> Self {
        Self {
            row,
            scope: CheckScope::DeterministicSubset,
        }
    }

    /// Return the row this plan checks.
    #[must_use]
    pub const fn row(&self) -> StandardRow {
        self.row
    }

    /// Return the honest scope of this plan.
    #[must_use]
    pub const fn scope(&self) -> CheckScope {
        self.scope
    }

    /// Return a fixed sentence describing what is and is not covered.
    #[must_use]
    pub const fn describe(self) -> &'static str {
        match self.row {
            StandardRow::TransparencyBlend => {
                "Blend descriptors, alpha and backend requirements are planned deterministically; pixel compositing belongs to the render lane."
            }
            StandardRow::TaggedStructure => {
                "Structure roles, validation, parent tree and token round-trip are checked; reading order and rendering belong to the accessibility lane."
            }
            StandardRow::PdfA => {
                "Version, encryption, output intent, XMP, embedded fonts and forbidden entries are checked from core facts; ICC byte validation and veraPDF are external."
            }
            StandardRow::PdfX => {
                "Version, encryption, GTS_PDFX intent, transparency, fonts and forbidden entries are checked from core facts; ICC/veraPDF validation is external."
            }
            StandardRow::PdfUa => {
                "Marked content, structure tree and language are checked from core facts; reading-order and alt-text verification belong to the accessibility lane."
            }
            StandardRow::IncrementalPreservation => {
                "Exact prefix, /Prev chain, trailer keys and page text are validated by pdfxi-core; signing and long-term packaging are unsupported."
            }
        }
    }

    /// Return the operations deliberately not implemented in this library.
    #[must_use]
    pub const fn unsupported_operations(self) -> &'static [&'static str] {
        match self.row {
            StandardRow::TransparencyBlend => &["pixel compositing (render lane)"],
            StandardRow::TaggedStructure => {
                &["reading-order and rendering checks (accessibility lane)"]
            }
            StandardRow::PdfA => &["ICC profile byte validation", "veraPDF oracle conformance"],
            StandardRow::PdfX => &[
                "GTS_PDFX ICC profile validation",
                "veraPDF oracle conformance",
            ],
            StandardRow::PdfUa => &["reading-order and alt-text verification (accessibility lane)"],
            StandardRow::IncrementalPreservation => {
                &["cryptographic signing", "long-term archival packaging"]
            }
        }
    }

    /// Run the deterministic subset check against `facts`.
    ///
    /// # Errors
    ///
    /// Returns a typed [`ConformanceError`] when the snapshot exceeds its
    /// bounds. Unknown facts never fail the check; they yield informational
    /// findings so the caller knows what must be verified elsewhere.
    pub fn check(self, facts: &DocumentFacts) -> Result<ConformanceReport, ConformanceError> {
        facts.validate()?;
        let prefix = self.row.prefix();
        let mut findings = Vec::new();
        match self.row {
            StandardRow::TransparencyBlend => check_transparency(facts, prefix, &mut findings),
            StandardRow::TaggedStructure => check_tagged(facts, prefix, &mut findings),
            StandardRow::PdfA => check_pdf_a(facts, prefix, &mut findings),
            StandardRow::PdfX => check_pdf_x(facts, prefix, &mut findings),
            StandardRow::PdfUa => check_pdf_ua(facts, prefix, &mut findings),
            StandardRow::IncrementalPreservation => {
                check_preservation(facts, prefix, &mut findings);
            }
        }
        findings.sort_by(|a, b| a.sort_key().cmp(&b.sort_key()));
        let verdict = if findings
            .iter()
            .any(|finding| finding.severity == FindingSeverity::Error)
        {
            ConformanceVerdict::NonConforming
        } else if findings
            .iter()
            .any(|finding| finding.code().ends_with(".DATA_UNAVAILABLE"))
        {
            ConformanceVerdict::Incomplete
        } else {
            ConformanceVerdict::Conforming
        };
        Ok(ConformanceReport {
            row: self.row,
            scope: self.scope,
            verdict,
            findings,
        })
    }
}

/// A conformance operation a caller may request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConformanceOperation {
    /// Run the deterministic subset check.
    RunSubset,
    /// Certify full conformance to a standard.
    Certify,
}

impl ConformanceOperation {
    /// Check whether the library implements `self` for `row`.
    ///
    /// # Errors
    ///
    /// Returns [`ConformanceError::UnsupportedOperation`] for certification:
    /// a library subset check is never a conformance certificate.
    pub const fn check(self, _row: StandardRow) -> Result<(), ConformanceError> {
        match self {
            Self::RunSubset => Ok(()),
            Self::Certify => Err(ConformanceError::UnsupportedOperation(
                "standards certification requires an external validator",
            )),
        }
    }
}

fn error(prefix: &str, code: &str, message: &str, location: Option<&str>) -> ConformanceFinding {
    let finding =
        ConformanceFinding::new(FindingSeverity::Error, format!("{prefix}.{code}"), message);
    match location {
        Some(location) => finding.with_location(location),
        None => finding,
    }
}

fn require_true(
    out: &mut Vec<ConformanceFinding>,
    prefix: &str,
    code: &str,
    fact: &'static str,
    value: Option<bool>,
    message: &str,
) {
    match value {
        Some(true) => {}
        Some(false) => out.push(error(prefix, code, message, Some(fact))),
        None => out.push(
            ConformanceFinding::new(
                FindingSeverity::Info,
                format!("{prefix}.DATA_UNAVAILABLE"),
                format!("fact '{fact}' was not supplied; verify manually"),
            )
            .with_location(fact),
        ),
    }
}

fn require_false(
    out: &mut Vec<ConformanceFinding>,
    prefix: &str,
    code: &str,
    fact: &'static str,
    value: Option<bool>,
    message: &str,
) {
    match value {
        Some(false) => {}
        Some(true) => out.push(error(prefix, code, message, Some(fact))),
        None => out.push(
            ConformanceFinding::new(
                FindingSeverity::Info,
                format!("{prefix}.DATA_UNAVAILABLE"),
                format!("fact '{fact}' was not supplied; verify manually"),
            )
            .with_location(fact),
        ),
    }
}

fn check_version(
    out: &mut Vec<ConformanceFinding>,
    prefix: &str,
    minimum: PdfVersion,
    version: PdfVersion,
) {
    if version < minimum {
        out.push(error(
            prefix,
            "VERSION_TOO_LOW",
            &format!("PDF version {version} is below the required {minimum}"),
            Some("header"),
        ));
    }
}

fn check_forbidden(
    out: &mut Vec<ConformanceFinding>,
    prefix: &str,
    entries: &[String],
    forbidden: &[&str],
) {
    for entry in forbidden {
        if entries
            .iter()
            .any(|present| present.eq_ignore_ascii_case(entry))
        {
            out.push(error(
                prefix,
                "FORBIDDEN_ENTRY",
                &format!("catalog entry /{entry} is not permitted"),
                Some(entry),
            ));
        }
    }
}

fn check_transparency(facts: &DocumentFacts, prefix: &str, out: &mut Vec<ConformanceFinding>) {
    check_version(out, prefix, PdfVersion::new(1, 4), facts.version);
    if facts.uses_transparency.is_none() {
        out.push(
            ConformanceFinding::new(
                FindingSeverity::Info,
                format!("{prefix}.DATA_UNAVAILABLE"),
                "fact 'uses_transparency' was not supplied; verify manually",
            )
            .with_location("uses_transparency"),
        );
    }
    for name in &facts.transparency_modes {
        match BlendMode::parse(name) {
            Ok(mode) => {
                if mode.category() == crate::transparency::BlendCategory::NonSeparable {
                    out.push(
                        ConformanceFinding::new(
                            FindingSeverity::Info,
                            format!("{prefix}.CMM_REQUIRED"),
                            format!("blend mode {} requires colour management", mode.name()),
                        )
                        .with_location(mode.name()),
                    );
                }
            }
            Err(_) => out.push(error(
                prefix,
                "UNKNOWN_BLEND_MODE",
                &format!("'{name}' is not an ISO 32000 blend mode"),
                Some(name),
            )),
        }
    }
    if facts.uses_transparency == Some(true) && facts.transparency_modes.is_empty() {
        out.push(ConformanceFinding::new(
            FindingSeverity::Info,
            format!("{prefix}.MODES_UNAVAILABLE"),
            "transparency is used but no blend modes were supplied; verify manually",
        ));
    }
}

fn check_tagged(facts: &DocumentFacts, prefix: &str, out: &mut Vec<ConformanceFinding>) {
    check_version(out, prefix, PdfVersion::new(1, 4), facts.version);
    require_true(
        out,
        prefix,
        "NOT_MARKED",
        "marked",
        facts.marked,
        "tagged PDF requires /MarkInfo with /Marked true",
    );
    require_true(
        out,
        prefix,
        "NO_STRUCT_TREE_ROOT",
        "has_struct_tree_root",
        facts.has_struct_tree_root,
        "tagged PDF requires a /StructTreeRoot",
    );
    require_true(
        out,
        prefix,
        "NO_LANG",
        "has_lang",
        facts.has_lang,
        "the document language (/Lang) is required",
    );
    require_true(
        out,
        prefix,
        "STRUCT_TREE_INVALID",
        "struct_tree_valid",
        facts.struct_tree_valid,
        "the structure tree failed validation",
    );
}

fn check_pdf_a(facts: &DocumentFacts, prefix: &str, out: &mut Vec<ConformanceFinding>) {
    check_version(out, prefix, PdfVersion::new(1, 4), facts.version);
    require_false(
        out,
        prefix,
        "ENCRYPTED",
        "is_encrypted",
        facts.is_encrypted,
        "PDF/A does not permit encryption",
    );
    require_true(
        out,
        prefix,
        "NO_OUTPUT_INTENT",
        "has_output_intent_pdfa",
        facts.has_output_intent_pdfa,
        "PDF/A requires a GTS_PDFA1 output intent",
    );
    require_true(
        out,
        prefix,
        "NO_XMP_METADATA",
        "has_xmp_metadata",
        facts.has_xmp_metadata,
        "PDF/A requires an XMP /Metadata stream",
    );
    require_true(
        out,
        prefix,
        "FONT_NOT_EMBEDDED",
        "all_fonts_embedded",
        facts.all_fonts_embedded,
        "PDF/A requires every font to be embedded",
    );
    if facts.uses_transparency == Some(true) && facts.version < PdfVersion::new(1, 5) {
        out.push(error(
            prefix,
            "TRANSPARENCY_FORBIDDEN",
            "PDF/A-1b does not permit transparency",
            Some("uses_transparency"),
        ));
    }
    check_forbidden(
        out,
        prefix,
        &facts.catalog_entries,
        &["JavaScript", "EmbeddedFiles", "AA"],
    );
}

fn check_pdf_x(facts: &DocumentFacts, prefix: &str, out: &mut Vec<ConformanceFinding>) {
    check_version(out, prefix, PdfVersion::new(1, 3), facts.version);
    require_false(
        out,
        prefix,
        "ENCRYPTED",
        "is_encrypted",
        facts.is_encrypted,
        "PDF/X does not permit encryption",
    );
    require_true(
        out,
        prefix,
        "NO_OUTPUT_INTENT",
        "has_output_intent_pdfx",
        facts.has_output_intent_pdfx,
        "PDF/X requires a GTS_PDFX output intent",
    );
    require_false(
        out,
        prefix,
        "TRANSPARENCY_FORBIDDEN",
        "uses_transparency",
        facts.uses_transparency,
        "this PDF/X subset does not permit transparency",
    );
    require_true(
        out,
        prefix,
        "FONT_NOT_EMBEDDED",
        "all_fonts_embedded",
        facts.all_fonts_embedded,
        "PDF/X requires every font to be embedded",
    );
    check_forbidden(
        out,
        prefix,
        &facts.catalog_entries,
        &["JavaScript", "EmbeddedFiles", "AA"],
    );
}

fn check_pdf_ua(facts: &DocumentFacts, prefix: &str, out: &mut Vec<ConformanceFinding>) {
    check_version(out, prefix, PdfVersion::new(1, 7), facts.version);
    require_false(
        out,
        prefix,
        "ENCRYPTED",
        "is_encrypted",
        facts.is_encrypted,
        "PDF/UA does not permit encryption",
    );
    require_true(
        out,
        prefix,
        "NOT_MARKED",
        "marked",
        facts.marked,
        "PDF/UA requires /MarkInfo with /Marked true",
    );
    require_true(
        out,
        prefix,
        "NO_STRUCT_TREE_ROOT",
        "has_struct_tree_root",
        facts.has_struct_tree_root,
        "PDF/UA requires a /StructTreeRoot",
    );
    require_true(
        out,
        prefix,
        "NO_LANG",
        "has_lang",
        facts.has_lang,
        "PDF/UA requires the document language (/Lang)",
    );
    require_true(
        out,
        prefix,
        "STRUCT_TREE_INVALID",
        "struct_tree_valid",
        facts.struct_tree_valid,
        "PDF/UA requires a valid structure tree",
    );
}

fn check_preservation(facts: &DocumentFacts, prefix: &str, out: &mut Vec<ConformanceFinding>) {
    match facts.is_incrementally_updated {
        Some(false) => out.push(ConformanceFinding::new(
            FindingSeverity::Warning,
            format!("{prefix}.NO_INCREMENTAL_REVISION"),
            "document has no appended revision; there is nothing to validate",
        )),
        None => out.push(
            ConformanceFinding::new(
                FindingSeverity::Info,
                format!("{prefix}.DATA_UNAVAILABLE"),
                "fact 'is_incrementally_updated' was not supplied; verify manually",
            )
            .with_location("is_incrementally_updated"),
        ),
        Some(true) => {}
    }
    if facts.is_incrementally_updated == Some(true) {
        require_true(
            out,
            prefix,
            "PRESERVATION_INVALID",
            "preservation_valid",
            facts.preservation_valid,
            "the appended revision failed preservation validation",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clean_facts() -> DocumentFacts {
        DocumentFacts {
            version: PdfVersion::new(1, 7),
            page_count: 1,
            is_encrypted: Some(false),
            marked: Some(true),
            has_struct_tree_root: Some(true),
            struct_tree_valid: Some(true),
            has_lang: Some(true),
            uses_transparency: Some(false),
            transparency_modes: Vec::new(),
            has_output_intent_pdfa: Some(true),
            has_output_intent_pdfx: Some(true),
            has_xmp_metadata: Some(true),
            all_fonts_embedded: Some(true),
            is_incrementally_updated: Some(true),
            preservation_valid: Some(true),
            catalog_entries: Vec::new(),
        }
    }

    #[test]
    fn every_row_has_stable_code_and_scope() {
        let rows = StandardRow::all();
        assert_eq!(rows.len(), 6);
        let codes: Vec<&str> = rows.iter().map(|row| row.code()).collect();
        assert_eq!(
            codes,
            vec![
                "STD-005", "STD-006", "STD-007", "STD-008", "STD-009", "STD-010"
            ]
        );
        for row in rows {
            assert_eq!(StandardRow::from_code(row.code()).expect("code"), *row);
            assert_eq!(
                StandardRow::from_code(&row.label().to_ascii_lowercase()).expect("label"),
                *row
            );
            assert_eq!(
                ConformancePlan::for_row(*row).scope(),
                CheckScope::DeterministicSubset
            );
        }
        assert!(matches!(
            StandardRow::from_code("STD-999"),
            Err(ConformanceError::UnknownRow(_))
        ));
    }

    #[test]
    fn plan_describes_honest_scope_and_unsupported_operations() {
        for row in StandardRow::all() {
            let plan = ConformancePlan::for_row(*row);
            assert!(!plan.describe().is_empty());
            assert!(!plan.unsupported_operations().is_empty());
            assert!(ConformanceOperation::RunSubset.check(*row).is_ok());
        }
        let preservation = ConformancePlan::for_row(StandardRow::IncrementalPreservation);
        assert_eq!(
            preservation.unsupported_operations(),
            &["cryptographic signing", "long-term archival packaging"]
        );
        assert!(preservation.describe().contains("signing"));
    }

    #[test]
    fn pdf_a_report_is_deterministic_and_sorted() {
        let facts = clean_facts();
        let report = ConformancePlan::for_row(StandardRow::PdfA)
            .check(&facts)
            .expect("clean report");
        assert!(report.is_conforming());
        assert_eq!(report.verdict(), ConformanceVerdict::Conforming);
        assert_eq!(
            report.summary(),
            "STD-007: conforming (0 errors, 0 warnings, 0 info, scope=DeterministicSubset)"
        );
        assert_eq!(report.row(), StandardRow::PdfA);
        assert_eq!(
            ConformancePlan::for_row(StandardRow::PdfA)
                .check(&facts)
                .expect("second report"),
            report
        );
    }

    #[test]
    fn pdf_a_violations_are_stable_coded_and_negative() {
        let mut facts = clean_facts();
        facts.version = PdfVersion::new(1, 3);
        facts.is_encrypted = Some(true);
        facts.has_output_intent_pdfa = Some(false);
        facts.catalog_entries.push("JavaScript".to_owned());
        let report = ConformancePlan::for_row(StandardRow::PdfA)
            .check(&facts)
            .expect("violating report");
        assert!(!report.is_conforming());
        let codes: Vec<&str> = report
            .findings()
            .iter()
            .map(ConformanceFinding::code)
            .collect();
        assert_eq!(
            codes,
            vec![
                "STD007.ENCRYPTED",
                "STD007.FORBIDDEN_ENTRY",
                "STD007.NO_OUTPUT_INTENT",
                "STD007.VERSION_TOO_LOW",
            ]
        );
        assert!(
            report
                .findings()
                .iter()
                .all(|finding| finding.code().starts_with("STD007."))
        );
    }

    #[test]
    fn pdf_x_violations_are_typed_and_stable() {
        let mut facts = clean_facts();
        facts.has_output_intent_pdfx = Some(false);
        facts.uses_transparency = Some(true);
        let report = ConformancePlan::for_row(StandardRow::PdfX)
            .check(&facts)
            .expect("violating report");
        assert_eq!(report.error_count(), 2);
        assert!(report.findings().iter().any(|finding| {
            finding.code() == "STD008.TRANSPARENCY_FORBIDDEN"
                && finding.severity() == FindingSeverity::Error
        }));
        assert!(
            report
                .findings()
                .iter()
                .any(|finding| finding.code() == "STD008.NO_OUTPUT_INTENT")
        );
        assert_eq!(
            report,
            ConformancePlan::for_row(StandardRow::PdfX)
                .check(&facts)
                .expect("repeat")
        );
    }

    #[test]
    fn pdf_ua_requires_tagging_facts() {
        let mut facts = clean_facts();
        facts.marked = Some(false);
        facts.has_lang = Some(false);
        facts.struct_tree_valid = Some(false);
        let report = ConformancePlan::for_row(StandardRow::PdfUa)
            .check(&facts)
            .expect("violating report");
        let codes: Vec<&str> = report
            .findings()
            .iter()
            .map(ConformanceFinding::code)
            .collect();
        assert!(codes.contains(&"STD009.NOT_MARKED"));
        assert!(codes.contains(&"STD009.NO_LANG"));
        assert!(codes.contains(&"STD009.STRUCT_TREE_INVALID"));
        assert!(!report.is_conforming());
    }

    #[test]
    fn tagged_structure_row_reports_invalid_tree() {
        let mut facts = clean_facts();
        facts.struct_tree_valid = Some(false);
        facts.has_struct_tree_root = Some(false);
        let report = ConformancePlan::for_row(StandardRow::TaggedStructure)
            .check(&facts)
            .expect("violating report");
        assert!(!report.is_conforming());
        assert!(
            report
                .findings()
                .iter()
                .any(|finding| finding.code() == "STD006.STRUCT_TREE_INVALID")
        );
        assert!(
            report
                .findings()
                .iter()
                .any(|finding| finding.code() == "STD006.NO_STRUCT_TREE_ROOT")
        );
    }

    #[test]
    fn transparency_row_validates_blend_modes_and_flags_cmm() {
        let mut facts = clean_facts();
        facts.uses_transparency = Some(true);
        facts.transparency_modes =
            vec!["Multiply".to_owned(), "hue".to_owned(), "bogus".to_owned()];
        let report = ConformancePlan::for_row(StandardRow::TransparencyBlend)
            .check(&facts)
            .expect("report");
        assert!(!report.is_conforming());
        let cmm: Vec<&ConformanceFinding> = report
            .findings()
            .iter()
            .filter(|finding| finding.code() == "STD005.CMM_REQUIRED")
            .collect();
        assert_eq!(cmm.len(), 1);
        assert!(report.findings().iter().any(|finding| {
            finding.code() == "STD005.UNKNOWN_BLEND_MODE" && finding.location() == Some("bogus")
        }));
    }

    #[test]
    fn preservation_row_reports_typed_unsupported_operations() {
        let mut facts = clean_facts();
        facts.preservation_valid = Some(false);
        let report = ConformancePlan::for_row(StandardRow::IncrementalPreservation)
            .check(&facts)
            .expect("report");
        assert!(!report.is_conforming());
        assert!(
            report
                .findings()
                .iter()
                .any(|finding| finding.code() == "STD010.PRESERVATION_INVALID")
        );
        for operation in
            ConformancePlan::for_row(StandardRow::IncrementalPreservation).unsupported_operations()
        {
            assert!(!operation.is_empty());
        }
        assert_eq!(
            ConformanceOperation::Certify
                .check(StandardRow::IncrementalPreservation)
                .expect_err("certification unsupported"),
            ConformanceError::UnsupportedOperation(
                "standards certification requires an external validator"
            )
        );
    }

    #[test]
    fn bounded_facts_are_rejected_with_typed_errors() {
        let mut facts = clean_facts();
        facts.catalog_entries = vec!["x".repeat(MAX_NAME_LEN + 1)];
        assert!(matches!(
            ConformancePlan::for_row(StandardRow::PdfA).check(&facts),
            Err(ConformanceError::InvalidFactName(_))
        ));
        facts.catalog_entries = Vec::new();
        facts.page_count = MAX_FACT_PAGES + 1;
        assert_eq!(
            ConformancePlan::for_row(StandardRow::PdfA)
                .check(&facts)
                .expect_err("too many pages"),
            ConformanceError::TooManyPages {
                count: MAX_FACT_PAGES + 1,
                limit: MAX_FACT_PAGES
            }
        );
    }

    #[test]
    fn unknown_facts_yield_manual_check_instead_of_false_pass() {
        let facts = DocumentFacts {
            version: PdfVersion::new(1, 7),
            page_count: 1,
            ..DocumentFacts::default()
        };
        let report = ConformancePlan::for_row(StandardRow::PdfA)
            .check(&facts)
            .expect("report with unknown facts");
        assert!(
            !report.is_conforming(),
            "unknown facts must not pass silently"
        );
        assert_eq!(report.error_count(), 0);
        assert!(report.warning_count() == 0);
        assert!(
            report
                .findings()
                .iter()
                .all(|finding| finding.code() == "STD007.DATA_UNAVAILABLE")
        );
    }

    #[test]
    fn from_document_exposes_catalog_facts() {
        let document = Document::from_bytes(pdfxi_testkit::minimal_pdf()).expect("minimal fixture");
        let facts = DocumentFacts::from_document(&document).expect("facts");
        assert_eq!(facts.version, PdfVersion::new(1, 7));
        assert_eq!(facts.page_count, 1);
        assert_eq!(facts.has_struct_tree_root, Some(false));
        assert_eq!(facts.has_lang, Some(false));
        assert_eq!(facts.has_xmp_metadata, Some(false));
        assert_eq!(facts.has_output_intent_pdfa, None);
        assert_eq!(facts.is_incrementally_updated, Some(false));
        // Facts that need a specialist are left unknown, not guessed.
        assert_eq!(facts.struct_tree_valid, None);
        assert_eq!(facts.all_fonts_embedded, None);
        let report = ConformancePlan::for_row(StandardRow::PdfUa)
            .check(&facts)
            .expect("report");
        assert!(!report.is_conforming());
    }
}
