//! Deterministic accessibility tree and screen-reader host boundary.
//!
//! This module produces a window-free [`A11yTree`] from a caller-supplied
//! [`AccessibilitySnapshot`] of the app state. Nodes carry stable ids, roles,
//! labels, descriptions, and an explicit focus; [`AnnouncementLog`] turns focus
//! changes into spoken [`FocusAnnouncement`]s. A host publishes the tree and
//! announcements through the [`AccessibilityHost`] trait — this crate never
//! talks to a screen reader directly.
//!
//! With the `ui` feature, [`ui_bridge`] translates the deterministic tree into
//! the AccessKit object model that `egui`/`eframe` expose to platform screen
//! readers. `egui` re-exports its `accesskit` dependency, so no new crate
//! dependency is introduced.

use crate::nav::FocusTarget;

// ---------------------------------------------------------------------------
// Roles, nodes, and tree
// ---------------------------------------------------------------------------

/// Semantic role of an accessibility node.
///
/// [`Self::accesskit_name`] documents the mapping to AccessKit 0.21 role names
/// (the names used by the `ui` bridge's `Role` variants).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum A11yRole {
    /// Top-level application window.
    Window,
    /// The application as a whole.
    Application,
    /// A container of document tabs.
    TabStrip,
    /// A single document tab.
    Tab,
    /// The main toolbar.
    Toolbar,
    /// A push button.
    Button,
    /// A docked or logical pane.
    Pane,
    /// A loaded document.
    Document,
    /// One page of a document.
    Page,
    /// A section heading.
    Heading,
    /// A paragraph of text.
    Paragraph,
    /// A run of static text.
    Text,
    /// An editable text field.
    Edit,
    /// A named side panel.
    Panel,
    /// A checkable option.
    CheckBox,
    /// One option of a radio group.
    RadioButton,
    /// An ordered collection.
    List,
    /// One item of a list.
    ListItem,
    /// The menu bar.
    MenuBar,
    /// One menu item.
    MenuItem,
    /// The status bar.
    StatusBar,
    /// An image or page thumbnail.
    Image,
    /// A hyperlink.
    Link,
}

impl A11yRole {
    /// The AccessKit 0.21 `Role` name this role maps to.
    pub fn accesskit_name(self) -> &'static str {
        match self {
            Self::Window => "Window",
            Self::Application => "Application",
            Self::TabStrip => "GenericContainer",
            Self::Tab => "Tab",
            Self::Toolbar => "Toolbar",
            Self::Button => "Button",
            Self::Pane => "Pane",
            Self::Document => "Document",
            Self::Page => "Group",
            Self::Heading => "Heading",
            Self::Paragraph => "Paragraph",
            Self::Text => "TextRun",
            Self::Edit => "TextInput",
            Self::Panel => "Pane",
            Self::CheckBox => "CheckBox",
            Self::RadioButton => "RadioButton",
            Self::List => "List",
            Self::ListItem => "ListItem",
            Self::MenuBar => "MenuBar",
            Self::MenuItem => "MenuItem",
            Self::StatusBar => "Group",
            Self::Image => "Image",
            Self::Link => "Link",
        }
    }
}

/// A single node in the accessibility tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct A11yNode {
    /// Stable, unique node id within the tree.
    pub id: u64,
    /// Semantic role of the node.
    pub role: A11yRole,
    /// Primary accessible name.
    pub label: String,
    /// Longer accessible description, empty when not needed.
    pub description: String,
    /// Ids of this node's children, in reading order.
    pub children: Vec<u64>,
    /// Whether the node accepts keyboard focus.
    pub focusable: bool,
    /// Whether the node is currently disabled.
    pub disabled: bool,
}

impl A11yNode {
    /// Create a node with no description, children, or disabled state.
    pub fn new(id: u64, role: A11yRole, label: impl Into<String>) -> Self {
        Self {
            id,
            role,
            label: label.into(),
            description: String::new(),
            children: Vec::new(),
            focusable: false,
            disabled: false,
        }
    }

    /// Set the accessible description.
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = description.into();
        self
    }

    /// Set whether the node accepts keyboard focus.
    pub fn focusable(mut self, focusable: bool) -> Self {
        self.focusable = focusable;
        self
    }

    /// Set whether the node is disabled.
    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// Replace the node's children.
    pub fn with_children(mut self, children: Vec<u64>) -> Self {
        self.children = children;
        self
    }
}

