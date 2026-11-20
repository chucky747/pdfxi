//! Deterministic PostScript ingestion plan for CREATE-013 (ADR 0008).
//!
//! [`crate::postscript_ingest`] already implements a bounded DSC reader with a
//! documented text/line/rectangle subset and typed findings for everything
//! else. This module closes the remaining *library-level* gap by turning that
//! ingested document into a stable page-by-page assembly plan: per-page counts
//! of the supported constructs, a resolved media box, and a typed
//! representation of every unsupported construct. No PostScript interpreter,
//! external Distiller process, or CLI is introduced.

use crate::create::PageSize;
use crate::postscript::PsContent;
use crate::postscript_ingest::{
    PostScriptKind, PsIngestError, PsIngestOptions, PsUnsupportedFinding,
    ingest_postscript_with_options,
};

/// A typed construct reported by the bounded DSC reader.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PsUnsupportedConstruct {
    /// Binary data that is never decoded.
    BinaryData {
        /// The marker that triggered the finding.
        token: String,
    },
    /// Raster image content that is never embedded.
    Image {
        /// The operator that triggered the finding.
        token: String,
    },
    /// A coordinate transform that is not modelled.
    Transform {
        /// The operator that triggered the finding.
        token: String,
    },
    /// A curved path that is not approximated.
    Curve {
        /// The operator that triggered the finding.
        token: String,
    },
    /// A procedure body that is never executed.
    Procedure {
        /// The token that triggered the finding.
        token: String,
    },
    /// A path operation outside the straight-line/rectangle subset.
    ComplexPath {
        /// The operator that triggered the finding.
        token: String,
    },
    /// Font metrics that are not resolved.
    FontMetrics {
        /// The operator that triggered the finding.
        token: String,
    },
    /// A numeric operand that could not be parsed.
    InvalidNumber {
        /// The token that triggered the finding.
        token: String,
    },
    /// An operand stack underflow.
    StackUnderflow {
        /// The operator that triggered the finding.
        token: String,
    },
    /// An operator outside the documented subset.
    Operator {
        /// The operator that triggered the finding.
        token: String,
    },
    /// A finding kind added by the reader after this contract was written.
    Other {
        /// Stable machine-readable kind from the reader.
        kind: &'static str,
        /// The token that triggered the finding.
        token: String,
    },
}

impl PsUnsupportedConstruct {
    /// Convert a reader finding into its typed representation.
    #[must_use]
    pub fn from_finding(finding: &PsUnsupportedFinding) -> Self {
        match finding.kind {
            "binary-data" => Self::BinaryData {
                token: finding.token.clone(),
            },
            "image" => Self::Image {
                token: finding.token.clone(),
            },
            "transform" => Self::Transform {
                token: finding.token.clone(),
            },
            "curve" => Self::Curve {
                token: finding.token.clone(),
            },
            "procedure" => Self::Procedure {
                token: finding.token.clone(),
            },
            "complex-path" => Self::ComplexPath {
                token: finding.token.clone(),
            },
            "font-metrics" => Self::FontMetrics {
                token: finding.token.clone(),
            },
            "invalid-number" => Self::InvalidNumber {
                token: finding.token.clone(),
            },
            "stack-underflow" => Self::StackUnderflow {
                token: finding.token.clone(),
            },
            "operator" => Self::Operator {
                token: finding.token.clone(),
            },
            other => Self::Other {
                kind: other,
                token: finding.token.clone(),
            },
        }
    }

    /// Return a stable machine-readable kind label.
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::BinaryData { .. } => "binary-data",
            Self::Image { .. } => "image",
            Self::Transform { .. } => "transform",
            Self::Curve { .. } => "curve",
            Self::Procedure { .. } => "procedure",
            Self::ComplexPath { .. } => "complex-path",
            Self::FontMetrics { .. } => "font-metrics",
            Self::InvalidNumber { .. } => "invalid-number",
            Self::StackUnderflow { .. } => "stack-underflow",
            Self::Operator { .. } => "operator",
            Self::Other { kind, .. } => kind,
        }
    }

    /// Return the token that triggered the finding.
    #[must_use]
    pub fn token(&self) -> &str {
        match self {
            Self::BinaryData { token }
            | Self::Image { token }
            | Self::Transform { token }
            | Self::Curve { token }
            | Self::Procedure { token }
            | Self::ComplexPath { token }
            | Self::FontMetrics { token }
            | Self::InvalidNumber { token }
            | Self::StackUnderflow { token }
            | Self::Operator { token }
            | Self::Other { token, .. } => token,
        }
    }
}

/// Counts of the supported constructs extracted on one page.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PsContentCounts {
    text: usize,
    line: usize,
    rect: usize,
    image: usize,
}

