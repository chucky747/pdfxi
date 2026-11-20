//! Library-level Microsoft Office integration closure for PLAT-003 (ADR 0008).
//!
//! The crate already ships the individual halves of an Office conversion
//! pipeline: [`crate::office_import`] validates caller-supplied Office-derived
//! markup, [`crate::office_contract`] validates the intake envelope and exposes
//! typed unsupported constructs, [`crate::extract_model`] carries recovered page
//! content, [`crate::office_export_plan`] maps it into an editable document, and
//! [`crate::office_export`] writes real `.docx`/`.xlsx`/`.pptx` archives in
//! process while [`crate::create_from_text`] produces PDF bytes. This module is
//! the single, deterministic front door that ties those adapters together:
//!
//! - [`integration_descriptor`] returns a validated [`IntegrationDescriptor`]
//!   with the supported families, a per-family extension allowlist, a
//!   round-trip capability report, and a deterministic [`CompatibilityMatrix`].
//! - [`plan_office_integration`] validates a caller-supplied
//!   [`OfficeIntegrationEnvelope`] and returns a typed [`OfficeIntegrationPlan`]
//!   for either `ImportToPdf` or `PdfToOffice`, reusing the existing adapters.
//! - [`OfficeIntegrationPlan::execute`] runs that plan and returns the produced
//!   bytes plus a typed [`OfficeIntegrationReport`].
//!
//! # Boundaries documented honestly
//!
//! No OOXML ZIP *reader* is added: the caller supplies already-extracted markup
//! or page content. There is no COM automation, no installed Office ribbon or
//! add-in, no external Office/LibreOffice process, no network access, and no
//! file-system read. Those are host-boundary concerns and remain outside this
//! crate. All parsing is bounded, deterministic, and dependency-free.

use crate::create::CreateOptions;
use crate::office_contract::{
    OfficeEnvelopeError, build_office_conversion_plan, validate_office_envelope,
};
use crate::office_export::{
    CellValue, OfficeBlock, OfficeDocument, OfficeExportError, OfficeFormat, OfficePage,
};
use crate::office_export_plan::{
    OfficeExportEnvelope, OfficePlanError, OfficePlanFormat, OfficePlanOptions,
    build_office_export_plan,
};
use crate::office_import::{
    OFFICE_BUNDLE_MAX_BYTES, OfficeBundle, OfficeBundleKind, OfficeImportError,
    import_office_bundle_with_report,
};
use crate::{ExtractedDocument, LineEnding};

/// Maximum accepted length, in bytes, of an output document name (stem plus
/// extension). Names longer than this are rejected before any adapter runs.
pub const OFFICE_INTEGRATION_MAX_NAME_BYTES: usize = 256;

/// Maximum accepted length, in bytes, of the sanitized output name stem.
const MAX_STEM_CHARS: usize = 64;

/// The three Microsoft Office families exposed by the integration surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OfficeFamily {
    /// Word / WordprocessingML documents.
    Word,
    /// Excel / SpreadsheetML workbooks.
    Excel,
    /// PowerPoint / PresentationML presentations.
    PowerPoint,
}

impl OfficeFamily {
    /// Every supported family in a stable order.
    pub const ALL: &'static [Self] = &[Self::Word, Self::Excel, Self::PowerPoint];

    /// Return every supported family in a stable order.
    #[must_use]
    pub const fn all() -> &'static [Self] {
        Self::ALL
    }

    /// Return a stable human-readable label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Word => "Word",
            Self::Excel => "Excel",
            Self::PowerPoint => "PowerPoint",
        }
    }

    /// Map this family to the existing import bundle kind.
    #[must_use]
    pub const fn bundle_kind(self) -> OfficeBundleKind {
        match self {
            Self::Word => OfficeBundleKind::Word,
            Self::Excel => OfficeBundleKind::Excel,
            Self::PowerPoint => OfficeBundleKind::PowerPoint,
        }
    }

    /// Map this family to the existing editable export plan format.
    #[must_use]
    pub const fn plan_format(self) -> OfficePlanFormat {
        match self {
            Self::Word => OfficePlanFormat::Word,
            Self::Excel => OfficePlanFormat::Excel,
            Self::PowerPoint => OfficePlanFormat::Presentation,
        }
    }

    /// Map this family to the underlying OOXML export format.
    #[must_use]
    pub const fn office_format(self) -> OfficeFormat {
        self.plan_format().office_format()
    }

    /// Return the conventional editable output extension for this family.
    #[must_use]
    pub const fn export_extension(self) -> &'static str {
        self.plan_format().extension()
    }

    /// Resolve a file extension (with or without a leading dot) to a family.
    ///
    /// Returns `None` for an extension outside the documented allowlist.
    #[must_use]
    pub fn from_extension(extension: &str) -> Option<Self> {
        let extension = extension.trim_start_matches('.').to_ascii_lowercase();
        EXTENSION_FAMILIES
            .iter()
            .find(|(allowed, _)| *allowed == extension)
            .map(|(_, family)| *family)
    }

    /// Resolve a file name (which may include a path and an extension) to a
    /// family using only its final extension.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        let file_name = name.rsplit(['/', '\\']).next().unwrap_or(name);
        let extension = file_name.rsplit_once('.').map_or(file_name, |(_, ext)| ext);
        Self::from_extension(extension)
    }
}

/// Input extension allowlist shared by family resolution and the descriptor.
/// Kept sorted by extension so [`IntegrationDescriptor::extensions`] is stable.
const EXTENSION_FAMILIES: &[(&str, OfficeFamily)] = &[
    ("doc", OfficeFamily::Word),
    ("docx", OfficeFamily::Word),
    ("excel", OfficeFamily::Excel),
    ("pml", OfficeFamily::PowerPoint),
    ("ppt", OfficeFamily::PowerPoint),
    ("pptx", OfficeFamily::PowerPoint),
    ("powerpoint", OfficeFamily::PowerPoint),
    ("sml", OfficeFamily::Excel),
    ("wml", OfficeFamily::Word),
    ("word", OfficeFamily::Word),
    ("xls", OfficeFamily::Excel),
    ("xlsx", OfficeFamily::Excel),
];

/// Direction of an Office integration conversion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OfficeFlow {
    /// Office-derived markup becomes a PDF.
    ImportToPdf,
    /// Recovered PDF page content becomes an editable Office document.
    PdfToOffice,
}

impl OfficeFlow {
    /// Both supported flows in a stable order.
    pub const ALL: &'static [Self] = &[Self::ImportToPdf, Self::PdfToOffice];

    /// Return both supported flows in a stable order.
    #[must_use]
    pub const fn all() -> &'static [Self] {
        Self::ALL
    }

    /// Return a stable machine-readable label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::ImportToPdf => "import-to-pdf",
            Self::PdfToOffice => "pdf-to-office",
        }
    }

    /// Return the expected output extension for this flow and family.
    #[must_use]
    pub const fn output_extension(self, family: OfficeFamily) -> &'static str {
        match self {
            Self::ImportToPdf => "pdf",
            Self::PdfToOffice => family.export_extension(),
        }
    }
}

