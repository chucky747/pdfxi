//! Bookmark (outline) destination tree and article-thread summary.
//!
//! This module implements the *model* behind the viewer's Bookmarks panel. It is
//! deliberately dependency-light: it has no `egui`, renderer, or PDF backend
//! dependency. A caller parses a document's outline and article threads into a
//! [`BookmarkSummary`], and [`BookmarkModel::from_summary`] validates it into a
//! deterministic tree. [`BookmarkState`] holds the mutable per-document
//! expand/collapse and selection state, and [`BookmarkRegistry`] isolates that
//! state per open tab.
//!
//! # Ordering and determinism
//!
//! Siblings are kept in declaration (parser) order, which is the order PDF
//! outlines use for `/Next` / `/Prev` chains. A [`BookmarkModel`] always yields
//! nodes in a stable pre-order traversal; a [`BookmarkState`] filters that
//! traversal by each node's expanded flag to produce the visible rows. Ids are
//! compared exactly, so no locale or hash ordering can change the result.
//!
//! # Display boundary
//!
//! [`BookmarkState::visible_rows`] produces a flat list of
//! [`VisibleBookmark`] rows the panel can render. Selecting a row and activating
//! its destination (scrolling to a page and applying a fit mode) is left to the
//! host shell; this module only resolves and reports the destination. No I/O is
//! performed here.

use std::collections::{HashMap, HashSet};

use crate::tabs::TabId;

/// Maximum number of bookmarks accepted in a single summary.
///
/// Bounding the input keeps validation and traversal linear and prevents a
/// hostile document from allocating unbounded memory.
pub const MAX_BOOKMARKS: usize = 4096;

/// Maximum number of characters in a bookmark title.
pub const MAX_BOOKMARK_TITLE_LEN: usize = 512;

/// Maximum number of article threads accepted in a single summary.
pub const MAX_ARTICLE_THREADS: usize = 1024;

/// A stable key for one bookmark.
///
/// In a real PDF this is the indirect object number of an outline item; here it
/// is an owned string so callers can supply whatever identifier their parser
/// produces. Ids are compared exactly.
pub type BookmarkId = String;

/// The fit mode of a bookmark destination (the `/Dest` fit type).
///
/// Optional coordinates mirror a `null` entry in a PDF destination array.
#[derive(Clone, Debug, PartialEq)]
pub enum Fit {
    /// `/Fit` — fit the whole page in the view.
    Fit,
    /// `/FitH top` — fit the page width at the given top coordinate.
    FitH(Option<f32>),
    /// `/FitV left` — fit the page height at the given left coordinate.
    FitV(Option<f32>),
    /// `/FitR left bottom right top` — fit the given rectangle.
    FitR(f32, f32, f32, f32),
    /// `/FitB` — fit the page bounding box.
    FitB,
    /// `/FitBH top` — fit the bounding-box width at `top`.
    FitBH(Option<f32>),
    /// `/FitBV left` — fit the bounding-box height at `left`.
    FitBV(Option<f32>),
    /// `/XYZ left top zoom` — explicit coordinates and zoom.
    Xyz {
        /// Left coordinate, or `None` for "leave unchanged".
        left: Option<f32>,
        /// Top coordinate, or `None` for "leave unchanged".
        top: Option<f32>,
        /// Zoom factor, or `None` for "leave unchanged".
        zoom: Option<f32>,
    },
}

impl Fit {
    /// A short, stable label for this fit mode, suitable for a status line.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Fit => "Fit",
            Self::FitH(_) => "FitH",
            Self::FitV(_) => "FitV",
            Self::FitR(..) => "FitR",
            Self::FitB => "FitB",
            Self::FitBH(_) => "FitBH",
            Self::FitBV(_) => "FitBV",
            Self::Xyz { .. } => "XYZ",
        }
    }

    /// Whether every numeric coordinate carried by this fit is finite.
    fn is_finite(&self) -> bool {
        fn finite(option: &Option<f32>) -> bool {
            option.is_none_or(|value| value.is_finite())
        }
        match self {
            Self::Fit | Self::FitB => true,
            Self::FitH(top) | Self::FitV(top) | Self::FitBH(top) | Self::FitBV(top) => finite(top),
            Self::FitR(left, bottom, right, top) => {
                left.is_finite() && bottom.is_finite() && right.is_finite() && top.is_finite()
            }
            Self::Xyz { left, top, zoom } => finite(left) && finite(top) && finite(zoom),
        }
    }
}

