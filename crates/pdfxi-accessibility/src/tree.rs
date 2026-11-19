//! Validated accessibility node tree.
//!
//! [`A11yTree`] is a directed acyclic graph of [`A11yNode`] values in
//! presentation order. Each node carries a stable [`NodeId`], a [`TagRole`], a
//! page-space [`BBox`], optional text and alternative text, and an ordered list
//! of child identifiers. Construction validates every structural invariant so
//! that consumers (export, assistive output) never have to defend against
//! duplicate ids, cycles, multiple parents, or orphan nodes.
//!
//! Coordinates follow the PDF default user-space convention used elsewhere in
//! the workspace: the origin is at the lower-left of the page and the axes grow
//! rightward and upward. A [`BBox`] must have finite coordinates and strictly
//! positive width and height; empty or non-finite boxes are rejected with a
//! structured [`TreeError`].

use core::fmt;
use std::collections::{HashMap, HashSet};

use crate::TagRole;

/// Stable identifier of an accessibility node.
///
/// Identifiers are opaque numbers. The reading-order builder assigns them
/// deterministically, so the same logical document always produces the same
/// ids regardless of the order in which source blocks were supplied.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NodeId(u64);

impl NodeId {
    /// Creates a node identifier from its numeric value.
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Returns the numeric value of this identifier.
    pub const fn get(self) -> u64 {
        self.0
    }
}

impl fmt::Display for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Errors produced while validating an accessibility tree or a bounding box.
#[derive(Debug, Clone, PartialEq)]
pub enum TreeError {
    /// A coordinate was infinite or `NaN`.
    NonFiniteCoordinate {
        /// Name of the offending field.
        field: &'static str,
        /// The rejected value.
        value: f32,
    },
    /// A box had zero extent or inverted corners.
    EmptyBox {
        /// Lower-left x.
        x0: f32,
        /// Lower-left y.
        y0: f32,
        /// Upper-right x.
        x1: f32,
        /// Upper-right y.
        y1: f32,
    },
    /// A supplied font size was not finite and strictly positive.
    NonFiniteFontSize {
        /// The rejected value.
        value: f32,
    },
    /// Two nodes shared an identifier.
    DuplicateNode(NodeId),
    /// A node listed a child that does not exist.
    UnknownChild {
        /// Identifier of the parent node.
        parent: NodeId,
        /// Identifier of the missing child.
        child: NodeId,
    },
    /// A declared root identifier does not exist.
    UnknownRoot(NodeId),
    /// A node was referenced as a child by more than one parent.
    MultipleParents(NodeId),
    /// The node graph contained a cycle.
    Cycle,
    /// A node was neither a declared root nor reachable from any root.
    OrphanNode(NodeId),
}

impl fmt::Display for TreeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NonFiniteCoordinate { field, value } => {
                write!(f, "coordinate `{field}` must be finite, got {value}")
            }
            Self::EmptyBox { x0, y0, x1, y1 } => {
                write!(f, "empty or inverted box ({x0}, {y0}) - ({x1}, {y1})")
            }
            Self::NonFiniteFontSize { value } => {
                write!(f, "font size must be finite and positive, got {value}")
            }
            Self::DuplicateNode(id) => write!(f, "duplicate accessibility node {id}"),
            Self::UnknownChild { parent, child } => {
                write!(f, "node {parent} references unknown child {child}")
            }
            Self::UnknownRoot(id) => write!(f, "root {id} is not a node in the tree"),
            Self::MultipleParents(id) => write!(f, "node {id} has more than one parent"),
            Self::Cycle => write!(f, "accessibility tree contains a cycle"),
            Self::OrphanNode(id) => write!(f, "node {id} is not reachable from any root"),
        }
    }
}

impl std::error::Error for TreeError {}

/// A finite, axis-aligned box in page space with strictly positive extent.
///
/// Instances are created through [`BBox::new`], which rejects non-finite
/// coordinates and empty or inverted boxes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BBox {
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
}

impl BBox {
    /// Creates a box from its lower-left and upper-right corners.
    ///
    /// Returns [`TreeError::NonFiniteCoordinate`] when a coordinate is not
    /// finite, or [`TreeError::EmptyBox`] when the extent is zero or inverted.
    pub fn new(x0: f32, y0: f32, x1: f32, y1: f32) -> Result<Self, TreeError> {
        for (field, value) in [("x0", x0), ("y0", y0), ("x1", x1), ("y1", y1)] {
            if !value.is_finite() {
                return Err(TreeError::NonFiniteCoordinate { field, value });
            }
        }
        if x1 <= x0 || y1 <= y0 {
            return Err(TreeError::EmptyBox { x0, y0, x1, y1 });
        }
        Ok(Self { x0, y0, x1, y1 })
    }

