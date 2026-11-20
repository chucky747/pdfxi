//! Bounded tagged-PDF structure tree contract at library level.
//!
//! The accessibility lane owns the writer that embeds a structure tree into PDF
//! objects and the reading-order editor. This module supplies the piece that can
//! live in the dependency-free core: a validated, deterministic tree model with
//! a canonical token round-trip and a `/ParentTree`-style index. A hostile token
//! cannot grow the tree past [`MAX_STRUCTURE_NODES`] or nest it past
//! [`MAX_STRUCTURE_DEPTH`], and every rejection is a typed [`StructureError`].
//!
//! The token grammar is deliberately tiny and has no escaping: a node is
//! `<role>[@<page>][{ <child> ... }]`. Roles are validated names, so the
//! characters `@`, `{`, `}` and white space are never part of a role.

use std::fmt;

/// Maximum number of nodes in a validated structure tree.
pub const MAX_STRUCTURE_NODES: usize = 4096;
/// Maximum nesting depth of a validated structure tree.
pub const MAX_STRUCTURE_DEPTH: u8 = 16;
/// Maximum length of a single role name.
pub const MAX_ROLE_LEN: usize = 32;
/// Maximum accepted length of a whole structure token.
pub const MAX_STRUCTURE_TOKEN_LEN: usize = 1 << 20;

/// A typed reason a structure tree or token was rejected.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum StructureError {
    /// The token exceeded [`MAX_STRUCTURE_TOKEN_LEN`] before parsing began.
    #[error("structure token exceeds the {limit} byte limit")]
    TokenTooLong {
        /// The maximum accepted length.
        limit: usize,
    },
    /// A role name was empty, over-long or contained a grammar delimiter.
    #[error("invalid structure role: {0}")]
    InvalidRole(String),
    /// The root node was not the standard `/Document` role.
    #[error("structure root must be Document, found {0}")]
    InvalidRoot(String),
    /// The tree held more nodes than the policy allows.
    #[error("structure tree has more than {limit} nodes")]
    TooManyNodes {
        /// The maximum accepted number of nodes.
        limit: usize,
    },
    /// The tree nested deeper than the policy allows.
    #[error("structure tree is deeper than {limit} levels")]
    TooDeep {
        /// The maximum accepted depth.
        limit: u8,
    },
    /// The token did not follow the grammar at a given byte offset.
    #[error("malformed structure token at byte {offset}: {reason}")]
    Malformed {
        /// Byte offset where parsing failed.
        offset: usize,
        /// Why the token was rejected.
        reason: String,
    },
    /// Data remained after the root node had been parsed.
    #[error("unexpected data after structure root at byte {offset}")]
    TrailingData {
        /// Byte offset of the first unexpected byte.
        offset: usize,
    },
}

/// A standard or custom PDF structure role.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StructureRole {
    /// `/Document` — the required root of a tagged structure tree.
    Document,
    /// `/Part`.
    Part,
    /// `/Chapter`.
    Chapter,
    /// `/Sect`.
    Sect,
    /// `/P` — a paragraph.
    Paragraph,
    /// `/H1`.
    Heading1,
    /// `/H2`.
    Heading2,
    /// `/H3`.
    Heading3,
    /// `/H4`.
    Heading4,
    /// `/H5`.
    Heading5,
    /// `/H6`.
    Heading6,
    /// `/L` — a list.
    List,
    /// `/Li` — a list item.
    ListItem,
    /// `/LBody` — a list item body.
    ListBody,
    /// `/Table`.
    Table,
    /// `/TR` — a table row.
    TableRow,
    /// `/TH` — a table header cell.
    TableHeader,
    /// `/TD` — a table data cell.
    TableData,
    /// `/Figure`.
    Figure,
    /// `/Image`.
    Image,
    /// `/Art` — an artifact.
    Artifact,
    /// `/Span`.
    Span,
    /// `/Quote`.
    Quote,
    /// `/Note`.
    Note,
    /// `/Reference`.
    Reference,
    /// `/BibEntry`.
    BibliographyEntry,
    /// `/Code`.
    Code,
    /// `/BlockQuote`.
    BlockQuote,
    /// `/Div`.
    Div,
    /// `/Aside`.
    Aside,
    /// `/Title`.
    Title,
    /// A custom role, stored without a leading slash.
    Custom(String),
}

