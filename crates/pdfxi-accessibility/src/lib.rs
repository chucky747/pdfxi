//! Tag trees, reading order, alt text, PDF/UA checks, reflow, and assistive output.
//!
//! This crate owns the accessibility-side document model. It is
//! renderer-agnostic and depends only on the standard library. The
//! [`reading_order`] module provides a validated tag tree with acyclic
//! presentation order; [`tree`] adds geometry, text, and alternative text to a
//! validated [`A11yTree`]; [`builder`] derives such a tree from ordered page
//! text blocks; [`export`] serializes it to plain text with a stable
//! summary; [`structure`] writes a validated tree into tagged-PDF structure
//! objects (including `/ActualText`, `/E`, and `/Artifact` marked content),
//! projects it into deterministic object-level dictionary entries, and reads it
//! back for verification; and [`checker`] audits a tree
//! plus document metadata with a deterministic, non-certifying PDF/UA-oriented
//! rule set; [`editor`] adds copy-on-write tag-tree edits, reading-order edits,
//! alt-text/table models, and a bounded undo/redo journal; [`authoring`] layers
//! deterministic, replayable authoring plans and reports with a structure-tree
//! round-trip check and a validated reading-order export over those edits.

#![deny(missing_docs)]
#![forbid(unsafe_code)]

pub mod authoring;
pub mod builder;
pub mod checker;
pub mod editor;
pub mod export;
pub mod reading_order;
pub mod structure;
pub mod tree;

pub use authoring::{
    AuthoringError, AuthoringPlan, AuthoringReport, AuthoringStep, export_reading_order,
};
pub use builder::{
    HEADING1_RATIO, HEADING2_RATIO, HEADING3_RATIO, PARAGRAPH_GAP_MULTIPLIER, ReadingOrderBuilder,
    TextBlock,
};
pub use checker::{
    CheckError, CheckOptions, Finding, FixPlan, FixPlanError, FixPlanReport, FixStep,
    FixupSuggestion, Report, Rule, Severity, Summary, build_fix_plan, check, suggest_fixups,
};
pub use editor::{
    AltRequirement, AltState, CellKind, CoverageReport, DEFAULT_JOURNAL_LIMIT, EditError,
    HeaderScope, TableCell, TableError, TableModel, TableValidation, TagEditor, coverage_report,
    requires_alt_text,
};
pub use export::{AltTextCoverage, RoleCount, TreeSummary, summarize, to_plain_text};
pub use reading_order::{ReadingOrder, ReadingOrderError, ReadingOrderNode};
pub use structure::{
    ObjRef, PageTagInfo, PdfDict, PdfValue, StructDocument, StructEntryType, StructError,
    StructKid, StructOptions, StructureEntry, StructureProjection, ViewerPreferences,
    compose_page_content, is_standard_pdf_role, project_structure, read_structure_tree,
    role_forbids_expansion, role_map, role_pdf_name, validate_structure_semantics,
    write_structure_tree,
};
pub use tree::{A11yNode, A11yTree, BBox, NodeId, TreeError};

use core::fmt;

/// Semantic role of a tagged content node.
///
/// The variants mirror the standard PDF structure-tag roles used to give
/// assistive technology a meaningful outline of a document.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum TagRole {
    /// Root of the tag tree.
    Document,
    /// A single page of the document.
    Page,
    /// A major division of the document.
    Part,
    /// A self-contained artistic composition.
    Art,
    /// A section of a document.
    Sect,
    /// A generic division.
    Div,
    /// A paragraph.
    Paragraph,
    /// A span of inline content.
    Span,
    /// First-level heading.
    Heading1,
    /// Second-level heading.
    Heading2,
    /// Third-level heading.
    Heading3,
    /// Fourth-level heading.
    Heading4,
    /// Fifth-level heading.
    Heading5,
    /// Sixth-level heading.
    Heading6,
    /// A list.
    List,
    /// An item in a list.
    ListItem,
    /// The body of a list item.
    ListBody,
    /// A table.
    Table,
    /// A row in a table.
    TableRow,
    /// A header cell in a table.
    TableHeader,
    /// A data cell in a table.
    TableData,
    /// A figure.
    Figure,
    /// An image or graphic.
    Image,
    /// A form.
    Form,
    /// A form field.
    FormField,
    /// A hyperlink.
    Link,
    /// Decorative or non-content artifact.
    Artifact,
    /// A quotation.
    Quote,
    /// A note or footnote.
    Note,
    /// A bibliographic entry.
    BibEntry,
    /// A block of code.
    Code,
    /// A caption for a figure or table.
    Caption,
}

impl TagRole {
    /// Returns a stable lowercase name for diagnostics and serialization.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Document => "document",
            Self::Page => "page",
            Self::Part => "part",
            Self::Art => "art",
            Self::Sect => "sect",
            Self::Div => "div",
            Self::Paragraph => "paragraph",
            Self::Span => "span",
            Self::Heading1 => "heading1",
            Self::Heading2 => "heading2",
            Self::Heading3 => "heading3",
            Self::Heading4 => "heading4",
            Self::Heading5 => "heading5",
            Self::Heading6 => "heading6",
            Self::List => "list",
            Self::ListItem => "listitem",
            Self::ListBody => "listbody",
            Self::Table => "table",
            Self::TableRow => "tablerow",
            Self::TableHeader => "tableheader",
            Self::TableData => "tabledata",
            Self::Figure => "figure",
            Self::Image => "image",
            Self::Form => "form",
            Self::FormField => "formfield",
            Self::Link => "link",
            Self::Artifact => "artifact",
            Self::Quote => "quote",
            Self::Note => "note",
            Self::BibEntry => "bibentry",
            Self::Code => "code",
            Self::Caption => "caption",
        }
    }
}

impl fmt::Display for TagRole {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Return the implementation name for diagnostics.
pub fn engine_name() -> &'static str {
    "pdfxi-accessibility"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tag_role_has_stable_names() {
        assert_eq!(TagRole::Document.as_str(), "document");
        assert_eq!(TagRole::Heading1.to_string(), "heading1");
        assert_eq!(TagRole::TableData.as_str(), "tabledata");
        assert_eq!(TagRole::Page.as_str(), "page");
    }
}