    /// Lower-left x coordinate.
    pub const fn x0(&self) -> f32 {
        self.x0
    }

    /// Lower-left y coordinate.
    pub const fn y0(&self) -> f32 {
        self.y0
    }

    /// Upper-right x coordinate.
    pub const fn x1(&self) -> f32 {
        self.x1
    }

    /// Upper-right y coordinate.
    pub const fn y1(&self) -> f32 {
        self.y1
    }

    /// Width of the box.
    pub const fn width(&self) -> f32 {
        self.x1 - self.x0
    }

    /// Height of the box.
    pub const fn height(&self) -> f32 {
        self.y1 - self.y0
    }

    /// Returns `true` when the two boxes overlap with positive vertical extent.
    pub(crate) const fn vertically_overlaps(&self, other: &Self) -> bool {
        self.y1 > other.y0 && other.y1 > self.y0
    }

    /// Returns the smallest box containing both inputs.
    ///
    /// Both inputs are validated, so the union is always valid.
    pub fn union(self, other: Self) -> Self {
        Self {
            x0: self.x0.min(other.x0),
            y0: self.y0.min(other.y0),
            x1: self.x1.max(other.x1),
            y1: self.y1.max(other.y1),
        }
    }

    /// Re-validates this box, returning a structured error if it is invalid.
    pub(crate) fn check(&self) -> Result<(), TreeError> {
        if !self.x0.is_finite() {
            return Err(TreeError::NonFiniteCoordinate {
                field: "x0",
                value: self.x0,
            });
        }
        if !self.y0.is_finite() {
            return Err(TreeError::NonFiniteCoordinate {
                field: "y0",
                value: self.y0,
            });
        }
        if !self.x1.is_finite() {
            return Err(TreeError::NonFiniteCoordinate {
                field: "x1",
                value: self.x1,
            });
        }
        if !self.y1.is_finite() {
            return Err(TreeError::NonFiniteCoordinate {
                field: "y1",
                value: self.y1,
            });
        }
        if self.x1 <= self.x0 || self.y1 <= self.y0 {
            return Err(TreeError::EmptyBox {
                x0: self.x0,
                y0: self.y0,
                x1: self.x1,
                y1: self.y1,
            });
        }
        Ok(())
    }
}

/// A single node in an accessibility tree.
///
/// The fields are public so callers can assemble trees directly (for example
/// to describe figures, tables, and form fields that the text-block builder
/// cannot infer). The tree is validated when it is constructed.
#[derive(Clone, Debug, PartialEq)]
pub struct A11yNode {
    /// Stable identifier of the node.
    pub id: NodeId,
    /// Semantic role of the node.
    pub role: TagRole,
    /// Zero-based page index associated with the node.
    ///
    /// Content nodes inherit the page of their source block; the document root
    /// uses zero as a neutral value.
    pub page: usize,
    /// Bounding box of the node in page space.
    pub bbox: BBox,
    /// Text content of the node, empty for structural nodes.
    pub text: String,
    /// Alternative text for visual content, when available.
    pub alt_text: Option<String>,
    /// Replacement text (`/ActualText`) used when the node's glyphs do not
    /// map to Unicode, when available.
    pub actual_text: Option<String>,
    /// Abbreviation expansion (`/E`) for the node's text, when available.
    pub expansion: Option<String>,
    /// Whether this node is a decorative artifact (not real content).
    ///
    /// Artifacts are excluded from assistive presentation by consumers but are
    /// kept in the tree so an editor can toggle the flag without losing the
    /// node's other data.
    pub artifact: bool,
    /// Identifiers of the node's children, in presentation order.
    pub children: Vec<NodeId>,
}

impl A11yNode {
    /// Creates a node with no alternative text.
    pub fn new(
        id: NodeId,
        role: TagRole,
        page: usize,
        bbox: BBox,
        text: impl Into<String>,
        children: Vec<NodeId>,
    ) -> Self {
        Self {
            id,
            role,
            page,
            bbox,
            text: text.into(),
            alt_text: None,
            actual_text: None,
            expansion: None,
            artifact: false,
            children,
        }
    }