/// How well a construct survives one direction of an Office conversion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SupportLevel {
    /// The construct is converted faithfully.
    Survives,
    /// The construct cannot be represented faithfully but is retained as a
    /// typed finding rather than silently dropped.
    TypedUnsupported,
    /// The construct is rejected with a typed error for security or fidelity
    /// reasons.
    Rejected,
    /// The construct does not arise in this direction of the conversion.
    NotApplicable,
}

impl SupportLevel {
    /// Return a stable machine-readable label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Survives => "survives",
            Self::TypedUnsupported => "typed-unsupported",
            Self::Rejected => "rejected",
            Self::NotApplicable => "not-applicable",
        }
    }
}

/// One row of the deterministic Office compatibility matrix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompatibilityEntry {
    /// Family this row describes.
    pub family: OfficeFamily,
    /// Stable construct identifier.
    pub construct: &'static str,
    /// Support when importing Office-derived markup.
    pub import: SupportLevel,
    /// Support when exporting to an editable Office document.
    pub export: SupportLevel,
    /// Short deterministic explanation of any fidelity loss.
    pub note: &'static str,
}

impl CompatibilityEntry {
    /// Create a compatibility matrix row.
    #[must_use]
    pub const fn new(
        family: OfficeFamily,
        construct: &'static str,
        import: SupportLevel,
        export: SupportLevel,
        note: &'static str,
    ) -> Self {
        Self {
            family,
            construct,
            import,
            export,
            note,
        }
    }
}

/// A deterministic matrix of which constructs survive import, which survive
/// export, and which are typed unsupported or rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompatibilityMatrix {
    entries: Vec<CompatibilityEntry>,
}

impl CompatibilityMatrix {
    /// Build the canonical matrix in a stable order.
    #[must_use]
    pub fn new() -> Self {
        let mut entries = Vec::new();
        for &family in OfficeFamily::ALL {
            entries.extend(rows_for_family(family));
        }
        entries.sort_by(|left, right| {
            left.family
                .label()
                .cmp(right.family.label())
                .then_with(|| left.construct.cmp(right.construct))
        });
        Self { entries }
    }

    /// Return all rows in canonical order.
    #[must_use]
    pub fn entries(&self) -> &[CompatibilityEntry] {
        &self.entries
    }

    /// Find the row for a family and construct, if it exists.
    #[must_use]
    pub fn get(&self, family: OfficeFamily, construct: &str) -> Option<&CompatibilityEntry> {
        self.entries
            .iter()
            .find(|entry| entry.family == family && entry.construct == construct)
    }

    /// Return every construct identifier recorded for a family.
    #[must_use]
    pub fn constructs(&self, family: OfficeFamily) -> Vec<&'static str> {
        self.entries
            .iter()
            .filter(|entry| entry.family == family)
            .map(|entry| entry.construct)
            .collect()
    }

    /// Return a byte-stable signature of the whole matrix.
    #[must_use]
    pub fn signature(&self) -> String {
        self.entries
            .iter()
            .map(|entry| {
                format!(
                    "{}/{}={}:{}",
                    entry.family.label(),
                    entry.construct,
                    entry.import.label(),
                    entry.export.label()
                )
            })
            .collect::<Vec<_>>()
            .join(";")
    }

    /// Return deterministic text lines describing the matrix.
    #[must_use]
    pub fn to_lines(&self) -> Vec<String> {
        self.entries
            .iter()
            .map(|entry| {
                format!(
                    "{:<11} {:<18} import={:<16} export={:<16} {}",
                    entry.family.label(),
                    entry.construct,
                    entry.import.label(),
                    entry.export.label(),
                    entry.note
                )
            })
            .collect()
    }
}

impl Default for CompatibilityMatrix {
    fn default() -> Self {
        Self::new()
    }
}

/// Build the fixed set of compatibility rows for one family.
fn rows_for_family(family: OfficeFamily) -> Vec<CompatibilityEntry> {
    let (text_export, textbox_export, page_break_export) = match family {
        OfficeFamily::Word => (
            SupportLevel::Survives,
            SupportLevel::TypedUnsupported,
            SupportLevel::Survives,
        ),
        OfficeFamily::Excel => (
            SupportLevel::Survives,
            SupportLevel::TypedUnsupported,
            SupportLevel::Survives,
        ),
        OfficeFamily::PowerPoint => (
            SupportLevel::TypedUnsupported,
            SupportLevel::Survives,
            SupportLevel::Survives,
        ),
    };
    vec![
        CompatibilityEntry::new(
            family,
            "paragraph",
            SupportLevel::Survives,
            text_export,
            "body text; PowerPoint flattens it into a generated text box",
        ),
        CompatibilityEntry::new(
            family,
            "heading",
            SupportLevel::Survives,
            text_export,
            "heading level kept on import; PowerPoint flattens it",
        ),
        CompatibilityEntry::new(
            family,
            "list_item",
            SupportLevel::Survives,
            text_export,
            "list item kept on import; PowerPoint flattens it",
        ),
        CompatibilityEntry::new(
            family,
            "table",
            SupportLevel::Survives,
            if family == OfficeFamily::PowerPoint {
                SupportLevel::TypedUnsupported
            } else {
                SupportLevel::Survives
            },
            "simple rectangular tables only; PowerPoint flattens to text",
        ),
        CompatibilityEntry::new(
            family,
            "page_break",
            SupportLevel::Survives,
            page_break_export,
            "page breaks become pages, sheets, or slides",
        ),
        CompatibilityEntry::new(
            family,
            "inline_style",
            SupportLevel::Survives,
            SupportLevel::NotApplicable,
            "text kept; inline styling is not captured by the importer",
        ),
        CompatibilityEntry::new(
            family,
            "entity",
            SupportLevel::Survives,
            SupportLevel::NotApplicable,
            "named and numeric entities are decoded",
        ),
        CompatibilityEntry::new(
            family,
            "text_box",
            SupportLevel::NotApplicable,
            textbox_export,
            "positioned boxes survive only in the presentation writer",
        ),
        CompatibilityEntry::new(
            family,
            "media",
            SupportLevel::TypedUnsupported,
            SupportLevel::NotApplicable,
            "recorded as a finding; never decoded or embedded",
        ),
        CompatibilityEntry::new(
            family,
            "unknown_tag",
            SupportLevel::TypedUnsupported,
            SupportLevel::NotApplicable,
            "recorded as a finding; never guessed into content",
        ),
        CompatibilityEntry::new(
            family,
            "active_content",
            SupportLevel::Rejected,
            SupportLevel::NotApplicable,
            "script/iframe/object/embed/form and siblings are rejected",
        ),
        CompatibilityEntry::new(
            family,
            "event_handler",
            SupportLevel::Rejected,
            SupportLevel::NotApplicable,
            "HTML event-handler attributes are never executed",
        ),
        CompatibilityEntry::new(
            family,
            "remote_reference",
            SupportLevel::Rejected,
            SupportLevel::NotApplicable,
            "remote http(s) references are never fetched",
        ),
        CompatibilityEntry::new(
            family,
            "vector_art",
            SupportLevel::NotApplicable,
            SupportLevel::TypedUnsupported,
            "reported; no editable drawing equivalent",
        ),
        CompatibilityEntry::new(
            family,
            "complex_table",
            SupportLevel::NotApplicable,
            SupportLevel::TypedUnsupported,
            "merged or non-rectangular tables are reported",
        ),
        CompatibilityEntry::new(
            family,
            "embedded_media",
            SupportLevel::NotApplicable,
            SupportLevel::TypedUnsupported,
            "audio/video and other media are reported",
        ),
        CompatibilityEntry::new(
            family,
            "unsupported_content",
            SupportLevel::NotApplicable,
            SupportLevel::TypedUnsupported,
            "image/annotation/drawing/hyperlink/field are reported",
        ),
    ]
}