/// A resolved bookmark destination: a zero-based page and a fit mode.
#[derive(Clone, Debug, PartialEq)]
pub struct Destination {
    page: usize,
    fit: Fit,
}

impl Destination {
    /// Build a destination for `page` with the given fit mode.
    pub fn new(page: usize, fit: Fit) -> Self {
        Self { page, fit }
    }

    /// The zero-based target page.
    pub fn page(&self) -> usize {
        self.page
    }

    /// The fit mode applied when the destination is activated.
    pub fn fit(&self) -> &Fit {
        &self.fit
    }
}

/// One caller-supplied bookmark (outline item) declaration.
#[derive(Clone, Debug, PartialEq)]
pub struct BookmarkInfo {
    id: BookmarkId,
    title: String,
    destination: Destination,
    parent: Option<BookmarkId>,
    open: bool,
}

impl BookmarkInfo {
    /// Describe one top-level, initially collapsed bookmark.
    pub fn new(
        id: impl Into<BookmarkId>,
        title: impl Into<String>,
        destination: Destination,
    ) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
            destination,
            parent: None,
            open: false,
        }
    }

    /// Nest this bookmark beneath `parent`.
    pub fn with_parent(mut self, parent: impl Into<BookmarkId>) -> Self {
        self.parent = Some(parent.into());
        self
    }

    /// Set whether this bookmark starts expanded.
    pub fn with_open(mut self, open: bool) -> Self {
        self.open = open;
        self
    }

    /// The stable id of this bookmark.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// The display title of this bookmark.
    pub fn title(&self) -> &str {
        &self.title
    }

    /// The destination this bookmark points at.
    pub fn destination(&self) -> &Destination {
        &self.destination
    }

    /// The parent id, if this is a nested bookmark.
    pub fn parent(&self) -> Option<&str> {
        self.parent.as_deref()
    }

    /// Whether this bookmark starts expanded.
    pub fn is_open(&self) -> bool {
        self.open
    }
}

/// One caller-supplied article thread (a `/Threads` entry).
#[derive(Clone, Debug, PartialEq)]
pub struct ArticleThread {
    id: String,
    title: String,
    pages: Vec<usize>,
}

impl ArticleThread {
    /// Describe an article thread with its id, title, and ordered page indices.
    pub fn new(
        id: impl Into<String>,
        title: impl Into<String>,
        pages: impl IntoIterator<Item = usize>,
    ) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
            pages: pages.into_iter().collect(),
        }
    }

    /// The stable id of this thread.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// The human-readable title of this thread.
    pub fn title(&self) -> &str {
        &self.title
    }

    /// The zero-based pages the thread flows through, in reading order.
    pub fn pages(&self) -> &[usize] {
        &self.pages
    }
}

/// Caller-supplied outline and article-thread data.
///
/// Build one with [`Self::new`] and the builder methods, then pass it to
/// [`BookmarkModel::from_summary`] for validation. No ordering guarantee comes
/// from this type beyond declaration order; determinism is established by the
/// validated model.
#[derive(Clone, Debug, Default)]
pub struct BookmarkSummary {
    bookmarks: Vec<BookmarkInfo>,
    articles: Vec<ArticleThread>,
}

impl BookmarkSummary {
    /// Create an empty summary.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a bookmark declaration, in outline order.
    pub fn add_bookmark(mut self, bookmark: BookmarkInfo) -> Self {
        self.bookmarks.push(bookmark);
        self
    }

    /// Add an article thread, in thread order.
    pub fn add_article(mut self, article: ArticleThread) -> Self {
        self.articles.push(article);
        self
    }

    /// The declared bookmarks.
    pub fn bookmarks(&self) -> &[BookmarkInfo] {
        &self.bookmarks
    }

    /// The declared article threads.
    pub fn articles(&self) -> &[ArticleThread] {
        &self.articles
    }
}