/// A window-free description of the app state used to build a tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccessibilitySnapshot {
    /// Application name, used as the window label.
    pub app_name: String,
    /// Display names of open tabs, in tab-strip order.
    pub tabs: Vec<String>,
    /// Index of the active tab, if any.
    pub active_tab: Option<usize>,
    /// Number of pages in the active document.
    pub page_count: usize,
    /// Zero-based current page.
    pub current_page: usize,
    /// Region that currently holds keyboard focus.
    pub focus_target: FocusTarget,
    /// Whether the high-contrast theme is active.
    pub high_contrast: bool,
}

impl AccessibilitySnapshot {
    /// Create a snapshot. Inputs are used as-is; callers are responsible for
    /// keeping indices consistent with the tab/page counts.
    pub fn new(
        app_name: impl Into<String>,
        tabs: Vec<String>,
        active_tab: Option<usize>,
        page_count: usize,
        current_page: usize,
        focus_target: FocusTarget,
        high_contrast: bool,
    ) -> Self {
        Self {
            app_name: app_name.into(),
            tabs,
            active_tab,
            page_count,
            current_page,
            focus_target,
            high_contrast,
        }
    }
}

/// A deterministic accessibility tree with a root and a focused node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct A11yTree {
    nodes: Vec<A11yNode>,
    focus: u64,
}

impl Default for A11yTree {
    fn default() -> Self {
        Self::new()
    }
}

impl A11yTree {
    /// The id of the root window node.
    pub const ROOT_ID: u64 = 0;

    /// Create a minimal tree with just the root window node.
    pub fn new() -> Self {
        Self {
            nodes: vec![A11yNode::new(Self::ROOT_ID, A11yRole::Window, "PDFXI")],
            focus: Self::ROOT_ID,
        }
    }

    /// Build a full tree from an app-state snapshot.
    ///
    /// Node ids are assigned in a fixed order (root, tab strip, tabs, toolbar,
    /// document, pages, status bar), so equal snapshots always produce equal
    /// trees. Out-of-range active-tab or current-page values are ignored rather
    /// than panicking.
    pub fn from_snapshot(snapshot: &AccessibilitySnapshot) -> Self {
        let mut builder = TreeBuilder::default();
        let root = builder.push(A11yNode::new(
            Self::ROOT_ID,
            A11yRole::Window,
            snapshot.app_name.clone(),
        ));

        // Tab strip and one node per tab.
        let tab_strip =
            builder.push(A11yNode::new(0, A11yRole::TabStrip, "Document tabs").focusable(true));
        let mut tab_ids = Vec::new();
        for (index, name) in snapshot.tabs.iter().enumerate() {
            let is_active = snapshot.active_tab == Some(index);
            tab_ids.push(
                builder.push(
                    A11yNode::new(0, A11yRole::Tab, name.clone())
                        .with_description(if is_active { "active" } else { "inactive" })
                        .focusable(true),
                ),
            );
        }
        builder.link(tab_strip, tab_ids);

        // Toolbar with a fixed set of always-available commands.
        let toolbar = builder.push(A11yNode::new(0, A11yRole::Toolbar, "Toolbar").focusable(true));
        let tool_ids: Vec<u64> = [
            "Open document",
            "Save document",
            "Find",
            "Read aloud",
            "Toggle high contrast",
        ]
        .iter()
        .map(|label| builder.push(A11yNode::new(0, A11yRole::Button, *label).focusable(true)))
        .collect();
        builder.link(toolbar, tool_ids);

        // Active document and its pages.
        let document_label = snapshot
            .active_tab
            .and_then(|index| snapshot.tabs.get(index))
            .cloned()
            .unwrap_or_else(|| "No document".to_string());
        let document = builder.push(
            A11yNode::new(0, A11yRole::Document, document_label)
                .with_description("PDF page canvas")
                .focusable(true),
        );
        let mut page_ids = Vec::new();
        for page in 0..snapshot.page_count {
            let is_current = page == snapshot.current_page;
            page_ids.push(
                builder.push(
                    A11yNode::new(0, A11yRole::Page, format!("Page {}", page + 1))
                        .with_description(if is_current { "current page" } else { "page" })
                        .focusable(true),
                ),
            );
        }
        builder.link(document, page_ids);

        // Status bar reflects the theme; it is informational, not focusable.
        let status_text = if snapshot.high_contrast {
            "High contrast on"
        } else {
            "High contrast off"
        };
        let status = builder.push(
            A11yNode::new(0, A11yRole::StatusBar, status_text).with_description("Viewer status"),
        );

        builder.link(root, vec![tab_strip, toolbar, document, status]);

        let focus = builder.focus_for(snapshot.focus_target, tab_strip, toolbar, document, root);
        let mut tree = builder.finish(focus);
        // The root label must be the snapshot app name, not the builder default.
        if let Some(root_node) = tree.nodes.get_mut(0) {
            root_node.label = snapshot.app_name.clone();
        }
        tree
    }