/// One entry in the per-family extension allowlist.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExtensionRule {
    /// Lower-case file extension without a leading dot.
    pub extension: &'static str,
    /// Family that accepts this extension as input.
    pub family: OfficeFamily,
}

/// Per-family round-trip import/export capability report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RoundTripCapability {
    /// Family this report describes.
    pub family: OfficeFamily,
    /// Whether caller-supplied markup can become a PDF.
    pub import_to_pdf: bool,
    /// Whether recovered page content can become an editable document.
    pub pdf_to_office: bool,
    /// Canonical editable input extension for the family.
    pub input_extension: &'static str,
    /// Extension produced by the editable export adapter.
    pub export_extension: &'static str,
    /// Existing adapters used by the import direction.
    pub import_chain: &'static str,
    /// Existing adapters used by the export direction.
    pub export_chain: &'static str,
}

impl RoundTripCapability {
    /// Return `true` only when both directions are available.
    #[must_use]
    pub const fn is_round_trip(self) -> bool {
        self.import_to_pdf && self.pdf_to_office
    }

    /// Return a stable one-line summary.
    #[must_use]
    pub fn summary(self) -> String {
        format!(
            "family={} import-to-pdf={} pdf-to-office={} in={} out={}",
            self.family.label(),
            self.import_to_pdf,
            self.pdf_to_office,
            self.input_extension,
            self.export_extension
        )
    }
}

/// Validated Office integration descriptor for all supported families.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntegrationDescriptor {
    families: Vec<OfficeFamily>,
    capabilities: Vec<RoundTripCapability>,
    extensions: Vec<ExtensionRule>,
    matrix: CompatibilityMatrix,
    summary: String,
}

impl IntegrationDescriptor {
    /// Return the supported families in stable order.
    #[must_use]
    pub fn families(&self) -> &[OfficeFamily] {
        &self.families
    }

    /// Return one round-trip capability report per family.
    #[must_use]
    pub fn capabilities(&self) -> &[RoundTripCapability] {
        &self.capabilities
    }

    /// Return the capability report for one family.
    #[must_use]
    pub fn capability(&self, family: OfficeFamily) -> Option<&RoundTripCapability> {
        self.capabilities.iter().find(|item| item.family == family)
    }

    /// Return the sorted input extension allowlist.
    #[must_use]
    pub fn extensions(&self) -> &[ExtensionRule] {
        &self.extensions
    }

    /// Resolve an extension through this descriptor's allowlist.
    #[must_use]
    pub fn family_for_extension(&self, extension: &str) -> Option<OfficeFamily> {
        let extension = extension.trim_start_matches('.').to_ascii_lowercase();
        self.extensions
            .iter()
            .find(|rule| rule.extension == extension)
            .map(|rule| rule.family)
    }

    /// Return the deterministic compatibility matrix.
    #[must_use]
    pub fn matrix(&self) -> &CompatibilityMatrix {
        &self.matrix
    }

    /// Return a byte-stable summary of the descriptor.
    #[must_use]
    pub fn summary(&self) -> &str {
        &self.summary
    }

    /// Serialize the descriptor to deterministic bytes.
    #[must_use]
    pub fn to_manifest_bytes(&self) -> Vec<u8> {
        let mut lines = vec![format!("descriptor: {}", self.summary)];
        for capability in &self.capabilities {
            lines.push(format!("capability: {}", capability.summary()));
        }
        lines.extend(self.matrix.to_lines());
        lines.push(String::new());
        LineEnding::Lf.join(lines).into_bytes()
    }
}

/// Build a fresh, deterministic [`IntegrationDescriptor`].
#[must_use]
pub fn integration_descriptor() -> IntegrationDescriptor {
    let families = OfficeFamily::ALL.to_vec();
    let capabilities: Vec<RoundTripCapability> = families
        .iter()
        .map(|&family| RoundTripCapability {
            family,
            import_to_pdf: true,
            pdf_to_office: true,
            input_extension: canonical_input_extension(family),
            export_extension: family.export_extension(),
            import_chain: "office_import + office_contract + create_from_text",
            export_chain: "office_export_plan + office_export",
        })
        .collect();
    let extensions: Vec<ExtensionRule> = EXTENSION_FAMILIES
        .iter()
        .map(|&(extension, family)| ExtensionRule { extension, family })
        .collect();
    let matrix = CompatibilityMatrix::new();
    let extensions_text: Vec<&str> = extensions.iter().map(|rule| rule.extension).collect();
    let families_text: Vec<&str> = families.iter().map(|family| family.label()).collect();
    let summary = format!(
        "families=[{}] extensions=[{}] matrix={}",
        families_text.join(","),
        extensions_text.join(","),
        matrix.signature().len()
    );
    IntegrationDescriptor {
        families,
        capabilities,
        extensions,
        matrix,
        summary,
    }
}

/// Return the canonical editable input extension for a family.
const fn canonical_input_extension(family: OfficeFamily) -> &'static str {
    match family {
        OfficeFamily::Word => "docx",
        OfficeFamily::Excel => "xlsx",
        OfficeFamily::PowerPoint => "pptx",
    }
}

/// Parse a caller-supplied family name into a typed family.
///
/// # Errors
///
/// Returns [`OfficeIntegrationError::UnknownFamily`] when the name matches no
/// extension in the documented allowlist.
pub fn parse_family(name: &str) -> Result<OfficeFamily, OfficeIntegrationError> {
    OfficeFamily::from_name(name).ok_or_else(|| OfficeIntegrationError::UnknownFamily {
        name: name.to_owned(),
    })
}

/// Caller-supplied payload for an integration conversion.
#[derive(Debug, Clone, PartialEq)]
pub enum OfficeEnvelopePayload {
    /// Office-derived HTML/XML markup for the `ImportToPdf` flow.
    Markup(String),
    /// Recovered page content for the `PdfToOffice` flow.
    Extracted(ExtractedDocument),
}