/// Typed errors produced while parsing or manipulating bookmarks.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BookmarkError {
    /// More than [`MAX_BOOKMARKS`] bookmarks were declared.
    #[error("too many bookmarks (maximum {MAX_BOOKMARKS})")]
    TooManyBookmarks,
    /// More than [`MAX_ARTICLE_THREADS`] threads were declared.
    #[error("too many article threads (maximum {MAX_ARTICLE_THREADS})")]
    TooManyArticleThreads,
    /// A bookmark was declared with an empty id.
    #[error("bookmark id must not be empty")]
    EmptyId,
    /// A bookmark was declared with an empty or over-long title.
    #[error("bookmark '{0}' has an invalid title")]
    InvalidTitle(BookmarkId),
    /// Two bookmarks were declared with the same id.
    #[error("duplicate bookmark id '{0}'")]
    DuplicateId(BookmarkId),
    /// A bookmark named a parent that was never declared.
    #[error("bookmark '{0}' references unknown parent '{1}'")]
    UnknownParent(BookmarkId, BookmarkId),
    /// A bookmark was declared as its own parent.
    #[error("bookmark '{0}' cannot be its own parent")]
    SelfParent(BookmarkId),
    /// The parent chain of a bookmark contains a cycle.
    #[error("bookmark '{0}' has a cyclic parent chain")]
    Cycle(BookmarkId),
    /// A destination page was outside the document.
    #[error("bookmark '{0}' targets page {1} outside the document")]
    PageOutOfBounds(BookmarkId, usize),
    /// A destination or article carried a non-finite coordinate.
    #[error("bookmark or article '{0}' carries a non-finite coordinate")]
    NonFiniteCoordinate(String),
    /// An article thread had an empty id/title or no pages.
    #[error("article thread has an invalid id, title, or page list")]
    InvalidArticle,
    /// Two article threads shared an id.
    #[error("duplicate article thread id '{0}'")]
    DuplicateArticle(String),
    /// A runtime operation named a bookmark that is not in the model.
    #[error("no bookmark with id '{0}'")]
    BookmarkNotFound(BookmarkId),
    /// The tab has no installed bookmark model.
    #[error("tab {0} has no bookmarks")]
    NoBookmarksForTab(TabId),
}

/// A read-only view of one bookmark node in a [`BookmarkModel`].
#[derive(Clone, Copy, Debug)]
pub struct Bookmark<'a> {
    info: &'a BookmarkInfo,
}

impl<'a> Bookmark<'a> {
    fn new(info: &'a BookmarkInfo) -> Self {
        Self { info }
    }

    /// The stable id of this bookmark.
    pub fn id(&self) -> &'a str {
        &self.info.id
    }

    /// The display title of this bookmark.
    pub fn title(&self) -> &'a str {
        &self.info.title
    }

    /// The resolved destination of this bookmark.
    pub fn destination(&self) -> &'a Destination {
        &self.info.destination
    }

    /// The parent id, if this is a nested bookmark.
    pub fn parent(&self) -> Option<&'a str> {
        self.info.parent.as_deref()
    }

    /// Whether this bookmark starts expanded.
    pub fn is_open(&self) -> bool {
        self.info.open
    }
}

/// A validated, deterministically ordered bookmark tree.
///
/// Construct one with [`Self::from_summary`]. The model is immutable; mutable
/// expand/collapse and selection state lives in [`BookmarkState`] so the same
/// model can back independent sessions (for example, one per tab).
#[derive(Clone, Debug, PartialEq)]
pub struct BookmarkModel {
    bookmarks: Vec<BookmarkInfo>,
    index: HashMap<BookmarkId, usize>,
    children: HashMap<BookmarkId, Vec<BookmarkId>>,
    roots: Vec<BookmarkId>,
    articles: Vec<ArticleThread>,
    article_index: HashMap<String, usize>,
}

