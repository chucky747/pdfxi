//! Editing model for a validated [`A11yTree`]: structural edits, reading-order
//! edits, alt-text/table models, and a bounded undo/redo journal.
//!
//! [`TagEditor`] owns an [`A11yTree`] and applies copy-on-write edits. Every
//! operation builds a candidate tree, validates it, and only commits on success;
//! an invalid operation returns a typed [`EditError`] and leaves the editor
//! untouched. No operation can panic on caller-supplied identifiers, indices, or
//! node data.
//!
//! [`CoverageReport`] and [`TableModel`] are pure, deterministic views used by
//! an alt-text/table editor UI. The journal stores bounded snapshots so a host
//! can offer undo/redo without any UI code living in this crate.

use core::fmt;
use std::collections::{HashMap, HashSet, VecDeque};

use crate::{A11yNode, A11yTree, NodeId, TagRole, TreeError};

/// Default number of edit snapshots retained for undo.
pub const DEFAULT_JOURNAL_LIMIT: usize = 64;

/// Typed errors produced by editing operations.
#[derive(Debug, Clone, PartialEq)]
pub enum EditError {
    /// The requested node does not exist.
    UnknownNode(NodeId),
    /// A new node reused an identifier that is already present.
    DuplicateId(NodeId),
    /// The requested parent does not exist.
    UnknownParent(NodeId),
    /// A root identifier does not exist.
    UnknownRoot(NodeId),
    /// A move would place a node under itself or one of its descendants.
    Cycle,
    /// An insertion index was past the end of its sibling list.
    InvalidIndex {
        /// Requested index.
        index: usize,
        /// Number of elements in the target list.
        len: usize,
    },
    /// A reading-order move was structurally impossible (self-move, moving a
    /// node relative to a descendant, or demoting a first child).
    InvalidMove,
    /// The node is not attached to a root or parent (should not occur for a
    /// validated tree, but is reported defensively).
    OrphanNode(NodeId),
    /// The resulting tree failed structural validation.
    Validation(TreeError),
}

impl fmt::Display for EditError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownNode(id) => write!(f, "no accessibility node {id}"),
            Self::DuplicateId(id) => write!(f, "node id {id} is already in use"),
            Self::UnknownParent(id) => write!(f, "parent node {id} does not exist"),
            Self::UnknownRoot(id) => write!(f, "root node {id} does not exist"),
            Self::Cycle => write!(f, "move would create a cycle in the tag tree"),
            Self::InvalidIndex { index, len } => {
                write!(
                    f,
                    "index {index} is out of bounds for a list of length {len}"
                )
            }
            Self::InvalidMove => write!(f, "invalid reading-order move"),
            Self::OrphanNode(id) => write!(f, "node {id} is not attached to the tree"),
            Self::Validation(error) => write!(f, "invalid tree after edit: {error}"),
        }
    }
}

impl std::error::Error for EditError {}

/// State captured before an edit so it can be restored.
#[derive(Clone, Debug)]
struct Snapshot {
    tree: A11yTree,
    next_id: u64,
}

/// Returns the position of `id` in `nodes`, if present.
fn node_index(nodes: &[A11yNode], id: NodeId) -> Option<usize> {
    nodes.iter().position(|node| node.id == id)
}

/// Returns every descendant of `id` using an explicit, iterative walk.
fn descendants(nodes: &[A11yNode], id: NodeId) -> HashSet<NodeId> {
    let mut found = HashSet::new();
    let Some(index) = node_index(nodes, id) else {
        return found;
    };
    let mut stack: Vec<NodeId> = nodes[index].children.clone();
    while let Some(child) = stack.pop() {
        if found.insert(child)
            && let Some(child_index) = node_index(nodes, child)
        {
            stack.extend(nodes[child_index].children.iter().copied());
        }
    }
    found
}

/// Identifies the list that contains a node: a root list or a parent's children.
#[derive(Clone, Copy)]
enum Container {
    /// The tree's root list.
    Root,
    /// A parent node's children list.
    Parent(NodeId),
}

/// Returns the container and position of `id`, if attached.
fn position_in(nodes: &[A11yNode], roots: &[NodeId], id: NodeId) -> Option<(Container, usize)> {
    if let Some(position) = roots.iter().position(|&root| root == id) {
        return Some((Container::Root, position));
    }
    for node in nodes {
        if let Some(position) = node.children.iter().position(|&child| child == id) {
            return Some((Container::Parent(node.id), position));
        }
    }
    None
}

/// Removes `id` from its current container without validating.
fn detach(nodes: &mut [A11yNode], roots: &mut Vec<NodeId>, id: NodeId) -> Result<(), EditError> {
    match position_in(nodes, roots, id) {
        Some((Container::Root, position)) => {
            roots.remove(position);
            Ok(())
        }
        Some((Container::Parent(parent), position)) => {
            let index = node_index(nodes, parent).ok_or(EditError::UnknownParent(parent))?;
            nodes[index].children.remove(position);
            Ok(())
        }
        None => Err(EditError::OrphanNode(id)),
    }
}

/// Inserts `id` into a container at `position`, with a bounds check.
fn insert_into(
    nodes: &mut [A11yNode],
    roots: &mut Vec<NodeId>,
    container: Container,
    position: usize,
    id: NodeId,
) -> Result<(), EditError> {
    match container {
        Container::Root => {
            if position > roots.len() {
                return Err(EditError::InvalidIndex {
                    index: position,
                    len: roots.len(),
                });
            }
            roots.insert(position, id);
        }
        Container::Parent(parent) => {
            let index = node_index(nodes, parent).ok_or(EditError::UnknownParent(parent))?;
            if position > nodes[index].children.len() {
                return Err(EditError::InvalidIndex {
                    index: position,
                    len: nodes[index].children.len(),
                });
            }
            nodes[index].children.insert(position, id);
        }
    }
    Ok(())
}

/// A bounded, deterministic editor over an [`A11yTree`].
///
/// Identifiers for newly inserted nodes are allocated monotonically from the
/// largest id currently in the tree, so repeated edit sequences are stable and
/// freed ids are never reused.
#[derive(Clone, Debug)]
pub struct TagEditor {
    tree: A11yTree,
    next_id: u64,
    undo: VecDeque<Snapshot>,
    redo: Vec<Snapshot>,
    limit: usize,
}