    /// Attaches alternative text to this node.
    pub fn with_alt_text(mut self, alt: impl Into<String>) -> Self {
        self.alt_text = Some(alt.into());
        self
    }

    /// Attaches replacement (`/ActualText`) text to this node.
    pub fn with_actual_text(mut self, text: impl Into<String>) -> Self {
        self.actual_text = Some(text.into());
        self
    }

    /// Attaches an abbreviation expansion (`/E`) to this node.
    pub fn with_expansion(mut self, text: impl Into<String>) -> Self {
        self.expansion = Some(text.into());
        self
    }

    /// Marks this node as a decorative artifact.
    pub fn with_artifact(mut self, artifact: bool) -> Self {
        self.artifact = artifact;
        self
    }
}

/// Validates the node set and returns `Ok` when every invariant holds.
fn validate(nodes: &[A11yNode], roots: &[NodeId]) -> Result<(), TreeError> {
    let mut index_by_id: HashMap<NodeId, usize> = HashMap::with_capacity(nodes.len());
    for (index, node) in nodes.iter().enumerate() {
        if index_by_id.insert(node.id, index).is_some() {
            return Err(TreeError::DuplicateNode(node.id));
        }
        node.bbox.check()?;
    }

    let mut in_degree = vec![0usize; nodes.len()];
    let mut parent_count = vec![0usize; nodes.len()];
    for node in nodes {
        for &child in &node.children {
            let child_index = index_by_id.get(&child).ok_or(TreeError::UnknownChild {
                parent: node.id,
                child,
            })?;
            in_degree[*child_index] += 1;
            parent_count[*child_index] += 1;
        }
    }
    for (index, &count) in parent_count.iter().enumerate() {
        if count > 1 {
            return Err(TreeError::MultipleParents(nodes[index].id));
        }
    }

    let mut declared_roots: HashSet<NodeId> = HashSet::with_capacity(roots.len());
    for &root in roots {
        if !index_by_id.contains_key(&root) {
            return Err(TreeError::UnknownRoot(root));
        }
        declared_roots.insert(root);
    }

    for (index, node) in nodes.iter().enumerate() {
        if in_degree[index] == 0 && !declared_roots.contains(&node.id) {
            return Err(TreeError::OrphanNode(node.id));
        }
    }

    // Iterative three-colour DFS: a back edge to a node still on the stack
    // proves a cycle. Iteration avoids recursion limits on deep trees.
    #[derive(Clone, Copy, PartialEq)]
    enum Colour {
        White,
        Gray,
        Black,
    }
    let mut colour = vec![Colour::White; nodes.len()];
    for start in 0..nodes.len() {
        if colour[start] != Colour::White {
            continue;
        }
        colour[start] = Colour::Gray;
        let mut stack: Vec<(usize, usize)> = vec![(start, 0)];
        while let Some((index, child_pos)) = stack.last_mut() {
            let children = &nodes[*index].children;
            if *child_pos < children.len() {
                let child = children[*child_pos];
                *child_pos += 1;
                // Every child was confirmed to exist in the validation pass above.
                let Some(&child_index) = index_by_id.get(&child) else {
                    continue;
                };
                match colour[child_index] {
                    Colour::Gray => return Err(TreeError::Cycle),
                    Colour::Black => continue,
                    Colour::White => {
                        colour[child_index] = Colour::Gray;
                        stack.push((child_index, 0));
                    }
                }
            } else {
                colour[*index] = Colour::Black;
                stack.pop();
            }
        }
    }

    Ok(())
}

/// A validated, acyclic accessibility tree in presentation order.
#[derive(Clone, Debug, PartialEq)]
pub struct A11yTree {
    nodes: Vec<A11yNode>,
    roots: Vec<NodeId>,
}

impl A11yTree {
    /// Builds and validates an accessibility tree.
    pub fn new(nodes: Vec<A11yNode>, roots: Vec<NodeId>) -> Result<Self, TreeError> {
        validate(&nodes, &roots)?;
        Ok(Self { nodes, roots })
    }

    /// Creates an empty tree with no nodes and no roots.
    pub fn empty() -> Self {
        Self {
            nodes: Vec::new(),
            roots: Vec::new(),
        }
    }

    /// Re-validates this tree.
    pub fn validate(&self) -> Result<(), TreeError> {
        validate(&self.nodes, &self.roots)
    }

    /// Identifiers of the root nodes, in declaration order.
    pub fn roots(&self) -> &[NodeId] {
        &self.roots
    }

