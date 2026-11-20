//! Editing operations for bookmarks, links, and embedded-file attachments.
//!
//! The model types in [`super`] are round-trip containers; this module adds the
//! copy-on-write editing verbs an authoring tool needs. Bookmarks are addressed
//! by a [`BookmarkPath`] of zero-based child indices from the outline root, so
//! an edit never depends on the transient ids assigned during serialization.
//! Moving a bookmark into itself or one of its descendants is rejected as a
//! [`NavigationError::MoveCreatesCycle`] instead of producing a malformed tree.
//!
//! [`LinkStore`] keeps link annotations grouped by page with stable ids and a
//! deterministic dictionary form. Attachment add/remove is provided by
//! [`super::NameTree::insert`] and [`super::NameTree::remove`]; both keep keys
//! sorted so name-tree output stays deterministic.

use std::collections::BTreeMap;

use crate::geometry::PageIndex;
use crate::pdf::{Dict, Value};

use super::{Bookmark, Link, NavigationError, Outline, link_to_dict, validate_name};

/// A path from the outline root to a bookmark, expressed as child indices.
///
/// The empty path addresses the outline's root list; `[i]` addresses
/// `roots[i]`, `[i, j]` addresses `roots[i].children[j]`, and so on.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BookmarkPath {
    indices: Vec<usize>,
}

impl BookmarkPath {
    /// The empty path, which addresses the root list.
    pub fn root() -> Self {
        Self::default()
    }

    /// Appends a child index, returning the extended path.
    pub fn child(mut self, index: usize) -> Self {
        self.indices.push(index);
        self
    }

    /// Returns the child indices of this path.
    pub fn indices(&self) -> &[usize] {
        &self.indices
    }

    /// Returns `true` when this is the empty root path.
    pub fn is_root(&self) -> bool {
        self.indices.is_empty()
    }

    /// Splits this path into its parent path and final child index.
    ///
    /// Returns `None` for the root path, which has no node of its own.
    pub fn parent_and_index(&self) -> Option<(Self, usize)> {
        let (&last, rest) = self.indices.split_last()?;
        Some((
            Self {
                indices: rest.to_vec(),
            },
            last,
        ))
    }

    /// Returns `true` when `other` is this path or one of its descendants.
    pub fn is_prefix_of(&self, other: &Self) -> bool {
        other.indices.starts_with(&self.indices)
    }
}

/// Resolves a parent path to its immutable child list.
fn list_at<'a>(
    roots: &'a [Bookmark],
    indices: &[usize],
) -> Result<&'a [Bookmark], NavigationError> {
    let Some((&index, rest)) = indices.split_first() else {
        return Ok(roots);
    };
    let len = roots.len();
    let node = roots
        .get(index)
        .ok_or(NavigationError::BookmarkIndexOutOfRange {
            depth: 0,
            index,
            len,
        })?;
    list_at(&node.children, rest).map_err(|error| match error {
        NavigationError::BookmarkIndexOutOfRange { depth, index, len } => {
            NavigationError::BookmarkIndexOutOfRange {
                depth: depth + 1,
                index,
                len,
            }
        }
        other => other,
    })
}

/// Resolves a parent path to its mutable child list.
fn list_at_mut<'a>(
    roots: &'a mut Vec<Bookmark>,
    indices: &[usize],
) -> Result<&'a mut Vec<Bookmark>, NavigationError> {
    let Some((&index, rest)) = indices.split_first() else {
        return Ok(roots);
    };
    let len = roots.len();
    let node = roots
        .get_mut(index)
        .ok_or(NavigationError::BookmarkIndexOutOfRange {
            depth: 0,
            index,
            len,
        })?;
    list_at_mut(&mut node.children, rest).map_err(|error| match error {
        NavigationError::BookmarkIndexOutOfRange { depth, index, len } => {
            NavigationError::BookmarkIndexOutOfRange {
                depth: depth + 1,
                index,
                len,
            }
        }
        other => other,
    })
}

/// Resolves a node path to a mutable bookmark.
fn node_mut<'a>(
    roots: &'a mut Vec<Bookmark>,
    path: &BookmarkPath,
) -> Result<&'a mut Bookmark, NavigationError> {
    let (parent, index) = path
        .parent_and_index()
        .ok_or(NavigationError::BookmarkPathEmpty)?;
    let list = list_at_mut(roots, parent.indices())?;
    let len = list.len();
    list.get_mut(index)
        .ok_or(NavigationError::BookmarkIndexOutOfRange {
            depth: parent.indices().len(),
            index,
            len,
        })
}