impl TagEditor {
    /// Creates an editor over `tree` with the default journal limit.
    pub fn new(tree: A11yTree) -> Self {
        let highest = tree
            .nodes()
            .iter()
            .map(|node| node.id.get())
            .max()
            .unwrap_or(0);
        Self {
            tree,
            next_id: highest.saturating_add(1),
            undo: VecDeque::new(),
            redo: Vec::new(),
            limit: DEFAULT_JOURNAL_LIMIT,
        }
    }

    /// Creates an editor that retains at most `limit` undo snapshots.
    ///
    /// A limit of zero disables undo/redo while edits still work.
    pub fn with_journal_limit(mut self, limit: usize) -> Self {
        self.limit = limit;
        self.undo.truncate(limit);
        self.redo.truncate(limit);
        self
    }

    /// Returns the current, always-valid tree.
    pub fn tree(&self) -> &A11yTree {
        &self.tree
    }

    /// Returns the next identifier that [`Self::insert_node`] would prefer.
    ///
    /// This is a read-only peek; it does not reserve the id.
    pub fn peek_id(&self) -> NodeId {
        NodeId::new(self.next_id)
    }

    /// Returns the configured journal limit.
    pub const fn journal_limit(&self) -> usize {
        self.limit
    }

    /// Returns the number of retained undo snapshots.
    pub fn undo_len(&self) -> usize {
        self.undo.len()
    }

    /// Returns the number of retained redo snapshots.
    pub fn redo_len(&self) -> usize {
        self.redo.len()
    }

    /// Returns `true` when [`Self::undo`] would succeed.
    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    /// Returns `true` when [`Self::redo`] would succeed.
    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// Records the current state as an undo point and installs a candidate.
    fn commit(
        &mut self,
        nodes: Vec<A11yNode>,
        roots: Vec<NodeId>,
        next_id: u64,
    ) -> Result<(), EditError> {
        let tree = A11yTree::new(nodes, roots).map_err(EditError::Validation)?;
        if self.limit > 0 {
            self.undo.push_back(Snapshot {
                tree: self.tree.clone(),
                next_id: self.next_id,
            });
            while self.undo.len() > self.limit {
                self.undo.pop_front();
            }
        }
        self.tree = tree;
        self.next_id = next_id;
        self.redo.clear();
        Ok(())
    }

    fn nodes(&self) -> Vec<A11yNode> {
        self.tree.nodes().to_vec()
    }

    fn roots(&self) -> Vec<NodeId> {
        self.tree.roots().to_vec()
    }

    /// Inserts a node as a child of `parent` (or as a root when `None`) at
    /// `index`, appending when `index` is `None`.
    ///
    /// Returns the inserted node's id. A reused id, unknown parent, out-of-range
    /// index, or a resulting invalid tree is reported as a typed error.
    pub fn insert_node(
        &mut self,
        node: A11yNode,
        parent: Option<NodeId>,
        index: Option<usize>,
    ) -> Result<NodeId, EditError> {
        let id = node.id;
        let mut nodes = self.nodes();
        let mut roots = self.roots();
        if node_index(&nodes, id).is_some() {
            return Err(EditError::DuplicateId(id));
        }
        let container = match parent {
            Some(parent_id) => {
                if node_index(&nodes, parent_id).is_none() {
                    return Err(EditError::UnknownParent(parent_id));
                }
                Container::Parent(parent_id)
            }
            None => Container::Root,
        };
        let target_len = match container {
            Container::Root => roots.len(),
            Container::Parent(parent_id) => {
                let index =
                    node_index(&nodes, parent_id).ok_or(EditError::UnknownParent(parent_id))?;
                nodes[index].children.len()
            }
        };
        let position = index.unwrap_or(target_len);
        insert_into(&mut nodes, &mut roots, container, position, id)?;
        nodes.push(node);
        let new_next = id.get().saturating_add(1).max(self.next_id);
        self.commit(nodes, roots, new_next)?;
        Ok(id)
    }

    /// Removes a node, promoting its children into its own position so the tree
    /// stays valid and sibling order is preserved.
    ///
    /// Returns the removed node as it appeared before the edit.
    pub fn remove_node(&mut self, id: NodeId) -> Result<A11yNode, EditError> {
        let mut nodes = self.nodes();
        let mut roots = self.roots();
        let index = node_index(&nodes, id).ok_or(EditError::UnknownNode(id))?;
        let removed = nodes[index].clone();
        let children = removed.children.clone();
        let (container, position) =
            position_in(&nodes, &roots, id).ok_or(EditError::OrphanNode(id))?;
        // Detach the node first, then splice its children into the freed slot.
        match container {
            Container::Root => {
                roots.remove(position);
                for (offset, child) in children.iter().copied().enumerate() {
                    roots.insert(position.saturating_add(offset), child);
                }
            }
            Container::Parent(parent) => {
                let parent_index =
                    node_index(&nodes, parent).ok_or(EditError::UnknownParent(parent))?;
                nodes[parent_index].children.remove(position);
                for (offset, child) in children.iter().copied().enumerate() {
                    nodes[parent_index]
                        .children
                        .insert(position.saturating_add(offset), child);
                }
            }
        }
        let current_index = node_index(&nodes, id).ok_or(EditError::UnknownNode(id))?;
        nodes.remove(current_index);
        let next_id = self.next_id;
        self.commit(nodes, roots, next_id)?;
        Ok(removed)
    }

    /// Moves `id` into `new_parent` (or the root list) at `index`.
    ///
    /// Reparenting under the node itself or a descendant is rejected as a
    /// [`EditError::Cycle`] before any change is committed.
    pub fn move_node(
        &mut self,
        id: NodeId,
        new_parent: Option<NodeId>,
        index: usize,
    ) -> Result<(), EditError> {
        let mut nodes = self.nodes();
        let mut roots = self.roots();
        if node_index(&nodes, id).is_none() {
            return Err(EditError::UnknownNode(id));
        }
        if let Some(parent) = new_parent {
            if parent == id {
                return Err(EditError::Cycle);
            }
            if node_index(&nodes, parent).is_none() {
                return Err(EditError::UnknownParent(parent));
            }
            if descendants(&nodes, id).contains(&parent) {
                return Err(EditError::Cycle);
            }
        }
        detach(&mut nodes, &mut roots, id)?;
        let container = match new_parent {
            Some(parent) => Container::Parent(parent),
            None => Container::Root,
        };
        insert_into(&mut nodes, &mut roots, container, index, id)?;
        let next_id = self.next_id;
        self.commit(nodes, roots, next_id)
    }

