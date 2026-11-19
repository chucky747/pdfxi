//! Automated accessibility and PDF/UA-oriented conformance checker.
//!
//! [`check`] examines a validated [`A11yTree`] together with document-level
//! tagged metadata ([`CheckOptions`]) and returns a deterministic [`Report`] of
//! [`Finding`]s. Every finding carries a stable [`Rule`] identifier, a
//! [`Severity`], and optional node/page context so callers can jump to the
//! offending content.
//!
//! The rule set approximates the structural requirements of PDF/UA-1
//! (ISO 14289-1): document title and language, tagged content, alternative
//! text for visual content, heading hierarchy, list/table structure, form
//! field labels, reading-order continuity, non-empty links, and valid role
//! maps. **It is a heuristic self-check, not a certification.** Passing does
//! not claim conformance, and failing does not prove non-conformance; a full
//! audit still requires a qualified tool and human review.
//!
//! All traversals are iterative (no recursion), so arbitrarily deep trees are
//! safe. The module forbids unsafe code and uses no `unwrap`/`expect`/`panic`
//! outside tests. [`suggest_fixups`] returns human-readable suggestions and
//! never mutates the tree.

use core::fmt;
use std::collections::{HashMap, HashSet};

use crate::{
    A11yNode, A11yTree, NodeId, TagRole, TreeError, is_standard_pdf_role, role_map, role_pdf_name,
};

/// Stable identifier of a conformance rule.
///
/// The `UA-nnnn` strings are part of the public contract: report consumers and
/// tests may rely on them. New rules are appended, never renumbered.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Rule {
    /// The document information dictionary has no non-empty title.
    MissingTitle,
    /// The catalog has no non-empty natural-language (`/Lang`) entry.
    MissingLanguage,
    /// The document declares no tagged content, or the tree has no content.
    UntaggedContent,
    /// A figure or image has no meaningful alternative text.
    MissingAltText,
    /// A heading jumps more than one level, or starts above level one.
    HeadingLevelSkip,
    /// A list has no items, or a list item/body sits outside its parent role.
    ListStructure,
    /// A table has no rows or header cells, or a cell sits outside a row.
    TableStructure,
    /// A form field has no accessible label (text or alternative text).
    FormFieldLabel,
    /// Reading order moves backwards across pages or references an invalid page.
    ReadingOrder,
    /// A link has neither visible text nor alternative text.
    EmptyLink,
    /// A declared structure role is neither standard nor present in the role map.
    UnknownRole,
    /// A role name is mapped more than once in the role map.
    DuplicateRole,
}

impl Rule {
    /// Returns the stable `UA-nnnn` identifier of this rule.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::MissingTitle => "UA-0001",
            Self::MissingLanguage => "UA-0002",
            Self::UntaggedContent => "UA-0003",
            Self::MissingAltText => "UA-0004",
            Self::HeadingLevelSkip => "UA-0005",
            Self::ListStructure => "UA-0006",
            Self::TableStructure => "UA-0007",
            Self::FormFieldLabel => "UA-0008",
            Self::ReadingOrder => "UA-0009",
            Self::EmptyLink => "UA-0010",
            Self::UnknownRole => "UA-0011",
            Self::DuplicateRole => "UA-0012",
        }
    }
}

impl fmt::Display for Rule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Severity of a [`Finding`].
///
/// The derived ordering places [`Severity::Error`] before
/// [`Severity::Warning`] before [`Severity::Info`], matching report sort order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    /// A structural requirement that PDF/UA treats as mandatory.
    Error,
    /// A likely conformance problem that may be acceptable with justification.
    Warning,
    /// Informational context that does not affect the pass/fail summary.
    Info,
}

impl Severity {
    /// Returns the lowercase stable name of this severity.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warning => "warning",
            Self::Info => "info",
        }
    }

    /// Numeric rank used for deterministic ordering.
    const fn rank(self) -> u8 {
        self as u8
    }
}

impl fmt::Display for Severity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A single conformance finding.
///
/// Document-wide findings leave both [`Self::node`] and [`Self::page`] empty;
/// content-specific findings fill in the node identifier and its page index.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Finding {
    /// Stable rule identifier that produced this finding.
    pub rule: Rule,
    /// Severity of the finding.
    pub severity: Severity,
    /// Human-readable explanation of the problem.
    pub message: String,
    /// Offending accessibility node, when the problem is node-specific.
    pub node: Option<NodeId>,
    /// Zero-based page index of the offending node, when known.
    pub page: Option<usize>,
}

