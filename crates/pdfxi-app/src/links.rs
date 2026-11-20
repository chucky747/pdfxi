//! Per-page link index, destination resolution, and hit testing.
//!
//! This module implements the *model* behind the viewer's Links panel and the
//! canvas link-hit-test path. It is dependency-light: no `egui`, renderer, or PDF
//! backend dependency. A caller parses link annotations and named destinations
//! into a [`LinkSummary`], and [`LinkModel::from_summary`] validates it into a
//! per-page index with deterministic ordering. [`LinkRegistry`] isolates models
//! per open tab.
//!
//! # Destination resolution
//!
//! A link target is one of [`LinkTarget::Internal`] (a zero-based page),
//! [`LinkTarget::Uri`], or [`LinkTarget::Named`] (a named destination resolved
//! through the summary's name tree). [`LinkModel::resolve`] turns any target into
//! a [`ResolvedLink`] with a concrete page when one is available; a named target
//! with no matching name resolves to `page: None` rather than an error, so a
//! partially parsed document never fails to load.
//!
//! # Hit testing
//!
//! [`LinkModel::hit_test`] maps a point in unrotated page space (origin at the
//! page's bottom-left, y increasing upward, as PDF uses) to a link. Rooms overlap
//! rarely; when they do, the *last declared* containing link wins (the topmost
//! annotation), which is deterministic. No GUI context or I/O is involved.

use std::collections::HashMap;

use crate::tabs::TabId;

/// Maximum number of link annotations accepted in one summary.
pub const MAX_LINKS: usize = 16_384;

/// Maximum number of named destinations accepted in one summary.
pub const MAX_NAMES: usize = 16_384;

/// Maximum number of characters in a URI or named destination.
pub const MAX_LINK_TEXT_LEN: usize = 2048;

/// A stable key for one link annotation.
pub type LinkId = String;

/// A point in unrotated PDF page space (origin bottom-left, y upward).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LinkPoint {
    /// Horizontal coordinate in PDF points.
    pub x: f32,
    /// Vertical coordinate in PDF points.
    pub y: f32,
}

impl LinkPoint {
    /// Build a page-space point.
    pub fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }
}

/// An axis-aligned link rectangle in unrotated PDF page space.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LinkRect {
    /// Left edge in PDF points.
    pub x: f32,
    /// Bottom edge in PDF points.
    pub y: f32,
    /// Width in PDF points (must be positive).
    pub width: f32,
    /// Height in PDF points (must be positive).
    pub height: f32,
}

impl LinkRect {
    /// Build a rectangle from its lower-left corner and positive extents.
    pub fn new(x: f32, y: f32, width: f32, height: f32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    /// Whether the rectangle has finite, positive extents.
    fn is_valid(&self) -> bool {
        self.x.is_finite()
            && self.y.is_finite()
            && self.width.is_finite()
            && self.height.is_finite()
            && self.width > 0.0
            && self.height > 0.0
    }

    /// Whether `point` lies inside this rectangle (edges inclusive).
    pub fn contains(&self, point: LinkPoint) -> bool {
        point.x.is_finite()
            && point.y.is_finite()
            && point.x >= self.x
            && point.x <= self.x + self.width
            && point.y >= self.y
            && point.y <= self.y + self.height
    }
}

/// The action target of a link annotation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LinkTarget {
    /// A GoTo action to a zero-based page in this document.
    Internal {
        /// The zero-based destination page.
        page: usize,
    },
    /// A URI action, resolved by the host (the app never opens URLs itself).
    Uri(String),
    /// A named destination, resolved through the summary's name tree.
    Named(String),
}

/// One caller-supplied link annotation declaration.
#[derive(Clone, Debug, PartialEq)]
pub struct LinkInfo {
    id: LinkId,
    page: usize,
    rect: LinkRect,
    target: LinkTarget,
}

impl LinkInfo {
    /// Describe one link annotation on `page` occupying `rect`.
    pub fn new(id: impl Into<LinkId>, page: usize, rect: LinkRect, target: LinkTarget) -> Self {
        Self {
            id: id.into(),
            page,
            rect,
            target,
        }
    }