    /// Reparents `id` under `new_parent`, appending it as the last child.
    pub fn reparent(&mut self, id: NodeId, new_parent: NodeId) -> Result<(), EditError> {
        let nodes = self.nodes();
        let index = node_index(&nodes, new_parent).ok_or(EditError::UnknownParent(new_parent))?;
        let last = nodes[index].children.len();
        self.move_node(id, Some(new_parent), last)
    }

    /// Changes the semantic role of a node.
    pub fn change_role(&mut self, id: NodeId, role: TagRole) -> Result<(), EditError> {
        let mut nodes = self.nodes();
        let index = node_index(&nodes, id).ok_or(EditError::UnknownNode(id))?;
        nodes[index].role = role;
        let roots = self.roots();
        let next_id = self.next_id;
        self.commit(nodes, roots, next_id)
    }

    /// Sets (`Some`) or clears (`None`) a node's alternative text.
    pub fn set_alt_text(&mut self, id: NodeId, text: Option<String>) -> Result<(), EditError> {
        let mut nodes = self.nodes();
        let index = node_index(&nodes, id).ok_or(EditError::UnknownNode(id))?;
        nodes[index].alt_text = text;
        let roots = self.roots();
        let next_id = self.next_id;
        self.commit(nodes, roots, next_id)
    }

    /// Sets (`Some`) or clears (`None`) a node's `/ActualText` replacement text.
    pub fn set_actual_text(&mut self, id: NodeId, text: Option<String>) -> Result<(), EditError> {
        let mut nodes = self.nodes();
        let index = node_index(&nodes, id).ok_or(EditError::UnknownNode(id))?;
        nodes[index].actual_text = text;
        let roots = self.roots();
        let next_id = self.next_id;
        self.commit(nodes, roots, next_id)
    }

    /// Sets (`Some`) or clears (`None`) a node's `/E` expansion text.
    pub fn set_expansion(&mut self, id: NodeId, text: Option<String>) -> Result<(), EditError> {
        let mut nodes = self.nodes();
        let index = node_index(&nodes, id).ok_or(EditError::UnknownNode(id))?;
        nodes[index].expansion = text;
        let roots = self.roots();
        let next_id = self.next_id;
        self.commit(nodes, roots, next_id)
    }

    /// Sets or clears a node's decorative-artifact flag.
    pub fn mark_artifact(&mut self, id: NodeId, artifact: bool) -> Result<(), EditError> {
        let mut nodes = self.nodes();
        let index = node_index(&nodes, id).ok_or(EditError::UnknownNode(id))?;
        nodes[index].artifact = artifact;
        let roots = self.roots();
        let next_id = self.next_id;
        self.commit(nodes, roots, next_id)
    }

    /// Moves `id` so it immediately precedes `target` in reading order.
    pub fn move_before(&mut self, id: NodeId, target: NodeId) -> Result<(), EditError> {
        self.reorder_relative(id, target, false)
    }

    /// Moves `id` so it immediately follows `target` in reading order.
    pub fn move_after(&mut self, id: NodeId, target: NodeId) -> Result<(), EditError> {
        self.reorder_relative(id, target, true)
    }

    fn reorder_relative(
        &mut self,
        id: NodeId,
        target: NodeId,
        after: bool,
    ) -> Result<(), EditError> {
        let mut nodes = self.nodes();
        let mut roots = self.roots();
        if node_index(&nodes, id).is_none() {
            return Err(EditError::UnknownNode(id));
        }
        if node_index(&nodes, target).is_none() {
            return Err(EditError::UnknownNode(target));
        }
        if id == target || descendants(&nodes, id).contains(&target) {
            return Err(EditError::InvalidMove);
        }
        detach(&mut nodes, &mut roots, id)?;
        let (container, position) =
            position_in(&nodes, &roots, target).ok_or(EditError::UnknownNode(target))?;
        let insert_at = if after {
            position.saturating_add(1)
        } else {
            position
        };
        insert_into(&mut nodes, &mut roots, container, insert_at, id)?;
        let next_id = self.next_id;
        self.commit(nodes, roots, next_id)
    }

    /// Promotes `id` one level: it becomes a sibling of its former parent,
    /// placed immediately after that parent. Root nodes are a no-op.
    pub fn promote(&mut self, id: NodeId) -> Result<(), EditError> {
        let mut nodes = self.nodes();
        let mut roots = self.roots();
        let (container, position) =
            position_in(&nodes, &roots, id).ok_or(EditError::OrphanNode(id))?;
        let parent = match container {
            Container::Root => return Ok(()),
            Container::Parent(parent) => parent,
        };
        let (parent_container, parent_position) =
            position_in(&nodes, &roots, parent).ok_or(EditError::OrphanNode(parent))?;
        let parent_index = node_index(&nodes, parent).ok_or(EditError::UnknownParent(parent))?;
        nodes[parent_index].children.remove(position);
        let insert_at = parent_position.saturating_add(1);
        insert_into(&mut nodes, &mut roots, parent_container, insert_at, id)?;
        let next_id = self.next_id;
        self.commit(nodes, roots, next_id)
    }

    /// Demotes `id` one level under its immediately preceding sibling.
    ///
    /// A first child has no preceding sibling and is rejected with
    /// [`EditError::InvalidMove`].
    pub fn demote(&mut self, id: NodeId) -> Result<(), EditError> {
        let mut nodes = self.nodes();
        let mut roots = self.roots();
        let (container, position) =
            position_in(&nodes, &roots, id).ok_or(EditError::OrphanNode(id))?;
        if position == 0 {
            return Err(EditError::InvalidMove);
        }
        let new_parent = match container {
            Container::Root => *roots.get(position - 1).ok_or(EditError::InvalidMove)?,
            Container::Parent(parent) => {
                let index = node_index(&nodes, parent).ok_or(EditError::UnknownParent(parent))?;
                *nodes[index]
                    .children
                    .get(position - 1)
                    .ok_or(EditError::InvalidMove)?
            }
        };
        detach(&mut nodes, &mut roots, id)?;
        let parent_index =
            node_index(&nodes, new_parent).ok_or(EditError::UnknownParent(new_parent))?;
        nodes[parent_index].children.push(id);
        let next_id = self.next_id;
        self.commit(nodes, roots, next_id)
    }