impl BookmarkModel {
    /// Validate `summary` into a deterministic bookmark tree.
    ///
    /// `page_count` bounds-checks destination pages; pass `0` to skip page
    /// bounds checking (useful when the caller has not loaded a document).
    /// Validation rejects empty or duplicate ids, empty or over-long titles,
    /// dangling or self parents, cyclic parent chains, non-finite coordinates,
    /// and out-of-range destination pages and article pages.
    pub fn from_summary(
        summary: BookmarkSummary,
        page_count: usize,
    ) -> Result<Self, BookmarkError> {
        if summary.bookmarks.len() > MAX_BOOKMARKS {
            return Err(BookmarkError::TooManyBookmarks);
        }
        if summary.articles.len() > MAX_ARTICLE_THREADS {
            return Err(BookmarkError::TooManyArticleThreads);
        }

        let mut index: HashMap<BookmarkId, usize> = HashMap::new();
        for (position, bookmark) in summary.bookmarks.iter().enumerate() {
            if bookmark.id.is_empty() {
                return Err(BookmarkError::EmptyId);
            }
            if bookmark.title.is_empty() || bookmark.title.chars().count() > MAX_BOOKMARK_TITLE_LEN
            {
                return Err(BookmarkError::InvalidTitle(bookmark.id.clone()));
            }
            if !bookmark.destination.fit.is_finite() {
                return Err(BookmarkError::NonFiniteCoordinate(bookmark.id.clone()));
            }
            if page_count > 0 && bookmark.destination.page >= page_count {
                return Err(BookmarkError::PageOutOfBounds(
                    bookmark.id.clone(),
                    bookmark.destination.page,
                ));
            }
            if index.insert(bookmark.id.clone(), position).is_some() {
                return Err(BookmarkError::DuplicateId(bookmark.id.clone()));
            }
        }

        let mut children: HashMap<BookmarkId, Vec<BookmarkId>> = HashMap::new();
        let mut roots: Vec<BookmarkId> = Vec::new();
        for bookmark in &summary.bookmarks {
            match bookmark.parent.as_deref() {
                None => roots.push(bookmark.id.clone()),
                Some(parent) if parent == bookmark.id => {
                    return Err(BookmarkError::SelfParent(bookmark.id.clone()));
                }
                Some(parent) => {
                    if !index.contains_key(parent) {
                        return Err(BookmarkError::UnknownParent(
                            bookmark.id.clone(),
                            parent.to_string(),
                        ));
                    }
                    children
                        .entry(parent.to_string())
                        .or_default()
                        .push(bookmark.id.clone());
                }
            }
        }

        // Parent chains are at most `n` links long before repeating. Detecting a
        // repeat rejects cycles of any shape, including indirect ones.
        for bookmark in &summary.bookmarks {
            let mut seen: HashSet<&str> = HashSet::new();
            let mut cursor: Option<&str> = Some(bookmark.id.as_str());
            while let Some(id) = cursor {
                if !seen.insert(id) {
                    return Err(BookmarkError::Cycle(bookmark.id.clone()));
                }
                cursor = index
                    .get(id)
                    .and_then(|position| summary.bookmarks.get(*position))
                    .and_then(|current| current.parent.as_deref());
            }
        }

        let mut articles: Vec<ArticleThread> = Vec::new();
        let mut article_index: HashMap<String, usize> = HashMap::new();
        for article in summary.articles {
            if article.id.is_empty()
                || article.title.is_empty()
                || article.pages.is_empty()
                || page_count > 0 && article.pages.iter().any(|page| *page >= page_count)
            {
                return Err(BookmarkError::InvalidArticle);
            }
            if article_index
                .insert(article.id.clone(), articles.len())
                .is_some()
            {
                return Err(BookmarkError::DuplicateArticle(article.id));
            }
            articles.push(article);
        }

        Ok(Self {
            bookmarks: summary.bookmarks,
            index,
            children,
            roots,
            articles,
            article_index,
        })
    }

    /// The number of bookmarks in this tree.
    pub fn len(&self) -> usize {
        self.bookmarks.len()
    }

    /// Whether this tree has no bookmarks.
    pub fn is_empty(&self) -> bool {
        self.bookmarks.is_empty()
    }

    /// Look up a bookmark by id.
    pub fn get(&self, id: &str) -> Option<Bookmark<'_>> {
        self.index
            .get(id)
            .map(|position| Bookmark::new(&self.bookmarks[*position]))
    }

    /// The root bookmark ids in declaration order.
    pub fn roots(&self) -> &[BookmarkId] {
        &self.roots
    }

    /// The direct child ids of `id` in declaration order.
    pub fn children(&self, id: &str) -> &[BookmarkId] {
        self.children.get(id).map_or(&[], Vec::as_slice)
    }

    /// All bookmark ids in deterministic pre-order, regardless of expansion.
    pub fn flatten(&self) -> Vec<(BookmarkId, usize)> {
        let mut rows = Vec::with_capacity(self.bookmarks.len());
        let mut stack: Vec<(BookmarkId, usize)> =
            self.roots.iter().rev().map(|id| (id.clone(), 0)).collect();
        while let Some((id, depth)) = stack.pop() {
            rows.push((id.clone(), depth));
            if let Some(children) = self.children.get(&id) {
                for child in children.iter().rev() {
                    stack.push((child.clone(), depth + 1));
                }
            }
        }
        rows
    }

    /// The article threads in declaration order.
    pub fn articles(&self) -> impl ExactSizeIterator<Item = &ArticleThread> + '_ {
        self.articles.iter()
    }

    /// Look up an article thread by id.
    pub fn article(&self, id: &str) -> Option<&ArticleThread> {
        self.article_index
            .get(id)
            .map(|position| &self.articles[*position])
    }
}

