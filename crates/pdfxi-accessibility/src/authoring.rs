//! Deterministic tag-authoring plans, structure checks, and reading-order export.
//!
//! This module closes the library-level gap between the low-level [`TagEditor`]
//! mutations and a host-facing authoring workflow. An [`AuthoringPlan`] is an
//! ordered list of [`AuthoringStep`] operations that is validated before it is
//! applied to a validated [`A11yTree`]. Applying a plan produces an
//! [`AuthoringReport`] that records the resulting tree, a deterministic
//! reading-order export, and the result of a tagged-PDF structure write/read
//! round-trip check. Every failure is a typed [`AuthoringError`]; no operation
//! can panic on caller-supplied nodes, indices, or identifiers.
//!
//! The structure check intentionally compares the fields the tagged-PDF writer
//! serializes (role, hierarchy, page association, text, and alternative text).
//! `ActualText`, abbreviation expansions, and the artifact flag remain
//! model-only in the writer; they are preserved in the reported tree but are not
//! part of the round-trip projection.

use core::fmt;

use crate::{
    A11yNode, A11yTree, EditError, NodeId, ObjRef, ReadingOrder, ReadingOrderError,
    ReadingOrderNode, StructError, StructOptions, TagEditor, TagRole, ViewerPreferences,
    read_structure_tree, write_structure_tree,
};

/// A single, replayable tag-authoring operation.
#[derive(Debug, Clone, PartialEq)]
pub enum AuthoringStep {
    /// Insert a node under `parent` (or as a root when `None`) at `index`.
    Insert {
        /// Node to insert.
        node: A11yNode,
        /// Parent node, or `None` for the root list.
        parent: Option<NodeId>,
        /// Insertion index, or `None` to append.
        index: Option<usize>,
    },
    /// Remove a node, promoting its children into its position.
    Remove(NodeId),
    /// Move a node into a container at an explicit index.
    Move {
        /// Node to move.
        id: NodeId,
        /// New parent, or `None` for the root list.
        parent: Option<NodeId>,
        /// Insertion index in the new container.
        index: usize,
    },
    /// Change a node's semantic role.
    SetRole {
        /// Node to edit.
        id: NodeId,
        /// Replacement role.
        role: TagRole,
    },
    /// Set or clear a node's alternative text.
    SetAlt {
        /// Node to edit.
        id: NodeId,
        /// Replacement text, or `None` to clear.
        text: Option<String>,
    },
    /// Set or clear a node's `/ActualText` replacement text.
    SetActualText {
        /// Node to edit.
        id: NodeId,
        /// Replacement text, or `None` to clear.
        text: Option<String>,
    },
    /// Set or clear a node's abbreviation expansion (`/E`).
    SetExpansion {
        /// Node to edit.
        id: NodeId,
        /// Replacement text, or `None` to clear.
        text: Option<String>,
    },
    /// Mark or unmark a node as a decorative artifact.
    MarkArtifact {
        /// Node to edit.
        id: NodeId,
        /// New artifact flag.
        artifact: bool,
    },
    /// Move a node immediately before another node in reading order.
    MoveBefore {
        /// Node to move.
        id: NodeId,
        /// Node that will follow the moved node.
        target: NodeId,
    },
    /// Move a node immediately after another node in reading order.
    MoveAfter {
        /// Node to move.
        id: NodeId,
        /// Node that will precede the moved node.
        target: NodeId,
    },
    /// Stable-sort siblings and roots by ascending page index.
    NormalizePageOrder,
}

/// Typed errors produced while validating or applying an [`AuthoringPlan`].
#[derive(Debug, Clone, PartialEq)]
pub enum AuthoringError {
    /// The plan itself was structurally invalid.
    InvalidStep(String),
    /// A planned edit was rejected by the tag editor.
    Edit(EditError),
    /// The tagged-PDF structure round-trip failed.
    Structure(StructError),
    /// The exported reading order failed validation.
    ReadingOrder(ReadingOrderError),
}