    /// All nodes in declaration order.
    pub fn nodes(&self) -> &[A11yNode] {
        &self.nodes
    }

    /// Returns the node with the given identifier, if present.
    pub fn node(&self, id: NodeId) -> Option<&A11yNode> {
        self.nodes.iter().find(|node| node.id == id)
    }

    /// Number of nodes.
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    /// Returns `true` when the tree has no nodes.
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// Returns node identifiers in depth-first presentation order.
    ///
    /// Children are visited in declaration order and each node is emitted once.
    /// The walk uses an explicit stack rather than recursion, so arbitrarily
    /// deep trees are safe.
    pub fn traversal(&self) -> Vec<NodeId> {
        let mut ordered = Vec::with_capacity(self.nodes.len());
        let mut visited: HashSet<NodeId> = HashSet::with_capacity(self.nodes.len());
        let index: HashMap<NodeId, usize> = self
            .nodes
            .iter()
            .enumerate()
            .map(|(index, node)| (node.id, index))
            .collect();
        let mut stack: Vec<NodeId> = Vec::new();
        for &root in self.roots.iter().rev() {
            if visited.insert(root) {
                stack.push(root);
            }
        }
        while let Some(id) = stack.pop() {
            ordered.push(id);
            let Some(&node_index) = index.get(&id) else {
                continue;
            };
            for &child in self.nodes[node_index].children.iter().rev() {
                if visited.insert(child) {
                    stack.push(child);
                }
            }
        }
        ordered
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn box_at(x0: f32, y0: f32, x1: f32, y1: f32) -> BBox {
        match BBox::new(x0, y0, x1, y1) {
            Ok(b) => b,
            Err(_) => panic!("test box must be valid"),
        }
    }

    fn node(id: u64, role: TagRole, children: Vec<NodeId>) -> A11yNode {
        A11yNode::new(
            NodeId::new(id),
            role,
            0,
            box_at(0.0, 0.0, 1.0, 1.0),
            "",
            children,
        )
    }

    #[test]
    fn bbox_rejects_nonfinite_and_empty_boxes() {
        assert!(matches!(
            BBox::new(0.0, 0.0, f32::NAN, 1.0),
            Err(TreeError::NonFiniteCoordinate { field: "x1", .. })
        ));
        assert_eq!(
            BBox::new(0.0, 0.0, f32::INFINITY, 1.0),
            Err(TreeError::NonFiniteCoordinate {
                field: "x1",
                value: f32::INFINITY
            })
        );
        assert_eq!(
            BBox::new(0.0, 0.0, 0.0, 1.0),
            Err(TreeError::EmptyBox {
                x0: 0.0,
                y0: 0.0,
                x1: 0.0,
                y1: 1.0
            })
        );
        assert_eq!(
            BBox::new(5.0, 0.0, 1.0, 1.0),
            Err(TreeError::EmptyBox {
                x0: 5.0,
                y0: 0.0,
                x1: 1.0,
                y1: 1.0
            })
        );
    }

    #[test]
    fn bbox_union_and_accessors_are_consistent() {
        let a = box_at(0.0, 2.0, 4.0, 8.0);
        let b = box_at(-2.0, 0.0, 1.0, 3.0);
        let u = a.union(b);
        assert_eq!((u.x0(), u.y0(), u.x1(), u.y1()), (-2.0, 0.0, 4.0, 8.0));
        assert_eq!((u.width(), u.height()), (6.0, 8.0));

        assert!(a.vertically_overlaps(&b));
    }

    #[test]
    fn valid_tree_traversal_and_accessors() {
        let tree = A11yTree::new(
            vec![
                node(1, TagRole::Document, vec![NodeId::new(2), NodeId::new(3)]),
                node(2, TagRole::Heading1, vec![]),
                node(3, TagRole::List, vec![NodeId::new(4)]),
                node(4, TagRole::ListItem, vec![]),
            ],
            vec![NodeId::new(1)],
        )
        .expect("valid tree");
        assert_eq!(tree.roots(), &[NodeId::new(1)]);
        assert_eq!(
            tree.traversal(),
            vec![
                NodeId::new(1),
                NodeId::new(2),
                NodeId::new(3),
                NodeId::new(4)
            ]
        );
        assert_eq!(tree.len(), 4);
        assert!(!tree.is_empty());
        assert_eq!(
            tree.node(NodeId::new(3)).map(|n| n.role),
            Some(TagRole::List)
        );
        assert!(tree.validate().is_ok());
    }

    #[test]
    fn duplicate_ids_are_rejected() {
        let tree = A11yTree::new(
            vec![
                node(1, TagRole::Document, vec![]),
                node(1, TagRole::Page, vec![]),
            ],
            vec![NodeId::new(1)],
        );
        assert_eq!(tree.err(), Some(TreeError::DuplicateNode(NodeId::new(1))));
    }

    #[test]
    fn cycles_are_rejected() {
        let tree = A11yTree::new(
            vec![
                node(1, TagRole::Document, vec![NodeId::new(2)]),
                node(2, TagRole::Paragraph, vec![NodeId::new(1)]),
            ],
            vec![NodeId::new(1)],
        );
        assert_eq!(tree.err(), Some(TreeError::Cycle));

        let self_reference = A11yTree::new(
            vec![node(1, TagRole::Document, vec![NodeId::new(1)])],
            vec![NodeId::new(1)],
        );
        assert_eq!(self_reference.err(), Some(TreeError::Cycle));
    }

    #[test]
    fn orphan_nodes_are_rejected() {
        let tree = A11yTree::new(
            vec![
                node(1, TagRole::Document, vec![]),
                node(2, TagRole::Paragraph, vec![]),
            ],
            vec![NodeId::new(1)],
        );
        assert_eq!(tree.err(), Some(TreeError::OrphanNode(NodeId::new(2))));
    }

    #[test]
    fn unknown_children_parents_and_roots_are_rejected() {
        let unknown_child = A11yTree::new(
            vec![node(1, TagRole::Document, vec![NodeId::new(99)])],
            vec![NodeId::new(1)],
        );
        assert_eq!(
            unknown_child.err(),
            Some(TreeError::UnknownChild {
                parent: NodeId::new(1),
                child: NodeId::new(99)
            })
        );

        let multiple_parents = A11yTree::new(
            vec![
                node(1, TagRole::Document, vec![NodeId::new(3)]),
                node(2, TagRole::Part, vec![NodeId::new(3)]),
                node(3, TagRole::Paragraph, vec![]),
            ],
            vec![NodeId::new(1), NodeId::new(2)],
        );
        assert_eq!(
            multiple_parents.err(),
            Some(TreeError::MultipleParents(NodeId::new(3)))
        );

        let unknown_root = A11yTree::new(
            vec![node(1, TagRole::Document, vec![])],
            vec![NodeId::new(7)],
        );
        assert_eq!(
            unknown_root.err(),
            Some(TreeError::UnknownRoot(NodeId::new(7)))
        );
    }

    #[test]
    fn disconnected_cycle_is_rejected() {
        // Every node has in-degree one and there are no roots, so the cycle is
        // unreachable from any root. Validation must still reject it.
        let tree = A11yTree::new(
            vec![
                node(1, TagRole::Document, vec![NodeId::new(2)]),
                node(2, TagRole::Part, vec![NodeId::new(3)]),
                node(3, TagRole::Div, vec![NodeId::new(1)]),
            ],
            vec![],
        );
        assert_eq!(tree.err(), Some(TreeError::Cycle));
    }

    #[test]
    fn deep_chain_traversal_uses_explicit_stack() {
        const DEPTH: usize = 10_000;
        let bounds = box_at(0.0, 0.0, 1.0, 1.0);
        let nodes: Vec<A11yNode> = (0..DEPTH)
            .map(|i| {
                let id = NodeId::new((i + 1) as u64);
                let children = if i + 1 < DEPTH {
                    vec![NodeId::new((i + 2) as u64)]
                } else {
                    Vec::new()
                };
                A11yNode::new(id, TagRole::Div, 0, bounds, String::new(), children)
            })
            .collect();
        let tree = A11yTree::new(nodes, vec![NodeId::new(1)]).expect("valid deep chain");
        let traversal = tree.traversal();
        assert_eq!(traversal.len(), DEPTH);
        assert_eq!(traversal[0], NodeId::new(1));
        assert_eq!(traversal[DEPTH - 1], NodeId::new(DEPTH as u64));
    }

    #[test]
    fn empty_tree_is_valid() {
        let tree = A11yTree::empty();
        assert!(tree.is_empty());
        assert!(tree.roots().is_empty());
        assert!(tree.traversal().is_empty());
        assert!(tree.validate().is_ok());
    }
}