    /// Reorders siblings and roots by ascending page index using a stable sort,
    /// so nodes on the same page keep their relative order.
    ///
    /// Returns `true` when the order changed and an undo point was recorded.
    pub fn normalize_page_order(&mut self) -> Result<bool, EditError> {
        let mut nodes = self.nodes();
        let mut roots = self.roots();
        let before_nodes = nodes.clone();
        let before_roots = roots.clone();
        let page_by_id: HashMap<NodeId, usize> =
            nodes.iter().map(|node| (node.id, node.page)).collect();
        roots.sort_by_key(|id| page_by_id.get(id).copied().unwrap_or(0));
        for node in &mut nodes {
            node.children
                .sort_by_key(|id| page_by_id.get(id).copied().unwrap_or(0));
        }
        if nodes == before_nodes && roots == before_roots {
            return Ok(false);
        }
        let next_id = self.next_id;
        self.commit(nodes, roots, next_id)?;
        Ok(true)
    }

    /// Undoes the most recent edit, returning `false` when the journal is empty.
    pub fn undo(&mut self) -> Result<bool, EditError> {
        let Some(snapshot) = self.undo.pop_back() else {
            return Ok(false);
        };
        self.redo.push(Snapshot {
            tree: self.tree.clone(),
            next_id: self.next_id,
        });
        while self.redo.len() > self.limit {
            self.redo.remove(0);
        }
        self.tree = snapshot.tree;
        self.next_id = snapshot.next_id;
        Ok(true)
    }

    /// Redoes the most recently undone edit, returning `false` when empty.
    pub fn redo(&mut self) -> Result<bool, EditError> {
        let Some(snapshot) = self.redo.pop() else {
            return Ok(false);
        };
        if self.limit > 0 {
            self.undo.push_back(Snapshot {
                tree: self.tree.clone(),
                next_id: self.next_id,
            });
            while self.undo.len() > self.limit {
                self.undo.pop_front();
            }
        }
        self.tree = snapshot.tree;
        self.next_id = snapshot.next_id;
        Ok(true)
    }
}

/// State of a node's alternative-text requirement.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AltState {
    /// The node's role does not require alternative text.
    NotRequired,
    /// Alternative text is required but absent.
    Missing,
    /// Alternative text is present but empty.
    Empty,
    /// Non-empty alternative text is present.
    Present,
}

/// One node's contribution to an alt-text coverage report.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AltRequirement {
    /// Node being reported.
    pub id: NodeId,
    /// Whether this role requires alternative text.
    pub required: bool,
    /// Coverage state of the node.
    pub state: AltState,
}

/// Deterministic alternative-text coverage over an entire tree.
#[derive(Clone, Debug, PartialEq)]
pub struct CoverageReport {
    /// Requirements sorted by node id.
    pub entries: Vec<AltRequirement>,
    /// Number of nodes whose role requires alternative text.
    pub required_count: usize,
    /// Number of required nodes with non-empty alternative text.
    pub satisfied_count: usize,
    /// Fraction of required nodes that are satisfied, in `0.0..=1.0`.
    ///
    /// This is `1.0` when nothing requires alternative text.
    pub coverage: f64,
}

/// Returns `true` when `role` requires alternative text for coverage.
pub const fn requires_alt_text(role: TagRole) -> bool {
    matches!(role, TagRole::Figure | TagRole::Image)
}

/// Computes a deterministic alt-text coverage report for `tree`.
pub fn coverage_report(tree: &A11yTree) -> CoverageReport {
    let mut entries: Vec<AltRequirement> = tree
        .nodes()
        .iter()
        .map(|node| {
            let required = requires_alt_text(node.role);
            let state = if !required {
                AltState::NotRequired
            } else {
                match &node.alt_text {
                    None => AltState::Missing,
                    Some(text) if text.is_empty() => AltState::Empty,
                    Some(_) => AltState::Present,
                }
            };
            AltRequirement {
                id: node.id,
                required,
                state,
            }
        })
        .collect();
    entries.sort_by_key(|entry| entry.id);
    let required_count = entries.iter().filter(|entry| entry.required).count();
    let satisfied_count = entries
        .iter()
        .filter(|entry| entry.state == AltState::Present)
        .count();
    let coverage = if required_count == 0 {
        1.0
    } else {
        satisfied_count as f64 / required_count as f64
    };
    CoverageReport {
        entries,
        required_count,
        satisfied_count,
        coverage,
    }
}

/// Kind of a table cell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CellKind {
    /// A header cell (`TH` / `TableHeader`).
    Header,
    /// A data cell (`TD` / `TableData`).
    Data,
}

/// PDF header-cell scope (`/Scope`) values.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HeaderScope {
    /// Header applies to the current row.
    Row,
    /// Header applies to the current column.
    Column,
    /// Header applies to the current row group.
    RowGroup,
    /// Header applies to the current column group.
    ColumnGroup,
}

impl HeaderScope {
    /// Returns the stable lowercase scope name.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Row => "row",
            Self::Column => "column",
            Self::RowGroup => "rowgroup",
            Self::ColumnGroup => "columngroup",
        }
    }

    /// Parses a scope name, returning `None` for unknown values.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "row" => Some(Self::Row),
            "column" => Some(Self::Column),
            "rowgroup" => Some(Self::RowGroup),
            "columngroup" => Some(Self::ColumnGroup),
            _ => None,
        }
    }
}

/// A single cell in an editable table model.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TableCell {
    /// Accessibility node id backing this cell.
    pub id: NodeId,
    /// Whether the cell is a header or a data cell.
    pub kind: CellKind,
    /// Zero-based row index.
    pub row: usize,
    /// Zero-based column index within the row.
    pub column: usize,
    /// Header scope, meaningful only for header cells.
    pub scope: Option<HeaderScope>,
    /// Header cells associated with this cell, in declaration order.
    pub headers: Vec<NodeId>,
}

