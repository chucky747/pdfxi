//! Reading-order model for tagged content.
//!
//! A [`ReadingOrder`] is an acyclic tree of tagged nodes. The order in which
//! children are listed is the order in which an assistive technology presents
//! them. Construction validates the tree so that duplicate identifiers,
//! dangling child references, multiple parents, and cycles are rejected before
//! the structure reaches a consumer.

use core::fmt;
use std::collections::{HashMap, HashSet, VecDeque};

use crate::TagRole;

/// A single tagged node in a reading order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReadingOrderNode {
    /// Unique identifier of the node.
    pub id: u64,
    /// Semantic role of the node.
    pub role: TagRole,
    /// Identifiers of the node's children, in presentation order.
    pub children: Vec<u64>,
}

impl ReadingOrderNode {
    /// Creates a tagged node.
    pub fn new(id: u64, role: TagRole, children: Vec<u64>) -> Self {
        Self { id, role, children }
    }
}

/// Errors produced while validating a [`ReadingOrder`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadingOrderError {
    /// Two nodes shared an identifier.
    DuplicateNode(u64),
    /// A node listed a child that does not exist.
    UnknownChild {
        /// Identifier of the parent node.
        parent: u64,
        /// Identifier of the missing child.
        child: u64,
    },
    /// A node was referenced as a child by more than one parent.
    MultipleParents(u64),
    /// The node graph contained a cycle.
    Cycle,
}

impl fmt::Display for ReadingOrderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateNode(id) => write!(f, "duplicate reading-order node {id}"),
            Self::UnknownChild { parent, child } => {
                write!(f, "node {parent} references unknown child {child}")
            }
            Self::MultipleParents(id) => {
                write!(f, "node {id} has more than one parent")
            }
            Self::Cycle => write!(f, "reading order contains a cycle"),
        }
    }
}

impl std::error::Error for ReadingOrderError {}

/// Validates the node set and returns its root identifiers in declaration
/// order.
fn validate_nodes(nodes: &[ReadingOrderNode]) -> Result<Vec<u64>, ReadingOrderError> {
    let mut index_by_id: HashMap<u64, usize> = HashMap::with_capacity(nodes.len());
    for (index, node) in nodes.iter().enumerate() {
        if index_by_id.insert(node.id, index).is_some() {
            return Err(ReadingOrderError::DuplicateNode(node.id));
        }
    }

    let mut in_degree = vec![0usize; nodes.len()];
    let mut parent_count = vec![0usize; nodes.len()];
    for node in nodes {
        for &child in &node.children {
            let child_index = index_by_id
                .get(&child)
                .ok_or(ReadingOrderError::UnknownChild {
                    parent: node.id,
                    child,
                })?;
            in_degree[*child_index] += 1;
            parent_count[*child_index] += 1;
        }
    }

    for (index, &count) in parent_count.iter().enumerate() {
        if count > 1 {
            return Err(ReadingOrderError::MultipleParents(nodes[index].id));
        }
    }

    let roots: Vec<u64> = nodes
        .iter()
        .enumerate()
        .filter_map(|(index, node)| (in_degree[index] == 0).then_some(node.id))
        .collect();

    // Kahn's algorithm: a non-empty acyclic graph always drains completely.
    let mut queue: VecDeque<u64> = roots.iter().copied().collect();
    let mut visited = 0usize;
    while let Some(id) = queue.pop_front() {
        visited += 1;
        let Some(&index) = index_by_id.get(&id) else {
            continue;
        };
        for &child in &nodes[index].children {
            // Every child was confirmed to exist in the validation pass above.
            let Some(&child_index) = index_by_id.get(&child) else {
                continue;
            };
            in_degree[child_index] = in_degree[child_index].saturating_sub(1);
            if in_degree[child_index] == 0 {
                queue.push_back(child);
            }
        }
    }
    if visited != nodes.len() {
        return Err(ReadingOrderError::Cycle);
    }

    Ok(roots)
}

/// An acyclic tree of tagged nodes in presentation order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReadingOrder {
    nodes: Vec<ReadingOrderNode>,
    roots: Vec<u64>,
}

impl ReadingOrder {
    /// Builds and validates a reading order from its nodes.
    pub fn new(nodes: Vec<ReadingOrderNode>) -> Result<Self, ReadingOrderError> {
        let roots = validate_nodes(&nodes)?;
        Ok(Self { nodes, roots })
    }

    /// Re-validates this reading order.
    pub fn validate(&self) -> Result<(), ReadingOrderError> {
        validate_nodes(&self.nodes).map(|_| ())
    }