    /// The stable id of this link.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// The zero-based page the annotation is drawn on.
    pub fn page(&self) -> usize {
        self.page
    }

    /// The clickable rectangle in page space.
    pub fn rect(&self) -> LinkRect {
        self.rect
    }

    /// The action target of this link.
    pub fn target(&self) -> &LinkTarget {
        &self.target
    }
}

/// Caller-supplied link annotations and named destinations.
#[derive(Clone, Debug, Default)]
pub struct LinkSummary {
    links: Vec<LinkInfo>,
    names: Vec<(String, usize)>,
}

impl LinkSummary {
    /// Create an empty summary.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a link annotation, in declaration order (later links drawn on top).
    pub fn add_link(mut self, link: LinkInfo) -> Self {
        self.links.push(link);
        self
    }

    /// Register a named destination mapping a name to a zero-based page.
    pub fn add_name(mut self, name: impl Into<String>, page: usize) -> Self {
        self.names.push((name.into(), page));
        self
    }

    /// The declared links.
    pub fn links(&self) -> &[LinkInfo] {
        &self.links
    }

    /// The declared named destinations.
    pub fn names(&self) -> &[(String, usize)] {
        &self.names
    }
}

/// Typed errors produced while parsing or manipulating links.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LinkError {
    /// More than [`MAX_LINKS`] link annotations were declared.
    #[error("too many links (maximum {MAX_LINKS})")]
    TooManyLinks,
    /// More than [`MAX_NAMES`] named destinations were declared.
    #[error("too many named destinations (maximum {MAX_NAMES})")]
    TooManyNames,
    /// A link was declared with an empty id.
    #[error("link id must not be empty")]
    EmptyId,
    /// Two links shared an id.
    #[error("duplicate link id '{0}'")]
    DuplicateId(LinkId),
    /// A link annotation was on a page outside the document.
    #[error("link '{0}' is on page {1} outside the document")]
    PageOutOfBounds(LinkId, usize),
    /// An internal destination page was outside the document.
    #[error("link '{0}' targets page {1} outside the document")]
    DestinationOutOfBounds(LinkId, usize),
    /// A named destination page was outside the document.
    #[error("named destination '{0}' targets page {1} outside the document")]
    NameOutOfBounds(String, usize),
    /// A URI or named destination was empty or over-long.
    #[error("link '{0}' has an invalid URI or name")]
    InvalidLinkText(LinkId),
    /// A named destination name was empty or over-long.
    #[error("named destination has an invalid name")]
    InvalidName,
    /// Two named destinations shared a name.
    #[error("duplicate named destination '{0}'")]
    DuplicateName(String),
    /// A link annotation had invalid (non-finite or non-positive) geometry.
    #[error("link '{0}' has an invalid rectangle")]
    InvalidRect(LinkId),
    /// A runtime operation named a link that is not in the model.
    #[error("no link with id '{0}'")]
    LinkNotFound(LinkId),
    /// The tab has no installed link model.
    #[error("tab {0} has no links")]
    NoLinksForTab(TabId),
}

/// A read-only view of one link in a [`LinkModel`].
#[derive(Clone, Copy, Debug)]
pub struct Link<'a> {
    info: &'a LinkInfo,
}

impl<'a> Link<'a> {
    fn new(info: &'a LinkInfo) -> Self {
        Self { info }
    }

    /// The stable id of this link.
    pub fn id(&self) -> &'a str {
        &self.info.id
    }

    /// The zero-based page the annotation is drawn on.
    pub fn page(&self) -> usize {
        self.info.page
    }

    /// The clickable rectangle in page space.
    pub fn rect(&self) -> LinkRect {
        self.info.rect
    }

    /// The action target of this link.
    pub fn target(&self) -> &'a LinkTarget {
        &self.info.target
    }
}

