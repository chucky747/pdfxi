//! Accessible tagged-text export from a `pdfxi-accessibility` tree.
//!
//! This module consumes a validated [`A11yTree`](pdfxi_accessibility::A11yTree)
//! in reading order and emits two stable text representations:
//!
//! - a nested, indented **structured** outline using `@role` markers, and
//! - a flat **plain-text** variant using `[role]` markers.
//!
//! Both representations preserve document semantics (headings, paragraphs,
//! lists, tables, figures) and can include or omit alternative text. They are
//! pure functions of the tree and the [`TaggedTextOptions`], so structurally
//! identical trees always serialize to identical strings.
//!
//! No PDF parsing, rendering, external tool, or unsafe code is involved. The
//! accessibility crate is a std-only workspace dependency.

use crate::export_options::{
    ExportOptionError, ExportPageSelection, LineEnding, validate_heading_levels,
};
use pdfxi_accessibility::{A11yNode, A11yTree, NodeId, TagRole};
use std::collections::{HashMap, HashSet};

/// Stable structured markers for heading levels one through six.
const HEADING_TOKENS: [&str; 6] = ["h1", "h2", "h3", "h4", "h5", "h6"];

/// Options controlling a tagged-text export.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaggedTextOptions {
    pages: ExportPageSelection,
    include_alt_text: bool,
    max_heading_level: u8,
    line_ending: LineEnding,
}

impl Default for TaggedTextOptions {
    fn default() -> Self {
        Self {
            pages: ExportPageSelection::All,
            include_alt_text: true,
            max_heading_level: 6,
            line_ending: LineEnding::Lf,
        }
    }
}

impl TaggedTextOptions {
    /// Creates options with conservative defaults: all pages, alternative text
    /// included, all six heading levels, and [`LineEnding::Lf`].
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Replaces the page selection.
    #[must_use]
    pub fn with_pages(mut self, pages: ExportPageSelection) -> Self {
        self.pages = pages;
        self
    }

    /// Enables or disables emission of alternative text for visual nodes.
    #[must_use]
    pub fn with_alt_text(mut self, include: bool) -> Self {
        self.include_alt_text = include;
        self
    }

    /// Sets the deepest heading level emitted as a heading.
    ///
    /// Headings deeper than `level` are demoted to paragraphs in both output
    /// variants. Accepted values are `1..=6`.
    #[must_use]
    pub fn with_max_heading_level(mut self, level: u8) -> Self {
        self.max_heading_level = level;
        self
    }

    /// Replaces the separator written between output lines.
    #[must_use]
    pub fn with_line_ending(mut self, line_ending: LineEnding) -> Self {
        self.line_ending = line_ending;
        self
    }

    /// Returns the configured page selection.
    #[must_use]
    pub fn pages(&self) -> &ExportPageSelection {
        &self.pages
    }

    /// Returns whether alternative text is emitted.
    #[must_use]
    pub const fn include_alt_text(&self) -> bool {
        self.include_alt_text
    }

    /// Returns the maximum emitted heading level.
    #[must_use]
    pub const fn max_heading_level(&self) -> u8 {
        self.max_heading_level
    }

    /// Returns the configured line ending.
    #[must_use]
    pub const fn line_ending(&self) -> LineEnding {
        self.line_ending
    }

    /// Validates the options.
    ///
    /// # Errors
    ///
    /// Returns a typed [`ExportOptionError`] for an invalid page selection or a
    /// heading-level limit outside `1..=6`.
    pub fn validate(&self) -> Result<(), ExportOptionError> {
        self.pages.validate()?;
        validate_heading_levels(self.max_heading_level)
    }
}

/// The two text representations produced by a tagged export.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaggedExport {
    /// Nested, indented outline with `@role` markers.
    pub structured: String,
    /// Flat text with `[role]` markers and optional `[alt: ...]` suffixes.
    pub plain: String,
}

/// Returns the heading level of a role, if it is a heading.
fn heading_level(role: TagRole) -> Option<u8> {
    match role {
        TagRole::Heading1 => Some(1),
        TagRole::Heading2 => Some(2),
        TagRole::Heading3 => Some(3),
        TagRole::Heading4 => Some(4),
        TagRole::Heading5 => Some(5),
        TagRole::Heading6 => Some(6),
        _ => None,
    }
}