impl StructureRole {
    /// All standard roles, in canonical token order.
    #[must_use]
    pub const fn all() -> &'static [StructureRole] {
        &[
            Self::Document,
            Self::Part,
            Self::Chapter,
            Self::Sect,
            Self::Paragraph,
            Self::Heading1,
            Self::Heading2,
            Self::Heading3,
            Self::Heading4,
            Self::Heading5,
            Self::Heading6,
            Self::List,
            Self::ListItem,
            Self::ListBody,
            Self::Table,
            Self::TableRow,
            Self::TableHeader,
            Self::TableData,
            Self::Figure,
            Self::Image,
            Self::Artifact,
            Self::Span,
            Self::Quote,
            Self::Note,
            Self::Reference,
            Self::BibliographyEntry,
            Self::Code,
            Self::BlockQuote,
            Self::Div,
            Self::Aside,
            Self::Title,
        ]
    }

    /// Return the canonical PDF role name, without a leading slash.
    #[must_use]
    pub fn name(&self) -> &str {
        match self {
            Self::Document => "Document",
            Self::Part => "Part",
            Self::Chapter => "Chapter",
            Self::Sect => "Sect",
            Self::Paragraph => "P",
            Self::Heading1 => "H1",
            Self::Heading2 => "H2",
            Self::Heading3 => "H3",
            Self::Heading4 => "H4",
            Self::Heading5 => "H5",
            Self::Heading6 => "H6",
            Self::List => "L",
            Self::ListItem => "Li",
            Self::ListBody => "LBody",
            Self::Table => "Table",
            Self::TableRow => "TR",
            Self::TableHeader => "TH",
            Self::TableData => "TD",
            Self::Figure => "Figure",
            Self::Image => "Image",
            Self::Artifact => "Art",
            Self::Span => "Span",
            Self::Quote => "Quote",
            Self::Note => "Note",
            Self::Reference => "Reference",
            Self::BibliographyEntry => "BibEntry",
            Self::Code => "Code",
            Self::BlockQuote => "BlockQuote",
            Self::Div => "Div",
            Self::Aside => "Aside",
            Self::Title => "Title",
            Self::Custom(name) => name,
        }
    }

    /// Parse a role token, tolerating an optional leading slash and ASCII case.
    ///
    /// # Errors
    ///
    /// Returns [`StructureError::InvalidRole`] for an empty role, a role longer
    /// than [`MAX_ROLE_LEN`], or one containing a grammar delimiter or white
    /// space. Unknown but well-formed names become [`StructureRole::Custom`].
    pub fn parse(token: &str) -> Result<Self, StructureError> {
        let name = token.strip_prefix('/').unwrap_or(token);
        validate_role_name(name)?;
        for role in Self::all() {
            if name.eq_ignore_ascii_case(role.name()) {
                return Ok(role.clone());
            }
        }
        Ok(Self::Custom(name.to_owned()))
    }
}

impl fmt::Display for StructureRole {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

fn validate_role_name(name: &str) -> Result<(), StructureError> {
    if name.is_empty()
        || name.len() > MAX_ROLE_LEN
        || name
            .bytes()
            .any(|byte| byte.is_ascii_whitespace() || matches!(byte, b'@' | b'{' | b'}' | b'|'))
    {
        return Err(StructureError::InvalidRole(name.to_owned()));
    }
    Ok(())
}

/// One structure element: a role, an optional marked-content page, and children.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StructureNode {
    role: StructureRole,
    page: Option<u32>,
    children: Vec<StructureNode>,
}

impl StructureNode {
    /// Create a node with a role, an optional page number and no children.
    #[must_use]
    pub fn new(role: StructureRole, page: Option<u32>) -> Self {
        Self {
            role,
            page,
            children: Vec::new(),
        }
    }

    /// Append a child node in document order.
    pub fn push(&mut self, child: StructureNode) {
        self.children.push(child);
    }

    /// Return this element's role.
    #[must_use]
    pub fn role(&self) -> &StructureRole {
        &self.role
    }

    /// Return the marked-content page number attached to this element.
    #[must_use]
    pub const fn page(&self) -> Option<u32> {
        self.page
    }

    /// Return child elements in document order.
    #[must_use]
    pub fn children(&self) -> &[StructureNode] {
        &self.children
    }
}