impl Finding {
    fn document(rule: Rule, severity: Severity, message: impl Into<String>) -> Self {
        Self {
            rule,
            severity,
            message: message.into(),
            node: None,
            page: None,
        }
    }

    fn at_node(
        rule: Rule,
        severity: Severity,
        message: impl Into<String>,
        node: &A11yNode,
    ) -> Self {
        Self {
            rule,
            severity,
            message: message.into(),
            node: Some(node.id),
            page: Some(node.page),
        }
    }

    /// Total ordering key: rule, severity, page, node, then message.
    fn sort_key(&self) -> (&'static str, u8, u64, u64, &str) {
        (
            self.rule.as_str(),
            self.severity.rank(),
            self.page.map_or(u64::MAX, |page| page as u64),
            self.node.map_or(u64::MAX, NodeId::get),
            self.message.as_str(),
        )
    }
}

/// Counts of findings by severity plus a heuristic pass/fail verdict.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Summary {
    /// Number of error-severity findings.
    pub errors: usize,
    /// Number of warning-severity findings.
    pub warnings: usize,
    /// Number of informational findings.
    pub info: usize,
    /// Total number of findings.
    pub total: usize,
    /// `true` when there are no error-severity findings.
    ///
    /// This is a heuristic approximation of PDF/UA readiness, **not** a
    /// certification or a guarantee of conformance.
    pub passed: bool,
}

/// A deterministic, sorted collection of findings and its summary.
#[derive(Clone, Debug, PartialEq)]
pub struct Report {
    findings: Vec<Finding>,
    summary: Summary,
}

impl Report {
    /// Builds a report from findings, sorting them into deterministic order.
    pub fn new(mut findings: Vec<Finding>) -> Self {
        findings.sort_by(|left, right| left.sort_key().cmp(&right.sort_key()));
        let errors = findings
            .iter()
            .filter(|finding| finding.severity == Severity::Error)
            .count();
        let warnings = findings
            .iter()
            .filter(|finding| finding.severity == Severity::Warning)
            .count();
        let info = findings
            .iter()
            .filter(|finding| finding.severity == Severity::Info)
            .count();
        let total = findings.len();
        Self {
            findings,
            summary: Summary {
                errors,
                warnings,
                info,
                total,
                passed: errors == 0,
            },
        }
    }

    /// Findings in deterministic order.
    pub fn findings(&self) -> &[Finding] {
        &self.findings
    }

    /// Severity counts and heuristic verdict.
    pub fn summary(&self) -> Summary {
        self.summary
    }

    /// Returns `true` when no error-severity findings were produced.
    pub fn is_pass(&self) -> bool {
        self.summary.passed
    }

    /// Returns only the findings for `rule`, in report order.
    pub fn findings_for_rule(&self, rule: Rule) -> impl Iterator<Item = &Finding> {
        self.findings
            .iter()
            .filter(move |finding| finding.rule == rule)
    }
}

/// Document-level metadata the checker needs in addition to the tag tree.
///
/// Callers populate this from the document information dictionary, the catalog
/// `/Lang` entry, the `/MarkInfo` declaration, and the tagged structure's role
/// map. An empty [`Self::declared_role_names`] means "derive the declared role
/// names from the tree"; an empty [`Self::role_map`] means "use the role map the
/// structure writer would emit for this tree".
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CheckOptions {
    /// Document title from the information dictionary, when present.
    pub title: Option<String>,
    /// Natural-language tag from the catalog `/Lang` entry, when present.
    pub lang: Option<String>,
    /// Total number of pages in the document.
    pub page_count: usize,
    /// Whether the document declares tagged content (`/MarkInfo /Marked`).
    pub tagged: bool,
    /// Raw structure role names found in the tagged metadata.
    pub declared_role_names: Vec<String>,
    /// Custom role map as `(role name, standard ancestor)` pairs.
    pub role_map: Vec<(String, String)>,
}

impl Default for CheckOptions {
    fn default() -> Self {
        Self {
            title: None,
            lang: None,
            page_count: 0,
            tagged: true,
            declared_role_names: Vec::new(),
            role_map: Vec::new(),
        }
    }
}