    /// The root node id.
    pub fn root(&self) -> u64 {
        Self::ROOT_ID
    }

    /// The currently focused node id.
    pub fn focus(&self) -> u64 {
        self.focus
    }

    /// Move focus to `id`, reporting whether such a node exists.
    pub fn set_focus(&mut self, id: u64) -> bool {
        if self.nodes.iter().any(|node| node.id == id) {
            self.focus = id;
            true
        } else {
            false
        }
    }

    /// All nodes in id order.
    pub fn nodes(&self) -> &[A11yNode] {
        &self.nodes
    }

    /// Look up a node by id.
    pub fn node(&self, id: u64) -> Option<&A11yNode> {
        self.nodes.iter().find(|node| node.id == id)
    }

    /// Number of nodes.
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    /// Whether the tree has no nodes (never true for a constructed tree).
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// Find the first node with a matching role and label.
    pub fn find_by_label(&self, role: A11yRole, label: &str) -> Option<u64> {
        self.nodes
            .iter()
            .find(|node| node.role == role && node.label == label)
            .map(|node| node.id)
    }

    /// The announcement describing the currently focused node.
    pub fn focus_announcement(&self) -> FocusAnnouncement {
        match self.node(self.focus) {
            Some(node) => FocusAnnouncement {
                node_id: node.id,
                label: node.label.clone(),
                role: node.role,
            },
            None => FocusAnnouncement {
                node_id: Self::ROOT_ID,
                label: String::new(),
                role: A11yRole::Window,
            },
        }
    }

    /// Whether every child id resolves and the root and focus ids exist.
    pub fn is_well_formed(&self) -> bool {
        let ids: Vec<u64> = self.nodes.iter().map(|node| node.id).collect();
        let unique = ids.iter().collect::<std::collections::BTreeSet<_>>().len() == ids.len();
        unique
            && self.node(Self::ROOT_ID).is_some()
            && self.node(self.focus).is_some()
            && self
                .nodes
                .iter()
                .all(|node| node.children.iter().all(|child| ids.contains(child)))
    }
}

/// Incremental builder that assigns ids and records parent/child links.
#[derive(Default)]
struct TreeBuilder {
    nodes: Vec<A11yNode>,
    next_id: u64,
}

impl TreeBuilder {
    fn push(&mut self, mut node: A11yNode) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        node.id = id;
        self.nodes.push(node);
        id
    }

    fn link(&mut self, parent: u64, children: Vec<u64>) {
        if let Some(node) = self.nodes.iter_mut().find(|node| node.id == parent) {
            node.children = children;
        }
    }

    fn focus_for(
        &self,
        target: FocusTarget,
        tab_strip: u64,
        toolbar: u64,
        document: u64,
        root: u64,
    ) -> u64 {
        match target {
            FocusTarget::TabStrip => tab_strip,
            FocusTarget::Toolbar => toolbar,
            FocusTarget::PageCanvas | FocusTarget::ThumbnailPane => document,
            FocusTarget::MenuBar => root,
            FocusTarget::FindBar
            | FocusTarget::LayersPanel
            | FocusTarget::BookmarksPanel
            | FocusTarget::AttachmentsPanel
            | FocusTarget::LinksPanel
            | FocusTarget::StatusBar => root,
        }
    }

    fn finish(self, focus: u64) -> A11yTree {
        A11yTree {
            nodes: self.nodes,
            focus,
        }
    }
}

// ---------------------------------------------------------------------------
// Focus announcements
// ---------------------------------------------------------------------------

/// What a screen reader should announce when focus moves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FocusAnnouncement {
    /// Id of the focused node.
    pub node_id: u64,
    /// Accessible name to announce.
    pub label: String,
    /// Role to announce with the name.
    pub role: A11yRole,
}