/// Returns the structured marker for a role under a heading-level limit.
fn structured_token(role: TagRole, max_heading_level: u8) -> &'static str {
    if let Some(level) = heading_level(role) {
        if level <= max_heading_level {
            // `level` is validated as 1..=6 before this function is called.
            return HEADING_TOKENS[usize::from(level - 1)];
        }
        return "p";
    }
    match role {
        TagRole::Paragraph | TagRole::ListBody => "p",
        TagRole::Span => "span",
        TagRole::List => "list",
        TagRole::ListItem => "li",
        TagRole::Table => "table",
        TagRole::TableRow => "row",
        TagRole::TableHeader => "th",
        TagRole::TableData => "td",
        TagRole::Figure => "figure",
        TagRole::Image => "image",
        TagRole::Link => "link",
        TagRole::Quote => "quote",
        TagRole::Note => "note",
        TagRole::Code => "code",
        TagRole::Caption => "caption",
        other => other.as_str(),
    }
}

/// Returns the plain-text marker for a role under a heading-level limit.
fn plain_token(role: TagRole, max_heading_level: u8) -> &'static str {
    if heading_level(role).is_some_and(|level| level > max_heading_level) {
        return "paragraph";
    }
    role.as_str()
}

/// Collapses all whitespace runs in `text` to single spaces.
fn flatten(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Returns node identifiers and their depth in depth-first reading order.
///
/// Uses an explicit stack rather than recursion, so arbitrarily deep trees are
/// safe. The tree is already validated, so every referenced node exists.
fn traversal_with_depth(tree: &A11yTree) -> Vec<(NodeId, usize)> {
    let index: HashMap<NodeId, &A11yNode> =
        tree.nodes().iter().map(|node| (node.id, node)).collect();
    let mut visited: HashSet<NodeId> = HashSet::with_capacity(tree.len());
    let mut stack: Vec<(NodeId, usize)> = Vec::new();
    for &root in tree.roots().iter().rev() {
        if visited.insert(root) {
            stack.push((root, 0));
        }
    }
    let mut ordered = Vec::with_capacity(tree.len());
    while let Some((id, depth)) = stack.pop() {
        ordered.push((id, depth));
        if let Some(node) = index.get(&id) {
            for &child in node.children.iter().rev() {
                if visited.insert(child) {
                    stack.push((child, depth + 1));
                }
            }
        }
    }
    ordered
}

/// Appends a flattened alternative-text suffix when it is available and enabled.
fn push_alt(out: &mut String, alt_text: &Option<String>, include: bool) {
    if include
        && let Some(alt) = alt_text
        && !alt.trim().is_empty()
    {
        out.push_str(" [alt: ");
        out.push_str(&flatten(alt));
        out.push(']');
    }
}

/// Exports an accessibility tree into structured and plain tagged text.
///
/// # Errors
///
/// Returns a typed [`ExportOptionError`] when the options are invalid. An empty
/// tree produces two empty strings. Nodes whose zero-based page is not selected
/// are omitted; output order always follows the tree's reading order.
pub fn export_tagged_text(
    tree: &A11yTree,
    options: &TaggedTextOptions,
) -> Result<TaggedExport, ExportOptionError> {
    options.validate()?;

    let mut structured_lines: Vec<String> = Vec::new();
    let mut plain_lines: Vec<String> = Vec::new();

    for (id, depth) in traversal_with_depth(tree) {
        // The tree is validated, so every id from `traversal_with_depth` exists.
        let Some(node) = tree.node(id) else {
            continue;
        };
        if !options.pages().contains(node.page) {
            continue;
        }

        let text = flatten(&node.text);
        let mut structured = String::new();
        structured.push_str(&"  ".repeat(depth));
        structured.push('@');
        structured.push_str(structured_token(node.role, options.max_heading_level()));
        if !text.is_empty() {
            structured.push(' ');
            structured.push_str(&text);
        }
        push_alt(&mut structured, &node.alt_text, options.include_alt_text());
        structured_lines.push(structured);

        let mut plain = String::from("[");
        plain.push_str(plain_token(node.role, options.max_heading_level()));
        plain.push(']');
        if !text.is_empty() {
            plain.push(' ');
            plain.push_str(&text);
        }
        push_alt(&mut plain, &node.alt_text, options.include_alt_text());
        plain_lines.push(plain);
    }

    Ok(TaggedExport {
        structured: options.line_ending().join(structured_lines),
        plain: options.line_ending().join(plain_lines),
    })
}

/// Exports only the nested structured representation.
///
/// # Errors
///
/// Returns a typed [`ExportOptionError`] when the options are invalid.
pub fn export_structured_text(
    tree: &A11yTree,
    options: &TaggedTextOptions,
) -> Result<String, ExportOptionError> {
    Ok(export_tagged_text(tree, options)?.structured)
}

/// Exports only the flat plain-text representation.
///
/// # Errors
///
/// Returns a typed [`ExportOptionError`] when the options are invalid.
pub fn export_plain_text(
    tree: &A11yTree,
    options: &TaggedTextOptions,
) -> Result<String, ExportOptionError> {
    Ok(export_tagged_text(tree, options)?.plain)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bounds() -> pdfxi_accessibility::BBox {
        pdfxi_accessibility::BBox::new(0.0, 0.0, 1.0, 1.0).expect("valid test bounds")
    }

    fn node(id: u64, role: TagRole, page: usize, text: &str, children: Vec<u64>) -> A11yNode {
        A11yNode::new(
            NodeId::new(id),
            role,
            page,
            bounds(),
            text,
            children.into_iter().map(NodeId::new).collect(),
        )
    }

    fn semantic_tree() -> A11yTree {
        A11yTree::new(
            vec![
                node(1, TagRole::Document, 0, "", vec![2, 3, 4, 7]),
                node(2, TagRole::Heading1, 0, "Title", vec![]),
                node(3, TagRole::Paragraph, 0, "Body text", vec![]),
                node(4, TagRole::List, 0, "", vec![5, 6]),
                node(5, TagRole::ListItem, 0, "First", vec![]),
                node(6, TagRole::ListItem, 0, "Second", vec![]),
                node(7, TagRole::Table, 0, "", vec![8]),
                node(8, TagRole::TableRow, 0, "", vec![9]),
                node(9, TagRole::TableHeader, 0, "Name", vec![]),
            ],
            vec![NodeId::new(1)],
        )
        .expect("valid semantic tree")
    }

    fn figure_tree() -> A11yTree {
        A11yTree::new(
            vec![
                node(1, TagRole::Document, 0, "", vec![2]),
                node(2, TagRole::Figure, 0, "", vec![]).with_alt_text("A bar chart"),
            ],
            vec![NodeId::new(1)],
        )
        .expect("valid figure tree")
    }

    #[test]
    fn tagged_tree_round_trip_is_stable_and_indented() {
        let tree = semantic_tree();
        let first = export_tagged_text(&tree, &TaggedTextOptions::new()).expect("export");
        assert_eq!(first.structured.lines().count(), 9);
        assert!(
            first.structured.starts_with("@document\n  @h1 Title"),
            "{first:?}"
        );
        assert!(
            first.structured.contains("  @list\n    @li First"),
            "{first:?}"
        );
        assert!(first.structured.contains("      @th Name"), "{first:?}");
        assert_eq!(
            export_tagged_text(&tree, &TaggedTextOptions::new()).expect("again"),
            first
        );
        assert_eq!(
            first.structured,
            export_structured_text(&tree, &TaggedTextOptions::new()).expect("structured")
        );
        assert_eq!(
            first.plain,
            export_plain_text(&tree, &TaggedTextOptions::new()).expect("plain")
        );
    }

    #[test]
    fn tagged_headings_lists_tables_and_figures_get_semantic_markers() {
        let out = export_tagged_text(&semantic_tree(), &TaggedTextOptions::new()).expect("export");
        assert!(out.plain.contains("[heading1] Title"), "{:?}", out.plain);
        assert!(
            out.plain.contains("[paragraph] Body text"),
            "{:?}",
            out.plain
        );
        assert!(
            out.plain.contains("[list]\n[listitem] First"),
            "{:?}",
            out.plain
        );
        assert!(
            out.plain
                .contains("[table]\n[tablerow]\n[tableheader] Name"),
            "{:?}",
            out.plain
        );

        let figure = export_tagged_text(&figure_tree(), &TaggedTextOptions::new()).expect("export");
        assert!(
            figure.structured.contains("@figure [alt: A bar chart]"),
            "{figure:?}"
        );
        assert!(
            figure.plain.contains("[figure] [alt: A bar chart]"),
            "{figure:?}"
        );
    }

    #[test]
    fn tagged_alt_text_toggle_removes_suffixes() {
        let with = export_tagged_text(&figure_tree(), &TaggedTextOptions::new()).expect("export");
        let without = export_tagged_text(
            &figure_tree(),
            &TaggedTextOptions::new().with_alt_text(false),
        )
        .expect("export");
        assert!(with.plain.contains("[alt: A bar chart]"));
        assert!(!without.plain.contains("alt:"), "{:?}", without.plain);
        assert!(
            !without.structured.contains("alt:"),
            "{:?}",
            without.structured
        );
    }

    #[test]
    fn tagged_heading_level_limit_demotes_deep_headings() {
        let tree = A11yTree::new(
            vec![
                node(1, TagRole::Document, 0, "", vec![2, 3]),
                node(2, TagRole::Heading1, 0, "Top", vec![]),
                node(3, TagRole::Heading4, 0, "Deep", vec![]),
            ],
            vec![NodeId::new(1)],
        )
        .expect("valid tree");
        let out = export_tagged_text(&tree, &TaggedTextOptions::new().with_max_heading_level(2))
            .expect("export");
        assert!(out.structured.contains("@h1 Top"), "{out:?}");
        assert!(out.structured.contains("@p Deep"), "{out:?}");
        assert!(out.plain.contains("[paragraph] Deep"), "{:?}", out.plain);
    }

    #[test]
    fn tagged_page_selection_filters_nodes() {
        let tree = A11yTree::new(
            vec![
                node(1, TagRole::Document, 0, "", vec![2, 3]),
                node(2, TagRole::Paragraph, 0, "first page", vec![]),
                node(3, TagRole::Paragraph, 1, "second page", vec![]),
            ],
            vec![NodeId::new(1)],
        )
        .expect("valid tree");
        let options = TaggedTextOptions::new().with_pages(ExportPageSelection::Pages(vec![1]));
        let out = export_tagged_text(&tree, &options).expect("export");
        assert!(out.plain.contains("second page"), "{:?}", out.plain);
        assert!(!out.plain.contains("first page"), "{:?}", out.plain);
        assert_eq!(out.plain.lines().count(), 1, "{:?}", out.plain);
    }

    #[test]
    fn tagged_line_ending_option_is_honoured() {
        let options = TaggedTextOptions::new().with_line_ending(LineEnding::CrLf);
        let out = export_tagged_text(&semantic_tree(), &options).expect("export");
        assert!(out.plain.contains("\r\n"), "{:?}", out.plain);
        assert!(!out.plain.contains('\n') || out.plain.contains("\r\n"));
        assert!(!out.structured.replace("\r\n", "").contains('\n'));
    }

    #[test]
    fn tagged_empty_tree_produces_empty_strings() {
        let out =
            export_tagged_text(&A11yTree::empty(), &TaggedTextOptions::new()).expect("export");
        assert!(out.structured.is_empty());
        assert!(out.plain.is_empty());
    }

    #[test]
    fn tagged_invalid_options_are_typed_errors() {
        let tree = semantic_tree();
        let bad_range =
            TaggedTextOptions::new().with_pages(ExportPageSelection::Range { first: 9, last: 1 });
        assert_eq!(
            export_tagged_text(&tree, &bad_range).err(),
            Some(ExportOptionError::InvalidPageRange { first: 9, last: 1 })
        );
        let bad_heading = TaggedTextOptions::new().with_max_heading_level(0);
        assert_eq!(
            export_tagged_text(&tree, &bad_heading).err(),
            Some(ExportOptionError::InvalidHeadingLevels { levels: 0 })
        );
        let empty_pages =
            TaggedTextOptions::new().with_pages(ExportPageSelection::Pages(Vec::new()));
        assert_eq!(
            export_tagged_text(&tree, &empty_pages).err(),
            Some(ExportOptionError::EmptyExportPageSelection)
        );
    }

    #[test]
    fn tagged_options_getters_round_trip() {
        let options = TaggedTextOptions::new()
            .with_pages(ExportPageSelection::Pages(vec![2, 0]))
            .with_alt_text(false)
            .with_max_heading_level(3)
            .with_line_ending(LineEnding::Cr);
        assert_eq!(options.pages(), &ExportPageSelection::Pages(vec![2, 0]));
        assert!(!options.include_alt_text());
        assert_eq!(options.max_heading_level(), 3);
        assert_eq!(options.line_ending(), LineEnding::Cr);
        assert!(options.validate().is_ok());
    }
}