/// A caller-supplied Office integration envelope.
#[derive(Debug, Clone, PartialEq)]
pub struct OfficeIntegrationEnvelope {
    /// Declared Office family.
    pub family: OfficeFamily,
    /// Requested conversion direction.
    pub flow: OfficeFlow,
    /// Output document name; the expected extension is appended when absent.
    pub name: String,
    /// Direction-specific payload.
    pub payload: OfficeEnvelopePayload,
}

impl OfficeIntegrationEnvelope {
    /// Create an envelope from its family, flow, output name, and payload.
    #[must_use]
    pub fn new(
        family: OfficeFamily,
        flow: OfficeFlow,
        name: impl Into<String>,
        payload: OfficeEnvelopePayload,
    ) -> Self {
        Self {
            family,
            flow,
            name: name.into(),
            payload,
        }
    }

    /// Build an envelope from a caller-supplied family name, returning a typed
    /// error for an unknown family.
    ///
    /// # Errors
    ///
    /// Returns [`OfficeIntegrationError::UnknownFamily`] when `family_name` is
    /// outside the extension allowlist.
    pub fn from_family_name(
        family_name: &str,
        flow: OfficeFlow,
        name: impl Into<String>,
        payload: OfficeEnvelopePayload,
    ) -> Result<Self, OfficeIntegrationError> {
        Ok(Self::new(parse_family(family_name)?, flow, name, payload))
    }
}

/// A typed failure while validating or running an Office integration.
#[derive(Debug, thiserror::Error)]
pub enum OfficeIntegrationError {
    /// The output document name was empty.
    #[error("Office integration output name must not be empty")]
    EmptyName,
    /// The output document name exceeded the documented byte limit.
    #[error("Office integration output name exceeds {limit} bytes")]
    NameTooLong {
        /// Enforced byte limit.
        limit: usize,
    },
    /// The output document name contained an unsafe path or device component.
    #[error("unsafe Office integration output name '{name}': {reason}")]
    UnsafeName {
        /// Rejected name.
        name: String,
        /// Stable reason.
        reason: &'static str,
    },
    /// The output name carried an extension that does not match the flow.
    #[error("unexpected output extension '{extension}'; expected '{expected}'")]
    UnexpectedExtension {
        /// Extension supplied by the caller.
        extension: String,
        /// Extension required by the flow.
        expected: &'static str,
    },
    /// The caller supplied a family name outside the allowlist.
    #[error("unknown Office family '{name}'")]
    UnknownFamily {
        /// Rejected family name.
        name: String,
    },
    /// The payload did not match the requested direction.
    #[error("payload '{payload}' does not match flow {flow}")]
    PayloadFlowMismatch {
        /// Payload kind supplied by the caller.
        payload: &'static str,
        /// Requested flow label.
        flow: &'static str,
    },
    /// The extracted envelope exceeded the documented byte budget.
    #[error("Office integration envelope exceeds {limit} bytes")]
    TooLarge {
        /// Enforced byte limit.
        limit: usize,
    },
    /// The markup contained no convertible text.
    #[error("Office integration markup produced no convertible text")]
    NoConvertibleText,
    /// The shared envelope validator rejected the bundle.
    #[error(transparent)]
    Envelope(#[from] OfficeEnvelopeError),
    /// The Office importer rejected the markup.
    #[error(transparent)]
    Import(#[from] OfficeImportError),
    /// The export mapper rejected the recovered content.
    #[error(transparent)]
    Plan(#[from] OfficePlanError),
    /// The local OOXML writer rejected the mapped document.
    #[error(transparent)]
    Export(#[from] OfficeExportError),
    /// The PDF creator rejected the extracted text.
    #[error("PDF creation failed: {detail}")]
    PdfCreationFailed {
        /// Deterministic error text from the existing creator.
        detail: String,
    },
}

/// One typed construct recorded by an integration step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntegrationUnsupported {
    /// Stage that produced the finding: `import` or `export`.
    pub stage: &'static str,
    /// Stable machine-readable kind.
    pub kind: String,
    /// Token or explanation that triggered the finding.
    pub detail: String,
}

impl IntegrationUnsupported {
    /// Create a typed unsupported finding.
    #[must_use]
    pub fn new(stage: &'static str, kind: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            stage,
            kind: kind.into(),
            detail: detail.into(),
        }
    }
}

/// One deterministic step of an integration plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IntegrationStep {
    /// One-based execution order.
    pub order: usize,
    /// Existing adapter or validator used by this step.
    pub adapter: &'static str,
    /// Stable action description.
    pub action: &'static str,
}

/// A validated, deterministic plan for one Office integration envelope.
#[derive(Debug, Clone, PartialEq)]
pub struct OfficeIntegrationPlan {
    family: OfficeFamily,
    flow: OfficeFlow,
    output_name: String,
    stem: String,
    capability: RoundTripCapability,
    steps: Vec<IntegrationStep>,
    unsupported: Vec<IntegrationUnsupported>,
    page_count: usize,
    block_count: usize,
    summary: String,
    payload: OfficeEnvelopePayload,
}

impl OfficeIntegrationPlan {
    /// Declared family.
    #[must_use]
    pub const fn family(&self) -> OfficeFamily {
        self.family
    }

    /// Requested direction.
    #[must_use]
    pub const fn flow(&self) -> OfficeFlow {
        self.flow
    }

    /// Sanitized output name including the flow's extension.
    #[must_use]
    pub fn output_name(&self) -> &str {
        &self.output_name
    }

    /// Round-trip capability report for this family.
    #[must_use]
    pub const fn capability(&self) -> RoundTripCapability {
        self.capability
    }

    /// Deterministic execution steps, reusing existing adapters.
    #[must_use]
    pub fn steps(&self) -> &[IntegrationStep] {
        &self.steps
    }

    /// Typed constructs recorded while planning.
    #[must_use]
    pub fn unsupported(&self) -> &[IntegrationUnsupported] {
        &self.unsupported
    }

    /// Number of pages the adapters will produce.
    #[must_use]
    pub const fn page_count(&self) -> usize {
        self.page_count
    }

    /// Number of content blocks the adapters will produce.
    #[must_use]
    pub const fn block_count(&self) -> usize {
        self.block_count
    }

    /// Stable byte-for-byte report line for this plan.
    #[must_use]
    pub fn summary(&self) -> &str {
        &self.summary
    }

    /// Serialize the plan to deterministic bytes.
    #[must_use]
    pub fn to_manifest_bytes(&self) -> Vec<u8> {
        let mut lines = vec![self.summary.clone()];
        lines.extend(
            self.steps
                .iter()
                .map(|step| format!("step {}: {} {}", step.order, step.adapter, step.action)),
        );
        lines.extend(self.unsupported.iter().map(|item| {
            format!(
                "unsupported: {} [{}] {}",
                item.stage, item.kind, item.detail
            )
        }));
        lines.push(String::new());
        LineEnding::Lf.join(lines).into_bytes()
    }