impl PsContentCounts {
    /// Number of positioned text runs.
    #[must_use]
    pub const fn text(&self) -> usize {
        self.text
    }

    /// Number of stroked straight lines.
    #[must_use]
    pub const fn line(&self) -> usize {
        self.line
    }

    /// Number of axis-aligned rectangles.
    #[must_use]
    pub const fn rect(&self) -> usize {
        self.rect
    }

    /// Number of image references carried in the model (never embedded here).
    #[must_use]
    pub const fn image(&self) -> usize {
        self.image
    }
}

/// One planned page of an ingested PostScript document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PsPlannedPage {
    ordinal: usize,
    label: String,
    counts: PsContentCounts,
}

impl PsPlannedPage {
    /// One-based `%%Page` ordinal.
    #[must_use]
    pub const fn ordinal(&self) -> usize {
        self.ordinal
    }

    /// DSC page label.
    #[must_use]
    pub fn label(&self) -> &str {
        &self.label
    }

    /// Supported-construct counts for the page.
    #[must_use]
    pub const fn counts(&self) -> PsContentCounts {
        self.counts
    }
}

/// A deterministic assembly plan produced from PostScript input.
#[derive(Debug, Clone, PartialEq)]
pub struct PsAssemblyPlan {
    kind: PostScriptKind,
    media: PageSize,
    pages: Vec<PsPlannedPage>,
    unsupported: Vec<PsUnsupportedConstruct>,
    summary: String,
}

impl PsAssemblyPlan {
    /// Whether the reader identified EPS or plain PostScript.
    #[must_use]
    pub const fn kind(&self) -> PostScriptKind {
        self.kind
    }

    /// Media box assigned to each planned page.
    #[must_use]
    pub const fn media(&self) -> PageSize {
        self.media
    }

    /// Pages ordered by ascending DSC ordinal.
    #[must_use]
    pub fn pages(&self) -> &[PsPlannedPage] {
        &self.pages
    }

    /// Typed constructs the reader reported instead of interpreting.
    #[must_use]
    pub fn unsupported(&self) -> &[PsUnsupportedConstruct] {
        &self.unsupported
    }

    /// Stable, byte-for-byte deterministic report line for this plan.
    #[must_use]
    pub fn summary(&self) -> &str {
        &self.summary
    }
}

/// Options controlling the PostScript assembly plan.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PsAssemblyOptions {
    media: PageSize,
    strict: bool,
}

impl PsAssemblyOptions {
    /// Defaults: US Letter media and reporting (non-strict) mode.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            media: PageSize::LETTER,
            strict: false,
        }
    }

    /// Replace the media box assigned to planned pages.
    #[must_use]
    pub const fn with_media(mut self, media: PageSize) -> Self {
        self.media = media;
        self
    }

    /// Promote reported findings to [`PsIngestError::Unsupported`].
    #[must_use]
    pub const fn with_strict(mut self, strict: bool) -> Self {
        self.strict = strict;
        self
    }

    /// Return the media box assigned to planned pages.
    #[must_use]
    pub const fn media(&self) -> PageSize {
        self.media
    }

    /// Return whether reported findings are promoted to errors.
    #[must_use]
    pub const fn strict(&self) -> bool {
        self.strict
    }
}

impl Default for PsAssemblyOptions {
    fn default() -> Self {
        Self::new()
    }
}

/// Count the supported constructs extracted on one ingested page.
fn count_content(content: &[PsContent]) -> PsContentCounts {
    let mut counts = PsContentCounts::default();
    for item in content {
        match item {
            PsContent::Text(_) => counts.text = counts.text.saturating_add(1),
            PsContent::Line(_) => counts.line = counts.line.saturating_add(1),
            PsContent::Rect(_) => counts.rect = counts.rect.saturating_add(1),
            PsContent::Image(_) => counts.image = counts.image.saturating_add(1),
        }
    }
    counts
}