impl FocusAnnouncement {
    /// A plain sentence suitable for a live-region announcement.
    pub fn spoken_text(&self) -> String {
        format!("{}, {}", self.label, self.role.accesskit_name())
    }
}

/// Records focus-change announcements for a screen-reader host.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct AnnouncementLog {
    entries: Vec<FocusAnnouncement>,
    last_focus: Option<u64>,
}

impl AnnouncementLog {
    /// Create an empty log.
    pub fn new() -> Self {
        Self::default()
    }

    /// Observe a tree, appending an announcement only when its focus id
    /// differs from the last observed id. Returns whether a new announcement
    /// was produced.
    pub fn observe(&mut self, tree: &A11yTree) -> bool {
        if self.last_focus == Some(tree.focus()) {
            return false;
        }
        self.last_focus = Some(tree.focus());
        self.entries.push(tree.focus_announcement());
        true
    }

    /// All announcements so far, in order.
    pub fn entries(&self) -> &[FocusAnnouncement] {
        &self.entries
    }

    /// The most recent announcement, if any.
    pub fn last(&self) -> Option<&FocusAnnouncement> {
        self.entries.last()
    }

    /// Remove all recorded announcements and reset the last-seen focus.
    pub fn clear(&mut self) {
        self.entries.clear();
        self.last_focus = None;
    }
}

// ---------------------------------------------------------------------------
// Host adapter contract
// ---------------------------------------------------------------------------

/// Contract a shell implements to publish accessibility updates to a screen
/// reader. Implementations own the platform-specific bridge; this crate only
/// produces the deterministic data.
pub trait AccessibilityHost {
    /// Publish a new tree snapshot.
    fn publish_tree(&mut self, tree: &A11yTree);
    /// Announce a focus change.
    fn announce(&mut self, announcement: &FocusAnnouncement);
}

/// A no-op host for headless hosts and tests.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct NullAccessibilityHost;

impl AccessibilityHost for NullAccessibilityHost {
    fn publish_tree(&mut self, _tree: &A11yTree) {}
    fn announce(&mut self, _announcement: &FocusAnnouncement) {}
}

// ---------------------------------------------------------------------------
// Feature-gated UI registration
// ---------------------------------------------------------------------------

/// AccessKit translation and registration available with the `ui` feature.
///
/// The shell builds an [`A11yTree`] from its state, publishes it to a
/// `UiAccessibilityBridge`, and attaches the resulting
/// `accesskit::TreeUpdate` to `egui`'s platform output so `eframe` can hand it
/// to the platform screen reader. Because `egui` re-exports its `accesskit`
/// dependency, this adds no new crate to `pdfxi-app`.
#[cfg(feature = "ui")]
pub mod ui_bridge {
    use super::{A11yRole, A11yTree, FocusAnnouncement};
    use egui::accesskit;

    /// Maps an [`A11yRole`] to an AccessKit role.
    fn accesskit_role(role: A11yRole) -> accesskit::Role {
        match role {
            A11yRole::Window => accesskit::Role::Window,
            A11yRole::Application => accesskit::Role::Application,
            A11yRole::TabStrip => accesskit::Role::GenericContainer,
            A11yRole::Tab => accesskit::Role::Tab,
            A11yRole::Toolbar => accesskit::Role::Toolbar,
            A11yRole::Button => accesskit::Role::Button,
            A11yRole::Pane | A11yRole::Panel => accesskit::Role::Pane,
            A11yRole::Document => accesskit::Role::Document,
            // AccessKit has no dedicated page role; pages are exposed as groups.
            A11yRole::Page => accesskit::Role::Group,
            A11yRole::Heading => accesskit::Role::Heading,
            A11yRole::Paragraph => accesskit::Role::Paragraph,
            A11yRole::Text => accesskit::Role::TextRun,
            A11yRole::Edit => accesskit::Role::TextInput,
            A11yRole::CheckBox => accesskit::Role::CheckBox,
            A11yRole::RadioButton => accesskit::Role::RadioButton,
            A11yRole::List => accesskit::Role::List,
            A11yRole::ListItem => accesskit::Role::ListItem,
            A11yRole::MenuBar => accesskit::Role::MenuBar,
            A11yRole::MenuItem => accesskit::Role::MenuItem,
            A11yRole::StatusBar => accesskit::Role::Group,
            A11yRole::Image => accesskit::Role::Image,
            A11yRole::Link => accesskit::Role::Link,
        }
    }