    /// Identifiers of the root nodes, in declaration order.
    pub fn roots(&self) -> &[u64] {
        &self.roots
    }

    /// All nodes in declaration order.
    pub fn nodes(&self) -> &[ReadingOrderNode] {
        &self.nodes
    }

    /// Returns the node with the given identifier, if present.
    pub fn node(&self, id: u64) -> Option<&ReadingOrderNode> {
        self.nodes.iter().find(|node| node.id == id)
    }

    /// Number of nodes.
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    /// Returns `true` when the reading order has no nodes.
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// Returns node identifiers in depth-first presentation order.
    ///
    /// Children are visited in the order they are declared, and each node is
    /// emitted at most once. The tree is already validated as acyclic.
    pub fn traversal(&self) -> Vec<u64> {
        let index_by_id: HashMap<u64, usize> = self
            .nodes
            .iter()
            .enumerate()
            .map(|(index, node)| (node.id, index))
            .collect();
        let mut ordered = Vec::with_capacity(self.nodes.len());
        let mut visited: HashSet<u64> = HashSet::with_capacity(self.nodes.len());

        for &root in &self.roots {
            if visited.contains(&root) {
                continue;
            }
            let mut stack = vec![root];
            while let Some(id) = stack.pop() {
                if !visited.insert(id) {
                    continue;
                }
                ordered.push(id);
                let Some(&index) = index_by_id.get(&id) else {
                    continue;
                };
                for &child in self.nodes[index].children.iter().rev() {
                    if !visited.contains(&child) {
                        stack.push(child);
                    }
                }
            }
        }
        ordered
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_reading_order_traverses_in_presentation_order() {
        let order = ReadingOrder::new(vec![
            ReadingOrderNode::new(1, TagRole::Document, vec![2, 3, 4]),
            ReadingOrderNode::new(2, TagRole::Heading1, vec![]),
            ReadingOrderNode::new(3, TagRole::Paragraph, vec![]),
            ReadingOrderNode::new(4, TagRole::List, vec![5, 6]),
            ReadingOrderNode::new(5, TagRole::ListItem, vec![]),
            ReadingOrderNode::new(6, TagRole::ListItem, vec![]),
        ])
        .expect("valid order");
        assert_eq!(order.roots(), &[1]);
        assert_eq!(order.traversal(), vec![1, 2, 3, 4, 5, 6]);
        assert_eq!(order.node(4).map(|n| n.role), Some(TagRole::List));
        assert_eq!(order.len(), 6);
        assert!(!order.is_empty());
        assert!(order.validate().is_ok());
    }

    #[test]
    fn cyclic_reading_order_is_rejected() {
        let order = ReadingOrder::new(vec![
            ReadingOrderNode::new(1, TagRole::Document, vec![2]),
            ReadingOrderNode::new(2, TagRole::Paragraph, vec![1]),
        ]);
        assert_eq!(order.err(), Some(ReadingOrderError::Cycle));

        let self_reference =
            ReadingOrder::new(vec![ReadingOrderNode::new(1, TagRole::Document, vec![1])]);
        assert_eq!(self_reference.err(), Some(ReadingOrderError::Cycle));
    }

    #[test]
    fn duplicate_ids_and_unknown_children_are_rejected() {
        let duplicate = ReadingOrder::new(vec![
            ReadingOrderNode::new(1, TagRole::Document, vec![]),
            ReadingOrderNode::new(1, TagRole::Paragraph, vec![]),
        ]);
        assert_eq!(duplicate.err(), Some(ReadingOrderError::DuplicateNode(1)));

        let unknown =
            ReadingOrder::new(vec![ReadingOrderNode::new(1, TagRole::Document, vec![99])]);
        assert_eq!(
            unknown.err(),
            Some(ReadingOrderError::UnknownChild {
                parent: 1,
                child: 99
            })
        );
    }

    #[test]
    fn multiple_parents_are_rejected() {
        let order = ReadingOrder::new(vec![
            ReadingOrderNode::new(1, TagRole::Document, vec![3]),
            ReadingOrderNode::new(2, TagRole::Part, vec![3]),
            ReadingOrderNode::new(3, TagRole::Paragraph, vec![]),
        ]);
        assert_eq!(order.err(), Some(ReadingOrderError::MultipleParents(3)));
    }

    #[test]
    fn empty_reading_order_is_valid() {
        let order = ReadingOrder::new(vec![]).expect("empty order must be valid");
        assert!(order.is_empty());
        assert!(order.roots().is_empty());
        assert!(order.traversal().is_empty());
    }
}