impl Outline {
    /// Returns the bookmark addressed by `path`.
    pub fn bookmark_at(&self, path: &BookmarkPath) -> Result<&Bookmark, NavigationError> {
        let (parent, index) = path
            .parent_and_index()
            .ok_or(NavigationError::BookmarkPathEmpty)?;
        let list = list_at(&self.roots, parent.indices())?;
        let len = list.len();
        list.get(index)
            .ok_or(NavigationError::BookmarkIndexOutOfRange {
                depth: parent.indices().len(),
                index,
                len,
            })
    }

    /// Inserts a bookmark at `index` in the child list addressed by `parent`.
    ///
    /// The root list is addressed by [`BookmarkPath::root`]. `index` may equal
    /// the list length to append. The bookmark title is validated before any
    /// mutation occurs.
    pub fn insert_bookmark(
        &mut self,
        parent: &BookmarkPath,
        index: usize,
        bookmark: Bookmark,
    ) -> Result<(), NavigationError> {
        validate_name("Title", &bookmark.title)?;
        let list = list_at_mut(&mut self.roots, parent.indices())?;
        if index > list.len() {
            return Err(NavigationError::BookmarkIndexOutOfRange {
                depth: parent.indices().len(),
                index,
                len: list.len(),
            });
        }
        list.insert(index, bookmark);
        Ok(())
    }

    /// Replaces the title of the bookmark at `path`.
    pub fn rename_bookmark(
        &mut self,
        path: &BookmarkPath,
        title: impl Into<String>,
    ) -> Result<(), NavigationError> {
        let title = title.into();
        validate_name("Title", &title)?;
        node_mut(&mut self.roots, path)?.title = title;
        Ok(())
    }

    /// Removes and returns the bookmark at `path`.
    ///
    /// The root path cannot be removed; use [`Self::insert_bookmark`] and
    /// individual node paths instead.
    pub fn remove_bookmark(&mut self, path: &BookmarkPath) -> Result<Bookmark, NavigationError> {
        let (parent, index) = path
            .parent_and_index()
            .ok_or(NavigationError::BookmarkPathEmpty)?;
        let list = list_at_mut(&mut self.roots, parent.indices())?;
        if index >= list.len() {
            return Err(NavigationError::BookmarkIndexOutOfRange {
                depth: parent.indices().len(),
                index,
                len: list.len(),
            });
        }
        Ok(list.remove(index))
    }

    /// Moves the bookmark at `from` into `to_parent` at `to_index`.
    ///
    /// Both endpoints are validated before any mutation. Moving a node into
    /// itself or a descendant returns [`NavigationError::MoveCreatesCycle`].
    /// When source and destination are the same list, the insertion index is
    /// adjusted for the removal so callers can pass the pre-move index.
    pub fn move_bookmark(
        &mut self,
        from: &BookmarkPath,
        to_parent: &BookmarkPath,
        to_index: usize,
    ) -> Result<(), NavigationError> {
        let (from_parent, from_index) = from
            .parent_and_index()
            .ok_or(NavigationError::BookmarkPathEmpty)?;

        // A cycle forms only when the destination parent is the moved node
        // itself or one of its descendants (i.e. `from` prefixes `to_parent`).
        // Moving a node up to an ancestor or to the root list is always safe.
        if from.is_prefix_of(to_parent) {
            return Err(NavigationError::MoveCreatesCycle);
        }

        let moving = self.bookmark_at(from)?.clone();
        let target_len = list_at(&self.roots, to_parent.indices())?.len();
        if to_index > target_len {
            return Err(NavigationError::BookmarkIndexOutOfRange {
                depth: to_parent.indices().len(),
                index: to_index,
                len: target_len,
            });
        }
        let same_parent = from_parent == *to_parent;
        let insert_index = if same_parent && to_index > from_index {
            to_index - 1
        } else {
            to_index
        };

        let source = list_at_mut(&mut self.roots, from_parent.indices())?;
        source.remove(from_index);
        let target = list_at_mut(&mut self.roots, to_parent.indices())?;
        target.insert(insert_index, moving);
        Ok(())
    }
}

/// A document-level collection of link annotations keyed by page and id.
///
/// Ids are allocated deterministically from zero in insertion order. Links are
/// stored in a [`BTreeMap`] ordered by `(page, id)`, so iteration,
/// [`Self::links_on`], and [`Self::to_dict`] produce stable output.
#[derive(Clone, Debug, Default)]
pub struct LinkStore {
    links: BTreeMap<(usize, u64), Link>,
    next_id: u64,
}

