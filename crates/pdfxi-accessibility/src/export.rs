//! Plain-text export and deterministic summary of an accessibility tree.
//!
//! [`to_plain_text`] serializes a tree in depth-first reading order with role
//! labels and two-space indentation per level. [`summarize`] reports node counts
//! per role and alternative-text coverage. Both are pure functions of the tree,
//! so repeated calls and structurally identical trees produce identical output.

use std::collections::{BTreeMap, HashMap, HashSet};

use crate::{A11yNode, A11yTree, NodeId, TagRole};

/// Number of roles that require alternative text for the coverage metric.
const ALT_TEXT_ROLES: [TagRole; 2] = [TagRole::Figure, TagRole::Image];

fn requires_alt_text(role: TagRole) -> bool {
    ALT_TEXT_ROLES.contains(&role)
}

fn has_alt_text(node: &A11yNode) -> bool {
    node.alt_text.as_ref().is_some_and(|text| !text.is_empty())
}

fn render_line(node: &A11yNode, depth: usize, out: &mut String) {
    out.push_str(&"  ".repeat(depth));
    match node.role {
        TagRole::Page => {
            out.push_str("[page ");
            out.push_str(&node.page.saturating_add(1).to_string());
            out.push(']');
        }
        role => {
            out.push('[');
            out.push_str(role.as_str());
            out.push(']');
        }
    }
    if !node.text.is_empty() {
        out.push(' ');
        out.push_str(&node.text);
    }
    if let Some(alt) = &node.alt_text {
        out.push_str(" [alt: ");
        out.push_str(alt);
        out.push(']');
    }
    out.push('\n');
}

/// Serializes a tree in reading order with indentation and role labels.
///
/// The result is deterministic and ends with a newline when the tree is
/// non-empty. Empty trees serialize to an empty string. The walk uses an
/// explicit stack rather than recursion, so arbitrarily deep trees are safe.
pub fn to_plain_text(tree: &A11yTree) -> String {
    let mut out = String::new();
    let mut visited: HashSet<NodeId> = HashSet::with_capacity(tree.len());
    let index: HashMap<NodeId, &A11yNode> =
        tree.nodes().iter().map(|node| (node.id, node)).collect();
    let mut stack: Vec<(NodeId, usize)> = Vec::new();
    for &root in tree.roots().iter().rev() {
        if visited.insert(root) {
            stack.push((root, 0));
        }
    }
    while let Some((id, depth)) = stack.pop() {
        let Some(node) = index.get(&id) else {
            continue;
        };
        render_line(node, depth, &mut out);
        for &child in node.children.iter().rev() {
            if visited.insert(child) {
                stack.push((child, depth + 1));
            }
        }
    }
    out
}

/// Number of nodes carrying a given role.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RoleCount {
    /// Role being counted.
    pub role: TagRole,
    /// Number of nodes with that role.
    pub count: usize,
}

/// Alternative-text coverage among roles that require it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AltTextCoverage {
    /// Number of nodes whose role requires alternative text.
    pub visual_nodes: usize,
    /// Number of those nodes that carry non-empty alternative text.
    pub with_alt_text: usize,
    /// Fraction of visual nodes with alternative text, in `0.0..=1.0`.
    ///
    /// This is `1.0` when there are no visual nodes, so a document with no
    /// figures or images is trivially fully covered.
    pub coverage: f64,
}

/// Deterministic summary of an accessibility tree.
#[derive(Clone, Debug, PartialEq)]
pub struct TreeSummary {
    /// Total number of nodes in the tree.
    pub total_nodes: usize,
    /// Non-zero role counts in `TagRole` declaration order.
    pub role_counts: Vec<RoleCount>,
    /// Alternative-text coverage statistics.
    pub alt_text: AltTextCoverage,
}