/// One visible row produced by [`BookmarkState::visible_rows`].
#[derive(Clone, Debug, PartialEq)]
pub struct VisibleBookmark {
    id: BookmarkId,
    title: String,
    depth: usize,
    has_children: bool,
    expanded: bool,
    page: usize,
    fit: Fit,
    selected: bool,
}

impl VisibleBookmark {
    /// The bookmark id for this row.
    pub fn id(&self) -> &str {
        &self.id
    }
    /// The display title for this row.
    pub fn title(&self) -> &str {
        &self.title
    }
    /// The nesting depth (0 for a root).
    pub fn depth(&self) -> usize {
        self.depth
    }
    /// Whether the bookmark has children.
    pub fn has_children(&self) -> bool {
        self.has_children
    }
    /// Whether the bookmark is currently expanded.
    pub fn expanded(&self) -> bool {
        self.expanded
    }
    /// The zero-based destination page.
    pub fn page(&self) -> usize {
        self.page
    }
    /// The destination fit mode.
    pub fn fit(&self) -> &Fit {
        &self.fit
    }
    /// Whether this row is the current selection.
    pub fn selected(&self) -> bool {
        self.selected
    }
}

/// Mutable expand/collapse and selection state for one document's bookmarks.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BookmarkState {
    expanded: HashSet<BookmarkId>,
    selected: Option<BookmarkId>,
}

impl BookmarkState {
    /// Create initial state from a model's per-bookmark `open` flags.
    pub fn new(model: &BookmarkModel) -> Self {
        let expanded = model
            .bookmarks
            .iter()
            .filter(|bookmark| bookmark.open)
            .map(|bookmark| bookmark.id.clone())
            .collect();
        Self {
            expanded,
            selected: None,
        }
    }

    /// Whether `id` is currently expanded.
    pub fn is_expanded(&self, id: &str) -> bool {
        self.expanded.contains(id)
    }

    /// Set the expanded flag of `id`, returning the new value.
    pub fn set_expanded(&mut self, id: &str, value: bool) -> bool {
        if value {
            self.expanded.insert(id.to_string());
        } else {
            self.expanded.remove(id);
        }
        value
    }

    /// Flip the expanded flag of `id`, returning the new value.
    pub fn toggle_expanded(&mut self, id: &str) -> bool {
        let new_value = !self.is_expanded(id);
        self.set_expanded(id, new_value);
        new_value
    }

    /// Select `id`, returning a typed error when it is not in `model`.
    pub fn select(&mut self, model: &BookmarkModel, id: &str) -> Result<(), BookmarkError> {
        if model.get(id).is_none() {
            return Err(BookmarkError::BookmarkNotFound(id.to_string()));
        }
        self.selected = Some(id.to_string());
        Ok(())
    }

    /// Clear the current selection.
    pub fn clear_selection(&mut self) {
        self.selected = None;
    }

    /// The currently selected bookmark id, if any.
    pub fn selected(&self) -> Option<&str> {
        self.selected.as_deref()
    }

    /// Build the flat list of rows currently visible in the tree.
    ///
    /// The traversal is pre-order and skips children of collapsed nodes. It uses
    /// an explicit stack so a deeply nested tree cannot overflow the call stack.
    pub fn visible_rows(&self, model: &BookmarkModel) -> Vec<VisibleBookmark> {
        let mut rows = Vec::new();
        let mut stack: Vec<(BookmarkId, usize)> =
            model.roots.iter().rev().map(|id| (id.clone(), 0)).collect();
        while let Some((id, depth)) = stack.pop() {
            let Some(bookmark) = model.get(&id) else {
                continue;
            };
            let children = model.children(&id);
            let has_children = !children.is_empty();
            let expanded = has_children && self.is_expanded(&id);
            rows.push(VisibleBookmark {
                id: id.clone(),
                title: bookmark.title().to_string(),
                depth,
                has_children,
                expanded,
                page: bookmark.destination().page(),
                fit: bookmark.destination().fit().clone(),
                selected: self.selected.as_deref() == Some(id.as_str()),
            });
            if expanded {
                for child in children.iter().rev() {
                    stack.push((child.clone(), depth + 1));
                }
            }
        }
        rows
    }
}