    /// Translates a deterministic [`A11yTree`] into AccessKit node pairs.
    ///
    /// Returned in stable id order so equal trees always produce equal updates.
    pub fn tree_nodes(tree: &A11yTree) -> Vec<(accesskit::NodeId, accesskit::Node)> {
        tree.nodes()
            .iter()
            .map(|node| {
                let mut ak_node = accesskit::Node::new(accesskit_role(node.role));
                ak_node.set_label(node.label.clone());
                if !node.description.is_empty() {
                    ak_node.set_description(node.description.clone());
                }
                for child in &node.children {
                    ak_node.push_child(accesskit::NodeId(*child));
                }
                (accesskit::NodeId(node.id), ak_node)
            })
            .collect()
    }

    /// Build a complete [`accesskit::TreeUpdate`] for a tree.
    pub fn tree_update(tree: &A11yTree) -> accesskit::TreeUpdate {
        accesskit::TreeUpdate {
            nodes: tree_nodes(tree),
            tree: Some(accesskit::Tree::new(accesskit::NodeId(tree.root()))),
            focus: accesskit::NodeId(tree.focus()),
        }
    }

    /// An [`super::AccessibilityHost`] adapter that retains the latest tree
    /// and announcements and converts them for egui/eframe on demand.
    #[derive(Debug, Default)]
    pub struct UiAccessibilityBridge {
        last_tree: Option<A11yTree>,
        announcements: Vec<FocusAnnouncement>,
    }

    impl UiAccessibilityBridge {
        /// Create an empty bridge.
        pub fn new() -> Self {
            Self::default()
        }

        /// Retain a newly published tree.
        pub fn publish_tree(&mut self, tree: &A11yTree) {
            self.last_tree = Some(tree.clone());
        }

        /// Retain a focus announcement.
        pub fn announce(&mut self, announcement: &FocusAnnouncement) {
            self.announcements.push(announcement.clone());
        }

        /// The most recently published tree, if any.
        pub fn last_tree(&self) -> Option<&A11yTree> {
            self.last_tree.as_ref()
        }

        /// All retained announcements, in order.
        pub fn announcements(&self) -> &[FocusAnnouncement] {
            &self.announcements
        }

        /// The AccessKit update for the last published tree, if any.
        pub fn tree_update(&self) -> Option<accesskit::TreeUpdate> {
            self.last_tree.as_ref().map(tree_update)
        }

        /// Attach the latest tree update to an egui context's platform output.
        ///
        /// eframe forwards [`egui::PlatformOutput::accesskit_update`] to the
        /// platform adapter; setting it here is the registration point for the
        /// canvas tree that egui's widget pass does not describe.
        pub fn register_with_egui(&self, ctx: &egui::Context) -> bool {
            let Some(update) = self.tree_update() else {
                return false;
            };
            ctx.output_mut(|output| output.accesskit_update = Some(update));
            true
        }
    }

    impl super::AccessibilityHost for UiAccessibilityBridge {
        fn publish_tree(&mut self, tree: &A11yTree) {
            UiAccessibilityBridge::publish_tree(self, tree);
        }