/// One entry in the deterministic parent-tree index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParentTreeEntry {
    /// Stable `/StructParents` key assigned in depth-first order.
    pub struct_parent: usize,
    /// Page number of the referenced content.
    pub page: u32,
    /// Child-index path from the root to the structure element.
    pub path: Vec<usize>,
}

/// A validated, bounded tagged-PDF structure tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StructureTree {
    root: StructureNode,
}

impl StructureTree {
    /// Build and validate a tree rooted at `root`.
    ///
    /// # Errors
    ///
    /// Returns a typed error when the root is not `/Document`, the role names
    /// are invalid, or the tree exceeds the node or depth bounds.
    pub fn new(root: StructureNode) -> Result<Self, StructureError> {
        let tree = Self { root };
        tree.validate()?;
        Ok(tree)
    }

    /// Return the validated root element.
    #[must_use]
    pub fn root(&self) -> &StructureNode {
        &self.root
    }

    /// Re-check the root role, role names, bounds and parent-tree paths.
    ///
    /// # Errors
    ///
    /// Returns the first typed failure in deterministic depth-first order.
    pub fn validate(&self) -> Result<(), StructureError> {
        if self.root.role != StructureRole::Document {
            return Err(StructureError::InvalidRoot(
                self.root.role.name().to_owned(),
            ));
        }
        let mut nodes = 0usize;
        validate_node(&self.root, 1, &mut nodes)?;
        // Building the index also verifies that every recorded path resolves.
        let _ = self.parent_tree();
        Ok(())
    }

    /// Build the `/ParentTree`-style index in deterministic depth-first order.
    ///
    /// Each structure element that carries a page number receives a sequential
    /// `struct_parent` key starting at zero. Rebuilding the index from the same
    /// tree always yields the same entries and paths.
    #[must_use]
    pub fn parent_tree(&self) -> Vec<ParentTreeEntry> {
        let mut entries = Vec::new();
        collect_parent_tree(&self.root, &mut Vec::new(), &mut entries);
        entries
    }

    /// Serialise the tree into its canonical, deterministic token.
    #[must_use]
    pub fn to_token(&self) -> String {
        let mut out = String::new();
        write_node(&self.root, &mut out);
        out
    }

    /// Parse and validate a canonical token produced by [`Self::to_token`].
    ///
    /// The token length is checked before parsing, and parsing is depth- and
    /// node-bounded so hostile input does the least possible work.
    ///
    /// # Errors
    ///
    /// Returns a typed error for an over-long token, a grammar violation, or a
    /// tree that fails [`Self::validate`].
    pub fn from_token(token: &str) -> Result<Self, StructureError> {
        if token.len() > MAX_STRUCTURE_TOKEN_LEN {
            return Err(StructureError::TokenTooLong {
                limit: MAX_STRUCTURE_TOKEN_LEN,
            });
        }
        let bytes = token.as_bytes();
        let mut cursor = 0usize;
        let root = parse_node(bytes, &mut cursor, 1)?;
        while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        if cursor != bytes.len() {
            return Err(StructureError::TrailingData { offset: cursor });
        }
        Self::new(root)
    }
}

fn validate_node(node: &StructureNode, depth: u8, count: &mut usize) -> Result<(), StructureError> {
    if depth > MAX_STRUCTURE_DEPTH {
        return Err(StructureError::TooDeep {
            limit: MAX_STRUCTURE_DEPTH,
        });
    }
    *count += 1;
    if *count > MAX_STRUCTURE_NODES {
        return Err(StructureError::TooManyNodes {
            limit: MAX_STRUCTURE_NODES,
        });
    }
    validate_role_name(node.role.name())?;
    for child in &node.children {
        validate_node(child, depth + 1, count)?;
    }
    Ok(())
}

fn collect_parent_tree(
    node: &StructureNode,
    path: &mut Vec<usize>,
    entries: &mut Vec<ParentTreeEntry>,
) {
    if let Some(page) = node.page {
        entries.push(ParentTreeEntry {
            struct_parent: entries.len(),
            page,
            path: path.clone(),
        });
    }
    for (index, child) in node.children.iter().enumerate() {
        path.push(index);
        collect_parent_tree(child, path, entries);
        path.pop();
    }
}