impl CheckOptions {
    /// Creates options for a document with `page_count` pages.
    pub fn new(title: Option<&str>, lang: Option<&str>, page_count: usize) -> Self {
        Self {
            title: title.map(str::to_string),
            lang: lang.map(str::to_string),
            page_count,
            ..Self::default()
        }
    }

    /// Rejects metadata that is structurally inconsistent.
    ///
    /// A supplied title or language must be non-blank; omitting the field is how
    /// a caller reports a missing title/language and produces a finding instead.
    pub fn validate(&self) -> Result<(), CheckError> {
        if self
            .title
            .as_deref()
            .is_some_and(|title| title.trim().is_empty())
        {
            return Err(CheckError::InvalidOptions(
                "document title must not be blank when supplied".to_string(),
            ));
        }
        if self
            .lang
            .as_deref()
            .is_some_and(|lang| lang.trim().is_empty())
        {
            return Err(CheckError::InvalidOptions(
                "document language must not be blank when supplied".to_string(),
            ));
        }
        Ok(())
    }
}

/// Typed errors produced while validating checker inputs.
#[derive(Debug, Clone, PartialEq)]
pub enum CheckError {
    /// The supplied tree failed its structural validation.
    InvalidTree(TreeError),
    /// The supplied document metadata was inconsistent.
    InvalidOptions(String),
}

impl fmt::Display for CheckError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidTree(error) => write!(f, "invalid accessibility tree: {error}"),
            Self::InvalidOptions(message) => write!(f, "invalid checker options: {message}"),
        }
    }
}

impl std::error::Error for CheckError {}

/// A non-mutating suggestion for how to resolve a finding.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FixupSuggestion {
    /// Rule whose finding prompted this suggestion.
    pub rule: Rule,
    /// Node to edit, when the fix is node-specific.
    pub node: Option<NodeId>,
    /// Zero-based page index of the node, when known.
    pub page: Option<usize>,
    /// Human-readable description of the suggested edit.
    pub description: String,
}

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

fn requires_alt_text(role: TagRole) -> bool {
    matches!(role, TagRole::Figure | TagRole::Image)
}

fn has_text_or_alt(node: &A11yNode) -> bool {
    !node.text.trim().is_empty()
        || node
            .alt_text
            .as_ref()
            .is_some_and(|alt| !alt.trim().is_empty())
}

fn contributes_tagged_content(node: &A11yNode) -> bool {
    !matches!(node.role, TagRole::Document | TagRole::Page) || has_text_or_alt(node)
}

fn parent_map(tree: &A11yTree) -> HashMap<NodeId, NodeId> {
    let mut parents = HashMap::with_capacity(tree.len());
    for node in tree.nodes() {
        for &child in &node.children {
            parents.insert(child, node.id);
        }
    }
    parents
}

fn parent_has_role(
    tree: &A11yTree,
    parents: &HashMap<NodeId, NodeId>,
    node: &A11yNode,
    expected: TagRole,
) -> bool {
    parents
        .get(&node.id)
        .is_some_and(|parent| tree.node(*parent).is_some_and(|p| p.role == expected))
}

fn effective_role_map(tree: &A11yTree, options: &CheckOptions) -> HashMap<String, String> {
    let mut effective = HashMap::new();
    for (name, value) in role_map(tree) {
        if let Some(target) = value.as_name() {
            effective.insert(name, target.to_string());
        }
    }
    for (name, target) in &options.role_map {
        effective.insert(name.clone(), target.clone());
    }
    effective
}