    /// Execute the plan by reusing the existing adapters.
    ///
    /// # Errors
    ///
    /// Returns a typed [`OfficeIntegrationError`] for an invalid payload, a
    /// rejected model, or a failure in one of the existing adapters.
    pub fn execute(&self) -> Result<OfficeIntegrationOutput, OfficeIntegrationError> {
        let (bytes, extension, page_count, block_count, unsupported) =
            match (self.flow, &self.payload) {
                (OfficeFlow::ImportToPdf, OfficeEnvelopePayload::Markup(markup)) => {
                    let bundle =
                        OfficeBundle::new(self.family.bundle_kind(), &self.stem, markup.clone());
                    let imported = import_office_bundle_with_report(&bundle)?;
                    let text = document_plain_text(&imported.document);
                    if text.trim().is_empty() {
                        return Err(OfficeIntegrationError::NoConvertibleText);
                    }
                    let bytes =
                        crate::create_from_text(&text, &CreateOptions::new()).map_err(|error| {
                            OfficeIntegrationError::PdfCreationFailed {
                                detail: error.to_string(),
                            }
                        })?;
                    let findings = imported
                        .report
                        .findings
                        .iter()
                        .map(|finding| {
                            IntegrationUnsupported::new(
                                "import",
                                finding.kind,
                                finding.token.clone(),
                            )
                        })
                        .collect();
                    (
                        bytes,
                        "pdf",
                        imported.report.page_count,
                        imported.report.block_count,
                        findings,
                    )
                }
                (OfficeFlow::PdfToOffice, OfficeEnvelopePayload::Extracted(source)) => {
                    let envelope: OfficeExportEnvelope = build_office_export_plan(
                        source,
                        self.family.plan_format(),
                        &OfficePlanOptions::default(),
                    )?;
                    let export = envelope.write()?;
                    let mut findings: Vec<IntegrationUnsupported> = export
                        .report
                        .unsupported
                        .iter()
                        .map(|item| {
                            IntegrationUnsupported::new(
                                "export",
                                item.kind.clone(),
                                format!("page {}: {}", item.page + 1, item.detail),
                            )
                        })
                        .collect();
                    let page_count = envelope.page_labels().len();
                    let block_count = envelope
                        .document()
                        .pages
                        .iter()
                        .map(|page| page.blocks.len())
                        .sum();
                    findings.sort_by(|left, right| {
                        left.stage
                            .cmp(right.stage)
                            .then_with(|| left.kind.cmp(&right.kind))
                            .then_with(|| left.detail.cmp(&right.detail))
                    });
                    (
                        export.bytes,
                        self.family.export_extension(),
                        page_count,
                        block_count,
                        findings,
                    )
                }
                _ => {
                    return Err(OfficeIntegrationError::PayloadFlowMismatch {
                        payload: payload_kind(&self.payload),
                        flow: self.flow.label(),
                    });
                }
            };
        Ok(OfficeIntegrationOutput {
            report: OfficeIntegrationReport {
                family: self.family,
                flow: self.flow,
                output_name: self.output_name.clone(),
                page_count,
                block_count,
                unsupported,
                byte_count: bytes.len(),
            },
            bytes,
            extension,
        })
    }
}

/// Typed report returned with executed integration bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OfficeIntegrationReport {
    /// Family that was converted.
    pub family: OfficeFamily,
    /// Direction that was executed.
    pub flow: OfficeFlow,
    /// Sanitized output name.
    pub output_name: String,
    /// Number of pages produced.
    pub page_count: usize,
    /// Number of content blocks produced.
    pub block_count: usize,
    /// Typed findings recorded during execution.
    pub unsupported: Vec<IntegrationUnsupported>,
    /// Number of produced bytes.
    pub byte_count: usize,
}

/// Produced bytes plus their typed report and extension.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OfficeIntegrationOutput {
    /// Generated output bytes (a PDF or an OOXML archive).
    pub bytes: Vec<u8>,
    /// Extension matching the produced bytes.
    pub extension: &'static str,
    /// Typed execution report.
    pub report: OfficeIntegrationReport,
}

/// Validate a caller-supplied envelope and return a deterministic plan.
///
/// This is the single library entry point for PLAT-003. It validates the output
/// name (traversal, unsafe characters, reserved device names, extension), checks
/// the payload against the documented size budget, and reuses the existing
/// Office adapters to describe an executable plan for the requested direction.
///
/// # Errors
///
/// Returns a typed [`OfficeIntegrationError`] for an unsafe name, an unknown
/// family, a payload/flow mismatch, an oversized envelope, or an adapter
/// rejection.
pub fn plan_office_integration(
    envelope: &OfficeIntegrationEnvelope,
) -> Result<OfficeIntegrationPlan, OfficeIntegrationError> {
    let descriptor = integration_descriptor();
    let capability =
        *descriptor
            .capability(envelope.family)
            .ok_or(OfficeIntegrationError::UnknownFamily {
                name: envelope.family.label().to_owned(),
            })?;
    let expected_extension = envelope.flow.output_extension(envelope.family);
    let stem = validate_output_name(&envelope.name, expected_extension)?;
    let output_name = format!("{stem}.{expected_extension}");

    let (steps, unsupported, page_count, block_count) = match (envelope.flow, &envelope.payload) {
        (OfficeFlow::ImportToPdf, OfficeEnvelopePayload::Markup(markup)) => {
            let bundle = OfficeBundle::new(envelope.family.bundle_kind(), &stem, markup.clone());
            validate_office_envelope(&bundle)?;
            let conversion = build_office_conversion_plan(&bundle)?;
            if conversion.block_count() == 0 {
                return Err(OfficeIntegrationError::NoConvertibleText);
            }
            let findings: Vec<IntegrationUnsupported> = conversion
                .unsupported()
                .iter()
                .map(|item| IntegrationUnsupported::new("import", item.kind(), item.token()))
                .collect();
            (
                import_steps(),
                findings,
                conversion.page_count(),
                conversion.block_count(),
            )
        }
        (OfficeFlow::PdfToOffice, OfficeEnvelopePayload::Extracted(source)) => {
            ensure_extracted_size(source)?;
            let export_plan = build_office_export_plan(
                source,
                envelope.family.plan_format(),
                &OfficePlanOptions::default(),
            )?;
            let findings = export_plan
                .unsupported()
                .iter()
                .map(|item| IntegrationUnsupported::new("export", item.kind, item.detail.clone()))
                .collect();
            let page_count = export_plan.page_labels().len();
            let block_count = export_plan
                .document()
                .pages
                .iter()
                .map(|page| page.blocks.len())
                .sum();
            (
                export_steps(envelope.family),
                findings,
                page_count,
                block_count,
            )
        }
        _ => {
            return Err(OfficeIntegrationError::PayloadFlowMismatch {
                payload: payload_kind(&envelope.payload),
                flow: envelope.flow.label(),
            });
        }
    };

    let summary = format!(
        "family={} flow={} target={} output={} pages={} blocks={} steps={} unsupported=[{}]",
        envelope.family.label(),
        envelope.flow.label(),
        capability.export_extension,
        output_name,
        page_count,
        block_count,
        steps.len(),
        unsupported
            .iter()
            .map(|item| item.kind.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    );
    Ok(OfficeIntegrationPlan {
        family: envelope.family,
        flow: envelope.flow,
        output_name,
        stem,
        capability,
        steps,
        unsupported,
        page_count,
        block_count,
        summary,
        payload: envelope.payload.clone(),
    })
}

/// Return a stable label for a payload kind.
fn payload_kind(payload: &OfficeEnvelopePayload) -> &'static str {
    match payload {
        OfficeEnvelopePayload::Markup(_) => "markup",
        OfficeEnvelopePayload::Extracted(_) => "extracted",
    }
}