/// A fully resolved link target.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ResolvedLink {
    /// An internal GoTo destination with a concrete zero-based page.
    InternalPage(usize),
    /// A URI action. Opening it is the host's responsibility.
    Uri(String),
    /// A named destination, with a concrete page when the name was registered.
    NamedPage {
        /// The destination name.
        name: String,
        /// The resolved zero-based page, or `None` if the name was unknown.
        page: Option<usize>,
    },
}

/// A validated, per-page-indexed link model.
///
/// Construct one with [`Self::from_summary`]. Links for a page are stored in
/// declaration order; resolution consults the registered named destinations.
#[derive(Clone, Debug, PartialEq)]
pub struct LinkModel {
    links: Vec<LinkInfo>,
    index: HashMap<LinkId, usize>,
    by_page: HashMap<usize, Vec<usize>>,
    names: HashMap<String, usize>,
    page_count: usize,
}

impl LinkModel {
    /// Validate `summary` into a per-page link index.
    ///
    /// `page_count` bounds-checks annotation pages, internal destinations, and
    /// named-destination pages; pass `0` to skip. Validation rejects empty and
    /// duplicate ids, invalid rectangles, out-of-range pages, and empty or
    /// over-long URI/name strings. Named links whose name is not registered are
    /// allowed and resolve to `page: None`.
    pub fn from_summary(summary: LinkSummary, page_count: usize) -> Result<Self, LinkError> {
        if summary.links.len() > MAX_LINKS {
            return Err(LinkError::TooManyLinks);
        }
        if summary.names.len() > MAX_NAMES {
            return Err(LinkError::TooManyNames);
        }

        let mut names: HashMap<String, usize> = HashMap::new();
        for (name, page) in &summary.names {
            if name.is_empty() || name.chars().count() > MAX_LINK_TEXT_LEN {
                return Err(LinkError::InvalidName);
            }
            if page_count > 0 && *page >= page_count {
                return Err(LinkError::NameOutOfBounds(name.clone(), *page));
            }
            if names.insert(name.clone(), *page).is_some() {
                return Err(LinkError::DuplicateName(name.clone()));
            }
        }

        let mut index: HashMap<LinkId, usize> = HashMap::new();
        let mut by_page: HashMap<usize, Vec<usize>> = HashMap::new();
        for (position, link) in summary.links.iter().enumerate() {
            if link.id.is_empty() {
                return Err(LinkError::EmptyId);
            }
            if !link.rect.is_valid() {
                return Err(LinkError::InvalidRect(link.id.clone()));
            }
            if page_count > 0 && link.page >= page_count {
                return Err(LinkError::PageOutOfBounds(link.id.clone(), link.page));
            }
            match &link.target {
                LinkTarget::Internal { page } => {
                    if page_count > 0 && *page >= page_count {
                        return Err(LinkError::DestinationOutOfBounds(link.id.clone(), *page));
                    }
                }
                LinkTarget::Uri(uri) => {
                    if uri.is_empty() || uri.chars().count() > MAX_LINK_TEXT_LEN {
                        return Err(LinkError::InvalidLinkText(link.id.clone()));
                    }
                }
                LinkTarget::Named(name) => {
                    if name.is_empty() || name.chars().count() > MAX_LINK_TEXT_LEN {
                        return Err(LinkError::InvalidLinkText(link.id.clone()));
                    }
                }
            }
            if index.insert(link.id.clone(), position).is_some() {
                return Err(LinkError::DuplicateId(link.id.clone()));
            }
            by_page.entry(link.page).or_default().push(position);
        }

        Ok(Self {
            links: summary.links,
            index,
            by_page,
            names,
            page_count,
        })
    }

    /// The number of links in this model.
    pub fn len(&self) -> usize {
        self.links.len()
    }

    /// Whether this model has no links.
    pub fn is_empty(&self) -> bool {
        self.links.is_empty()
    }

    /// The page count this model was validated against.
    pub fn page_count(&self) -> usize {
        self.page_count
    }