impl LinkStore {
    /// Creates an empty link store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a link to `page`, returning its allocated id.
    pub fn add(&mut self, page: PageIndex, link: Link) -> Result<u64, NavigationError> {
        let id = self.next_id;
        self.next_id = id.checked_add(1).ok_or(NavigationError::LinkIdExhausted)?;
        self.links.insert((page.get(), id), link);
        Ok(id)
    }

    /// Removes and returns the link `id` on `page`.
    pub fn remove(&mut self, page: PageIndex, id: u64) -> Result<Link, NavigationError> {
        self.links
            .remove(&(page.get(), id))
            .ok_or(NavigationError::LinkNotFound {
                page: page.get(),
                id,
            })
    }

    /// Replaces the link `id` on `page`, returning the previous value.
    pub fn replace(
        &mut self,
        page: PageIndex,
        id: u64,
        link: Link,
    ) -> Result<Link, NavigationError> {
        let slot = self
            .links
            .get_mut(&(page.get(), id))
            .ok_or(NavigationError::LinkNotFound {
                page: page.get(),
                id,
            })?;
        Ok(core::mem::replace(slot, link))
    }

    /// Returns `(id, link)` pairs on `page` in ascending id order.
    pub fn links_on(&self, page: PageIndex) -> Vec<(u64, &Link)> {
        self.links
            .range((page.get(), 0)..=(page.get(), u64::MAX))
            .map(|((_, id), link)| (*id, link))
            .collect()
    }

    /// Number of links across all pages.
    pub fn len(&self) -> usize {
        self.links.len()
    }

    /// Returns `true` when the store has no links.
    pub fn is_empty(&self) -> bool {
        self.links.is_empty()
    }