        fn announce(&mut self, announcement: &FocusAnnouncement) {
            UiAccessibilityBridge::announce(self, announcement);
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use crate::a11y::{AccessibilitySnapshot, AnnouncementLog};
        use crate::nav::FocusTarget;

        fn snapshot() -> AccessibilitySnapshot {
            AccessibilitySnapshot::new(
                "PDFXI",
                vec!["a.pdf".to_string(), "b.pdf".to_string()],
                Some(1),
                2,
                0,
                FocusTarget::PageCanvas,
                true,
            )
        }

        #[test]
        fn bridge_converts_tree_to_accesskit_update() {
            let tree = A11yTree::from_snapshot(&snapshot());
            let update = tree_update(&tree);
            assert_eq!(update.nodes.len(), tree.len());
            assert!(update.tree.is_some());
            assert_eq!(update.focus, accesskit::NodeId(tree.focus()));
            let labels: Vec<&str> = update
                .nodes
                .iter()
                .map(|(_, node)| node.label().unwrap_or_default())
                .collect();
            assert!(labels.contains(&"a.pdf"));
            assert!(labels.contains(&"b.pdf"));
            assert!(labels.contains(&"Page 1"));
        }

        #[test]
        fn bridge_retains_published_tree_and_announcements() {
            let mut bridge = UiAccessibilityBridge::new();
            assert!(bridge.tree_update().is_none());
            let mut tree = A11yTree::from_snapshot(&snapshot());
            bridge.publish_tree(&tree);
            assert_eq!(bridge.last_tree(), Some(&tree));
            assert!(bridge.tree_update().is_some());

            let announcement = tree.focus_announcement();
            bridge.announce(&announcement);
            assert_eq!(bridge.announcements(), std::slice::from_ref(&announcement));

            assert!(tree.set_focus(tree.root()));
            let mut log = AnnouncementLog::new();
            assert!(log.observe(&tree));
            assert!(!log.observe(&tree));
        }

        #[test]
        fn register_with_egui_sets_platform_output() {
            let ctx = egui::Context::default();
            let mut bridge = UiAccessibilityBridge::new();
            assert!(!bridge.register_with_egui(&ctx), "no tree yet");
            bridge.publish_tree(&A11yTree::from_snapshot(&snapshot()));
            assert!(bridge.register_with_egui(&ctx));
            let has_update = ctx.output(|output| output.accesskit_update.is_some());
            assert!(has_update);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot() -> AccessibilitySnapshot {
        AccessibilitySnapshot::new(
            "PDFXI",
            vec!["first.pdf".to_string(), "second.pdf".to_string()],
            Some(0),
            3,
            1,
            FocusTarget::PageCanvas,
            false,
        )
    }

    #[test]
    fn tree_from_snapshot_is_deterministic_and_well_formed() {
        let a = A11yTree::from_snapshot(&snapshot());
        let b = A11yTree::from_snapshot(&snapshot());
        assert_eq!(a, b);
        assert!(a.is_well_formed());
        assert_eq!(a.root(), A11yTree::ROOT_ID);
        assert_eq!(a.node(a.root()).expect("root").role, A11yRole::Window);
    }

    #[test]
    fn tree_contains_labelled_tab_toolbar_page_and_status_nodes() {
        let tree = A11yTree::from_snapshot(&snapshot());
        assert!(tree.find_by_label(A11yRole::Tab, "first.pdf").is_some());
        assert!(tree.find_by_label(A11yRole::Tab, "second.pdf").is_some());
        assert!(tree.find_by_label(A11yRole::Toolbar, "Toolbar").is_some());
        assert!(tree.find_by_label(A11yRole::Page, "Page 2").is_some());
        assert!(
            tree.find_by_label(A11yRole::StatusBar, "High contrast off")
                .is_some()
        );
        assert!(tree.find_by_label(A11yRole::Button, "Read aloud").is_some());
        for node in tree.nodes() {
            assert!(!node.label.is_empty(), "role {:?} has no label", node.role);
        }
    }

    #[test]
    fn tree_marks_active_tab_and_current_page_in_descriptions() {
        let tree = A11yTree::from_snapshot(&snapshot());
        let first = tree
            .node(tree.find_by_label(A11yRole::Tab, "first.pdf").expect("tab"))
            .expect("tab node");
        assert_eq!(first.description, "active");
        let second = tree
            .node(
                tree.find_by_label(A11yRole::Tab, "second.pdf")
                    .expect("tab"),
            )
            .expect("tab node");
        assert_eq!(second.description, "inactive");
        let current = tree
            .node(tree.find_by_label(A11yRole::Page, "Page 2").expect("page"))
            .expect("page node");
        assert_eq!(current.description, "current page");
    }

    #[test]
    fn tree_high_contrast_changes_status_label() {
        let mut snap = snapshot();
        snap.high_contrast = true;
        let tree = A11yTree::from_snapshot(&snap);
        assert!(
            tree.find_by_label(A11yRole::StatusBar, "High contrast on")
                .is_some()
        );
    }

    #[test]
    fn tree_maps_focus_target_to_a_node_in_the_tree() {
        for target in [
            FocusTarget::TabStrip,
            FocusTarget::Toolbar,
            FocusTarget::MenuBar,
            FocusTarget::PageCanvas,
            FocusTarget::ThumbnailPane,
            FocusTarget::FindBar,
            FocusTarget::LayersPanel,
            FocusTarget::StatusBar,
        ] {
            let snap = AccessibilitySnapshot {
                focus_target: target,
                ..snapshot()
            };
            let tree = A11yTree::from_snapshot(&snap);
            assert!(
                tree.node(tree.focus()).is_some(),
                "focus for {target} missing"
            );
        }
    }

    #[test]
    fn set_focus_rejects_unknown_ids() {
        let mut tree = A11yTree::from_snapshot(&snapshot());
        let original = tree.focus();
        assert!(!tree.set_focus(9_999));
        assert_eq!(tree.focus(), original);
        let target = tree
            .find_by_label(A11yRole::Button, "Find")
            .expect("button");
        assert!(tree.set_focus(target));
        assert_eq!(tree.focus(), target);
    }

    #[test]
    fn focus_announcement_reports_label_role_and_spoken_text() {
        let mut tree = A11yTree::from_snapshot(&snapshot());
        let find = tree
            .find_by_label(A11yRole::Button, "Find")
            .expect("button");
        assert!(tree.set_focus(find));
        let announcement = tree.focus_announcement();
        assert_eq!(announcement.node_id, find);
        assert_eq!(announcement.label, "Find");
        assert_eq!(announcement.role, A11yRole::Button);
        assert_eq!(announcement.spoken_text(), "Find, Button");
    }

    #[test]
    fn announcement_log_observes_only_focus_changes() {
        let mut tree = A11yTree::from_snapshot(&snapshot());
        let mut log = AnnouncementLog::new();
        assert!(log.observe(&tree));
        assert_eq!(log.entries().len(), 1);
        assert!(!log.observe(&tree), "unchanged focus is silent");
        let find = tree
            .find_by_label(A11yRole::Button, "Find")
            .expect("button");
        assert!(tree.set_focus(find));
        assert!(log.observe(&tree));
        assert_eq!(log.entries().len(), 2);
        assert_eq!(log.last().expect("last").label, "Find");
        log.clear();
        assert!(log.entries().is_empty());
        assert!(log.observe(&tree));
    }

    #[test]
    fn role_accesskit_names_are_stable() {
        let roles = [
            A11yRole::Window,
            A11yRole::Application,
            A11yRole::TabStrip,
            A11yRole::Tab,
            A11yRole::Toolbar,
            A11yRole::Button,
            A11yRole::Pane,
            A11yRole::Document,
            A11yRole::Page,
            A11yRole::Heading,
            A11yRole::Paragraph,
            A11yRole::Text,
            A11yRole::Edit,
            A11yRole::Panel,
            A11yRole::CheckBox,
            A11yRole::RadioButton,
            A11yRole::List,
            A11yRole::ListItem,
            A11yRole::MenuBar,
            A11yRole::MenuItem,
            A11yRole::StatusBar,
            A11yRole::Image,
            A11yRole::Link,
        ];
        let mut names = std::collections::BTreeSet::new();
        for role in roles {
            assert!(!role.accesskit_name().is_empty());
            names.insert(role.accesskit_name());
        }
        assert_eq!(A11yRole::Tab.accesskit_name(), "Tab");
        assert_eq!(A11yRole::Toolbar.accesskit_name(), "Toolbar");
    }

    #[test]
    fn null_host_discards_updates_without_panicking() {
        let tree = A11yTree::from_snapshot(&snapshot());
        let announcement = tree.focus_announcement();
        let mut host = NullAccessibilityHost;
        host.publish_tree(&tree);
        host.announce(&announcement);
        assert_eq!(host, NullAccessibilityHost);
    }

    #[test]
    fn tree_handles_missing_active_tab_and_empty_snapshot() {
        let snap = AccessibilitySnapshot::new(
            "Empty",
            Vec::new(),
            None,
            0,
            99,
            FocusTarget::MenuBar,
            false,
        );
        let tree = A11yTree::from_snapshot(&snap);
        assert!(tree.is_well_formed());
        assert!(
            tree.find_by_label(A11yRole::Document, "No document")
                .is_some()
        );
        assert!(tree.find_by_label(A11yRole::Page, "Page 1").is_none());
        assert_eq!(tree.node(tree.focus()).expect("focus").label, "Empty");
    }

    #[test]
    fn node_builders_set_description_focus_and_children() {
        let node = A11yNode::new(7, A11yRole::Button, "Ok")
            .with_description("Confirm")
            .focusable(true)
            .disabled(true)
            .with_children(vec![8, 9]);
        assert_eq!(node.description, "Confirm");
        assert!(node.focusable);
        assert!(node.disabled);
        assert_eq!(node.children, vec![8, 9]);
    }
}