impl fmt::Display for AuthoringError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidStep(message) => write!(f, "invalid authoring step: {message}"),
            Self::Edit(error) => write!(f, "authoring edit failed: {error}"),
            Self::Structure(error) => write!(f, "structure check failed: {error}"),
            Self::ReadingOrder(error) => write!(f, "reading-order export failed: {error}"),
        }
    }
}

impl std::error::Error for AuthoringError {}

/// An ordered, replayable sequence of tag-authoring operations.
#[derive(Debug, Clone, PartialEq)]
pub struct AuthoringPlan {
    steps: Vec<AuthoringStep>,
}

impl AuthoringPlan {
    /// Builds a plan from its steps.
    pub fn new(steps: Vec<AuthoringStep>) -> Self {
        Self { steps }
    }

    /// The steps in execution order.
    pub fn steps(&self) -> &[AuthoringStep] {
        &self.steps
    }

    /// Number of steps in the plan.
    pub fn len(&self) -> usize {
        self.steps.len()
    }

    /// Returns `true` when the plan has no steps.
    pub fn is_empty(&self) -> bool {
        self.steps.is_empty()
    }

    /// Validates the plan without applying it.
    ///
    /// Two insert steps may not allocate the same node id; a duplicate makes the
    /// plan ambiguous and is rejected with a typed error before any tree is
    /// touched.
    pub fn validate(&self) -> Result<(), AuthoringError> {
        let mut inserted = std::collections::HashSet::new();
        for step in &self.steps {
            if let AuthoringStep::Insert { node, .. } = step
                && !inserted.insert(node.id)
            {
                return Err(AuthoringError::InvalidStep(format!(
                    "node id {} is inserted more than once",
                    node.id.get()
                )));
            }
        }
        Ok(())
    }

    /// Applies the plan to `base` and returns a deterministic report.
    ///
    /// The input tree is validated by the editor; every failed edit is reported
    /// as a typed [`AuthoringError::Edit`] without mutating `base`. After the
    /// edits, the resulting tree is exported as a validated reading order and
    /// checked by writing and reading a tagged-PDF structure intermediate.
    pub fn apply(&self, base: &A11yTree) -> Result<AuthoringReport, AuthoringError> {
        self.validate()?;
        let mut editor = TagEditor::new(base.clone());
        for step in &self.steps {
            apply_step(&mut editor, step).map_err(AuthoringError::Edit)?;
        }
        let tree = editor.tree().clone();
        let reading_order = export_reading_order(&tree).map_err(AuthoringError::ReadingOrder)?;
        let page_count = tree
            .nodes()
            .iter()
            .map(|node| node.page)
            .max()
            .map_or(1, |max_page| max_page.saturating_add(1));
        let options = StructOptions {
            lang: "en-US".to_string(),
            page_count,
            page_objects: (0..page_count)
                .map(|page| ObjRef::new(100_u32.saturating_add(page as u32)))
                .collect(),
            viewer_preferences: ViewerPreferences::default(),
        };
        let document = write_structure_tree(&tree, &options).map_err(AuthoringError::Structure)?;
        let restored = read_structure_tree(&document).map_err(AuthoringError::Structure)?;
        let structure_round_trip_ok = structure_projection_equal(&tree, &restored);
        Ok(AuthoringReport {
            steps_applied: self.steps.len(),
            node_count: tree.len(),
            page_count,
            structure_objects: document.struct_elem_count(),
            structure_round_trip_ok,
            reading_order: reading_order
                .traversal()
                .into_iter()
                .map(NodeId::new)
                .collect(),
            tree,
        })
    }
}