/// Build a deterministic page layout plan from PostScript bytes.
///
/// The bounded reader is the only parser used; its DSC headers, EPS detection,
/// token/byte budgets, and ordinal ordering are inherited unchanged. Supported
/// text and vector constructs are counted per page, and every reported finding
/// is surfaced as a typed [`PsUnsupportedConstruct`] instead of being guessed
/// at or silently dropped.
///
/// # Errors
///
/// Returns [`PsIngestError`] for empty, oversized, or malformed input and, in
/// strict mode, for the first reported unsupported construct.
pub fn build_ps_assembly_plan(
    input: &[u8],
    options: &PsAssemblyOptions,
) -> Result<PsAssemblyPlan, PsIngestError> {
    let ingested = ingest_postscript_with_options(
        input,
        PsIngestOptions {
            strict: options.strict(),
        },
    )?;
    let pages: Vec<PsPlannedPage> = ingested
        .pages
        .iter()
        .map(|page| PsPlannedPage {
            ordinal: page.ordinal,
            label: page.label.clone(),
            counts: count_content(&page.content),
        })
        .collect();
    let unsupported: Vec<PsUnsupportedConstruct> = ingested
        .report
        .unsupported
        .iter()
        .map(PsUnsupportedConstruct::from_finding)
        .collect();
    let fragments: Vec<String> = pages
        .iter()
        .map(|page| {
            format!(
                "{}:{}:t{}/l{}/r{}/i{}",
                page.ordinal(),
                page.label(),
                page.counts().text(),
                page.counts().line(),
                page.counts().rect(),
                page.counts().image()
            )
        })
        .collect();
    let kinds: Vec<&str> = unsupported
        .iter()
        .map(PsUnsupportedConstruct::kind)
        .collect();
    let summary = format!(
        "kind={} media={:.3}x{:.3} pages=[{}] unsupported=[{}]",
        ingested.report.kind.label(),
        options.media().width,
        options.media().height,
        fragments.join(", "),
        kinds.join(", ")
    );
    Ok(PsAssemblyPlan {
        kind: ingested.report.kind,
        media: options.media(),
        pages,
        unsupported,
        summary,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ps_input(header: &str, body: &str) -> String {
        format!("%!PS-Adobe-3.0\n{header}\n%%EndComments\n{body}\n%%Trailer\n%%EOF\n")
    }

    #[test]
    fn ps_assembly_plan_counts_supported_text_and_vector_content() {
        let body = "%%Page: p 1\n/Helvetica findfont 14 scalefont setfont\n\
                    10 20 moveto (Hello) show\n5 6 moveto 40 6 lineto stroke\n\
                    1 2 30 40 rectfill\n";
        let input = ps_input("%%Pages: 1", body);
        let plan = build_ps_assembly_plan(input.as_bytes(), &PsAssemblyOptions::new());
        assert!(plan.is_ok());
        let plan = plan.expect("plan");
        assert_eq!(plan.kind(), PostScriptKind::Ps);
        assert_eq!(plan.pages().len(), 1);
        let counts = plan.pages()[0].counts();
        assert_eq!(counts.text(), 1);
        assert_eq!(counts.line(), 1);
        assert_eq!(counts.rect(), 1);
        assert_eq!(counts.image(), 0);
        assert!(plan.unsupported().is_empty());
    }

    #[test]
    fn ps_assembly_plan_types_unsupported_constructs() {
        let input = ps_input("", "%%Page: p 1\n1 2 curveto\n");
        let plan = build_ps_assembly_plan(input.as_bytes(), &PsAssemblyOptions::new());
        assert!(plan.is_ok());
        let plan = plan.expect("plan");
        assert!(plan.unsupported().iter().any(|item| matches!(
            item,
            PsUnsupportedConstruct::Curve { token } if token == "curveto"
        )));
        assert!(
            plan.unsupported()
                .iter()
                .all(|item| !item.token().is_empty())
        );
    }

    #[test]
    fn ps_assembly_strict_mode_rejects_unsupported_constructs() {
        let input = ps_input("", "%%Page: p 1\n1 2 curveto\n");
        let result = build_ps_assembly_plan(
            input.as_bytes(),
            &PsAssemblyOptions::new().with_strict(true),
        );
        assert!(matches!(
            result,
            Err(PsIngestError::Unsupported { kind: "curve", .. })
        ));
    }

    #[test]
    fn ps_assembly_plan_is_byte_for_byte_deterministic() {
        let input = ps_input(
            "%%Pages: 2",
            "%%Page: b 2\n(B) show\n%%Page: a 1\n(A) show\n",
        );
        let first = build_ps_assembly_plan(input.as_bytes(), &PsAssemblyOptions::new());
        let second = build_ps_assembly_plan(input.as_bytes(), &PsAssemblyOptions::new());
        assert_eq!(first, second);
        let first = first.expect("first plan");
        let second = second.expect("second plan");
        assert_eq!(first.summary(), second.summary());
        assert_eq!(first.pages()[0].ordinal(), 1);
        assert_eq!(first.pages()[0].label(), "a");
    }

    #[test]
    fn ps_assembly_empty_and_malformed_input_are_typed_errors() {
        assert_eq!(
            build_ps_assembly_plan(b"", &PsAssemblyOptions::new()),
            Err(PsIngestError::EmptyInput)
        );
        assert_eq!(
            build_ps_assembly_plan(b"not postscript", &PsAssemblyOptions::new()),
            Err(PsIngestError::MissingHeader)
        );
    }
}