/// Runs the accessibility/PDF-UA checks over `tree` and `options`.
///
/// Returns a typed [`CheckError`] only when the inputs themselves are invalid;
/// conformance problems are reported as findings in the returned [`Report`].
/// The traversal is iterative, so deep trees cannot overflow the stack.
pub fn check(tree: &A11yTree, options: &CheckOptions) -> Result<Report, CheckError> {
    tree.validate().map_err(CheckError::InvalidTree)?;
    options.validate()?;

    let mut findings = Vec::new();

    if options
        .title
        .as_deref()
        .is_none_or(|title| title.trim().is_empty())
    {
        findings.push(Finding::document(
            Rule::MissingTitle,
            Severity::Error,
            "The document has no title; PDF/UA requires a title in the document information dictionary.",
        ));
    }
    if options
        .lang
        .as_deref()
        .is_none_or(|lang| lang.trim().is_empty())
    {
        findings.push(Finding::document(
            Rule::MissingLanguage,
            Severity::Error,
            "The document has no /Lang entry; assistive technology needs a natural language tag.",
        ));
    }

    let traversal = tree.traversal();
    let has_tagged_content = options.tagged
        && traversal
            .iter()
            .filter_map(|id| tree.node(*id))
            .any(contributes_tagged_content);
    if !has_tagged_content {
        findings.push(Finding::document(
            Rule::UntaggedContent,
            Severity::Error,
            "No tagged content was found; the document is not marked as tagged or the tag tree is empty.",
        ));
    }

    let parents = parent_map(tree);

    for node in tree.nodes() {
        if requires_alt_text(node.role)
            && node
                .alt_text
                .as_ref()
                .is_none_or(|alt| alt.trim().is_empty())
        {
            findings.push(Finding::at_node(
                Rule::MissingAltText,
                Severity::Warning,
                format!(
                    "{} node {} has no alternative text.",
                    node.role.as_str(),
                    node.id
                ),
                node,
            ));
        }
        if node.role == TagRole::Link && !has_text_or_alt(node) {
            findings.push(Finding::at_node(
                Rule::EmptyLink,
                Severity::Warning,
                format!(
                    "Link node {} has neither text nor alternative text.",
                    node.id
                ),
                node,
            ));
        }
        if node.role == TagRole::FormField && !has_text_or_alt(node) {
            findings.push(Finding::at_node(
                Rule::FormFieldLabel,
                Severity::Warning,
                format!("Form field node {} has no accessible label.", node.id),
                node,
            ));
        }
        if node.role == TagRole::List {
            let has_item = node
                .children
                .iter()
                .filter_map(|id| tree.node(*id))
                .any(|child| child.role == TagRole::ListItem);
            if !has_item {
                findings.push(Finding::at_node(
                    Rule::ListStructure,
                    Severity::Warning,
                    format!("List node {} has no list items.", node.id),
                    node,
                ));
            }
        }
        if node.role == TagRole::ListItem && !parent_has_role(tree, &parents, node, TagRole::List) {
            findings.push(Finding::at_node(
                Rule::ListStructure,
                Severity::Warning,
                format!("List item node {} is not inside a list.", node.id),
                node,
            ));
        }
        if node.role == TagRole::ListBody
            && !parent_has_role(tree, &parents, node, TagRole::ListItem)
        {
            findings.push(Finding::at_node(
                Rule::ListStructure,
                Severity::Warning,
                format!("List body node {} is not inside a list item.", node.id),
                node,
            ));
        }
        if node.role == TagRole::Table {
            let rows: Vec<&A11yNode> = node
                .children
                .iter()
                .filter_map(|id| tree.node(*id))
                .filter(|child| child.role == TagRole::TableRow)
                .collect();
            if rows.is_empty() {
                findings.push(Finding::at_node(
                    Rule::TableStructure,
                    Severity::Warning,
                    format!("Table node {} has no rows.", node.id),
                    node,
                ));
            } else {
                let has_header = rows.iter().any(|row| {
                    row.children
                        .iter()
                        .filter_map(|id| tree.node(*id))
                        .any(|cell| cell.role == TagRole::TableHeader)
                });
                if !has_header {
                    findings.push(Finding::at_node(
                        Rule::TableStructure,
                        Severity::Warning,
                        format!("Table node {} has no header cells.", node.id),
                        node,
                    ));
                }
            }
        }
        if node.role == TagRole::TableRow && !parent_has_role(tree, &parents, node, TagRole::Table)
        {
            findings.push(Finding::at_node(
                Rule::TableStructure,
                Severity::Warning,
                format!("Table row node {} is not inside a table.", node.id),
                node,
            ));
        }
        if matches!(node.role, TagRole::TableHeader | TagRole::TableData)
            && !parent_has_role(tree, &parents, node, TagRole::TableRow)
        {
            findings.push(Finding::at_node(
                Rule::TableStructure,
                Severity::Warning,
                format!("Table cell node {} is not inside a table row.", node.id),
                node,
            ));
        }
    }

    let mut last_heading: Option<u8> = None;
    let mut last_page: Option<usize> = None;
    for id in &traversal {
        let Some(node) = tree.node(*id) else {
            continue;
        };
        if let Some(level) = heading_level(node.role) {
            let skipped = match last_heading {
                Some(previous) => level > previous.saturating_add(1),
                None => level > 1,
            };
            if skipped {
                findings.push(Finding::at_node(
                    Rule::HeadingLevelSkip,
                    Severity::Warning,
                    match last_heading {
                        Some(previous) => format!(
                            "Heading jumps from level {previous} to {level}; a level is missing."
                        ),
                        None => format!("First heading is level {level}; expected level 1."),
                    },
                    node,
                ));
            }
            last_heading = Some(level);
        }
        if let Some(previous) = last_page
            && node.page < previous
        {
            findings.push(Finding::at_node(
                Rule::ReadingOrder,
                Severity::Warning,
                format!(
                    "Reading order moves backwards from page {} to {} at node {}.",
                    previous + 1,
                    node.page + 1,
                    node.id
                ),
                node,
            ));
        }
        if options.page_count > 0 && node.page >= options.page_count {
            findings.push(Finding::at_node(
                Rule::ReadingOrder,
                Severity::Warning,
                format!(
                    "Node {} references page {} outside the document ({} pages).",
                    node.id,
                    node.page + 1,
                    options.page_count
                ),
                node,
            ));
        }
        last_page = Some(node.page);
    }

    let effective = effective_role_map(tree, options);
    let mut seen_mappings: HashSet<String> = HashSet::new();
    for (name, _) in &options.role_map {
        if !seen_mappings.insert(name.clone()) {
            findings.push(Finding::document(
                Rule::DuplicateRole,
                Severity::Warning,
                format!("Role /{name} is mapped more than once in the role map."),
            ));
        }
    }

    let mut declared: Vec<String> = if options.declared_role_names.is_empty() {
        tree.nodes()
            .iter()
            .map(|node| role_pdf_name(node.role).to_string())
            .collect()
    } else {
        options.declared_role_names.clone()
    };
    declared.sort();
    declared.dedup();
    for name in declared {
        if !is_standard_pdf_role(&name) && !effective.contains_key(&name) {
            findings.push(Finding::document(
                Rule::UnknownRole,
                Severity::Warning,
                format!(
                    "Structure role /{name} is neither a standard role nor declared in the role map."
                ),
            ));
        }
    }

    Ok(Report::new(findings))
}