/// Deterministic steps for the import-to-PDF direction.
fn import_steps() -> Vec<IntegrationStep> {
    vec![
        IntegrationStep {
            order: 1,
            adapter: "office_contract",
            action: "validate-envelope",
        },
        IntegrationStep {
            order: 2,
            adapter: "office_import",
            action: "map-markup-to-document",
        },
        IntegrationStep {
            order: 3,
            adapter: "create_from_text",
            action: "write-pdf-bytes",
        },
    ]
}

/// Deterministic steps for the PDF-to-Office direction.
fn export_steps(family: OfficeFamily) -> Vec<IntegrationStep> {
    vec![
        IntegrationStep {
            order: 1,
            adapter: "extract_model",
            action: "validate-recovered-content",
        },
        IntegrationStep {
            order: 2,
            adapter: "office_export_plan",
            action: "map-to-office-document",
        },
        IntegrationStep {
            order: 3,
            adapter: family.plan_format().label(),
            action: "write-ooxml-archive",
        },
    ]
}

/// Validate and sanitize an output name, enforcing the extension allowlist.
fn validate_output_name(
    name: &str,
    expected_extension: &'static str,
) -> Result<String, OfficeIntegrationError> {
    if name.is_empty() {
        return Err(OfficeIntegrationError::EmptyName);
    }
    if name.len() > OFFICE_INTEGRATION_MAX_NAME_BYTES {
        return Err(OfficeIntegrationError::NameTooLong {
            limit: OFFICE_INTEGRATION_MAX_NAME_BYTES,
        });
    }
    if name.chars().any(|character| character.is_control()) {
        return Err(OfficeIntegrationError::UnsafeName {
            name: name.to_owned(),
            reason: "control-character",
        });
    }
    if name.contains('/') || name.contains('\\') {
        return Err(OfficeIntegrationError::UnsafeName {
            name: name.to_owned(),
            reason: "path-separator",
        });
    }
    if name.contains("..") {
        return Err(OfficeIntegrationError::UnsafeName {
            name: name.to_owned(),
            reason: "parent-traversal",
        });
    }
    if name.starts_with('.') {
        return Err(OfficeIntegrationError::UnsafeName {
            name: name.to_owned(),
            reason: "hidden-or-relative",
        });
    }
    if name.contains(':') {
        return Err(OfficeIntegrationError::UnsafeName {
            name: name.to_owned(),
            reason: "drive-or-alternate-stream",
        });
    }
    if name.ends_with('.') || name.ends_with(' ') {
        return Err(OfficeIntegrationError::UnsafeName {
            name: name.to_owned(),
            reason: "trailing-separator-or-space",
        });
    }

    let (stem_source, extension) = match name.rsplit_once('.') {
        Some((stem, extension)) => (stem, Some(extension)),
        None => (name, None),
    };
    if let Some(extension) = extension
        && !extension.eq_ignore_ascii_case(expected_extension)
    {
        return Err(OfficeIntegrationError::UnexpectedExtension {
            extension: extension.to_owned(),
            expected: expected_extension,
        });
    }
    if is_reserved_device_name(stem_source) {
        return Err(OfficeIntegrationError::UnsafeName {
            name: name.to_owned(),
            reason: "reserved-device-name",
        });
    }
    Ok(sanitize_stem(stem_source))
}

/// Return `true` for Windows reserved device names, case-insensitively.
fn is_reserved_device_name(stem: &str) -> bool {
    let base = stem.split('.').next().unwrap_or("").to_ascii_uppercase();
    if matches!(base.as_str(), "CON" | "PRN" | "AUX" | "NUL") {
        return true;
    }
    for prefix in ["COM", "LPT"] {
        if let Some(rest) = base.strip_prefix(prefix)
            && rest.len() == 1
            && rest.as_bytes()[0].is_ascii_digit()
            && rest != "0"
        {
            return true;
        }
    }
    false
}

/// Sanitize a name stem into a bounded, path-safe token.
fn sanitize_stem(stem: &str) -> String {
    let mut out: String = stem
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .take(MAX_STEM_CHARS)
        .collect();
    while out.ends_with('_') || out.ends_with('-') {
        let _ = out.pop();
    }
    if out.is_empty() {
        out.push_str("document");
    }
    out
}

/// Reject an extracted envelope whose total text exceeds the byte budget.
fn ensure_extracted_size(source: &ExtractedDocument) -> Result<(), OfficeIntegrationError> {
    let mut total = source.title.len();
    for page in &source.pages {
        for run in &page.runs {
            total = total.saturating_add(run.text.len());
        }
        for text_box in &page.text_boxes {
            total = total.saturating_add(text_box.text.len());
        }
        for table in &page.tables {
            for row in &table.rows {
                for cell in row {
                    total = total.saturating_add(cell.len());
                }
            }
        }
        for item in &page.unsupported {
            total = total.saturating_add(item.detail().len());
        }
    }
    if total > OFFICE_BUNDLE_MAX_BYTES {
        Err(OfficeIntegrationError::TooLarge {
            limit: OFFICE_BUNDLE_MAX_BYTES,
        })
    } else {
        Ok(())
    }
}