/// Computes a stable summary of a tree.
pub fn summarize(tree: &A11yTree) -> TreeSummary {
    let mut counts: BTreeMap<TagRole, usize> = BTreeMap::new();
    let mut visual_nodes = 0usize;
    let mut with_alt_text = 0usize;
    for node in tree.nodes() {
        *counts.entry(node.role).or_insert(0) += 1;
        if requires_alt_text(node.role) {
            visual_nodes += 1;
            if has_alt_text(node) {
                with_alt_text += 1;
            }
        }
    }
    let role_counts = counts
        .into_iter()
        .map(|(role, count)| RoleCount { role, count })
        .collect();
    let coverage = if visual_nodes == 0 {
        1.0
    } else {
        with_alt_text as f64 / visual_nodes as f64
    };
    TreeSummary {
        total_nodes: tree.len(),
        role_counts,
        alt_text: AltTextCoverage {
            visual_nodes,
            with_alt_text,
            coverage,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{A11yNode, BBox, NodeId};

    fn box_at() -> BBox {
        match BBox::new(0.0, 0.0, 10.0, 10.0) {
            Ok(value) => value,
            Err(_) => panic!("test box must be valid"),
        }
    }

    fn node(id: u64, role: TagRole, text: &str, children: Vec<NodeId>) -> A11yNode {
        A11yNode::new(NodeId::new(id), role, 0, box_at(), text, children)
    }

    fn sample_tree() -> A11yTree {
        let figure = node(3, TagRole::Figure, "", vec![]).with_alt_text("A bar chart");
        A11yTree::new(
            vec![
                node(1, TagRole::Document, "", vec![NodeId::new(2)]),
                node(2, TagRole::Page, "", vec![NodeId::new(3), NodeId::new(4)]),
                figure,
                node(4, TagRole::Paragraph, "Body text", vec![]),
            ],
            vec![NodeId::new(1)],
        )
        .expect("valid tree")
    }

    #[test]
    fn plain_text_export_is_stable_and_indented() {
        let tree = sample_tree();
        let expected = concat!(
            "[document]\n",
            "  [page 1]\n",
            "    [figure] [alt: A bar chart]\n",
            "    [paragraph] Body text\n",
        );
        let first = to_plain_text(&tree);
        assert_eq!(first, expected);
        assert_eq!(to_plain_text(&tree), first);
    }

    #[test]
    fn summary_counts_roles_and_alt_text_coverage() {
        let summary = summarize(&sample_tree());
        assert_eq!(summary.total_nodes, 4);
        assert_eq!(
            summary.role_counts,
            vec![
                RoleCount {
                    role: TagRole::Document,
                    count: 1
                },
                RoleCount {
                    role: TagRole::Page,
                    count: 1
                },
                RoleCount {
                    role: TagRole::Paragraph,
                    count: 1
                },
                RoleCount {
                    role: TagRole::Figure,
                    count: 1
                },
            ]
        );
        assert_eq!(summary.alt_text.visual_nodes, 1);
        assert_eq!(summary.alt_text.with_alt_text, 1);
        assert!((summary.alt_text.coverage - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn alt_text_coverage_handles_missing_and_empty_alt() {
        let with_alt = node(1, TagRole::Figure, "", vec![]).with_alt_text("described");
        let empty_alt = node(2, TagRole::Image, "", vec![]).with_alt_text("");
        let missing_alt = node(3, TagRole::Image, "", vec![]);
        let tree = A11yTree::new(
            vec![with_alt, empty_alt, missing_alt],
            vec![NodeId::new(1), NodeId::new(2), NodeId::new(3)],
        )
        .expect("valid tree");
        let summary = summarize(&tree);
        assert_eq!(summary.alt_text.visual_nodes, 3);
        assert_eq!(summary.alt_text.with_alt_text, 1);
        assert!((summary.alt_text.coverage - 1.0 / 3.0).abs() < f64::EPSILON);
    }

    #[test]
    fn summary_and_export_of_empty_tree_are_stable() {
        let tree = A11yTree::empty();
        assert_eq!(to_plain_text(&tree), "");
        let summary = summarize(&tree);
        assert_eq!(summary.total_nodes, 0);
        assert!(summary.role_counts.is_empty());
        assert_eq!(summary.alt_text.visual_nodes, 0);
        assert!((summary.alt_text.coverage - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn page_label_saturates_for_max_page() {
        let page = A11yNode::new(
            NodeId::new(1),
            TagRole::Page,
            usize::MAX,
            box_at(),
            String::new(),
            Vec::new(),
        );
        let tree = A11yTree::new(vec![page], vec![NodeId::new(1)]).expect("valid tree");
        let expected = format!("[page {}]\n", usize::MAX);
        assert_eq!(to_plain_text(&tree), expected);
    }

    #[test]
    fn deep_chain_export_uses_explicit_stack() {
        const DEPTH: usize = 10_000;
        let bounds = box_at();
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
        let text = to_plain_text(&tree);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), DEPTH);
        assert_eq!(lines[0], "[div]");
        assert_eq!(lines[DEPTH - 1], "  ".repeat(DEPTH - 1) + "[div]");
    }
}