/// Produces non-mutating fixup suggestions for the actionable rules.
///
/// Only [`Rule::MissingAltText`], [`Rule::MissingLanguage`],
/// [`Rule::HeadingLevelSkip`], and [`Rule::EmptyLink`] yield suggestions. The
/// returned order follows the report's deterministic finding order, and neither
/// `report` nor `tree` is modified.
pub fn suggest_fixups(report: &Report, tree: &A11yTree) -> Vec<FixupSuggestion> {
    let mut suggestions = Vec::new();
    for finding in report.findings() {
        let description = match finding.rule {
            Rule::MissingAltText => Some(
                "Add a concise /Alt entry describing the figure or image for assistive technology."
                    .to_string(),
            ),
            Rule::MissingLanguage => Some(
                "Set the catalog /Lang entry to a valid language tag such as 'en-US'.".to_string(),
            ),
            Rule::HeadingLevelSkip => {
                let target = finding
                    .node
                    .and_then(|id| tree.node(id))
                    .and_then(|node| heading_level(node.role))
                    .unwrap_or(2);
                Some(format!(
                    "Insert a level {} heading before this heading so levels do not skip.",
                    target.saturating_sub(1).max(1)
                ))
            }
            Rule::EmptyLink => Some(
                "Give the link visible text or an /Alt entry so its destination is announced."
                    .to_string(),
            ),
            _ => None,
        };
        if let Some(description) = description {
            suggestions.push(FixupSuggestion {
                rule: finding.rule,
                node: finding.node,
                page: finding.page,
                description,
            });
        }
    }
    suggestions
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::BBox;

    fn box_at() -> BBox {
        BBox::new(0.0, 0.0, 10.0, 10.0).expect("test box must be valid")
    }

    fn node(id: u64, role: TagRole, page: usize, text: &str, children: Vec<NodeId>) -> A11yNode {
        A11yNode::new(NodeId::new(id), role, page, box_at(), text, children)
    }

    fn options() -> CheckOptions {
        CheckOptions::new(Some("Sample document"), Some("en-US"), 1)
    }

    fn clean_tree() -> A11yTree {
        A11yTree::new(
            vec![
                node(1, TagRole::Document, 0, "", vec![NodeId::new(2)]),
                node(
                    2,
                    TagRole::Page,
                    0,
                    "",
                    vec![
                        NodeId::new(3),
                        NodeId::new(4),
                        NodeId::new(5),
                        NodeId::new(6),
                        NodeId::new(8),
                        NodeId::new(12),
                        NodeId::new(13),
                    ],
                ),
                node(3, TagRole::Heading1, 0, "Introduction", vec![]),
                node(4, TagRole::Heading2, 0, "Details", vec![]),
                node(5, TagRole::Figure, 0, "", vec![]).with_alt_text("A chart"),
                node(6, TagRole::List, 0, "", vec![NodeId::new(7)]),
                node(7, TagRole::ListItem, 0, "First", vec![]),
                node(8, TagRole::Table, 0, "", vec![NodeId::new(9)]),
                node(
                    9,
                    TagRole::TableRow,
                    0,
                    "",
                    vec![NodeId::new(10), NodeId::new(11)],
                ),
                node(10, TagRole::TableHeader, 0, "Name", vec![]),
                node(11, TagRole::TableData, 0, "Ada", vec![]),
                node(12, TagRole::FormField, 0, "Email", vec![]),
                node(13, TagRole::Link, 0, "Read more", vec![]),
            ],
            vec![NodeId::new(1)],
        )
        .expect("clean tree must be valid")
    }

    fn single_page_tree(child: A11yNode) -> A11yTree {
        let child_id = child.id;
        A11yTree::new(
            vec![
                node(1, TagRole::Document, 0, "", vec![NodeId::new(2)]),
                node(2, TagRole::Page, 0, "", vec![child_id]),
                child,
            ],
            vec![NodeId::new(1)],
        )
        .expect("test tree must be valid")
    }

    #[test]
    fn clean_document_passes() {
        let report = check(&clean_tree(), &options()).expect("check succeeds");
        assert!(report.findings().is_empty(), "{:?}", report.findings());
        assert!(report.is_pass());
        assert_eq!(
            report.summary(),
            Summary {
                errors: 0,
                warnings: 0,
                info: 0,
                total: 0,
                passed: true,
            }
        );
    }

    #[test]
    fn missing_language_is_an_error() {
        let mut opts = options();
        opts.lang = None;
        let report = check(&clean_tree(), &opts).expect("check succeeds");
        let found: Vec<Rule> = report.findings().iter().map(|f| f.rule).collect();
        assert_eq!(found, vec![Rule::MissingLanguage]);
        assert_eq!(report.findings()[0].severity, Severity::Error);
        assert!(!report.is_pass());
    }

    #[test]
    fn missing_title_is_an_error() {
        let mut opts = options();
        opts.title = None;
        let report = check(&clean_tree(), &opts).expect("check succeeds");
        assert_eq!(report.findings()[0].rule, Rule::MissingTitle);
        assert_eq!(report.findings()[0].severity, Severity::Error);
    }

    #[test]
    fn missing_alt_text_is_a_warning() {
        let tree = single_page_tree(node(3, TagRole::Figure, 0, "", vec![]));
        let report = check(&tree, &options()).expect("check succeeds");
        assert_eq!(report.findings().len(), 1);
        assert_eq!(report.findings()[0].rule, Rule::MissingAltText);
        assert_eq!(report.findings()[0].severity, Severity::Warning);
        assert_eq!(report.findings()[0].node, Some(NodeId::new(3)));
        assert_eq!(report.findings()[0].page, Some(0));
        assert!(
            report.is_pass(),
            "warnings do not fail the heuristic verdict"
        );
    }

    #[test]
    fn heading_level_skip_is_flagged() {
        let tree = A11yTree::new(
            vec![
                node(1, TagRole::Document, 0, "", vec![NodeId::new(2)]),
                node(
                    2,
                    TagRole::Page,
                    0,
                    "",
                    vec![NodeId::new(3), NodeId::new(4)],
                ),
                node(3, TagRole::Heading1, 0, "One", vec![]),
                node(4, TagRole::Heading3, 0, "Three", vec![]),
            ],
            vec![NodeId::new(1)],
        )
        .expect("valid tree");
        let report = check(&tree, &options()).expect("check succeeds");
        let skips: Vec<&Finding> = report.findings_for_rule(Rule::HeadingLevelSkip).collect();
        assert_eq!(skips.len(), 1);
        assert_eq!(skips[0].node, Some(NodeId::new(4)));
    }

    #[test]
    fn empty_link_is_flagged() {
        let tree = single_page_tree(node(3, TagRole::Link, 0, "   ", vec![]));
        let report = check(&tree, &options()).expect("check succeeds");
        let links: Vec<Rule> = report.findings().iter().map(|f| f.rule).collect();
        assert_eq!(links, vec![Rule::EmptyLink]);
    }

    #[test]
    fn untagged_content_is_an_error() {
        let mut opts = options();
        opts.tagged = false;
        let report = check(&clean_tree(), &opts).expect("check succeeds");
        assert!(
            report
                .findings()
                .iter()
                .any(|f| f.rule == Rule::UntaggedContent && f.severity == Severity::Error)
        );

        let empty = A11yTree::empty();
        let report = check(&empty, &options()).expect("check succeeds");
        assert_eq!(report.findings()[0].rule, Rule::UntaggedContent);
    }

    #[test]
    fn list_structure_issues_are_flagged() {
        let tree = A11yTree::new(
            vec![
                node(1, TagRole::Document, 0, "", vec![NodeId::new(2)]),
                node(
                    2,
                    TagRole::Page,
                    0,
                    "",
                    vec![NodeId::new(3), NodeId::new(4)],
                ),
                node(3, TagRole::List, 0, "", vec![]),
                node(4, TagRole::ListItem, 0, "orphan", vec![]),
            ],
            vec![NodeId::new(1)],
        )
        .expect("valid tree");
        let report = check(&tree, &options()).expect("check succeeds");
        assert_eq!(report.findings_for_rule(Rule::ListStructure).count(), 2);
    }

    #[test]
    fn table_structure_issues_are_flagged() {
        let tree = A11yTree::new(
            vec![
                node(1, TagRole::Document, 0, "", vec![NodeId::new(2)]),
                node(
                    2,
                    TagRole::Page,
                    0,
                    "",
                    vec![NodeId::new(3), NodeId::new(4)],
                ),
                node(3, TagRole::Table, 0, "", vec![]),
                node(4, TagRole::TableData, 0, "orphan", vec![]),
            ],
            vec![NodeId::new(1)],
        )
        .expect("valid tree");
        let report = check(&tree, &options()).expect("check succeeds");
        assert_eq!(report.findings_for_rule(Rule::TableStructure).count(), 2);
    }

    #[test]
    fn form_field_label_is_flagged() {
        let tree = single_page_tree(node(3, TagRole::FormField, 0, "", vec![]));
        let report = check(&tree, &options()).expect("check succeeds");
        assert_eq!(report.findings_for_rule(Rule::FormFieldLabel).count(), 1);
    }

    #[test]
    fn reading_order_regression_is_flagged() {
        let tree = A11yTree::new(
            vec![
                node(
                    1,
                    TagRole::Document,
                    0,
                    "",
                    vec![NodeId::new(2), NodeId::new(3)],
                ),
                node(2, TagRole::Page, 1, "", vec![]),
                node(3, TagRole::Page, 0, "", vec![]),
            ],
            vec![NodeId::new(1)],
        )
        .expect("valid tree");
        let mut opts = options();
        opts.page_count = 2;
        let report = check(&tree, &opts).expect("check succeeds");
        assert_eq!(report.findings_for_rule(Rule::ReadingOrder).count(), 1);
    }

    #[test]
    fn unknown_and_duplicate_roles_are_flagged() {
        let mut opts = options();
        opts.declared_role_names = vec!["Bogus".to_string()];
        opts.role_map = vec![
            ("Foo".to_string(), "Div".to_string()),
            ("Foo".to_string(), "Figure".to_string()),
        ];
        let report = check(&clean_tree(), &opts).expect("check succeeds");
        assert_eq!(report.findings_for_rule(Rule::UnknownRole).count(), 1);
        assert_eq!(report.findings_for_rule(Rule::DuplicateRole).count(), 1);
    }

    #[test]
    fn findings_are_deterministic_and_rule_ids_are_stable() {
        let tree = A11yTree::new(
            vec![
                node(1, TagRole::Document, 0, "", vec![NodeId::new(2)]),
                node(
                    2,
                    TagRole::Page,
                    0,
                    "",
                    vec![NodeId::new(3), NodeId::new(4), NodeId::new(5)],
                ),
                node(3, TagRole::Heading1, 0, "One", vec![]),
                node(4, TagRole::Heading3, 0, "Three", vec![]),
                node(5, TagRole::Figure, 0, "", vec![]),
            ],
            vec![NodeId::new(1)],
        )
        .expect("valid tree");
        let mut opts = options();
        opts.lang = None;
        let first = check(&tree, &opts).expect("check succeeds");
        let second = check(&tree, &opts).expect("check succeeds");
        assert_eq!(first, second);

        let ids: Vec<&str> = first
            .findings()
            .iter()
            .map(|finding| finding.rule.as_str())
            .collect();
        let mut sorted = ids.clone();
        sorted.sort_unstable();
        assert_eq!(ids, sorted);
        assert_eq!(ids, vec!["UA-0002", "UA-0004", "UA-0005"]);
    }

    #[test]
    fn fixup_suggestions_cover_the_actionable_rules() {
        let tree = A11yTree::new(
            vec![
                node(1, TagRole::Document, 0, "", vec![NodeId::new(2)]),
                node(
                    2,
                    TagRole::Page,
                    0,
                    "",
                    vec![NodeId::new(3), NodeId::new(4), NodeId::new(5)],
                ),
                node(3, TagRole::Image, 0, "", vec![]),
                node(4, TagRole::Heading3, 0, "Three", vec![]),
                node(5, TagRole::Link, 0, "", vec![]),
            ],
            vec![NodeId::new(1)],
        )
        .expect("valid tree");
        let mut opts = options();
        opts.lang = None;
        let report = check(&tree, &opts).expect("check succeeds");
        let before = report.clone();
        let suggestions = suggest_fixups(&report, &tree);
        assert_eq!(
            suggestions.iter().map(|s| s.rule).collect::<Vec<_>>(),
            vec![
                Rule::MissingLanguage,
                Rule::MissingAltText,
                Rule::HeadingLevelSkip,
                Rule::EmptyLink,
            ]
        );
        assert!(suggestions.iter().all(|s| !s.description.is_empty()));
        assert_eq!(report, before, "suggestions must not mutate the report");
        assert_eq!(
            tree.traversal().len(),
            5,
            "suggestions must not mutate the tree"
        );
    }

    #[test]
    fn deep_tree_is_safe_and_iterative() {
        const DEPTH: usize = 10_000;
        let bounds = box_at();
        let mut nodes: Vec<A11yNode> = Vec::with_capacity(DEPTH);
        for index in 0..DEPTH {
            let id = NodeId::new((index + 1) as u64);
            let children = if index + 1 < DEPTH {
                vec![NodeId::new((index + 2) as u64)]
            } else {
                Vec::new()
            };
            let role = if index == 0 {
                TagRole::Document
            } else {
                TagRole::Div
            };
            nodes.push(A11yNode::new(id, role, 0, bounds, String::new(), children));
        }
        let tree = A11yTree::new(nodes, vec![NodeId::new(1)]).expect("deep tree must be valid");
        let report = check(&tree, &options()).expect("check succeeds");
        assert!(report.findings().is_empty(), "{:?}", report.findings());
        assert!(report.is_pass());
    }

    #[test]
    fn typed_error_rejects_blank_metadata() {
        let mut opts = options();
        opts.lang = Some("   ".to_string());
        assert!(matches!(
            check(&clean_tree(), &opts),
            Err(CheckError::InvalidOptions(_))
        ));
        let mut opts = options();
        opts.title = Some(String::new());
        assert!(matches!(
            check(&clean_tree(), &opts),
            Err(CheckError::InvalidOptions(_))
        ));
    }

    #[test]
    fn severity_and_rule_names_are_stable() {
        assert_eq!(Severity::Error.as_str(), "error");
        assert_eq!(Severity::Warning.to_string(), "warning");
        assert_eq!(Severity::Info.as_str(), "info");
        assert_eq!(Rule::MissingTitle.as_str(), "UA-0001");
        assert_eq!(Rule::DuplicateRole.as_str(), "UA-0012");
        assert_eq!(Rule::MissingAltText.to_string(), "UA-0004");
    }
}