fn apply_step(editor: &mut TagEditor, step: &AuthoringStep) -> Result<(), EditError> {
    match step {
        AuthoringStep::Insert {
            node,
            parent,
            index,
        } => {
            editor.insert_node(node.clone(), *parent, *index)?;
            Ok(())
        }
        AuthoringStep::Remove(id) => {
            editor.remove_node(*id)?;
            Ok(())
        }
        AuthoringStep::Move { id, parent, index } => editor.move_node(*id, *parent, *index),
        AuthoringStep::SetRole { id, role } => editor.change_role(*id, *role),
        AuthoringStep::SetAlt { id, text } => editor.set_alt_text(*id, text.clone()),
        AuthoringStep::SetActualText { id, text } => editor.set_actual_text(*id, text.clone()),
        AuthoringStep::SetExpansion { id, text } => editor.set_expansion(*id, text.clone()),
        AuthoringStep::MarkArtifact { id, artifact } => editor.mark_artifact(*id, *artifact),
        AuthoringStep::MoveBefore { id, target } => editor.move_before(*id, *target),
        AuthoringStep::MoveAfter { id, target } => editor.move_after(*id, *target),
        AuthoringStep::NormalizePageOrder => editor.normalize_page_order().map(|_| ()),
    }
}

/// Builds a validated [`ReadingOrder`] from an [`A11yTree`].
pub fn export_reading_order(tree: &A11yTree) -> Result<ReadingOrder, ReadingOrderError> {
    let nodes = tree
        .nodes()
        .iter()
        .map(|node| {
            ReadingOrderNode::new(
                node.id.get(),
                node.role,
                node.children.iter().map(|child| child.get()).collect(),
            )
        })
        .collect();
    ReadingOrder::new(nodes)
}

type StructureRow = (u64, TagRole, usize, String, Option<String>, Vec<u64>);

fn structure_projection(tree: &A11yTree) -> (Vec<StructureRow>, Vec<u64>) {
    let mut rows: Vec<StructureRow> = tree
        .nodes()
        .iter()
        .map(|node| {
            (
                node.id.get(),
                node.role,
                node.page,
                node.text.clone(),
                node.alt_text.clone(),
                node.children.iter().map(|child| child.get()).collect(),
            )
        })
        .collect();
    rows.sort_by_key(|row| row.0);
    let roots = tree.roots().iter().map(|root| root.get()).collect();
    (rows, roots)
}

fn structure_projection_equal(left: &A11yTree, right: &A11yTree) -> bool {
    structure_projection(left) == structure_projection(right)
}

/// Result of applying an [`AuthoringPlan`].
///
/// The report owns the edited tree so a host can persist or further edit it. All
/// other fields are deterministic projections, so equal plans applied to equal
/// trees always produce equal reports and equal [`Self::to_text`] output.
#[derive(Debug, Clone, PartialEq)]
pub struct AuthoringReport {
    /// Number of steps applied.
    pub steps_applied: usize,
    /// Number of nodes in the edited tree.
    pub node_count: usize,
    /// Number of pages the structure check was parameterized with.
    pub page_count: usize,
    /// Number of generated structure elements.
    pub structure_objects: usize,
    /// Whether the structure write/read round-trip preserved the projection.
    pub structure_round_trip_ok: bool,
    /// Node identifiers in deterministic presentation order.
    pub reading_order: Vec<NodeId>,
    /// The edited, always-valid tree.
    pub tree: A11yTree,
}