    /// Returns a deterministic `/Links` dictionary sorted by `(page, id)`.
    pub fn to_dict(&self) -> Dict {
        let entries: Vec<Value> = self
            .links
            .iter()
            .map(|((page, id), link)| {
                let mut entry = Dict::new();
                entry.insert(
                    "Page".to_string(),
                    Value::Int(i64::try_from(*page).unwrap_or(i64::MAX)),
                );
                entry.insert(
                    "Id".to_string(),
                    Value::Int(i64::try_from(*id).unwrap_or(i64::MAX)),
                );
                entry.insert("Annot".to_string(), Value::Dictionary(link_to_dict(link)));
                Value::Dictionary(entry)
            })
            .collect();
        let mut dict = Dict::new();
        dict.insert("Links".to_string(), Value::Array(entries));
        dict
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::Rect;
    use crate::navigation::{LinkTarget, name_tree_to_dict};
    use crate::pdf::Value;

    fn simple(title: &str) -> Bookmark {
        Bookmark::new(title).expect("valid test title")
    }

    fn sample() -> Outline {
        Outline::new()
            .with_root(
                simple("A")
                    .with_child(simple("A1").with_child(simple("A1a")))
                    .with_child(simple("A2")),
            )
            .with_root(simple("B"))
    }

    fn path(indices: &[usize]) -> BookmarkPath {
        indices
            .iter()
            .fold(BookmarkPath::root(), |current, &index| current.child(index))
    }

    fn link(uri: &str) -> Link {
        let rect = Rect::new(0.0, 0.0, 10.0, 10.0).expect("valid test rect");
        Link::new(rect, LinkTarget::Uri(uri.to_string()))
    }

    #[test]
    fn bookmark_insert_at_root_and_nested_positions() {
        let mut outline = Outline::new();
        outline
            .insert_bookmark(&BookmarkPath::root(), 0, simple("First"))
            .expect("insert root");
        outline
            .insert_bookmark(&BookmarkPath::root(), 1, simple("Second"))
            .expect("append root");
        outline
            .insert_bookmark(&path(&[0]), 0, simple("Child"))
            .expect("insert child");
        assert_eq!(outline.roots.len(), 2);
        assert_eq!(outline.roots[0].title, "First");
        assert_eq!(outline.roots[0].children[0].title, "Child");
        assert_eq!(outline.roots[1].title, "Second");
        assert_eq!(
            outline.bookmark_at(&path(&[0, 0])).expect("node").title,
            "Child"
        );
    }

    #[test]
    fn bookmark_rename_validates_and_updates_title() {
        let mut outline = sample();
        outline
            .rename_bookmark(&path(&[0, 1]), "Renamed")
            .expect("rename");
        assert_eq!(outline.roots[0].children[1].title, "Renamed");
        assert!(matches!(
            outline.rename_bookmark(&path(&[0, 1]), ""),
            Err(NavigationError::EmptyName { key: "Title" })
        ));
        assert_eq!(
            outline.roots[0].children[1].title, "Renamed",
            "rename was atomic"
        );
    }

    #[test]
    fn bookmark_remove_returns_node_and_reindexes() {
        let mut outline = sample();
        let removed = outline.remove_bookmark(&path(&[0, 0])).expect("remove");
        assert_eq!(removed.title, "A1");
        assert_eq!(removed.children[0].title, "A1a");
        assert_eq!(outline.roots[0].children.len(), 1);
        assert_eq!(outline.roots[0].children[0].title, "A2");
        assert!(matches!(
            outline.remove_bookmark(&BookmarkPath::root()),
            Err(NavigationError::BookmarkPathEmpty)
        ));
    }

    #[test]
    fn bookmark_invalid_path_is_typed_error() {
        let outline = sample();
        assert!(matches!(
            outline.bookmark_at(&BookmarkPath::root()),
            Err(NavigationError::BookmarkPathEmpty)
        ));
        assert!(matches!(
            outline.bookmark_at(&path(&[9])),
            Err(NavigationError::BookmarkIndexOutOfRange {
                depth: 0,
                index: 9,
                len: 2
            })
        ));
        assert!(matches!(
            outline.bookmark_at(&path(&[0, 9])),
            Err(NavigationError::BookmarkIndexOutOfRange {
                depth: 1,
                index: 9,
                len: 2
            })
        ));
    }

    #[test]
    fn bookmark_move_between_parents_preserves_hierarchy() {
        let mut outline = sample();
        // Move A2 (path [0,1]) into B's children at index 0.
        outline
            .move_bookmark(&path(&[0, 1]), &path(&[1]), 0)
            .expect("move");
        assert_eq!(outline.roots[0].children.len(), 1);
        assert_eq!(outline.roots[0].children[0].title, "A1");
        assert_eq!(outline.roots[1].children.len(), 1);
        assert_eq!(outline.roots[1].children[0].title, "A2");
    }

    #[test]
    fn bookmark_move_into_descendant_is_rejected() {
        let mut outline = sample();
        let before = outline.clone();
        assert!(matches!(
            outline.move_bookmark(&path(&[0]), &path(&[0, 0]), 0),
            Err(NavigationError::MoveCreatesCycle)
        ));
        assert!(matches!(
            outline.move_bookmark(&path(&[0]), &path(&[0]), 0),
            Err(NavigationError::MoveCreatesCycle)
        ));
        assert_eq!(outline, before, "rejected moves leave the tree unchanged");
    }

    #[test]
    fn bookmark_move_within_same_parent_adjusts_index() {
        let mut outline = sample();
        // Move A1 (index 0) to pre-move index 2 (append) within the same list.
        outline
            .move_bookmark(&path(&[0, 0]), &path(&[0]), 2)
            .expect("move");
        let titles: Vec<&str> = outline.roots[0]
            .children
            .iter()
            .map(|child| child.title.as_str())
            .collect();
        assert_eq!(titles, vec!["A2", "A1"]);

        // A forward move by one is a no-op after index adjustment.
        outline
            .move_bookmark(&path(&[0, 1]), &path(&[0]), 2)
            .expect("no-op move");
        let titles: Vec<&str> = outline.roots[0]
            .children
            .iter()
            .map(|child| child.title.as_str())
            .collect();
        assert_eq!(titles, vec!["A2", "A1"]);
    }

    #[test]
    fn bookmark_move_to_root_is_allowed() {
        let mut outline = sample();
        outline
            .move_bookmark(&path(&[0, 0]), &BookmarkPath::root(), 2)
            .expect("promote to root");
        let roots: Vec<&str> = outline
            .roots
            .iter()
            .map(|root| root.title.as_str())
            .collect();
        assert_eq!(roots, vec!["A", "B", "A1"]);
        assert_eq!(outline.roots[0].children.len(), 1);
    }

    #[test]
    fn bookmark_edits_keep_serialization_deterministic() {
        let mut outline = sample();
        outline
            .rename_bookmark(&path(&[1]), "B-renamed")
            .expect("rename");
        outline
            .move_bookmark(&path(&[0, 1]), &path(&[1]), 0)
            .expect("move");
        let first = super::super::outline_to_dict(&outline);
        let second = super::super::outline_to_dict(&outline);
        assert_eq!(first, second);
        assert_eq!(
            super::super::dict_to_outline(&first).expect("round-trip"),
            outline
        );
    }

    #[test]
    fn link_store_add_remove_replace_round_trip() {
        let mut store = LinkStore::new();
        let first = store
            .add(PageIndex::new(0), link("https://a"))
            .expect("add");
        let second = store
            .add(PageIndex::new(0), link("https://b"))
            .expect("add");
        assert_eq!((first, second), (0, 1));
        assert_eq!(store.len(), 2);
        let previous = store
            .replace(PageIndex::new(0), first, link("https://c"))
            .expect("replace");
        assert_eq!(previous.target, LinkTarget::Uri("https://a".to_string()));
        let removed = store.remove(PageIndex::new(0), second).expect("remove");
        assert_eq!(removed.target, LinkTarget::Uri("https://b".to_string()));
        assert_eq!(store.len(), 1);
        let remaining = store.links_on(PageIndex::new(0));
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].0, first);
    }

    #[test]
    fn link_store_missing_link_is_typed_error() {
        let mut store = LinkStore::new();
        assert!(store.is_empty());
        assert!(matches!(
            store.remove(PageIndex::new(3), 7),
            Err(NavigationError::LinkNotFound { page: 3, id: 7 })
        ));
        assert!(matches!(
            store.replace(PageIndex::new(3), 7, link("https://x")),
            Err(NavigationError::LinkNotFound { page: 3, id: 7 })
        ));
    }

    #[test]
    fn link_store_links_on_page_is_ordered_and_isolated() {
        let mut store = LinkStore::new();
        for uri in ["https://c", "https://a", "https://b"] {
            store.add(PageIndex::new(1), link(uri)).expect("add page 1");
        }
        store
            .add(PageIndex::new(0), link("https://z"))
            .expect("add page 0");
        let page_one = store.links_on(PageIndex::new(1));
        let uris: Vec<&str> = page_one
            .iter()
            .map(|(_, link)| match &link.target {
                LinkTarget::Uri(uri) => uri.as_str(),
                _ => "other",
            })
            .collect();
        assert_eq!(uris, vec!["https://c", "https://a", "https://b"]);
        assert_eq!(store.links_on(PageIndex::new(2)).len(), 0);
        assert_eq!(store.links_on(PageIndex::new(0)).len(), 1);
    }

    #[test]
    fn link_store_to_dict_is_deterministic() {
        let mut first = LinkStore::new();
        let mut second = LinkStore::new();
        for store in [&mut first, &mut second] {
            store
                .add(PageIndex::new(1), link("https://a"))
                .expect("add");
            store
                .add(PageIndex::new(0), link("https://b"))
                .expect("add");
        }
        let dict = first.to_dict();
        assert_eq!(dict, second.to_dict());
        let entries = dict
            .get("Links")
            .and_then(Value::as_array)
            .expect("Links array");
        assert_eq!(entries.len(), 2);
        assert_eq!(
            entries[0]
                .as_dict()
                .and_then(|entry| entry.get("Page"))
                .and_then(Value::as_int),
            Some(0)
        );
        assert_eq!(
            entries[1]
                .as_dict()
                .and_then(|entry| entry.get("Page"))
                .and_then(Value::as_int),
            Some(1)
        );
    }

    #[test]
    fn name_tree_remove_then_insert_round_trip() {
        let mut tree = super::super::NameTree::new();
        let attachment = super::super::Attachment::new(
            "note.txt",
            "note.txt",
            String::new(),
            "text/plain",
            "stream-1",
        )
        .expect("attachment");
        tree.insert(attachment.clone()).expect("insert");
        let removed = tree.remove("note.txt").expect("remove");
        assert_eq!(removed, attachment);
        assert!(tree.is_empty());
        tree.insert(removed).expect("reinsert");
        let dict = name_tree_to_dict(&tree);
        assert_eq!(
            super::super::dict_to_name_tree(&dict).expect("round-trip"),
            tree
        );
    }

    #[test]
    fn name_tree_remove_missing_is_typed_error() {
        let mut tree = super::super::NameTree::new();
        assert!(matches!(
            tree.remove("missing.pdf"),
            Err(NavigationError::AttachmentNotFound(name)) if name == "missing.pdf"
        ));
    }

    #[test]
    fn bookmark_path_helpers_report_shape() {
        let root = BookmarkPath::root();
        assert!(root.is_root());
        let nested = root.clone().child(1).child(2);
        assert_eq!(nested.indices(), &[1, 2]);
        let (parent, index) = nested.parent_and_index().expect("parent");
        assert_eq!(parent.indices(), &[1]);
        assert_eq!(index, 2);
        assert!(nested.is_prefix_of(&nested.clone().child(9)));
        assert!(!nested.is_prefix_of(&path(&[1, 3])));
        assert!(root.is_prefix_of(&nested));
    }
}