    /// Links on `page` in deterministic declaration order.
    pub fn links_for_page(&self, page: usize) -> impl Iterator<Item = Link<'_>> + '_ {
        self.by_page
            .get(&page)
            .into_iter()
            .flat_map(|positions| positions.iter())
            .filter_map(|position| self.links.get(*position))
            .map(Link::new)
    }

    /// The number of link annotations on `page`.
    pub fn count_for_page(&self, page: usize) -> usize {
        self.by_page.get(&page).map_or(0, Vec::len)
    }

    /// Look up a link by id.
    pub fn get(&self, id: &str) -> Option<Link<'_>> {
        self.index
            .get(id)
            .map(|position| Link::new(&self.links[*position]))
    }

    /// All link ids in deterministic declaration order.
    pub fn ids(&self) -> impl ExactSizeIterator<Item = &LinkId> + '_ {
        self.links.iter().map(|link| &link.id)
    }

    /// Resolve a link's target to a concrete description.
    pub fn resolve(&self, id: &str) -> Option<ResolvedLink> {
        let link = self.get(id)?;
        Some(match link.target() {
            LinkTarget::Internal { page } => ResolvedLink::InternalPage(*page),
            LinkTarget::Uri(uri) => ResolvedLink::Uri(uri.clone()),
            LinkTarget::Named(name) => ResolvedLink::NamedPage {
                name: name.clone(),
                page: self.names.get(name).copied(),
            },
        })
    }

    /// Resolve a named destination to a zero-based page, if registered.
    pub fn resolve_name(&self, name: &str) -> Option<usize> {
        self.names.get(name).copied()
    }

    /// Hit-test a point on `page`, returning the topmost containing link id.
    ///
    /// The last declared link whose rectangle contains the point wins. A point
    /// outside every rectangle, or on a page with no links, returns `None`.
    pub fn hit_test(&self, page: usize, point: LinkPoint) -> Option<&str> {
        let positions = self.by_page.get(&page)?;
        for position in positions.iter().rev() {
            let link = self.links.get(*position)?;
            if link.rect.contains(point) {
                return Some(link.id.as_str());
            }
        }
        None
    }
}

/// Per-tab registry of link models.
#[derive(Debug, Default)]
pub struct LinkRegistry {
    models: HashMap<TabId, LinkModel>,
}