/// Typed errors for constructing and validating a [`TableModel`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TableError {
    /// The requested table node does not exist.
    TableNotFound(NodeId),
    /// The node is not tagged as a table.
    NotATable(NodeId),
    /// A cell id was added more than once.
    DuplicateCell(NodeId),
    /// A referenced cell does not exist in the model.
    UnknownCell(NodeId),
    /// A header reference pointed at a data cell.
    NotAHeader(NodeId),
    /// A scope was assigned to a data cell.
    ScopeOnData(NodeId),
    /// A header cell has no scope.
    MissingScope(NodeId),
    /// A data cell references a header not present in the model.
    UnknownHeader {
        /// Cell holding the bad reference.
        cell: NodeId,
        /// Referenced header id.
        header: NodeId,
    },
    /// The same header was associated twice with one cell.
    DuplicateAssociation {
        /// Cell holding the duplicate.
        cell: NodeId,
        /// Duplicated header id.
        header: NodeId,
    },
    /// The table has no cells.
    EmptyTable,
}

impl fmt::Display for TableError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TableNotFound(id) => write!(f, "no table node {id}"),
            Self::NotATable(id) => write!(f, "node {id} is not a table"),
            Self::DuplicateCell(id) => write!(f, "duplicate table cell {id}"),
            Self::UnknownCell(id) => write!(f, "unknown table cell {id}"),
            Self::NotAHeader(id) => write!(f, "table cell {id} is not a header"),
            Self::ScopeOnData(id) => write!(f, "data cell {id} cannot have a header scope"),
            Self::MissingScope(id) => write!(f, "header cell {id} has no scope"),
            Self::UnknownHeader { cell, header } => {
                write!(f, "cell {cell} references unknown header {header}")
            }
            Self::DuplicateAssociation { cell, header } => {
                write!(f, "cell {cell} references header {header} twice")
            }
            Self::EmptyTable => write!(f, "table has no cells"),
        }
    }
}

impl std::error::Error for TableError {}

/// Deterministic result of validating a [`TableModel`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TableValidation {
    /// Number of rows containing cells.
    pub rows: usize,
    /// Largest column index plus one.
    pub columns: usize,
    /// Number of header cells.
    pub header_count: usize,
    /// Number of data cells.
    pub data_count: usize,
    /// Number of data cells with at least one associated header.
    pub associated_data_count: usize,
}

/// An editable table header/association model built from an [`A11yTree`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TableModel {
    table_id: NodeId,
    cells: Vec<TableCell>,
}

impl TableModel {
    /// Creates an empty table model for `table_id`.
    pub fn new(table_id: NodeId) -> Self {
        Self {
            table_id,
            cells: Vec::new(),
        }
    }

    /// Builds a model from the `TableRow`/`TableHeader`/`TableData` descendants
    /// of `table_id` in presentation order.
    ///
    /// Scopes and associations start empty; callers set them with
    /// [`Self::set_scope`] and [`Self::associate_header`].
    pub fn from_tree(tree: &A11yTree, table_id: NodeId) -> Result<Self, TableError> {
        let table = tree
            .node(table_id)
            .ok_or(TableError::TableNotFound(table_id))?;
        if table.role != TagRole::Table {
            return Err(TableError::NotATable(table_id));
        }
        let mut cells = Vec::new();
        let mut row = 0usize;
        for &row_id in &table.children {
            let Some(row_node) = tree.node(row_id) else {
                continue;
            };
            if row_node.role != TagRole::TableRow {
                continue;
            }
            let mut column = 0usize;
            for &cell_id in &row_node.children {
                let Some(cell_node) = tree.node(cell_id) else {
                    continue;
                };
                let kind = match cell_node.role {
                    TagRole::TableHeader => CellKind::Header,
                    TagRole::TableData => CellKind::Data,
                    _ => continue,
                };
                cells.push(TableCell {
                    id: cell_id,
                    kind,
                    row,
                    column,
                    scope: None,
                    headers: Vec::new(),
                });
                column = column.saturating_add(1);
            }
            row = row.saturating_add(1);
        }
        Ok(Self { table_id, cells })
    }

    /// Returns the id of the backing table node.
    pub const fn table_id(&self) -> NodeId {
        self.table_id
    }

    /// Returns the cells in row-major presentation order.
    pub fn cells(&self) -> &[TableCell] {
        &self.cells
    }

    /// Adds a cell, rejecting duplicate ids.
    pub fn add_cell(&mut self, cell: TableCell) -> Result<(), TableError> {
        if self.cells.iter().any(|existing| existing.id == cell.id) {
            return Err(TableError::DuplicateCell(cell.id));
        }
        self.cells.push(cell);
        Ok(())
    }

    /// Assigns a scope to a header cell.
    pub fn set_scope(&mut self, id: NodeId, scope: HeaderScope) -> Result<(), TableError> {
        let index = self
            .cells
            .iter()
            .position(|cell| cell.id == id)
            .ok_or(TableError::UnknownCell(id))?;
        if self.cells[index].kind != CellKind::Header {
            return Err(TableError::ScopeOnData(id));
        }
        self.cells[index].scope = Some(scope);
        Ok(())
    }

    /// Associates a header cell with another cell.
    pub fn associate_header(&mut self, id: NodeId, header: NodeId) -> Result<(), TableError> {
        let index = self
            .cells
            .iter()
            .position(|cell| cell.id == id)
            .ok_or(TableError::UnknownCell(id))?;
        if !self.cells.iter().any(|cell| cell.id == header) {
            return Err(TableError::UnknownHeader { cell: id, header });
        }
        if self.cells[index].headers.contains(&header) {
            return Err(TableError::DuplicateAssociation { cell: id, header });
        }
        self.cells[index].headers.push(header);
        Ok(())
    }

    /// Validates scopes and header associations.
    pub fn validate(&self) -> Result<TableValidation, TableError> {
        if self.cells.is_empty() {
            return Err(TableError::EmptyTable);
        }
        let header_ids: HashSet<NodeId> = self
            .cells
            .iter()
            .filter(|cell| cell.kind == CellKind::Header)
            .map(|cell| cell.id)
            .collect();
        for cell in &self.cells {
            if cell.kind == CellKind::Header && cell.scope.is_none() {
                return Err(TableError::MissingScope(cell.id));
            }
            let mut seen = HashSet::new();
            for &header in &cell.headers {
                if !header_ids.contains(&header) {
                    return Err(TableError::UnknownHeader {
                        cell: cell.id,
                        header,
                    });
                }
                if !seen.insert(header) {
                    return Err(TableError::DuplicateAssociation {
                        cell: cell.id,
                        header,
                    });
                }
            }
        }
        let rows = self
            .cells
            .iter()
            .map(|cell| cell.row)
            .max()
            .map(|value| value.saturating_add(1))
            .unwrap_or(0);
        let columns = self
            .cells
            .iter()
            .map(|cell| cell.column)
            .max()
            .map(|value| value.saturating_add(1))
            .unwrap_or(0);
        let header_count = header_ids.len();
        let data_count = self.cells.len() - header_count;
        let associated_data_count = self
            .cells
            .iter()
            .filter(|cell| cell.kind == CellKind::Data && !cell.headers.is_empty())
            .count();
        Ok(TableValidation {
            rows,
            columns,
            header_count,
            data_count,
            associated_data_count,
        })
    }