fn write_node(node: &StructureNode, out: &mut String) {
    out.push_str(node.role.name());
    if let Some(page) = node.page {
        out.push('@');
        out.push_str(&page.to_string());
    }
    if !node.children.is_empty() {
        out.push('{');
        for (index, child) in node.children.iter().enumerate() {
            if index > 0 {
                out.push(' ');
            }
            write_node(child, out);
        }
        out.push('}');
    }
}

fn parse_node(
    bytes: &[u8],
    cursor: &mut usize,
    depth: u8,
) -> Result<StructureNode, StructureError> {
    if depth > MAX_STRUCTURE_DEPTH {
        return Err(StructureError::TooDeep {
            limit: MAX_STRUCTURE_DEPTH,
        });
    }
    let start = *cursor;
    while *cursor < bytes.len() {
        let byte = bytes[*cursor];
        if byte.is_ascii_whitespace() || matches!(byte, b'@' | b'{' | b'}' | b'|') {
            break;
        }
        *cursor += 1;
    }
    if *cursor == start {
        return Err(StructureError::Malformed {
            offset: start,
            reason: "expected a role name".to_owned(),
        });
    }
    let role =
        StructureRole::parse(std::str::from_utf8(&bytes[start..*cursor]).map_err(|_| {
            StructureError::Malformed {
                offset: start,
                reason: "role name is not UTF-8".to_owned(),
            }
        })?)?;

    let mut page = None;
    if *cursor < bytes.len() && bytes[*cursor] == b'@' {
        *cursor += 1;
        let digit_start = *cursor;
        while *cursor < bytes.len() && bytes[*cursor].is_ascii_digit() {
            *cursor += 1;
        }
        if *cursor == digit_start {
            return Err(StructureError::Malformed {
                offset: *cursor,
                reason: "expected a page number after '@'".to_owned(),
            });
        }
        let value = std::str::from_utf8(&bytes[digit_start..*cursor]).map_err(|_| {
            StructureError::Malformed {
                offset: digit_start,
                reason: "page number is not UTF-8".to_owned(),
            }
        })?;
        page = Some(
            value
                .parse::<u32>()
                .map_err(|_| StructureError::Malformed {
                    offset: digit_start,
                    reason: "page number is not a u32".to_owned(),
                })?,
        );
    }

    let mut node = StructureNode::new(role, page);
    if *cursor < bytes.len() && bytes[*cursor] == b'{' {
        *cursor += 1;
        loop {
            while *cursor < bytes.len() && bytes[*cursor].is_ascii_whitespace() {
                *cursor += 1;
            }
            if *cursor >= bytes.len() {
                return Err(StructureError::Malformed {
                    offset: *cursor,
                    reason: "unterminated child list".to_owned(),
                });
            }
            if bytes[*cursor] == b'}' {
                *cursor += 1;
                break;
            }
            let child = parse_node(bytes, cursor, depth + 1)?;
            node.push(child);
        }
    }
    Ok(node)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_tree() -> StructureTree {
        let mut document = StructureNode::new(StructureRole::Document, None);
        let mut chapter = StructureNode::new(StructureRole::Chapter, None);
        chapter.push(StructureNode::new(StructureRole::Heading1, Some(1)));
        chapter.push(StructureNode::new(StructureRole::Paragraph, Some(1)));
        let mut list = StructureNode::new(StructureRole::List, None);
        list.push(StructureNode::new(StructureRole::ListItem, Some(2)));
        document.push(chapter);
        document.push(list);
        StructureTree::new(document).expect("sample tree is valid")
    }

    #[test]
    fn fixed_roles_round_trip_canonical_names() {
        assert_eq!(StructureRole::all().len(), 31);
        for role in StructureRole::all() {
            let name = role.name();
            assert_eq!(StructureRole::parse(name).expect("canonical name"), *role);
            assert_eq!(
                StructureRole::parse(&format!("/{}", name.to_ascii_lowercase()))
                    .expect("tolerant name"),
                *role
            );
            assert_eq!(role.to_string(), name);
        }
    }

    #[test]
    fn custom_role_round_trips_and_rejects_bad_delimiters() {
        let custom = StructureRole::parse("CustomRole").expect("custom name");
        assert_eq!(custom, StructureRole::Custom("CustomRole".to_owned()));
        assert_eq!(custom.name(), "CustomRole");
        for bad in ["", "has space", "at@sign", "brace{", "pipe|x"] {
            assert!(
                matches!(
                    StructureRole::parse(bad),
                    Err(StructureError::InvalidRole(_))
                ),
                "{bad} must be rejected"
            );
        }
        let long = "x".repeat(MAX_ROLE_LEN + 1);
        assert!(StructureRole::parse(&long).is_err());
    }

    #[test]
    fn nested_tree_round_trips_exactly() {
        let tree = sample_tree();
        let token = tree.to_token();
        assert_eq!(token, "Document{Chapter{H1@1 P@1} L{Li@2}}");
        let parsed = StructureTree::from_token(&token).expect("round trip");
        assert_eq!(parsed, tree);
        assert_eq!(parsed.to_token(), token);
        assert_eq!(
            StructureTree::from_token(&parsed.to_token()).expect("second"),
            tree
        );
    }

    #[test]
    fn parent_tree_is_deterministic_and_resolves_paths() {
        let tree = sample_tree();
        let index = tree.parent_tree();
        assert_eq!(
            index
                .iter()
                .map(|entry| (entry.struct_parent, entry.page, entry.path.clone()))
                .collect::<Vec<_>>(),
            vec![(0, 1, vec![0, 0]), (1, 1, vec![0, 1]), (2, 2, vec![1, 0]),]
        );
        let mut cursor = tree.root();
        for &step in &index[0].path {
            cursor = &cursor.children()[step];
        }
        assert_eq!(cursor.role(), &StructureRole::Heading1);
        assert_eq!(tree.parent_tree(), index);
    }

    #[test]
    fn non_document_root_is_rejected() {
        let bad = StructureNode::new(StructureRole::Paragraph, Some(1));
        assert_eq!(
            StructureTree::new(bad).expect_err("bad root"),
            StructureError::InvalidRoot("P".to_owned())
        );
        let token = "P@1";
        assert!(matches!(
            StructureTree::from_token(token),
            Err(StructureError::InvalidRoot(_))
        ));
    }

    /// Build a left-deep chain of exactly `depth` levels, root included.
    fn chain(depth: u8) -> StructureNode {
        fn part(levels: u8) -> StructureNode {
            let mut node = StructureNode::new(StructureRole::Part, None);
            if levels > 1 {
                node.push(part(levels - 1));
            }
            node
        }
        let mut root = StructureNode::new(StructureRole::Document, None);
        if depth > 1 {
            root.push(part(depth - 1));
        }
        root
    }

    #[test]
    fn node_and_depth_limits_are_bounded() {
        assert!(StructureTree::new(chain(MAX_STRUCTURE_DEPTH)).is_ok());
        assert_eq!(
            StructureTree::new(chain(MAX_STRUCTURE_DEPTH + 1)).expect_err("too deep"),
            StructureError::TooDeep {
                limit: MAX_STRUCTURE_DEPTH
            }
        );

        let mut wide = StructureNode::new(StructureRole::Document, None);
        for _ in 0..(MAX_STRUCTURE_NODES - 1) {
            wide.push(StructureNode::new(StructureRole::Part, None));
        }
        assert!(StructureTree::new(wide.clone()).is_ok());
        wide.push(StructureNode::new(StructureRole::Part, None));
        assert_eq!(
            StructureTree::new(wide).expect_err("too many nodes"),
            StructureError::TooManyNodes {
                limit: MAX_STRUCTURE_NODES
            }
        );
    }

    #[test]
    fn malformed_tokens_return_typed_errors() {
        for token in ["", "@1", "Document{", "Document}", "Document{ P@x }"] {
            assert!(
                matches!(
                    StructureTree::from_token(token),
                    Err(StructureError::Malformed { .. })
                        | Err(StructureError::InvalidRoot(_))
                        | Err(StructureError::TrailingData { .. })
                ),
                "token {token:?} must be rejected"
            );
        }
        assert!(matches!(
            StructureTree::from_token("Document trailing"),
            Err(StructureError::TrailingData { .. })
        ));
    }

    #[test]
    fn oversized_token_is_rejected_before_parsing() {
        let token = "x".repeat(MAX_STRUCTURE_TOKEN_LEN + 1);
        assert_eq!(
            StructureTree::from_token(&token).expect_err("over-long"),
            StructureError::TokenTooLong {
                limit: MAX_STRUCTURE_TOKEN_LEN
            }
        );
    }
}