impl LinkRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Install a validated link model for `tab`.
    pub fn install(
        &mut self,
        tab: TabId,
        summary: LinkSummary,
        page_count: usize,
    ) -> Result<(), LinkError> {
        let model = LinkModel::from_summary(summary, page_count)?;
        self.models.insert(tab, model);
        Ok(())
    }

    /// Remove a tab's links, returning `true` if any were installed.
    pub fn remove(&mut self, tab: TabId) -> bool {
        self.models.remove(&tab).is_some()
    }

    /// Whether `tab` has links installed.
    pub fn contains(&self, tab: TabId) -> bool {
        self.models.contains_key(&tab)
    }

    /// The link model installed for `tab`, if any.
    pub fn model(&self, tab: TabId) -> Option<&LinkModel> {
        self.models.get(&tab)
    }

    /// Links on `page` for `tab`, or an empty iterator when none.
    pub fn links_for_page(&self, tab: TabId, page: usize) -> Vec<Link<'_>> {
        self.models
            .get(&tab)
            .map(|model| model.links_for_page(page).collect())
            .unwrap_or_default()
    }

    /// Resolve a link on `tab`.
    pub fn resolve(&self, tab: TabId, id: &str) -> Result<ResolvedLink, LinkError> {
        let model = self.models.get(&tab).ok_or(LinkError::NoLinksForTab(tab))?;
        model
            .resolve(id)
            .ok_or_else(|| LinkError::LinkNotFound(id.to_string()))
    }

    /// Hit-test a point on `tab`'s `page`.
    pub fn hit_test(&self, tab: TabId, page: usize, point: LinkPoint) -> Option<&str> {
        self.models.get(&tab)?.hit_test(page, point)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tab(id: u64) -> TabId {
        TabId::from_u64(id)
    }

    fn rect(x: f32, y: f32) -> LinkRect {
        LinkRect::new(x, y, 20.0, 10.0)
    }

    fn summary() -> LinkSummary {
        LinkSummary::new()
            .add_link(LinkInfo::new(
                "uri",
                0,
                rect(10.0, 10.0),
                LinkTarget::Uri("https://example.test".into()),
            ))
            .add_link(LinkInfo::new(
                "internal",
                0,
                rect(50.0, 10.0),
                LinkTarget::Internal { page: 2 },
            ))
            .add_link(LinkInfo::new(
                "named",
                1,
                rect(10.0, 30.0),
                LinkTarget::Named("section.intro".into()),
            ))
            .add_link(LinkInfo::new(
                "missing-name",
                1,
                rect(40.0, 30.0),
                LinkTarget::Named("unknown".into()),
            ))
            .add_name("section.intro", 4)
    }

    #[test]
    fn link_index_is_per_page_and_deterministic() {
        let model = LinkModel::from_summary(summary(), 5).expect("valid");
        let page0: Vec<&str> = model.links_for_page(0).map(|item| item.id()).collect();
        let page1: Vec<&str> = model.links_for_page(1).map(|item| item.id()).collect();
        let page2: Vec<&str> = model.links_for_page(2).map(|item| item.id()).collect();
        assert_eq!(page0, vec!["uri", "internal"]);
        assert_eq!(page1, vec!["named", "missing-name"]);
        assert!(page2.is_empty());
        assert_eq!(model.count_for_page(0), 2);
        assert_eq!(model.page_count(), 5);
        // Declaration order is preserved across rebuilds.
        let again = LinkModel::from_summary(summary(), 5).expect("valid");
        assert_eq!(
            model.ids().collect::<Vec<_>>(),
            again.ids().collect::<Vec<_>>()
        );
    }

    #[test]
    fn uri_internal_and_named_destinations_resolve() {
        let model = LinkModel::from_summary(summary(), 5).expect("valid");
        assert_eq!(
            model.resolve("uri"),
            Some(ResolvedLink::Uri("https://example.test".into()))
        );
        assert_eq!(
            model.resolve("internal"),
            Some(ResolvedLink::InternalPage(2))
        );
        assert_eq!(
            model.resolve("named"),
            Some(ResolvedLink::NamedPage {
                name: "section.intro".into(),
                page: Some(4),
            })
        );
        assert_eq!(
            model.resolve("missing-name"),
            Some(ResolvedLink::NamedPage {
                name: "unknown".into(),
                page: None,
            })
        );
        assert_eq!(model.resolve_name("section.intro"), Some(4));
        assert_eq!(model.resolve_name("unknown"), None);
        assert_eq!(model.resolve("ghost"), None);
    }

    #[test]
    fn hit_test_finds_topmost_containing_link() {
        let overlapping = LinkSummary::new()
            .add_link(LinkInfo::new(
                "bottom",
                0,
                LinkRect::new(0.0, 0.0, 100.0, 100.0),
                LinkTarget::Internal { page: 1 },
            ))
            .add_link(LinkInfo::new(
                "top",
                0,
                LinkRect::new(10.0, 10.0, 20.0, 20.0),
                LinkTarget::Uri("https://example.test".into()),
            ));
        let model = LinkModel::from_summary(overlapping, 3).expect("valid");
        assert_eq!(model.hit_test(0, LinkPoint::new(15.0, 15.0)), Some("top"));
        assert_eq!(
            model.hit_test(0, LinkPoint::new(60.0, 60.0)),
            Some("bottom")
        );
        assert_eq!(model.hit_test(0, LinkPoint::new(200.0, 200.0)), None);
        assert_eq!(model.hit_test(1, LinkPoint::new(15.0, 15.0)), None);
        // Non-finite points never match.
        assert_eq!(model.hit_test(0, LinkPoint::new(f32::NAN, 15.0)), None);
    }

    #[test]
    fn empty_link_model_has_no_rows() {
        let model = LinkModel::from_summary(LinkSummary::new(), 3).expect("empty");
        assert!(model.is_empty());
        assert_eq!(model.len(), 0);
        assert_eq!(model.links_for_page(0).count(), 0);
        assert_eq!(model.hit_test(0, LinkPoint::new(1.0, 1.0)), None);
    }

    #[test]
    fn invalid_links_are_rejected() {
        let base = || {
            LinkSummary::new().add_link(LinkInfo::new(
                "a",
                0,
                rect(0.0, 0.0),
                LinkTarget::Internal { page: 1 },
            ))
        };
        assert_eq!(
            LinkModel::from_summary(
                LinkSummary::new().add_link(LinkInfo::new(
                    "",
                    0,
                    rect(0.0, 0.0),
                    LinkTarget::Internal { page: 1 }
                )),
                5
            ),
            Err(LinkError::EmptyId)
        );
        assert_eq!(
            LinkModel::from_summary(
                base().add_link(LinkInfo::new(
                    "a",
                    0,
                    rect(1.0, 1.0),
                    LinkTarget::Internal { page: 2 }
                )),
                5
            ),
            Err(LinkError::DuplicateId("a".into()))
        );
        assert!(matches!(
            LinkModel::from_summary(
                LinkSummary::new().add_link(LinkInfo::new(
                    "bad",
                    0,
                    LinkRect::new(0.0, 0.0, 0.0, 10.0),
                    LinkTarget::Internal { page: 1 }
                )),
                5
            ),
            Err(LinkError::InvalidRect(_))
        ));
        assert_eq!(
            LinkModel::from_summary(
                LinkSummary::new().add_link(LinkInfo::new(
                    "a",
                    0,
                    rect(0.0, 0.0),
                    LinkTarget::Internal { page: 9 }
                )),
                5
            ),
            Err(LinkError::DestinationOutOfBounds("a".into(), 9))
        );
        assert_eq!(
            LinkModel::from_summary(
                LinkSummary::new().add_link(LinkInfo::new(
                    "a",
                    0,
                    rect(0.0, 0.0),
                    LinkTarget::Uri(String::new())
                )),
                5
            ),
            Err(LinkError::InvalidLinkText("a".into()))
        );
        assert_eq!(
            LinkModel::from_summary(
                LinkSummary::new()
                    .add_link(LinkInfo::new(
                        "a",
                        0,
                        rect(0.0, 0.0),
                        LinkTarget::Named("x".into())
                    ))
                    .add_name("x", 9),
                5
            ),
            Err(LinkError::NameOutOfBounds("x".into(), 9))
        );
    }

    #[test]
    fn per_tab_link_state_is_isolated() {
        let mut registry = LinkRegistry::new();
        let a = tab(1);
        let b = tab(2);
        registry.install(a, summary(), 5).expect("a");
        registry.install(b, LinkSummary::new(), 5).expect("b");
        assert_eq!(registry.links_for_page(a, 0).len(), 2);
        assert!(registry.links_for_page(b, 0).is_empty());
        assert_eq!(
            registry.hit_test(a, 0, LinkPoint::new(15.0, 15.0)),
            Some("uri")
        );
        assert_eq!(registry.hit_test(b, 0, LinkPoint::new(15.0, 15.0)), None);
        assert!(registry.remove(a));
        assert!(!registry.contains(a));
        assert!(registry.contains(b));
    }

    #[test]
    fn registry_reports_typed_errors_and_keeps_model_on_failed_install() {
        let mut registry = LinkRegistry::new();
        let a = tab(1);
        assert_eq!(registry.resolve(a, "uri"), Err(LinkError::NoLinksForTab(a)));
        registry.install(a, summary(), 5).expect("a");
        assert!(
            registry
                .install(a, summary().add_name("section.intro", 3), 5)
                .is_err()
        );
        // The original named destination is unchanged after a failed reinstall.
        let model = registry.model(a).expect("kept");
        assert_eq!(model.resolve_name("section.intro"), Some(4));
    }
}