    /// Fraction of data cells with at least one header, in `0.0..=1.0`.
    ///
    /// This is `1.0` when the table has no data cells.
    pub fn header_coverage(&self) -> f64 {
        let data_count = self
            .cells
            .iter()
            .filter(|cell| cell.kind == CellKind::Data)
            .count();
        if data_count == 0 {
            return 1.0;
        }
        let associated = self
            .cells
            .iter()
            .filter(|cell| cell.kind == CellKind::Data && !cell.headers.is_empty())
            .count();
        associated as f64 / data_count as f64
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::BBox;

    fn bounds() -> BBox {
        match BBox::new(0.0, 0.0, 10.0, 10.0) {
            Ok(value) => value,
            Err(_) => panic!("test box must be valid"),
        }
    }

    fn node(id: u64, role: TagRole, children: Vec<NodeId>) -> A11yNode {
        A11yNode::new(NodeId::new(id), role, 0, bounds(), String::new(), children)
    }

    fn sample() -> A11yTree {
        let nodes = vec![
            node(1, TagRole::Document, vec![NodeId::new(2)]),
            node(2, TagRole::Part, vec![NodeId::new(3), NodeId::new(4)]),
            node(3, TagRole::Paragraph, vec![]),
            node(4, TagRole::Figure, vec![]),
        ];
        match A11yTree::new(nodes, vec![NodeId::new(1)]) {
            Ok(tree) => tree,
            Err(error) => panic!("sample tree must be valid: {error}"),
        }
    }

    fn traversal(editor: &TagEditor) -> Vec<NodeId> {
        editor.tree().traversal()
    }

    #[test]
    fn insert_node_appends_and_allocates_stable_ids() {
        let mut editor = TagEditor::new(sample());
        assert_eq!(editor.peek_id(), NodeId::new(5));
        let inserted = editor
            .insert_node(node(9, TagRole::Span, vec![]), Some(NodeId::new(2)), None)
            .expect("insert");
        assert_eq!(inserted, NodeId::new(9));
        assert_eq!(editor.peek_id(), NodeId::new(10));
        assert_eq!(
            editor
                .tree()
                .node(NodeId::new(2))
                .map(|n| n.children.clone()),
            Some(vec![NodeId::new(3), NodeId::new(4), NodeId::new(9)])
        );
    }

    #[test]
    fn insert_node_rejects_bad_inputs_without_mutation() {
        let mut editor = TagEditor::new(sample());
        assert_eq!(
            editor.insert_node(node(3, TagRole::Span, vec![]), None, None),
            Err(EditError::DuplicateId(NodeId::new(3)))
        );
        assert_eq!(
            editor.insert_node(node(8, TagRole::Span, vec![]), Some(NodeId::new(99)), None),
            Err(EditError::UnknownParent(NodeId::new(99)))
        );
        assert_eq!(
            editor.insert_node(node(8, TagRole::Span, vec![]), None, Some(2)),
            Err(EditError::InvalidIndex { index: 2, len: 1 })
        );
        assert_eq!(editor.tree().len(), 4);
        assert!(!editor.can_undo());
    }

    #[test]
    fn duplicate_id_rejection() {
        let mut editor = TagEditor::new(sample());
        let before = editor.tree().clone();
        assert_eq!(
            editor.insert_node(node(4, TagRole::Span, vec![]), Some(NodeId::new(1)), None),
            Err(EditError::DuplicateId(NodeId::new(4)))
        );
        assert_eq!(*editor.tree(), before);
    }

    #[test]
    fn remove_node_promotes_children_and_preserves_order() {
        let tree = A11yTree::new(
            vec![
                node(1, TagRole::Document, vec![NodeId::new(2)]),
                node(
                    2,
                    TagRole::Part,
                    vec![NodeId::new(3), NodeId::new(5), NodeId::new(4)],
                ),
                node(3, TagRole::Paragraph, vec![]),
                node(5, TagRole::Span, vec![NodeId::new(6)]),
                node(6, TagRole::Span, vec![]),
                node(4, TagRole::Figure, vec![]),
            ],
            vec![NodeId::new(1)],
        )
        .expect("tree");
        let mut editor = TagEditor::new(tree);
        let removed = editor.remove_node(NodeId::new(5)).expect("remove");
        assert_eq!(removed.id, NodeId::new(5));
        assert_eq!(
            editor
                .tree()
                .node(NodeId::new(2))
                .map(|n| n.children.clone()),
            Some(vec![NodeId::new(3), NodeId::new(6), NodeId::new(4)])
        );
        assert!(editor.tree().validate().is_ok());
    }

    #[test]
    fn move_node_reorders_siblings_preserving_order() {
        let mut editor = TagEditor::new(sample());
        editor
            .move_node(NodeId::new(4), Some(NodeId::new(2)), 0)
            .expect("move");
        assert_eq!(
            editor
                .tree()
                .node(NodeId::new(2))
                .map(|n| n.children.clone()),
            Some(vec![NodeId::new(4), NodeId::new(3)])
        );
        assert_eq!(
            traversal(&editor),
            vec![
                NodeId::new(1),
                NodeId::new(2),
                NodeId::new(4),
                NodeId::new(3)
            ]
        );
    }

    #[test]
    fn reparent_cycle_rejected() {
        let mut editor = TagEditor::new(sample());
        let before = editor.tree().clone();
        assert_eq!(
            editor.reparent(NodeId::new(1), NodeId::new(3)),
            Err(EditError::Cycle)
        );
        assert_eq!(
            editor.reparent(NodeId::new(2), NodeId::new(2)),
            Err(EditError::Cycle)
        );
        assert_eq!(*editor.tree(), before);
    }

    #[test]
    fn change_role_updates_node() {
        let mut editor = TagEditor::new(sample());
        editor
            .change_role(NodeId::new(3), TagRole::Heading2)
            .expect("role");
        assert_eq!(
            editor.tree().node(NodeId::new(3)).map(|n| n.role),
            Some(TagRole::Heading2)
        );
        assert_eq!(
            editor.change_role(NodeId::new(99), TagRole::Span),
            Err(EditError::UnknownNode(NodeId::new(99)))
        );
    }

    #[test]
    fn alt_text_set_and_clear() {
        let mut editor = TagEditor::new(sample());
        editor
            .set_alt_text(NodeId::new(4), Some("chart".into()))
            .expect("set");
        assert_eq!(
            editor
                .tree()
                .node(NodeId::new(4))
                .and_then(|n| n.alt_text.clone()),
            Some("chart".into())
        );
        editor.set_alt_text(NodeId::new(4), None).expect("clear");
        assert_eq!(
            editor
                .tree()
                .node(NodeId::new(4))
                .and_then(|n| n.alt_text.clone()),
            None
        );
    }

    #[test]
    fn actual_text_and_expansion_round_trip() {
        let mut editor = TagEditor::new(sample());
        editor
            .set_actual_text(NodeId::new(3), Some("fb".into()))
            .expect("actual");
        editor
            .set_expansion(NodeId::new(3), Some("feedback".into()))
            .expect("expansion");
        let node = editor.tree().node(NodeId::new(3)).expect("node");
        assert_eq!(node.actual_text.as_deref(), Some("fb"));
        assert_eq!(node.expansion.as_deref(), Some("feedback"));
    }

    #[test]
    fn artifact_marking_toggles_flag() {
        let mut editor = TagEditor::new(sample());
        editor.mark_artifact(NodeId::new(4), true).expect("mark");
        assert!(
            editor
                .tree()
                .node(NodeId::new(4))
                .is_some_and(|n| n.artifact)
        );
        editor.mark_artifact(NodeId::new(4), false).expect("unmark");
        assert!(
            editor
                .tree()
                .node(NodeId::new(4))
                .is_none_or(|n| n.artifact)
        );
    }

    #[test]
    fn reading_order_move_before_and_move_after() {
        let mut editor = TagEditor::new(sample());
        editor
            .move_before(NodeId::new(4), NodeId::new(3))
            .expect("before");
        assert_eq!(
            editor
                .tree()
                .node(NodeId::new(2))
                .map(|n| n.children.clone()),
            Some(vec![NodeId::new(4), NodeId::new(3)])
        );
        editor
            .move_after(NodeId::new(4), NodeId::new(3))
            .expect("after");
        assert_eq!(
            editor
                .tree()
                .node(NodeId::new(2))
                .map(|n| n.children.clone()),
            Some(vec![NodeId::new(3), NodeId::new(4)])
        );
    }

    #[test]
    fn invalid_move_is_typed() {
        let mut editor = TagEditor::new(sample());
        assert_eq!(
            editor.move_before(NodeId::new(2), NodeId::new(2)),
            Err(EditError::InvalidMove)
        );
        assert_eq!(
            editor.move_after(NodeId::new(2), NodeId::new(3)),
            Err(EditError::InvalidMove)
        );
        assert_eq!(
            editor.move_before(NodeId::new(99), NodeId::new(3)),
            Err(EditError::UnknownNode(NodeId::new(99)))
        );
        assert!(!editor.can_undo());
    }

    #[test]
    fn promote_and_demote_change_levels() {
        let mut editor = TagEditor::new(sample());
        editor.promote(NodeId::new(3)).expect("promote");
        assert_eq!(
            editor
                .tree()
                .node(NodeId::new(1))
                .map(|n| n.children.clone()),
            Some(vec![NodeId::new(2), NodeId::new(3)])
        );
        editor.demote(NodeId::new(3)).expect("demote");
        assert_eq!(
            editor
                .tree()
                .node(NodeId::new(2))
                .map(|n| n.children.clone()),
            Some(vec![NodeId::new(4), NodeId::new(3)])
        );
        // Node 4 is now the first child, so it cannot be demoted.
        assert_eq!(editor.demote(NodeId::new(4)), Err(EditError::InvalidMove));
    }

    #[test]
    fn page_order_normalization_is_stable() {
        let mut root = node(1, TagRole::Document, vec![NodeId::new(2), NodeId::new(3)]);
        let mut a = node(2, TagRole::Paragraph, vec![]);
        let mut b = node(3, TagRole::Paragraph, vec![]);
        a.page = 2;
        b.page = 1;
        root.children = vec![NodeId::new(2), NodeId::new(3)];
        let tree = A11yTree::new(vec![root, a, b], vec![NodeId::new(1)]).expect("tree");
        let mut editor = TagEditor::new(tree);
        assert!(editor.normalize_page_order().expect("normalize"));
        assert_eq!(
            editor
                .tree()
                .node(NodeId::new(1))
                .map(|n| n.children.clone()),
            Some(vec![NodeId::new(3), NodeId::new(2)])
        );
        assert!(!editor.normalize_page_order().expect("idempotent"));
    }

    #[test]
    fn coverage_report_detects_required_empty_and_missing() {
        let mut with_alt = node(1, TagRole::Figure, vec![]);
        with_alt.alt_text = Some("described".into());
        let empty_alt = node(2, TagRole::Image, vec![]).with_alt_text("");
        let missing = node(3, TagRole::Image, vec![]);
        let text = node(4, TagRole::Paragraph, vec![]);
        let tree = A11yTree::new(
            vec![with_alt, empty_alt, missing, text],
            vec![
                NodeId::new(1),
                NodeId::new(2),
                NodeId::new(3),
                NodeId::new(4),
            ],
        )
        .expect("tree");
        let report = coverage_report(&tree);
        assert_eq!(report.required_count, 3);
        assert_eq!(report.satisfied_count, 1);
        assert_eq!(report.entries.len(), 4);
        assert_eq!(report.entries[0].state, AltState::Present);
        assert_eq!(report.entries[1].state, AltState::Empty);
        assert_eq!(report.entries[2].state, AltState::Missing);
        assert_eq!(report.entries[3].state, AltState::NotRequired);
        assert!((report.coverage - 1.0 / 3.0).abs() < f64::EPSILON);
    }

    #[test]
    fn coverage_report_of_empty_tree_is_full() {
        let report = coverage_report(&A11yTree::empty());
        assert!(report.entries.is_empty());
        assert_eq!(report.required_count, 0);
        assert!((report.coverage - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn table_model_from_tree_and_validation() {
        let mut header = node(3, TagRole::TableHeader, vec![]);
        header.alt_text = None;
        let tree = A11yTree::new(
            vec![
                node(1, TagRole::Document, vec![NodeId::new(2)]),
                node(2, TagRole::Table, vec![NodeId::new(5), NodeId::new(6)]),
                header,
                node(4, TagRole::TableData, vec![]),
                node(5, TagRole::TableRow, vec![NodeId::new(3)]),
                node(6, TagRole::TableRow, vec![NodeId::new(4)]),
            ],
            vec![NodeId::new(1)],
        )
        .expect("tree");
        let mut model = TableModel::from_tree(&tree, NodeId::new(2)).expect("model");
        assert_eq!(model.cells().len(), 2);
        assert_eq!(model.cells()[0].kind, CellKind::Header);
        assert_eq!(model.cells()[1].kind, CellKind::Data);
        assert_eq!(
            model.validate(),
            Err(TableError::MissingScope(NodeId::new(3)))
        );
        model
            .set_scope(NodeId::new(3), HeaderScope::Column)
            .expect("scope");
        model
            .associate_header(NodeId::new(4), NodeId::new(3))
            .expect("associate");
        let validation = model.validate().expect("valid table");
        assert_eq!(validation.rows, 2);
        assert_eq!(validation.columns, 1);
        assert_eq!(validation.header_count, 1);
        assert_eq!(validation.associated_data_count, 1);
        assert!((model.header_coverage() - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn table_header_scope_and_association_validation() {
        let mut model = TableModel::new(NodeId::new(1));
        model
            .add_cell(TableCell {
                id: NodeId::new(2),
                kind: CellKind::Data,
                row: 0,
                column: 0,
                scope: None,
                headers: vec![],
            })
            .expect("data cell");
        assert_eq!(
            model.set_scope(NodeId::new(2), HeaderScope::Row),
            Err(TableError::ScopeOnData(NodeId::new(2)))
        );
        assert_eq!(
            model.associate_header(NodeId::new(2), NodeId::new(9)),
            Err(TableError::UnknownHeader {
                cell: NodeId::new(2),
                header: NodeId::new(9)
            })
        );
        let validation = model.validate().expect("data-only table is valid");
        assert_eq!(validation.data_count, 1);
        assert_eq!(validation.associated_data_count, 0);
        assert!(model.header_coverage().abs() < f64::EPSILON);
        assert_eq!(
            TableModel::new(NodeId::new(9)).validate(),
            Err(TableError::EmptyTable)
        );
        assert_eq!(
            model.add_cell(TableCell {
                id: NodeId::new(2),
                kind: CellKind::Header,
                row: 0,
                column: 0,
                scope: None,
                headers: vec![],
            }),
            Err(TableError::DuplicateCell(NodeId::new(2)))
        );
        assert_eq!(HeaderScope::parse("rowgroup"), Some(HeaderScope::RowGroup));
        assert_eq!(HeaderScope::parse("nope"), None);
        assert_eq!(HeaderScope::ColumnGroup.as_str(), "columngroup");
    }

    #[test]
    fn undo_redo_round_trip() {
        let mut editor = TagEditor::new(sample());
        let original = editor.tree().clone();
        editor
            .set_alt_text(NodeId::new(4), Some("chart".into()))
            .expect("edit");
        assert!(editor.can_undo());
        assert!(editor.undo().expect("undo"));
        assert_eq!(*editor.tree(), original);
        assert!(editor.can_redo());
        assert!(editor.redo().expect("redo"));
        assert_eq!(
            editor
                .tree()
                .node(NodeId::new(4))
                .and_then(|n| n.alt_text.clone()),
            Some("chart".into())
        );
        // Redo restored the undo point, so undo works again.
        assert!(editor.undo().expect("undo again"));
        assert_eq!(*editor.tree(), original);
        assert!(!editor.undo().expect("journal drained"));
    }

    #[test]
    fn journal_is_bounded_and_deterministic() {
        let mut editor = TagEditor::new(sample()).with_journal_limit(2);
        for i in 0..5 {
            editor
                .set_alt_text(NodeId::new(4), Some(format!("alt-{i}")))
                .expect("edit");
        }
        assert_eq!(editor.undo_len(), 2);
        assert_eq!(editor.journal_limit(), 2);

        let mut other = TagEditor::new(sample()).with_journal_limit(2);
        for i in 0..5 {
            other
                .set_alt_text(NodeId::new(4), Some(format!("alt-{i}")))
                .expect("edit");
        }
        assert_eq!(*editor.tree(), *other.tree());
        assert_eq!(editor.undo_len(), other.undo_len());

        assert!(editor.undo().expect("undo"));
        assert!(editor.undo().expect("undo"));
        assert!(!editor.undo().expect("undo"));
        assert_eq!(editor.redo_len(), 2);
    }

    #[test]
    fn journal_disabled_with_zero_limit() {
        let mut editor = TagEditor::new(sample()).with_journal_limit(0);
        editor
            .change_role(NodeId::new(3), TagRole::Span)
            .expect("edit");
        assert_eq!(editor.undo_len(), 0);
        assert!(!editor.undo().expect("undo"));
    }

    #[test]
    fn determinism_same_operations_same_tree() {
        fn run() -> A11yTree {
            let mut editor = TagEditor::new(sample());
            editor
                .insert_node(
                    node(7, TagRole::Figure, vec![]),
                    Some(NodeId::new(2)),
                    Some(0),
                )
                .expect("insert");
            editor
                .set_alt_text(NodeId::new(7), Some("same".into()))
                .expect("alt");
            editor
                .move_before(NodeId::new(7), NodeId::new(4))
                .expect("move");
            editor
                .mark_artifact(NodeId::new(3), true)
                .expect("artifact");
            editor.tree().clone()
        }
        assert_eq!(run(), run());
    }

    #[test]
    fn edit_error_messages_are_descriptive() {
        assert!(EditError::Cycle.to_string().contains("cycle"));
        assert!(
            EditError::InvalidIndex { index: 3, len: 1 }
                .to_string()
                .contains("out of bounds")
        );
        assert!(
            TableError::MissingScope(NodeId::new(1))
                .to_string()
                .contains("scope")
        );
    }
}