impl AuthoringReport {
    /// Serializes the deterministic part of the report as stable text.
    pub fn to_text(&self) -> String {
        let order = self
            .reading_order
            .iter()
            .map(|id| id.get().to_string())
            .collect::<Vec<_>>()
            .join(",");
        format!(
            "steps={}\nnodes={}\npages={}\nstruct_objects={}\nround_trip={}\nreading_order={order}\n",
            self.steps_applied,
            self.node_count,
            self.page_count,
            self.structure_objects,
            if self.structure_round_trip_ok {
                "ok"
            } else {
                "mismatch"
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BBox, TreeError};

    fn bbox() -> BBox {
        match BBox::new(0.0, 0.0, 10.0, 10.0) {
            Ok(bounds) => bounds,
            Err(error) => panic!("fixed test box must be valid: {error}"),
        }
    }

    fn node(id: u64, role: TagRole, page: usize, text: &str, children: Vec<NodeId>) -> A11yNode {
        A11yNode::new(NodeId::new(id), role, page, bbox(), text, children)
    }

    fn sample_tree() -> A11yTree {
        let result = A11yTree::new(
            vec![
                node(1, TagRole::Document, 0, "", vec![NodeId::new(2)]),
                node(
                    2,
                    TagRole::Page,
                    0,
                    "",
                    vec![NodeId::new(3), NodeId::new(4), NodeId::new(5)],
                ),
                node(3, TagRole::Heading1, 0, "Title", vec![]),
                node(4, TagRole::Paragraph, 0, "Body", vec![]),
                node(5, TagRole::Figure, 0, "", vec![]),
            ],
            vec![NodeId::new(1)],
        );
        match result {
            Ok(tree) => tree,
            Err(error) => panic!("sample tree must be valid: {error}"),
        }
    }

    fn empty_plan() -> AuthoringPlan {
        AuthoringPlan::new(Vec::new())
    }

    #[test]
    fn empty_plan_reports_the_source_tree_and_stable_reading_order() {
        let tree = sample_tree();
        let report = empty_plan().apply(&tree).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(report.steps_applied, 0);
        assert_eq!(report.node_count, 5);
        assert_eq!(report.page_count, 1);
        assert_eq!(report.structure_objects, 5);
        assert!(report.structure_round_trip_ok);
        assert_eq!(
            report.reading_order,
            vec![
                NodeId::new(1),
                NodeId::new(2),
                NodeId::new(3),
                NodeId::new(4),
                NodeId::new(5),
            ]
        );
        assert_eq!(report.tree, tree);
    }

    #[test]
    fn plan_applies_role_alt_and_artifact_edits() {
        let plan = AuthoringPlan::new(vec![
            AuthoringStep::SetRole {
                id: NodeId::new(5),
                role: TagRole::Image,
            },
            AuthoringStep::SetAlt {
                id: NodeId::new(5),
                text: Some("chart".to_string()),
            },
            AuthoringStep::MarkArtifact {
                id: NodeId::new(4),
                artifact: true,
            },
        ]);
        let report = plan.apply(&sample_tree()).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(report.steps_applied, 3);
        let image = report.tree.nodes()[4].clone();
        assert_eq!(image.role, TagRole::Image);
        assert_eq!(image.alt_text.as_deref(), Some("chart"));
        assert!(report.tree.nodes()[3].artifact);
        // Artifact/ActualText/expansion are model-only, but the serialized
        // projection (including alt text) still round-trips.
        assert!(report.structure_round_trip_ok);
    }

    #[test]
    fn plan_applies_reading_order_and_page_normalization() {
        let plan = AuthoringPlan::new(vec![
            AuthoringStep::MoveAfter {
                id: NodeId::new(5),
                target: NodeId::new(3),
            },
            AuthoringStep::NormalizePageOrder,
        ]);
        let report = plan.apply(&sample_tree()).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            report.reading_order,
            vec![
                NodeId::new(1),
                NodeId::new(2),
                NodeId::new(3),
                NodeId::new(5),
                NodeId::new(4),
            ]
        );
    }

    #[test]
    fn plan_inserts_and_removes_nodes_with_structure_recheck() {
        let inserted = node(6, TagRole::Paragraph, 0, "added", vec![]);
        let plan = AuthoringPlan::new(vec![
            AuthoringStep::Insert {
                node: inserted,
                parent: Some(NodeId::new(2)),
                index: None,
            },
            AuthoringStep::Remove(NodeId::new(4)),
        ]);
        let report = plan.apply(&sample_tree()).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(report.node_count, 5);
        assert_eq!(report.structure_objects, 5);
        assert!(report.structure_round_trip_ok);
        assert!(report.tree.nodes().iter().all(|n| n.id != NodeId::new(4)));
    }

    #[test]
    fn duplicate_insert_is_rejected_before_apply() {
        let duplicate = node(9, TagRole::Paragraph, 0, "x", vec![]);
        let plan = AuthoringPlan::new(vec![
            AuthoringStep::Insert {
                node: duplicate.clone(),
                parent: None,
                index: None,
            },
            AuthoringStep::Insert {
                node: duplicate,
                parent: None,
                index: None,
            },
        ]);
        assert!(plan.validate().is_err());
        assert!(matches!(
            plan.apply(&sample_tree()),
            Err(AuthoringError::InvalidStep(_))
        ));
    }

    #[test]
    fn invalid_edit_is_a_typed_error_and_does_not_mutate_source() {
        let tree = sample_tree();
        let before = tree.clone();
        let plan = AuthoringPlan::new(vec![AuthoringStep::SetRole {
            id: NodeId::new(999),
            role: TagRole::Paragraph,
        }]);
        assert!(matches!(
            plan.apply(&tree),
            Err(AuthoringError::Edit(EditError::UnknownNode(_)))
        ));
        assert_eq!(tree, before);
    }

    #[test]
    fn cyclic_move_is_rejected_with_typed_error() {
        let plan = AuthoringPlan::new(vec![AuthoringStep::Move {
            id: NodeId::new(1),
            parent: Some(NodeId::new(3)),
            index: 0,
        }]);
        assert!(matches!(
            plan.apply(&sample_tree()),
            Err(AuthoringError::Edit(EditError::Cycle))
        ));
    }

    #[test]
    fn identical_plans_produce_identical_reports_and_text() {
        let plan = AuthoringPlan::new(vec![
            AuthoringStep::MoveBefore {
                id: NodeId::new(4),
                target: NodeId::new(3),
            },
            AuthoringStep::SetAlt {
                id: NodeId::new(5),
                text: Some("stable".to_string()),
            },
        ]);
        let left = plan.apply(&sample_tree()).unwrap_or_else(|e| panic!("{e}"));
        let right = plan.apply(&sample_tree()).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(left, right);
        assert_eq!(left.to_text(), right.to_text());
        assert!(left.to_text().contains("round_trip=ok"));
    }

    #[test]
    fn export_reading_order_rejects_a_tree_with_cycle_via_direct_nodes() {
        // A well-formed tree always exports cleanly; re-validating the export is
        // exercised here, and malformed node sets are rejected by ReadingOrder.
        let tree = sample_tree();
        let order = export_reading_order(&tree).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(order.len(), 5);
        assert_eq!(order.validate(), Ok(()));

        let cyclic = ReadingOrder::new(vec![
            ReadingOrderNode::new(1, TagRole::Document, vec![2]),
            ReadingOrderNode::new(2, TagRole::Page, vec![1]),
        ]);
        assert!(matches!(cyclic, Err(ReadingOrderError::Cycle)));
    }

    #[test]
    fn authoring_error_messages_are_descriptive() {
        let error = AuthoringError::InvalidStep("bad".to_string());
        assert!(error.to_string().contains("bad"));
        let error = AuthoringError::Edit(EditError::UnknownNode(NodeId::new(7)));
        assert!(error.to_string().contains("authoring edit failed"));
        let _: &dyn std::error::Error = &error;
    }

    #[test]
    fn multi_page_tree_is_handled_by_structure_check() {
        let tree = match A11yTree::new(
            vec![
                node(
                    1,
                    TagRole::Document,
                    0,
                    "",
                    vec![NodeId::new(2), NodeId::new(3)],
                ),
                node(2, TagRole::Page, 0, "", vec![]),
                node(3, TagRole::Page, 1, "", vec![]),
            ],
            vec![NodeId::new(1)],
        ) {
            Ok(tree) => tree,
            Err(error) => panic!("tree must be valid: {error}"),
        };
        let report = empty_plan().apply(&tree).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(report.page_count, 2);
        assert!(report.structure_round_trip_ok);
        let _: Result<(), TreeError> = report.tree.validate();
    }
}