/// Render an imported document to deterministic plain text for the PDF writer.
fn document_plain_text(document: &OfficeDocument) -> String {
    let mut pages: Vec<&OfficePage> = document.pages.iter().collect();
    pages.sort_by_key(|page| page.index);
    let rendered: Vec<String> = pages
        .iter()
        .map(|page| {
            let mut lines: Vec<String> = Vec::new();
            for block in &page.blocks {
                match block {
                    OfficeBlock::Paragraph { text, .. } | OfficeBlock::ListItem { text, .. } => {
                        if !text.trim().is_empty() {
                            lines.push(text.trim().to_owned());
                        }
                    }
                    OfficeBlock::Table(table) => {
                        for row in &table.rows {
                            let cells: Vec<String> = row
                                .iter()
                                .map(|cell| match &cell.value {
                                    CellValue::Text(text) => text.clone(),
                                    CellValue::Number(number) => number.to_string(),
                                    CellValue::Boolean(value) => value.to_string(),
                                    CellValue::Empty => String::new(),
                                })
                                .collect();
                            let line = cells.join(" | ");
                            if !line.trim().is_empty() {
                                lines.push(line);
                            }
                        }
                    }
                    OfficeBlock::TextBox(text_box) => {
                        if !text_box.text.trim().is_empty() {
                            lines.push(text_box.text.trim().to_owned());
                        }
                    }
                    OfficeBlock::Unsupported(_) => {}
                }
            }
            lines.join("\n")
        })
        .collect();
    rendered.join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract_model::{ExtractedPage, ExtractedRun, ExtractedTable, ExtractedUnsupported};

    fn markup_envelope(name: &str, markup: &str) -> OfficeIntegrationEnvelope {
        OfficeIntegrationEnvelope::new(
            OfficeFamily::Word,
            OfficeFlow::ImportToPdf,
            name,
            OfficeEnvelopePayload::Markup(markup.to_owned()),
        )
    }

    fn extracted_envelope(family: OfficeFamily, name: &str) -> OfficeIntegrationEnvelope {
        OfficeIntegrationEnvelope::new(
            family,
            OfficeFlow::PdfToOffice,
            name,
            OfficeEnvelopePayload::Extracted(ExtractedDocument::new(
                "Deck",
                vec![
                    ExtractedPage::new(0, 200.0, 300.0)
                        .with_runs(vec![ExtractedRun::new("Hello", 10.0, 280.0, 24.0)])
                        .with_table(ExtractedTable::new(vec![vec!["cell".to_owned()]]))
                        .with_unsupported(ExtractedUnsupported::VectorArt {
                            detail: "curve".to_owned(),
                        }),
                ],
            )),
        )
    }

    #[test]
    fn descriptor_is_deterministic_and_covers_all_families() {
        let first = integration_descriptor();
        let second = integration_descriptor();
        assert_eq!(first, second);
        assert_eq!(first.to_manifest_bytes(), second.to_manifest_bytes());
        assert_eq!(first.families(), OfficeFamily::ALL);
        assert_eq!(first.capabilities().len(), 3);
        assert!(first.capabilities().iter().all(|item| item.is_round_trip()));
        assert_eq!(first.extensions().len(), EXTENSION_FAMILIES.len());
        assert_eq!(first.family_for_extension("docx"), Some(OfficeFamily::Word));
        assert_eq!(first.family_for_extension("XLS"), Some(OfficeFamily::Excel));
        assert_eq!(first.family_for_extension("pdf"), None);
    }

    #[test]
    fn compatibility_matrix_is_deterministic_and_complete() {
        let matrix = CompatibilityMatrix::new();
        assert_eq!(matrix, CompatibilityMatrix::new());
        assert_eq!(matrix.signature(), CompatibilityMatrix::new().signature());
        assert_eq!(matrix.entries().len(), 17 * 3);
        for family in OfficeFamily::ALL {
            let constructs = matrix.constructs(*family);
            assert_eq!(constructs.len(), 17);
            let mut sorted = constructs.clone();
            sorted.sort_unstable();
            assert_eq!(constructs, sorted, "rows must be sorted by construct");
        }
    }

    #[test]
    fn matrix_reports_import_export_and_typed_unsupported_constructs() {
        let matrix = CompatibilityMatrix::new();
        let paragraph = matrix.get(OfficeFamily::Word, "paragraph").expect("row");
        assert_eq!(paragraph.import, SupportLevel::Survives);
        assert_eq!(paragraph.export, SupportLevel::Survives);
        let text_box = matrix
            .get(OfficeFamily::PowerPoint, "text_box")
            .expect("row");
        assert_eq!(text_box.export, SupportLevel::Survives);
        let word_text_box = matrix.get(OfficeFamily::Word, "text_box").expect("row");
        assert_eq!(word_text_box.export, SupportLevel::TypedUnsupported);
        let media = matrix.get(OfficeFamily::Excel, "media").expect("row");
        assert_eq!(media.import, SupportLevel::TypedUnsupported);
        let active = matrix
            .get(OfficeFamily::Word, "active_content")
            .expect("row");
        assert_eq!(active.import, SupportLevel::Rejected);
        let ppt_paragraph = matrix
            .get(OfficeFamily::PowerPoint, "paragraph")
            .expect("row");
        assert_eq!(ppt_paragraph.export, SupportLevel::TypedUnsupported);
    }

    #[test]
    fn round_trip_capability_reporting_is_per_family() {
        for family in OfficeFamily::ALL {
            let descriptor = integration_descriptor();
            let capability = descriptor.capability(*family).expect("capability");
            assert_eq!(capability.family, *family);
            assert!(capability.import_to_pdf);
            assert!(capability.pdf_to_office);
            assert!(capability.is_round_trip());
            assert_eq!(capability.export_extension, family.export_extension());
            assert!(capability.summary().contains(family.label()));
            assert!(capability.import_chain.contains("create_from_text"));
            assert!(capability.export_chain.contains("office_export"));
        }
    }

    #[test]
    fn import_plan_reuses_existing_adapters_and_is_deterministic() {
        let envelope = markup_envelope(
            "Quarterly Report",
            "<p>Hello</p><!--pdfxi:page--><p>World</p>",
        );
        let first = plan_office_integration(&envelope).expect("plan");
        let second = plan_office_integration(&envelope).expect("plan again");
        assert_eq!(first, second);
        assert_eq!(first.to_manifest_bytes(), second.to_manifest_bytes());
        assert_eq!(first.family(), OfficeFamily::Word);
        assert_eq!(first.flow(), OfficeFlow::ImportToPdf);
        assert_eq!(first.output_name(), "quarterly_report.pdf");
        assert_eq!(first.page_count(), 2);
        assert_eq!(first.block_count(), 2);
        assert_eq!(first.steps().len(), 3);
        assert_eq!(first.steps()[1].adapter, "office_import");
        assert!(first.summary().contains("flow=import-to-pdf"));
    }

    #[test]
    fn export_plan_reuses_existing_adapters_for_every_family() {
        for family in OfficeFamily::ALL {
            let envelope = extracted_envelope(*family, "Deck");
            let plan = plan_office_integration(&envelope).expect("plan");
            assert_eq!(plan.flow(), OfficeFlow::PdfToOffice);
            assert_eq!(
                plan.output_name(),
                format!("deck.{}", family.export_extension())
            );
            assert_eq!(plan.steps()[2].adapter, family.plan_format().label());
            assert!(
                plan.unsupported()
                    .iter()
                    .any(|item| item.kind == "vector-art")
            );
        }
    }

    #[test]
    fn execute_import_produces_pdf_bytes() {
        let envelope = markup_envelope(
            "Bridge",
            "<h1>Title</h1><p>Hello</p><table><tr><td>a</td><td>b</td></tr></table>",
        );
        let plan = plan_office_integration(&envelope).expect("plan");
        let output = plan.execute().expect("execute");
        assert_eq!(output.extension, "pdf");
        assert!(output.bytes.starts_with(b"%PDF-"));
        assert_eq!(output.report.output_name, "bridge.pdf");
        assert_eq!(output.report.byte_count, output.bytes.len());
        assert_eq!(plan.execute().expect("execute again").bytes, output.bytes);
    }

    #[test]
    fn execute_export_produces_valid_ooxml_for_every_family() {
        for family in OfficeFamily::ALL {
            let envelope = extracted_envelope(*family, "Deck");
            let plan = plan_office_integration(&envelope).expect("plan");
            let output = plan.execute().expect("execute");
            assert_eq!(output.extension, family.export_extension());
            assert_eq!(&output.bytes[..2], b"PK");
            assert!(
                output
                    .report
                    .unsupported
                    .iter()
                    .any(|item| item.kind == "vector-art")
            );
        }
    }

    #[test]
    fn path_traversal_and_unsafe_names_are_rejected() {
        for name in [
            "../escape",
            "a/../b",
            "a\\b",
            "C:\\temp",
            "a:b",
            ".hidden.pdf",
            "trail.pdf.",
            "aux.pdf",
            "com1.pdf",
            "bad\u{0000}name.pdf",
        ] {
            let result = plan_office_integration(&markup_envelope(name, "<p>x</p>"));
            assert!(
                matches!(result, Err(OfficeIntegrationError::UnsafeName { .. })),
                "expected UnsafeName for {name:?}: {result:?}"
            );
        }
    }

    #[test]
    fn oversized_and_empty_fields_are_rejected_with_typed_errors() {
        let long_name = "a".repeat(OFFICE_INTEGRATION_MAX_NAME_BYTES + 1);
        assert!(matches!(
            plan_office_integration(&markup_envelope(&long_name, "<p>x</p>")),
            Err(OfficeIntegrationError::NameTooLong { .. })
        ));
        assert!(matches!(
            plan_office_integration(&markup_envelope("", "<p>x</p>")),
            Err(OfficeIntegrationError::EmptyName)
        ));
        assert!(matches!(
            plan_office_integration(&markup_envelope("t", "   ")),
            Err(OfficeIntegrationError::Envelope(
                OfficeEnvelopeError::EmptyMarkup
            ))
        ));
        let huge = format!("<p>{}</p>", "x".repeat(OFFICE_BUNDLE_MAX_BYTES));
        assert!(matches!(
            plan_office_integration(&markup_envelope("t", &huge)),
            Err(OfficeIntegrationError::Envelope(
                OfficeEnvelopeError::TooLarge { .. }
            ))
        ));
    }

    #[test]
    fn unknown_families_are_rejected_with_typed_errors() {
        assert!(matches!(
            parse_family("pdf"),
            Err(OfficeIntegrationError::UnknownFamily { .. })
        ));
        assert_eq!(parse_family("docx").expect("docx"), OfficeFamily::Word);
        assert_eq!(parse_family("XLSX").expect("XLSX"), OfficeFamily::Excel);
        assert_eq!(
            parse_family("folder/ppt").expect("ppt"),
            OfficeFamily::PowerPoint
        );
        let built = OfficeIntegrationEnvelope::from_family_name(
            "pdf",
            OfficeFlow::ImportToPdf,
            "x",
            OfficeEnvelopePayload::Markup("<p>x</p>".to_owned()),
        );
        assert!(matches!(
            built,
            Err(OfficeIntegrationError::UnknownFamily { .. })
        ));
    }

    #[test]
    fn payload_flow_mismatch_and_extension_allowlist_are_enforced() {
        let mismatch = OfficeIntegrationEnvelope::new(
            OfficeFamily::Word,
            OfficeFlow::ImportToPdf,
            "x",
            OfficeEnvelopePayload::Extracted(ExtractedDocument::new(
                "T",
                vec![ExtractedPage::new(0, 10.0, 10.0)],
            )),
        );
        assert!(matches!(
            plan_office_integration(&mismatch),
            Err(OfficeIntegrationError::PayloadFlowMismatch { .. })
        ));
        let wrong_extension = OfficeIntegrationEnvelope::new(
            OfficeFamily::Excel,
            OfficeFlow::PdfToOffice,
            "deck.docx",
            OfficeEnvelopePayload::Extracted(ExtractedDocument::new(
                "T",
                vec![ExtractedPage::new(0, 10.0, 10.0)],
            )),
        );
        assert!(matches!(
            plan_office_integration(&wrong_extension),
            Err(OfficeIntegrationError::UnexpectedExtension { .. })
        ));
    }

    #[test]
    fn oversized_extracted_envelope_is_rejected() {
        let huge_run = ExtractedRun::new("x".repeat(OFFICE_BUNDLE_MAX_BYTES + 1), 0.0, 0.0, 10.0);
        let envelope = OfficeIntegrationEnvelope::new(
            OfficeFamily::Word,
            OfficeFlow::PdfToOffice,
            "huge",
            OfficeEnvelopePayload::Extracted(ExtractedDocument::new(
                "T",
                vec![ExtractedPage::new(0, 100.0, 100.0).with_runs(vec![huge_run])],
            )),
        );
        assert!(matches!(
            plan_office_integration(&envelope),
            Err(OfficeIntegrationError::TooLarge { .. })
        ));
    }

    #[test]
    fn typed_unsupported_constructs_flow_into_plans() {
        let envelope = markup_envelope("Media", "<p>kept</p><img src=\"a.png\"/><widget/>");
        let plan = plan_office_integration(&envelope).expect("plan");
        let kinds: Vec<&str> = plan
            .unsupported()
            .iter()
            .map(|item| item.kind.as_str())
            .collect();
        assert!(kinds.contains(&"media"));
        assert!(kinds.contains(&"unknown-tag"));
        assert!(plan.unsupported().iter().all(|item| item.stage == "import"));
        let active = markup_envelope("Active", "<script>alert(1)</script>");
        assert!(matches!(
            plan_office_integration(&active),
            Err(OfficeIntegrationError::Envelope(
                OfficeEnvelopeError::Import(OfficeImportError::UnsupportedConstruct { .. })
            ))
        ));
    }

    #[test]
    fn media_only_markup_has_no_convertible_text() {
        let envelope = markup_envelope("Media", "<img src=\"a.png\"/>");
        assert!(matches!(
            plan_office_integration(&envelope),
            Err(OfficeIntegrationError::NoConvertibleText)
        ));
    }

    #[test]
    fn family_mapping_helpers_are_total_and_stable() {
        for family in OfficeFamily::ALL {
            assert_eq!(
                OfficeFamily::from_extension(family.export_extension()),
                Some(*family)
            );
            assert_eq!(
                family.office_format().extension(),
                family.export_extension()
            );
        }
        assert_eq!(
            OfficeFlow::ImportToPdf.output_extension(OfficeFamily::Word),
            "pdf"
        );
        assert_eq!(
            OfficeFlow::PdfToOffice.output_extension(OfficeFamily::Excel),
            "xlsx"
        );
        assert_eq!(SupportLevel::Survives.label(), "survives");
        assert_eq!(SupportLevel::Rejected.label(), "rejected");
        assert_eq!(OfficeFlow::all().len(), 2);
    }
}