/// One tab's installed bookmark model and state.
#[derive(Clone, Debug)]
struct BookmarkDocument {
    model: BookmarkModel,
    state: BookmarkState,
}

/// Per-tab registry of bookmark trees and their expand/selection state.
///
/// Each tab gets its own [`BookmarkState`], so collapsing or selecting a
/// bookmark in one document never affects another. All operations are purely
/// in-memory and return immediately; the UI never blocks on I/O here.
#[derive(Debug, Default)]
pub struct BookmarkRegistry {
    documents: HashMap<TabId, BookmarkDocument>,
}

impl BookmarkRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Install a validated bookmark tree for `tab`, resetting its state.
    ///
    /// An invalid summary is rejected and leaves any previous installation for
    /// that tab untouched.
    pub fn install(
        &mut self,
        tab: TabId,
        summary: BookmarkSummary,
        page_count: usize,
    ) -> Result<(), BookmarkError> {
        let model = BookmarkModel::from_summary(summary, page_count)?;
        let state = BookmarkState::new(&model);
        self.documents
            .insert(tab, BookmarkDocument { model, state });
        Ok(())
    }

    /// Remove a tab's bookmarks, returning `true` if any were installed.
    pub fn remove(&mut self, tab: TabId) -> bool {
        self.documents.remove(&tab).is_some()
    }

    /// Whether `tab` has bookmarks installed.
    pub fn contains(&self, tab: TabId) -> bool {
        self.documents.contains_key(&tab)
    }

    /// The bookmark model installed for `tab`, if any.
    pub fn model(&self, tab: TabId) -> Option<&BookmarkModel> {
        self.documents.get(&tab).map(|document| &document.model)
    }

    /// The expand/selection state installed for `tab`, if any.
    pub fn state(&self, tab: TabId) -> Option<&BookmarkState> {
        self.documents.get(&tab).map(|document| &document.state)
    }

    /// Mutable access to the state installed for `tab`, if any.
    ///
    /// Callers must use the matching [`Self::model`] with this state.
    pub fn state_mut(&mut self, tab: TabId) -> Option<&mut BookmarkState> {
        self.documents
            .get_mut(&tab)
            .map(|document| &mut document.state)
    }

    /// Toggle a bookmark's expanded flag for `tab`, returning the new value.
    pub fn toggle_expanded(&mut self, tab: TabId, id: &str) -> Result<bool, BookmarkError> {
        let document = self
            .documents
            .get_mut(&tab)
            .ok_or(BookmarkError::NoBookmarksForTab(tab))?;
        if document.model.get(id).is_none() {
            return Err(BookmarkError::BookmarkNotFound(id.to_string()));
        }
        Ok(document.state.toggle_expanded(id))
    }

    /// Select a bookmark for `tab`.
    pub fn select(&mut self, tab: TabId, id: &str) -> Result<(), BookmarkError> {
        let document = self
            .documents
            .get_mut(&tab)
            .ok_or(BookmarkError::NoBookmarksForTab(tab))?;
        document.state.select(&document.model, id)
    }

    /// The visible bookmark rows for `tab`, or an empty slice when none.
    pub fn visible_rows(&self, tab: TabId) -> Vec<VisibleBookmark> {
        self.documents
            .get(&tab)
            .map(|document| document.state.visible_rows(&document.model))
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tab(id: u64) -> TabId {
        TabId::from_u64(id)
    }

    fn dest(page: usize) -> Destination {
        Destination::new(page, Fit::Fit)
    }

    fn tree() -> BookmarkSummary {
        BookmarkSummary::new()
            .add_bookmark(
                BookmarkInfo::new("a", "Chapter A", dest(0))
                    .with_open(true)
                    .with_parent("root"),
            )
            .add_bookmark(BookmarkInfo::new("root", "Root", dest(0)).with_open(true))
            .add_bookmark(BookmarkInfo::new("a1", "Section A.1", dest(2)).with_parent("a"))
            .add_bookmark(BookmarkInfo::new("b", "Chapter B", dest(5)))
    }

    #[test]
    fn bookmark_tree_order_and_nesting_are_deterministic() {
        let model = BookmarkModel::from_summary(tree(), 10).expect("valid tree");
        // Roots are in declaration order (root was declared second, b last).
        assert_eq!(model.roots(), &["root", "b"]);
        assert_eq!(model.children("root"), &["a"]);
        assert_eq!(model.children("a"), &["a1"]);
        // Pre-order: root, a, a1, then b.
        let flattened = model.flatten();
        let flat: Vec<(&str, usize)> = flattened
            .iter()
            .map(|(id, depth)| (id.as_str(), *depth))
            .collect();
        assert_eq!(flat, vec![("root", 0), ("a", 1), ("a1", 2), ("b", 0)]);
        // Building twice yields the identical order.
        let again = BookmarkModel::from_summary(tree(), 10).expect("valid tree");
        assert_eq!(model.flatten(), again.flatten());
    }

    #[test]
    fn collapse_and_selection_shape_visible_rows() {
        let model = BookmarkModel::from_summary(tree(), 10).expect("valid tree");
        let mut state = BookmarkState::new(&model);
        // root and a start open, so all nodes are visible.
        let rows = state.visible_rows(&model);
        assert_eq!(
            rows.iter().map(VisibleBookmark::id).collect::<Vec<_>>(),
            vec!["root", "a", "a1", "b"]
        );
        // `a` starts expanded, so toggling collapses it and hides child a1.
        assert!(!state.toggle_expanded("a"));
        assert!(!state.is_expanded("a"));
        let collapsed = state.visible_rows(&model);
        let ids: Vec<&str> = collapsed.iter().map(VisibleBookmark::id).collect();
        assert_eq!(ids, vec!["root", "a", "b"]);
        // Selection is reflected on the row and does not change ordering.
        state.select(&model, "b").expect("known bookmark");
        let selected_rows = state.visible_rows(&model);
        let selected: Vec<&str> = selected_rows
            .iter()
            .filter(|row| row.selected())
            .map(VisibleBookmark::id)
            .collect();
        assert_eq!(selected, vec!["b"]);
        state.clear_selection();
        assert_eq!(state.selected(), None);
    }

    #[test]
    fn destination_fit_variants_resolve() {
        let fits = [
            Fit::Fit,
            Fit::FitH(Some(720.0)),
            Fit::FitV(None),
            Fit::FitR(10.0, 20.0, 30.0, 40.0),
            Fit::FitB,
            Fit::FitBH(Some(100.0)),
            Fit::FitBV(None),
            Fit::Xyz {
                left: Some(0.0),
                top: None,
                zoom: Some(1.5),
            },
        ];
        for (index, fit) in fits.iter().enumerate() {
            let destination = Destination::new(index, fit.clone());
            assert_eq!(destination.page(), index);
            assert_eq!(destination.fit(), fit);
            assert!(!fit.label().is_empty());
        }
        let model = BookmarkModel::from_summary(
            BookmarkSummary::new().add_bookmark(BookmarkInfo::new(
                "xyz",
                "Zoom target",
                Destination::new(1, fits[7].clone()),
            )),
            5,
        )
        .expect("valid destination");
        let got = model.get("xyz").expect("bookmark present");
        assert_eq!(got.destination().page(), 1);
        assert_eq!(got.destination().fit().label(), "XYZ");
    }

    #[test]
    fn article_thread_summary_is_ordered_and_queryable() {
        let summary = tree()
            .add_article(ArticleThread::new("t1", "Feature", [0, 1, 2]))
            .add_article(ArticleThread::new("t2", "Review", [3, 4]));
        let model = BookmarkModel::from_summary(summary, 10).expect("valid articles");
        let titles: Vec<&str> = model.articles().map(ArticleThread::title).collect();
        assert_eq!(titles, vec!["Feature", "Review"]);
        assert_eq!(model.article("t1").expect("thread").pages(), &[0, 1, 2]);
        assert!(model.article("missing").is_none());
    }

    #[test]
    fn empty_summary_has_no_rows_or_articles() {
        let model = BookmarkModel::from_summary(BookmarkSummary::new(), 3).expect("empty tree");
        assert!(model.is_empty());
        assert_eq!(model.len(), 0);
        assert!(model.flatten().is_empty());
        assert_eq!(model.articles().len(), 0);
        let state = BookmarkState::new(&model);
        assert!(state.visible_rows(&model).is_empty());
    }

    #[test]
    fn invalid_bookmarks_are_rejected() {
        let base = || BookmarkSummary::new().add_bookmark(BookmarkInfo::new("a", "A", dest(0)));
        assert_eq!(
            BookmarkModel::from_summary(
                BookmarkSummary::new().add_bookmark(BookmarkInfo::new("", "A", dest(0))),
                5
            ),
            Err(BookmarkError::EmptyId)
        );
        assert_eq!(
            BookmarkModel::from_summary(
                BookmarkSummary::new().add_bookmark(BookmarkInfo::new("a", "", dest(0))),
                5
            ),
            Err(BookmarkError::InvalidTitle("a".into()))
        );
        assert_eq!(
            BookmarkModel::from_summary(
                base().add_bookmark(BookmarkInfo::new("a", "Again", dest(1))),
                5
            ),
            Err(BookmarkError::DuplicateId("a".into()))
        );
        assert_eq!(
            BookmarkModel::from_summary(
                base().add_bookmark(BookmarkInfo::new("b", "B", dest(0)).with_parent("ghost")),
                5
            ),
            Err(BookmarkError::UnknownParent("b".into(), "ghost".into()))
        );
        assert_eq!(
            BookmarkModel::from_summary(
                base().add_bookmark(BookmarkInfo::new("b", "B", dest(0)).with_parent("b")),
                5
            ),
            Err(BookmarkError::SelfParent("b".into()))
        );
        assert_eq!(
            BookmarkModel::from_summary(
                base().add_bookmark(BookmarkInfo::new("a", "A", dest(9))),
                5
            ),
            Err(BookmarkError::PageOutOfBounds("a".into(), 9))
        );
        assert_eq!(
            BookmarkModel::from_summary(
                BookmarkSummary::new().add_bookmark(BookmarkInfo::new(
                    "a",
                    "A",
                    Destination::new(0, Fit::FitH(Some(f32::NAN)))
                )),
                5
            ),
            Err(BookmarkError::NonFiniteCoordinate("a".into()))
        );
    }

    #[test]
    fn cyclic_parent_chain_is_rejected() {
        // a -> b -> c -> a
        let cyclic = BookmarkSummary::new()
            .add_bookmark(BookmarkInfo::new("a", "A", dest(0)).with_parent("c"))
            .add_bookmark(BookmarkInfo::new("b", "B", dest(0)).with_parent("a"))
            .add_bookmark(BookmarkInfo::new("c", "C", dest(0)).with_parent("b"));
        assert!(matches!(
            BookmarkModel::from_summary(cyclic, 5),
            Err(BookmarkError::Cycle(_))
        ));
    }

    #[test]
    fn per_tab_bookmark_state_is_isolated() {
        let mut registry = BookmarkRegistry::new();
        let a = tab(1);
        let b = tab(2);
        registry.install(a, tree(), 10).expect("tree a");
        registry.install(b, tree(), 10).expect("tree b");
        assert!(!registry.toggle_expanded(a, "a").expect("known"));
        registry.select(a, "b").expect("known");
        assert!(!registry.state(a).expect("a").is_expanded("a"));
        assert!(registry.state(b).expect("b").is_expanded("a"));
        assert_eq!(registry.state(a).expect("a").selected(), Some("b"));
        assert_eq!(registry.state(b).expect("b").selected(), None);
        assert!(registry.remove(a));
        assert!(!registry.contains(a));
        assert!(registry.contains(b));
        assert!(registry.visible_rows(a).is_empty());
    }

    #[test]
    fn registry_reports_typed_errors_and_keeps_state_on_failed_install() {
        let mut registry = BookmarkRegistry::new();
        let a = tab(1);
        assert_eq!(
            registry.toggle_expanded(a, "a"),
            Err(BookmarkError::NoBookmarksForTab(a))
        );
        registry.install(a, tree(), 10).expect("tree");
        assert_eq!(
            registry.select(a, "ghost"),
            Err(BookmarkError::BookmarkNotFound("ghost".into()))
        );
        assert_eq!(
            registry.toggle_expanded(a, "ghost"),
            Err(BookmarkError::BookmarkNotFound("ghost".into()))
        );
        registry.toggle_expanded(a, "a").expect("known");
        let bad = BookmarkSummary::new().add_bookmark(BookmarkInfo::new("a", "", dest(0)));
        assert!(registry.install(a, bad, 10).is_err());
        // Previous state survives a failed reinstall.
        assert!(!registry.state(a).expect("kept").is_expanded("a"));
    }
}
