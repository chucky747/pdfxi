//! Reading a real document's `/Outlines` tree into [`Outline`].
//!
//! This is the document-backed counterpart of the id-based dictionary model in
//! [`super::outline`]. It walks the catalog `/Outlines` `/First`/`/Next` linked
//! list through [`pdfxi_core::Document`], resolves each item's `/Dest` or
//! `/A /D` destination to a zero-based page index through the document page
//! tree (including `/Dests` and `/Names /Dests` named destinations), and builds
//! the existing [`Outline`]/[`Bookmark`] model.
//!
//! # Honest destinations
//!
//! An item whose destination is absent, is not a `GoTo` action, or points at a
//! name or page that cannot be resolved still becomes a bookmark, but with no
//! destination ([`Bookmark::destination`] is `None`) and an [`OutlineIssue`]
//! that names the reason. No page index is invented.
//!
//! # Hostile trees
//!
//! A malformed linked list — a repeated object, a wrong `/Parent`, a `/First`
//! without a matching `/Last`, an item that is not a dictionary, or a missing
//! title — returns a typed [`OutlineReadError`]. The walk is bounded by an item
//! budget and a depth limit, and every reference is followed through a
//! cycle-checked resolver, so a hostile file cannot loop or overflow the stack.
//!
//! # Named destinations
//!
//! `/Dest` and `/A /D` may be a name or string instead of an array. The reader
//! looks the name up in the catalog `/Dests` dictionary first and then in the
//! `/Names /Dests` name tree, following `/Kids`. A named destination value may
//! be a destination array or a dictionary with `/D`.

use std::collections::{HashMap, HashSet};

use pdfxi_core::{
    Dictionary, Document, Object, ObjectId, Page, decode_text_string, encode_text_string,
};
use thiserror::Error;

use crate::annotation::Color;
use crate::geometry::PageIndex;

use super::{Bookmark, FitType, Outline};

/// Maximum number of outline items read from one document.
pub const MAX_OUTLINE_ITEMS: usize = 100_000;

/// Maximum nesting depth of an outline tree.
pub const MAX_OUTLINE_DEPTH: usize = 64;

/// Maximum number of indirect references followed while resolving one value.
const MAX_REFERENCE_DEPTH: usize = 32;

/// Maximum depth of a `/Names` name tree.
const MAX_NAME_TREE_DEPTH: usize = 32;

/// `/F` flag bit for an italic bookmark title.
const FLAG_ITALIC: i64 = 1;
/// `/F` flag bit for a bold bookmark title.
const FLAG_BOLD: i64 = 2;

/// A parsed outline plus the items whose destination could not be resolved.
///
/// `present` distinguishes a document with no catalog `/Outlines` entry from
/// one whose outline dictionary is present but empty.
#[derive(Clone, Debug, PartialEq)]
pub struct OutlineRead {
    /// `true` when the catalog has an `/Outlines` entry.
    pub present: bool,
    /// The parsed bookmark tree, in display order.
    pub outline: Outline,
    /// Items kept without a destination, with the reason.
    pub issues: Vec<OutlineIssue>,
}

/// One outline item whose destination could not be represented.
///
/// The item is still present in [`OutlineRead::outline`], with
/// [`Bookmark::destination`] set to `None`, so a caller can show the title and
/// report why it does not jump anywhere.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutlineIssue {
    /// Indirect identity of the outline item dictionary.
    pub object: ObjectId,
    /// The item `/Title`, decoded.
    pub title: String,
    /// Why the destination is absent.
    pub reason: OutlineIssueReason,
}

/// Why an outline item has no resolved destination.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OutlineIssueReason {
    /// The item has neither `/Dest` nor a `/GoTo` `/A /D`.
    MissingDestination,
    /// The item has an action, but its `/S` is not `/GoTo`.
    NonGoToAction(String),
    /// A named destination was referenced but is not present in the document.
    UnresolvedNamedDestination(String),
    /// A destination value was the wrong shape or used an invalid fit.
    InvalidDestination(String),
    /// A destination's page reference is not a page of this document.
    DanglingPageReference(ObjectId),
}

/// A failure while reading a document's outline tree.
///
/// Malformed structure is an error; an unresolvable destination is an
/// [`OutlineIssue`] and does not stop the read.
#[derive(Debug, Error)]
pub enum OutlineReadError {
    /// The core document could not resolve an object.
    #[error("PDF structure error: {0}")]
    Core(#[from] pdfxi_core::Error),
    /// The catalog object is not a dictionary.
    #[error("catalog object {0} is not a dictionary")]
    CatalogNotDictionary(ObjectId),
    /// The catalog `/Outlines` entry is neither a dictionary nor a reference.
    #[error("catalog /Outlines is neither a dictionary nor an indirect reference")]
    MalformedOutlines,
    /// An item in the `/First`/`/Next` chain is not a dictionary.
    #[error("outline item {object} is not a dictionary")]
    ItemNotDictionary {
        /// The item that was not a dictionary.
        object: ObjectId,
    },
    /// An outline item has no string `/Title`.
    #[error("outline item {object} has no string /Title")]
    MissingTitle {
        /// The item without a title.
        object: ObjectId,
    },
    /// An outline item title is empty or too long for the model.
    #[error("outline item {object} has an invalid /Title: {reason}")]
    InvalidTitle {
        /// The item with the invalid title.
        object: ObjectId,
        /// The validation failure.
        reason: String,
    },
    /// An outline item was reached twice through `/First`/`/Next`.
    #[error("outline item {object} repeats in the /First or /Next chain")]
    Cycle {
        /// The repeated item.
        object: ObjectId,
    },
    /// An item's `/Parent` does not match the chain that reaches it.
    #[error("outline item {object} has /Parent {found:?}, expected {expected:?}")]
    BrokenParent {
        /// The item with the wrong parent.
        object: ObjectId,
        /// The expected parent id.
        expected: ObjectId,
        /// The actual `/Parent` value, if any.
        found: Option<ObjectId>,
    },
    /// A linked-list entry is inconsistent or the wrong type.
    #[error("outline linked list is inconsistent at {object}: {reason}")]
    BrokenLink {
        /// The object where the inconsistency was found.
        object: ObjectId,
        /// What was inconsistent.
        reason: &'static str,
    },
    /// The outline has more items than [`MAX_OUTLINE_ITEMS`].
    #[error("outline exceeds the {limit} item budget")]
    ItemBudgetExceeded {
        /// The configured limit.
        limit: usize,
    },
    /// The outline nests deeper than [`MAX_OUTLINE_DEPTH`].
    #[error("outline exceeds the {limit} level depth limit")]
    DepthLimitExceeded {
        /// The configured limit.
        limit: usize,
    },
    /// An indirect reference chain is cyclic.
    #[error("indirect reference cycle at object {object}")]
    ReferenceCycle {
        /// The repeated object.
        object: ObjectId,
    },
    /// An indirect reference chain exceeded the internal link limit.
    #[error("indirect reference chain exceeds {limit} links")]
    ReferenceDepthExceeded {
        /// The configured limit.
        limit: usize,
    },
}

/// Read a document's catalog `/Outlines` tree into [`Outline`].
///
/// A document without `/Outlines` returns `present = false` and an empty
/// outline. An item without a resolvable destination is kept with no
/// destination and reported in [`OutlineRead::issues`]. Malformed linked-list
/// structure returns [`OutlineReadError`].
pub fn read_outline(document: &Document) -> Result<OutlineRead, OutlineReadError> {
    let root_id = document.root()?;
    let catalog_object = document.object(root_id)?;
    let catalog = catalog_object
        .as_dictionary()
        .ok_or(OutlineReadError::CatalogNotDictionary(root_id))?
        .clone();

    let outlines_value = match catalog.get("Outlines") {
        None | Some(Object::Null) => {
            return Ok(OutlineRead {
                present: false,
                outline: Outline::new(),
                issues: Vec::new(),
            });
        }
        Some(value) => value.clone(),
    };

    let (outline_id, outline_dictionary) = match outlines_value {
        Object::Reference(id) => match document.object(id)? {
            Object::Dictionary(dictionary) => (Some(id), dictionary),
            _ => return Err(OutlineReadError::MalformedOutlines),
        },
        Object::Dictionary(dictionary) => (None, dictionary),
        _ => return Err(OutlineReadError::MalformedOutlines),
    };

    let error_anchor = outline_id.unwrap_or(root_id);
    let first = optional_reference(&outline_dictionary, "First", error_anchor)?;
    let last = optional_reference(&outline_dictionary, "Last", error_anchor)?;
    if first.is_some() != last.is_some() {
        return Err(OutlineReadError::BrokenLink {
            object: error_anchor,
            reason: "the root /First and /Last entries disagree",
        });
    }

    let pages = document.pages()?;
    let page_index: HashMap<ObjectId, usize> = pages
        .iter()
        .enumerate()
        .map(|(index, page)| (page.id, index))
        .collect();
    let page_count = pages.len();

    let mut walker = OutlineWalker {
        document,
        catalog,
        page_index,
        page_count,
        visited: HashSet::new(),
        issues: Vec::new(),
        items: 0,
    };

    let roots = if first.is_some() {
        walker.read_chain(first, last, outline_id, 0)?
    } else {
        Vec::new()
    };

    Ok(OutlineRead {
        present: true,
        outline: Outline { roots },
        issues: walker.issues,
    })
}

/// A failure while writing an [`Outline`] into a real document.
#[derive(Debug, Error)]
pub enum OutlineWriteError {
    /// The core document could not be read or staged.
    #[error("PDF structure error: {0}")]
    Core(#[from] pdfxi_core::Error),
    /// The catalog object is not a dictionary.
    #[error("catalog object {0} is not a dictionary")]
    CatalogNotDictionary(ObjectId),
    /// An existing catalog `/Outlines` entry is neither a dictionary nor a
    /// reference to one.
    #[error("existing catalog /Outlines is not a dictionary")]
    MalformedOutlines,
    /// A bookmark title is empty or too long for the model.
    #[error("outline title is invalid: {0}")]
    InvalidTitle(String),
    /// A destination names a page that this document does not have.
    #[error("outline destination page {page} does not exist")]
    PageNotFound {
        /// The zero-based page index that was requested.
        page: usize,
    },
    /// A destination coordinate is `NaN` or infinite.
    #[error("outline destination coordinate is not finite")]
    NonFiniteDestination,
    /// The outline nests deeper than [`MAX_OUTLINE_DEPTH`].
    #[error("outline exceeds the {limit} level depth limit")]
    DepthLimitExceeded {
        /// The configured limit.
        limit: usize,
    },
    /// The outline has more items than [`MAX_OUTLINE_ITEMS`].
    #[error("outline exceeds the {limit} item budget")]
    ItemBudgetExceeded {
        /// The configured limit.
        limit: usize,
    },
}

/// Install `outline` as the catalog `/Outlines` linked list.
///
/// Existing page objects and page ids are never changed: each destination is
/// written as `[page_reference /Fit ...]` using the document's real page
/// objects. Unrelated catalog entries are preserved. When the catalog has no
/// `/Outlines` (or holds a direct dictionary), a new indirect outline object is
/// allocated and the catalog gains `/Outlines`; an existing indirect outline
/// object id is reused so repeated writes replace its content in place.
///
/// The writer validates every title, destination page and coordinate before
/// staging anything, so an invalid outline leaves the document unchanged. The
/// object ids are assigned in pre-order and every dictionary is a
/// [`pdfxi_core::Dictionary`], so identical input produces identical staged
/// objects.
pub fn write_outline(document: &mut Document, outline: &Outline) -> Result<(), OutlineWriteError> {
    let effective = effective_document(document)?;
    let catalog_id = effective.root()?;
    let mut catalog = effective
        .object(catalog_id)?
        .as_dictionary()
        .ok_or(OutlineWriteError::CatalogNotDictionary(catalog_id))?
        .clone();
    let pages = effective.pages()?;

    let mut item_count = 0;
    validate_outline(&outline.roots, 0, pages.len(), &mut item_count)?;

    let existing_outline = match catalog.get("Outlines") {
        None | Some(Object::Null) => None,
        Some(Object::Reference(id)) => match effective.object(*id)? {
            Object::Dictionary(_) => Some(*id),
            _ => return Err(OutlineWriteError::MalformedOutlines),
        },
        Some(Object::Dictionary(_)) => None,
        Some(_) => return Err(OutlineWriteError::MalformedOutlines),
    };
    let catalog_changes = existing_outline.is_none();

    let mut next_number = None;
    let outline_id = match existing_outline {
        Some(id) => id,
        None => {
            let number = document.allocate_object_number()?;
            next_number = Some(
                number
                    .checked_add(1)
                    .ok_or(pdfxi_core::Error::ObjectNumberExhausted)?,
            );
            ObjectId::new(number, 0)
        }
    };
    let items_base = if item_count == 0 {
        0
    } else {
        match next_number {
            Some(number) => number,
            None => document.allocate_object_number()?,
        }
    };

    let mut next = items_base;
    let mut assigned = Vec::with_capacity(item_count);
    let (first, last) = assign_items(&outline.roots, Some(outline_id), &mut next, &mut assigned)?;

    let mut root = Dictionary::new();
    root.insert("Type".to_owned(), Object::Name("Outlines".to_owned()));
    root.insert(
        "Count".to_owned(),
        Object::Integer(outline.total_visible_count()),
    );
    if let Some(first) = first {
        root.insert("First".to_owned(), Object::Reference(first));
    }
    if let Some(last) = last {
        root.insert("Last".to_owned(), Object::Reference(last));
    }

    let mut staged = Vec::with_capacity(assigned.len() + 2);
    for assigned in &assigned {
        staged.push((
            assigned.id,
            Object::Dictionary(build_item(assigned, &pages)?),
        ));
    }
    staged.push((outline_id, Object::Dictionary(root)));
    if catalog_changes {
        catalog.insert("Outlines".to_owned(), Object::Reference(outline_id));
        staged.push((catalog_id, Object::Dictionary(catalog)));
    }

    for (id, object) in staged {
        document.set_object(id, object)?;
    }
    Ok(())
}

/// Return a reparsed view that includes every staged change.
///
/// Repeated writes on one [`pdfxi_core::Document`] must see the previous
/// staged outline, but `Document::object` exposes only the source revision.
/// The incremental revision is read back exactly as the session layer does.
fn effective_document(document: &Document) -> Result<Document, OutlineWriteError> {
    let bytes = document.save_incremental()?;
    Document::from_bytes(bytes).map_err(OutlineWriteError::Core)
}

/// Validate titles, depth, item count, destinations and coordinates.
fn validate_outline(
    nodes: &[Bookmark],
    depth: usize,
    page_count: usize,
    items: &mut usize,
) -> Result<(), OutlineWriteError> {
    if depth > MAX_OUTLINE_DEPTH {
        return Err(OutlineWriteError::DepthLimitExceeded {
            limit: MAX_OUTLINE_DEPTH,
        });
    }
    for node in nodes {
        *items = items.saturating_add(1);
        if *items > MAX_OUTLINE_ITEMS {
            return Err(OutlineWriteError::ItemBudgetExceeded {
                limit: MAX_OUTLINE_ITEMS,
            });
        }
        super::validate_name("Title", &node.title)
            .map_err(|error| OutlineWriteError::InvalidTitle(error.to_string()))?;
        if let Some(destination) = &node.destination {
            let page = destination.page.get();
            if page >= page_count {
                return Err(OutlineWriteError::PageNotFound { page });
            }
            validate_fit(&destination.fit)?;
        }
        validate_outline(&node.children, depth + 1, page_count, items)?;
    }
    Ok(())
}

/// Check that every coordinate in a fit is finite.
fn validate_fit(fit: &FitType) -> Result<(), OutlineWriteError> {
    let finite = |value: f32| {
        if value.is_finite() {
            Ok(())
        } else {
            Err(OutlineWriteError::NonFiniteDestination)
        }
    };
    match *fit {
        FitType::Xyz { left, top, zoom } => {
            for value in [left, top, zoom].into_iter().flatten() {
                finite(value)?;
            }
        }
        FitType::Fit | FitType::FitB => {}
        FitType::FitH { top } | FitType::FitBH { top } => finite(top)?,
        FitType::FitV { left } | FitType::FitBV { left } => finite(left)?,
        FitType::FitR {
            left,
            bottom,
            right,
            top,
        } => {
            finite(left)?;
            finite(bottom)?;
            finite(right)?;
            finite(top)?;
        }
    }
    Ok(())
}

/// One bookmark with the ids assigned during serialization.
struct AssignedItem<'a> {
    id: ObjectId,
    node: &'a Bookmark,
    parent: Option<ObjectId>,
    first: Option<ObjectId>,
    last: Option<ObjectId>,
    next: Option<ObjectId>,
    prev: Option<ObjectId>,
}

/// Assign ids in pre-order and record every sibling/parent link.
fn assign_items<'a>(
    siblings: &'a [Bookmark],
    parent: Option<ObjectId>,
    next_number: &mut u32,
    out: &mut Vec<AssignedItem<'a>>,
) -> Result<(Option<ObjectId>, Option<ObjectId>), OutlineWriteError> {
    let mut first_id = None;
    let mut previous: Option<usize> = None;

    for node in siblings {
        let id = ObjectId::new(*next_number, 0);
        *next_number = next_number
            .checked_add(1)
            .ok_or(pdfxi_core::Error::ObjectNumberExhausted)?;
        let slot = out.len();
        out.push(AssignedItem {
            id,
            node,
            parent,
            first: None,
            last: None,
            next: None,
            prev: None,
        });
        let (child_first, child_last) = assign_items(&node.children, Some(id), next_number, out)?;
        out[slot].first = child_first;
        out[slot].last = child_last;
        if let Some(previous) = previous {
            out[previous].next = Some(id);
            out[slot].prev = Some(out[previous].id);
        } else {
            first_id = Some(id);
        }
        previous = Some(slot);
    }

    let last_id = previous.map(|index| out[index].id);
    Ok((first_id, last_id))
}

/// Build one outline item dictionary with a real page destination.
fn build_item(
    assigned: &AssignedItem<'_>,
    pages: &[Page],
) -> Result<Dictionary, OutlineWriteError> {
    let node = assigned.node;
    let mut dictionary = Dictionary::new();
    dictionary.insert(
        "Title".to_owned(),
        Object::String(encode_text_string(&node.title)),
    );
    if let Some(parent) = assigned.parent {
        dictionary.insert("Parent".to_owned(), Object::Reference(parent));
    }
    if let Some(first) = assigned.first {
        dictionary.insert("First".to_owned(), Object::Reference(first));
    }
    if let Some(last) = assigned.last {
        dictionary.insert("Last".to_owned(), Object::Reference(last));
    }
    if let Some(next) = assigned.next {
        dictionary.insert("Next".to_owned(), Object::Reference(next));
    }
    if let Some(prev) = assigned.prev {
        dictionary.insert("Prev".to_owned(), Object::Reference(prev));
    }

    let descendants = node.visible_descendant_count();
    if descendants != 0 {
        let count = if node.open { descendants } else { -descendants };
        dictionary.insert("Count".to_owned(), Object::Integer(count));
    }
    if let Some(destination) = &node.destination {
        dictionary.insert("Dest".to_owned(), destination_object(destination, pages)?);
    }
    if let Some(color) = node.color {
        dictionary.insert("C".to_owned(), color_object(color));
    }
    let flags = node.style_flags();
    if flags != 0 {
        dictionary.insert(
            "F".to_owned(),
            Object::Integer(i64::try_from(flags).unwrap_or(i64::MAX)),
        );
    }
    Ok(dictionary)
}

/// Serialize a destination as `[page_reference /Fit ...]`.
fn destination_object(
    destination: &super::Destination,
    pages: &[Page],
) -> Result<Object, OutlineWriteError> {
    let page = destination.page.get();
    let page = pages
        .get(page)
        .ok_or(OutlineWriteError::PageNotFound { page })?;
    let mut items = Vec::with_capacity(2 + fit_parameter_count(&destination.fit));
    items.push(Object::Reference(page.id));
    items.push(Object::Name(destination.fit.as_name().to_owned()));
    items.extend(fit_parameters(&destination.fit)?);
    Ok(Object::Array(items))
}

/// Number of coordinate values written after the fit name.
fn fit_parameter_count(fit: &FitType) -> usize {
    match fit {
        FitType::Xyz { .. } => 3,
        FitType::Fit | FitType::FitB => 0,
        FitType::FitH { .. }
        | FitType::FitV { .. }
        | FitType::FitBH { .. }
        | FitType::FitBV { .. } => 1,
        FitType::FitR { .. } => 4,
    }
}

/// Serialize the coordinate values after a fit name.
fn fit_parameters(fit: &FitType) -> Result<Vec<Object>, OutlineWriteError> {
    let real = |value: f32| {
        if value.is_finite() {
            Ok(Object::Real(f64::from(value)))
        } else {
            Err(OutlineWriteError::NonFiniteDestination)
        }
    };
    match *fit {
        FitType::Xyz { left, top, zoom } => Ok(vec![
            optional_real(left)?,
            optional_real(top)?,
            optional_real(zoom)?,
        ]),
        FitType::Fit | FitType::FitB => Ok(Vec::new()),
        FitType::FitH { top } | FitType::FitBH { top } => Ok(vec![real(top)?]),
        FitType::FitV { left } | FitType::FitBV { left } => Ok(vec![real(left)?]),
        FitType::FitR {
            left,
            bottom,
            right,
            top,
        } => Ok(vec![real(left)?, real(bottom)?, real(right)?, real(top)?]),
    }
}

/// Serialize an optional coordinate; `None` is PDF `null`.
fn optional_real(value: Option<f32>) -> Result<Object, OutlineWriteError> {
    match value {
        None => Ok(Object::Null),
        Some(value) if value.is_finite() => Ok(Object::Real(f64::from(value))),
        Some(_) => Err(OutlineWriteError::NonFiniteDestination),
    }
}

/// Serialize a [`Color`] as a three-component `/C` array in `0.0..=1.0`.
fn color_object(color: Color) -> Object {
    Object::Array(vec![
        Object::Real(f64::from(color.r()) / 255.0),
        Object::Real(f64::from(color.g()) / 255.0),
        Object::Real(f64::from(color.b()) / 255.0),
    ])
}

/// Reads one `/First`/`/Next` chain and its child chains.
struct OutlineWalker<'a> {
    document: &'a Document,
    catalog: Dictionary,
    page_index: HashMap<ObjectId, usize>,
    page_count: usize,
    visited: HashSet<ObjectId>,
    issues: Vec<OutlineIssue>,
    items: usize,
}

impl OutlineWalker<'_> {
    fn read_chain(
        &mut self,
        first: Option<ObjectId>,
        last: Option<ObjectId>,
        parent: Option<ObjectId>,
        depth: usize,
    ) -> Result<Vec<Bookmark>, OutlineReadError> {
        if depth > MAX_OUTLINE_DEPTH {
            return Err(OutlineReadError::DepthLimitExceeded {
                limit: MAX_OUTLINE_DEPTH,
            });
        }
        let mut bookmarks = Vec::new();
        let mut current = first;
        let mut previous: Option<ObjectId> = None;

        while let Some(object) = current {
            if !self.visited.insert(object) {
                return Err(OutlineReadError::Cycle { object });
            }
            self.items = self.items.saturating_add(1);
            if self.items > MAX_OUTLINE_ITEMS {
                return Err(OutlineReadError::ItemBudgetExceeded {
                    limit: MAX_OUTLINE_ITEMS,
                });
            }

            let value = self.document.object(object)?;
            let dictionary = value
                .as_dictionary()
                .ok_or(OutlineReadError::ItemNotDictionary { object })?;

            if let Some(expected) = parent {
                let found = optional_reference(dictionary, "Parent", object)?;
                if found != Some(expected) {
                    return Err(OutlineReadError::BrokenParent {
                        object,
                        expected,
                        found,
                    });
                }
            }
            if let Some(prev) = optional_reference(dictionary, "Prev", object)?
                && Some(prev) != previous
            {
                return Err(OutlineReadError::BrokenLink {
                    object,
                    reason: "the /Prev entry does not match the preceding item",
                });
            }

            let title = decode_title(dictionary, object)?;
            let first_child = optional_reference(dictionary, "First", object)?;
            let last_child = optional_reference(dictionary, "Last", object)?;
            if first_child.is_some() != last_child.is_some() {
                return Err(OutlineReadError::BrokenLink {
                    object,
                    reason: "the item /First and /Last entries disagree",
                });
            }
            let children = self.read_chain(first_child, last_child, Some(object), depth + 1)?;

            let count = dictionary.get("Count").and_then(Object::as_i64);
            let open = count.map(|value| value >= 0).unwrap_or(true);

            let destination = match self.destination_for(dictionary) {
                Ok(destination) => Some(destination),
                Err(reason) => {
                    self.issues.push(OutlineIssue {
                        object,
                        title: title.clone(),
                        reason,
                    });
                    None
                }
            };

            let mut bookmark =
                Bookmark::new(title.clone()).map_err(|error| OutlineReadError::InvalidTitle {
                    object,
                    reason: error.to_string(),
                })?;
            bookmark.destination = destination;
            bookmark.children = children;
            bookmark.color = color_from(dictionary);
            let flags = dictionary.get("F").and_then(Object::as_i64).unwrap_or(0);
            bookmark.italic = flags & FLAG_ITALIC != 0;
            bookmark.bold = flags & FLAG_BOLD != 0;
            bookmark.open = open;
            bookmarks.push(bookmark);

            previous = Some(object);
            current = optional_reference(dictionary, "Next", object)?;
        }

        if previous != last {
            let object = last.or(previous).unwrap_or(ObjectId::new(0, 0));
            return Err(OutlineReadError::BrokenLink {
                object,
                reason: "the /Last entry does not match the end of the chain",
            });
        }

        Ok(bookmarks)
    }

    /// Resolve the item's `/Dest` or `/A /D` to a page destination.
    fn destination_for(&self, item: &Dictionary) -> Result<super::Destination, OutlineIssueReason> {
        if let Some(destination) = item.get("Dest") {
            return self.resolve_destination(destination, 0, &mut HashSet::new());
        }
        let Some(action) = item.get("A") else {
            return Err(OutlineIssueReason::MissingDestination);
        };
        let action = self
            .resolve(action)
            .map_err(|error| OutlineIssueReason::InvalidDestination(error.to_string()))?;
        let action = action.as_dictionary().ok_or_else(|| {
            OutlineIssueReason::InvalidDestination("/A is not a dictionary".to_owned())
        })?;
        let subtype = action.get("S").and_then(Object::as_name).unwrap_or("");
        if subtype != "GoTo" {
            return Err(OutlineIssueReason::NonGoToAction(subtype.to_owned()));
        }
        let Some(destination) = action.get("D") else {
            return Err(OutlineIssueReason::MissingDestination);
        };
        self.resolve_destination(destination, 0, &mut HashSet::new())
    }

    /// Resolve a destination value, including named destinations.
    fn resolve_destination(
        &self,
        value: &Object,
        depth: usize,
        named_seen: &mut HashSet<Vec<u8>>,
    ) -> Result<super::Destination, OutlineIssueReason> {
        if depth > MAX_REFERENCE_DEPTH {
            return Err(OutlineIssueReason::InvalidDestination(
                "destination reference depth exceeded".to_owned(),
            ));
        }
        match value {
            Object::Reference(_) => {
                let resolved = self
                    .resolve(value)
                    .map_err(|error| OutlineIssueReason::InvalidDestination(error.to_string()))?;
                self.resolve_destination(&resolved, depth + 1, named_seen)
            }
            Object::Array(items) => self.parse_destination_array(items),
            Object::Dictionary(dictionary) => match dictionary.get("D") {
                Some(value) => self.resolve_destination(value, depth + 1, named_seen),
                None => Err(OutlineIssueReason::InvalidDestination(
                    "destination dictionary has no /D".to_owned(),
                )),
            },
            Object::Name(name) => {
                self.resolve_named_destination(name.as_bytes(), Some(name), depth, named_seen)
            }
            Object::String(bytes) => {
                let decoded = decode_text_string(bytes);
                self.resolve_named_destination(bytes, Some(&decoded), depth, named_seen)
            }
            _ => Err(OutlineIssueReason::InvalidDestination(
                "destination is not a name, string, array, or dictionary".to_owned(),
            )),
        }
    }

    /// Look up a named destination in `/Dests` and then `/Names /Dests`.
    fn resolve_named_destination(
        &self,
        key_bytes: &[u8],
        key_text: Option<&str>,
        depth: usize,
        named_seen: &mut HashSet<Vec<u8>>,
    ) -> Result<super::Destination, OutlineIssueReason> {
        if !named_seen.insert(key_bytes.to_vec()) {
            return Err(OutlineIssueReason::InvalidDestination(
                "named destination cycle".to_owned(),
            ));
        }
        let display = key_text
            .map(str::to_owned)
            .unwrap_or_else(|| String::from_utf8_lossy(key_bytes).into_owned());

        let mut found = None;
        if let Some(dests) = self.catalog.get("Dests")
            && let Some(value) = self.lookup_dests_dictionary(dests, key_text)
        {
            found = Some(value);
        }
        if found.is_none()
            && let Some(names) = self.catalog.get("Names")
            && let Some(dests) = self.names_entry(names, "Dests")
        {
            let mut seen = HashSet::new();
            found = name_tree_lookup(self.document, &dests, key_bytes, key_text, 0, &mut seen);
        }

        let Some(value) = found else {
            return Err(OutlineIssueReason::UnresolvedNamedDestination(display));
        };
        self.resolve_destination(&value, depth + 1, named_seen)
    }

    /// Look up a name in the catalog `/Dests` dictionary.
    fn lookup_dests_dictionary(&self, value: &Object, key: Option<&str>) -> Option<Object> {
        let dictionary = self.resolve(value).ok()?;
        let dictionary = dictionary.as_dictionary()?;
        let key = key?;
        dictionary.get(key).cloned()
    }

    /// Read one entry from the catalog `/Names` dictionary.
    fn names_entry(&self, value: &Object, key: &str) -> Option<Object> {
        let dictionary = self.resolve(value).ok()?;
        dictionary.as_dictionary()?.get(key).cloned()
    }

    /// Parse `[page /Fit ...]` into a destination with a page index.
    fn parse_destination_array(
        &self,
        items: &[Object],
    ) -> Result<super::Destination, OutlineIssueReason> {
        if items.len() < 2 {
            return Err(OutlineIssueReason::InvalidDestination(format!(
                "destination has {} elements, expected at least 2",
                items.len()
            )));
        }
        let page = match &items[0] {
            Object::Reference(id) => *self
                .page_index
                .get(id)
                .ok_or(OutlineIssueReason::DanglingPageReference(*id))?,
            Object::Integer(index) => {
                let index = usize::try_from(*index).map_err(|_| {
                    OutlineIssueReason::InvalidDestination(
                        "destination page index is negative".to_owned(),
                    )
                })?;
                if index >= self.page_count {
                    return Err(OutlineIssueReason::InvalidDestination(format!(
                        "destination page index {index} is out of range"
                    )));
                }
                index
            }
            _ => {
                return Err(OutlineIssueReason::InvalidDestination(
                    "destination page is not an indirect page reference".to_owned(),
                ));
            }
        };
        let fit_name = items[1].as_name().ok_or_else(|| {
            OutlineIssueReason::InvalidDestination("destination fit is not a name".to_owned())
        })?;
        let fit = fit_from_parameters(fit_name, &items[2..])?;
        Ok(super::Destination::new(PageIndex::new(page), fit))
    }

    /// Resolve indirect references with a cycle and depth guard.
    fn resolve(&self, value: &Object) -> Result<Object, OutlineReadError> {
        let mut current = value.clone();
        let mut seen = HashSet::new();
        for _ in 0..MAX_REFERENCE_DEPTH {
            match current {
                Object::Reference(id) => {
                    if !seen.insert(id) {
                        return Err(OutlineReadError::ReferenceCycle { object: id });
                    }
                    current = self.document.object(id)?;
                }
                other => return Ok(other),
            }
        }
        Err(OutlineReadError::ReferenceDepthExceeded {
            limit: MAX_REFERENCE_DEPTH,
        })
    }
}

/// Decode and validate one item title.
fn decode_title(dictionary: &Dictionary, object: ObjectId) -> Result<String, OutlineReadError> {
    let value = dictionary
        .get("Title")
        .ok_or(OutlineReadError::MissingTitle { object })?;
    let Object::String(bytes) = value else {
        return Err(OutlineReadError::MissingTitle { object });
    };
    Ok(decode_text_string(bytes))
}

/// Read an optional indirect reference entry.
fn optional_reference(
    dictionary: &Dictionary,
    key: &'static str,
    object: ObjectId,
) -> Result<Option<ObjectId>, OutlineReadError> {
    match dictionary.get(key) {
        None | Some(Object::Null) => Ok(None),
        Some(Object::Reference(id)) => Ok(Some(*id)),
        Some(_) => Err(OutlineReadError::BrokenLink {
            object,
            reason: match key {
                "First" => "/First is not an indirect reference",
                "Last" => "/Last is not an indirect reference",
                "Next" => "/Next is not an indirect reference",
                "Prev" => "/Prev is not an indirect reference",
                "Parent" => "/Parent is not an indirect reference",
                _ => "linked-list entry is not an indirect reference",
            },
        }),
    }
}

/// Parse a three-component `/C` colour array.
fn color_from(dictionary: &Dictionary) -> Option<Color> {
    let values = dictionary.get("C")?.as_array()?;
    if values.len() != 3 {
        return None;
    }
    let mut components = [0u8; 3];
    for (index, value) in values.iter().enumerate() {
        let number = value.as_f64()?;
        if !number.is_finite() || !(0.0..=1.0).contains(&number) {
            return None;
        }
        components[index] = (number * 255.0).round().clamp(0.0, 255.0) as u8;
    }
    Some(Color::new(components[0], components[1], components[2]))
}

/// Parse fit parameters from core objects into a [`FitType`].
fn fit_from_parameters(name: &str, params: &[Object]) -> Result<FitType, OutlineIssueReason> {
    let expect = |expected: usize| -> Result<(), OutlineIssueReason> {
        if params.len() == expected {
            Ok(())
        } else {
            Err(OutlineIssueReason::InvalidDestination(format!(
                "destination fit /{name} expects {expected} parameters, got {}",
                params.len()
            )))
        }
    };
    match name {
        "XYZ" => {
            expect(3)?;
            Ok(FitType::Xyz {
                left: optional_coordinate(params.first())?,
                top: optional_coordinate(params.get(1))?,
                zoom: optional_coordinate(params.get(2))?,
            })
        }
        "Fit" => {
            expect(0)?;
            Ok(FitType::Fit)
        }
        "FitH" => {
            expect(1)?;
            Ok(FitType::FitH {
                top: required_number(&params[0])?,
            })
        }
        "FitV" => {
            expect(1)?;
            Ok(FitType::FitV {
                left: required_number(&params[0])?,
            })
        }
        "FitR" => {
            expect(4)?;
            Ok(FitType::FitR {
                left: required_number(&params[0])?,
                bottom: required_number(&params[1])?,
                right: required_number(&params[2])?,
                top: required_number(&params[3])?,
            })
        }
        "FitB" => {
            expect(0)?;
            Ok(FitType::FitB)
        }
        "FitBH" => {
            expect(1)?;
            Ok(FitType::FitBH {
                top: required_number(&params[0])?,
            })
        }
        "FitBV" => {
            expect(1)?;
            Ok(FitType::FitBV {
                left: required_number(&params[0])?,
            })
        }
        other => Err(OutlineIssueReason::InvalidDestination(format!(
            "unknown destination fit /{other}"
        ))),
    }
}

/// Read an optional finite coordinate; PDF `null` means "unchanged".
fn optional_coordinate(value: Option<&Object>) -> Result<Option<f32>, OutlineIssueReason> {
    match value {
        None | Some(Object::Null) => Ok(None),
        Some(value) => required_number(value).map(Some),
    }
}

/// Read a required finite number, narrowing to `f32` before the finiteness
/// check so a finite `f64` such as `1e39` cannot become `f32::INFINITY`.
fn required_number(value: &Object) -> Result<f32, OutlineIssueReason> {
    let number = value.as_f64().ok_or_else(|| {
        OutlineIssueReason::InvalidDestination("destination coordinate is not a number".to_owned())
    })?;
    if !number.is_finite() {
        return Err(OutlineIssueReason::InvalidDestination(
            "destination coordinate is not finite".to_owned(),
        ));
    }
    let narrowed = number as f32;
    if !narrowed.is_finite() {
        return Err(OutlineIssueReason::InvalidDestination(
            "destination coordinate overflows f32".to_owned(),
        ));
    }
    Ok(narrowed)
}

/// Search a `/Names` name tree for `key`, following `/Kids`.
fn name_tree_lookup(
    document: &Document,
    node: &Object,
    key_bytes: &[u8],
    key_text: Option<&str>,
    depth: usize,
    seen: &mut HashSet<ObjectId>,
) -> Option<Object> {
    if depth > MAX_NAME_TREE_DEPTH {
        return None;
    }
    let node = match node {
        Object::Reference(id) => {
            if !seen.insert(*id) {
                return None;
            }
            document.object(*id).ok()?
        }
        other => other.clone(),
    };
    let dictionary = node.as_dictionary()?;

    if let Some(names) = dictionary.get("Names").and_then(Object::as_array) {
        let (pairs, _remainder) = names.as_chunks::<2>();
        for pair in pairs {
            if key_matches(&pair[0], key_bytes, key_text) {
                return Some(pair[1].clone());
            }
        }
    }
    if let Some(kids) = dictionary.get("Kids").and_then(Object::as_array) {
        for kid in kids {
            if let Some(found) =
                name_tree_lookup(document, kid, key_bytes, key_text, depth + 1, seen)
            {
                return Some(found);
            }
        }
    }
    None
}

/// Compare a name-tree key with the requested destination name.
fn key_matches(candidate: &Object, key_bytes: &[u8], key_text: Option<&str>) -> bool {
    match candidate {
        Object::String(bytes) => {
            bytes == key_bytes || key_text.is_some_and(|text| decode_text_string(bytes) == text)
        }
        Object::Name(name) => {
            name.as_bytes() == key_bytes || key_text.is_some_and(|text| name == text)
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use pdfxi_core::encode_text_string;
    use pdfxi_render::PageIndex as RenderPageIndex;
    use pdfxi_render::pdfium_backend::PdfiumRenderer;

    use super::*;
    use crate::navigation::{BookmarkPath, Destination};

    fn fixture(name: &str) -> Vec<u8> {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures")
            .join(name);
        std::fs::read(&path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
    }

    /// Stage a synthetic outline on a copy of `fixtures/minimal.pdf` and return
    /// the reopened document, so the reader sees it through the same parser as
    /// a real file.
    fn document_with_outline(
        build: impl FnOnce(&mut Document, &mut Dictionary, ObjectId, ObjectId, &[ObjectId]),
    ) -> Document {
        let mut document = Document::from_bytes(fixture("minimal.pdf")).expect("minimal parses");
        let root = document.root().expect("catalog id");
        let mut catalog = document
            .object(root)
            .expect("catalog object")
            .as_dictionary()
            .expect("catalog dictionary")
            .clone();
        let page_id = document.page(0).expect("page").id;

        let first_number = document.allocate_object_number().expect("allocate outline");
        let ids: Vec<ObjectId> = (0..8)
            .map(|offset| ObjectId::new(first_number + offset, 0))
            .collect();
        build(&mut document, &mut catalog, ids[0], page_id, &ids);
        document
            .set_object(root, Object::Dictionary(catalog))
            .expect("stage catalog");
        let saved = document.save().expect("full rewrite");
        Document::from_bytes(saved).expect("staged document parses")
    }

    fn item(title: &str, parent: ObjectId) -> Dictionary {
        let mut item = Dictionary::new();
        item.insert(
            "Title".to_owned(),
            Object::String(encode_text_string(title)),
        );
        item.insert("Parent".to_owned(), Object::Reference(parent));
        item
    }

    #[test]
    fn reads_real_powerpoint_outline_titles_and_pages() {
        let document =
            Document::from_bytes(fixture("realworld/powerpoint-slides.pdf")).expect("fixture");
        let read = read_outline(&document).expect("outline reads");

        assert!(read.present);
        assert!(read.issues.is_empty());
        let titles: Vec<&str> = read
            .outline
            .roots
            .iter()
            .map(|bookmark| bookmark.title.as_str())
            .collect();
        assert_eq!(
            titles,
            vec![
                "Slide 1: Lantern Slide",
                "Slide 2: Harbor Slide",
                "Slide 3: Meadow Slide",
            ]
        );
        let pages: Vec<usize> = read
            .outline
            .roots
            .iter()
            .map(|bookmark| {
                bookmark
                    .destination
                    .as_ref()
                    .expect("PowerPoint outline item has a destination")
                    .page
                    .get()
            })
            .collect();
        assert_eq!(pages, vec![0, 1, 2]);
        assert_eq!(
            read.outline.roots[0]
                .destination
                .as_ref()
                .expect("destination")
                .fit,
            FitType::Xyz {
                left: Some(0.0),
                top: Some(405.0),
                zoom: Some(0.0),
            }
        );
    }

    #[test]
    fn f32_overflow_coordinate_is_an_issue_and_other_items_still_read() {
        let mut document =
            Document::from_bytes(fixture("realworld/powerpoint-slides.pdf")).expect("fixture");
        let outline_id = rebuilt_outline_id(&document);
        let first_item = document
            .object(outline_id)
            .expect("outline")
            .as_dictionary()
            .expect("outline dictionary")
            .get("First")
            .and_then(Object::as_reference)
            .expect("first item");
        let page_id = document.page(0).expect("page").id;
        let mut item = document
            .object(first_item)
            .expect("item")
            .as_dictionary()
            .expect("item dictionary")
            .clone();
        item.insert(
            "Dest".to_owned(),
            Object::Array(vec![
                Object::Reference(page_id),
                Object::Name("XYZ".to_owned()),
                Object::Integer(0),
                Object::Real(1e39),
                Object::Integer(0),
            ]),
        );
        document
            .set_object(first_item, Object::Dictionary(item))
            .expect("stage overflowing item");
        let saved = document.save().expect("full rewrite");
        let reopened = Document::from_bytes(saved).expect("saved document parses");

        let read = read_outline(&reopened).expect("hostile coordinate is an issue, not an error");
        assert_eq!(read.outline.roots.len(), 3);
        assert_eq!(read.issues.len(), 1);
        assert_eq!(read.issues[0].object, first_item);
        assert!(read.outline.roots[0].destination.is_none());
        assert!(matches!(
            &read.issues[0].reason,
            OutlineIssueReason::InvalidDestination(reason)
                if reason.contains("overflows f32")
        ));
        assert_eq!(
            read.outline.roots[1]
                .destination
                .as_ref()
                .expect("second item destination")
                .page
                .get(),
            1
        );
        assert_eq!(
            read.outline.roots[2]
                .destination
                .as_ref()
                .expect("third item destination")
                .page
                .get(),
            2
        );
    }

    #[test]
    fn document_without_outlines_reads_empty() {
        let document = Document::from_bytes(fixture("realworld/microsoft-excel-2019-sample.pdf"))
            .expect("fixture");
        let read = read_outline(&document).expect("no outline is not an error");
        assert!(!read.present);
        assert!(read.outline.roots.is_empty());
        assert!(read.issues.is_empty());
    }

    #[test]
    fn named_dest_goto_action_and_missing_destination_are_reported() {
        let document = document_with_outline(|document, catalog, outline_id, page_id, ids| {
            let first = ids[1];
            let second = ids[2];
            let third = ids[3];

            let mut outline = Dictionary::new();
            outline.insert("Type".to_owned(), Object::Name("Outlines".to_owned()));
            outline.insert("First".to_owned(), Object::Reference(first));
            outline.insert("Last".to_owned(), Object::Reference(third));
            document
                .set_object(outline_id, Object::Dictionary(outline))
                .expect("stage outline");

            let mut named = item("Named chapter", outline_id);
            named.insert("Dest".to_owned(), Object::Name("target".to_owned()));
            named.insert("Next".to_owned(), Object::Reference(second));
            document
                .set_object(first, Object::Dictionary(named))
                .expect("stage named item");

            let mut action = Dictionary::new();
            action.insert("S".to_owned(), Object::Name("GoTo".to_owned()));
            action.insert(
                "D".to_owned(),
                Object::Array(vec![
                    Object::Reference(page_id),
                    Object::Name("XYZ".to_owned()),
                    Object::Integer(0),
                    Object::Integer(792),
                    Object::Integer(0),
                ]),
            );
            let mut goto = item("Action chapter", outline_id);
            goto.insert("Prev".to_owned(), Object::Reference(first));
            goto.insert("Next".to_owned(), Object::Reference(third));
            goto.insert("A".to_owned(), Object::Dictionary(action));
            document
                .set_object(second, Object::Dictionary(goto))
                .expect("stage action item");

            let mut missing = item("No target", outline_id);
            missing.insert("Prev".to_owned(), Object::Reference(second));
            document
                .set_object(third, Object::Dictionary(missing))
                .expect("stage missing item");

            let mut dests = Dictionary::new();
            dests.insert(
                "target".to_owned(),
                Object::Array(vec![
                    Object::Reference(page_id),
                    Object::Name("Fit".to_owned()),
                ]),
            );
            catalog.insert("Dests".to_owned(), Object::Dictionary(dests));
            catalog.insert("Outlines".to_owned(), Object::Reference(outline_id));
        });

        let read = read_outline(&document).expect("synthetic outline reads");
        assert_eq!(read.outline.roots.len(), 3);
        assert_eq!(read.outline.roots[0].title, "Named chapter");
        assert_eq!(
            read.outline.roots[0]
                .destination
                .as_ref()
                .expect("named destination")
                .page
                .get(),
            0
        );
        assert_eq!(
            read.outline.roots[0]
                .destination
                .as_ref()
                .expect("named destination")
                .fit,
            FitType::Fit
        );
        assert_eq!(read.outline.roots[1].title, "Action chapter");
        assert_eq!(
            read.outline.roots[1]
                .destination
                .as_ref()
                .expect("GoTo destination")
                .fit,
            FitType::Xyz {
                left: Some(0.0),
                top: Some(792.0),
                zoom: Some(0.0),
            }
        );
        assert!(read.outline.roots[2].destination.is_none());
        assert_eq!(read.issues.len(), 1);
        assert_eq!(read.issues[0].title, "No target");
        assert_eq!(
            read.issues[0].reason,
            OutlineIssueReason::MissingDestination
        );
    }

    #[test]
    fn cyclic_next_chain_is_a_typed_error() {
        let document = document_with_outline(|document, catalog, outline_id, _page_id, ids| {
            let first = ids[1];
            let mut outline = Dictionary::new();
            outline.insert("Type".to_owned(), Object::Name("Outlines".to_owned()));
            outline.insert("First".to_owned(), Object::Reference(first));
            outline.insert("Last".to_owned(), Object::Reference(first));
            document
                .set_object(outline_id, Object::Dictionary(outline))
                .expect("stage outline");

            let mut looping = item("Loop", outline_id);
            looping.insert("Next".to_owned(), Object::Reference(first));
            document
                .set_object(first, Object::Dictionary(looping))
                .expect("stage looping item");

            catalog.insert("Outlines".to_owned(), Object::Reference(outline_id));
        });

        assert!(matches!(
            read_outline(&document),
            Err(OutlineReadError::Cycle { .. })
        ));
    }

    #[test]
    fn wrong_parent_is_a_typed_error() {
        let document = document_with_outline(|document, catalog, outline_id, _page_id, ids| {
            let first = ids[1];
            let mut outline = Dictionary::new();
            outline.insert("Type".to_owned(), Object::Name("Outlines".to_owned()));
            outline.insert("First".to_owned(), Object::Reference(first));
            outline.insert("Last".to_owned(), Object::Reference(first));
            document
                .set_object(outline_id, Object::Dictionary(outline))
                .expect("stage outline");

            let mut child = item("Wrong parent", first);
            child.insert("Parent".to_owned(), Object::Reference(first));
            document
                .set_object(first, Object::Dictionary(child))
                .expect("stage item");

            catalog.insert("Outlines".to_owned(), Object::Reference(outline_id));
        });

        assert!(matches!(
            read_outline(&document),
            Err(OutlineReadError::BrokenParent { .. })
        ));
    }

    fn destination(page: usize, fit: FitType) -> Destination {
        Destination::new(PageIndex::new(page), fit)
    }

    fn catalog_keys(document: &Document) -> Vec<String> {
        let root = document.root().expect("catalog id");
        document
            .object(root)
            .expect("catalog")
            .as_dictionary()
            .expect("catalog dictionary")
            .keys()
            .cloned()
            .collect()
    }

    fn outline_dictionary(document: &Document) -> Dictionary {
        let root = document.root().expect("catalog id");
        let catalog = document
            .object(root)
            .expect("catalog")
            .as_dictionary()
            .expect("catalog dictionary")
            .clone();
        let id = catalog
            .get("Outlines")
            .and_then(Object::as_reference)
            .expect("catalog /Outlines reference");
        document
            .object(id)
            .expect("outline object")
            .as_dictionary()
            .expect("outline dictionary")
            .clone()
    }

    #[test]
    fn round_trip_edits_real_powerpoint_outline_and_reopens_with_pdfium() {
        let source = fixture("realworld/powerpoint-slides.pdf");
        let original = Document::from_bytes(source.clone()).expect("fixture");
        let mut outline = read_outline(&original).expect("original outline").outline;

        outline
            .insert_bookmark(
                &BookmarkPath::root(),
                0,
                Bookmark::new("Prelude")
                    .expect("title")
                    .with_destination(destination(0, FitType::Fit)),
            )
            .expect("add root");
        outline
            .insert_bookmark(
                &BookmarkPath::root().child(1),
                0,
                Bookmark::new("Slide 1 detail")
                    .expect("title")
                    .with_destination(destination(1, FitType::FitH { top: 405.0 })),
            )
            .expect("add child");
        outline.roots[1].open = false;
        outline
            .rename_bookmark(&BookmarkPath::root().child(2), "Harbor Slide (renamed)")
            .expect("rename");
        outline
            .move_bookmark(&BookmarkPath::root().child(3), &BookmarkPath::root(), 1)
            .expect("move");

        let mut document = Document::from_bytes(source).expect("fixture");
        write_outline(&mut document, &outline).expect("write outline");
        let saved = document.save().expect("full rewrite");
        let reopened = Document::from_bytes(saved.clone()).expect("saved document parses");
        let read = read_outline(&reopened).expect("written outline reads");

        let titles: Vec<&str> = read
            .outline
            .roots
            .iter()
            .map(|bookmark| bookmark.title.as_str())
            .collect();
        assert_eq!(
            titles,
            vec![
                "Prelude",
                "Slide 3: Meadow Slide",
                "Slide 1: Lantern Slide",
                "Harbor Slide (renamed)",
            ]
        );
        let pages: Vec<usize> = read
            .outline
            .roots
            .iter()
            .map(|bookmark| {
                bookmark
                    .destination
                    .as_ref()
                    .expect("destination")
                    .page
                    .get()
            })
            .collect();
        assert_eq!(pages, vec![0, 2, 0, 1]);
        assert_eq!(read.outline.roots[2].children.len(), 1);
        assert_eq!(read.outline.roots[2].children[0].title, "Slide 1 detail");
        assert_eq!(
            read.outline.roots[2].children[0]
                .destination
                .as_ref()
                .expect("child destination")
                .page
                .get(),
            1
        );
        assert!(
            !read.outline.roots[2].open,
            "closed state uses a negative /Count"
        );

        let root_outline = outline_dictionary(&reopened);
        assert_eq!(
            root_outline.get("Type").and_then(Object::as_name),
            Some("Outlines")
        );
        assert_eq!(
            root_outline.get("Count").and_then(Object::as_i64),
            Some(5),
            "four roots plus one visible child"
        );
        let outline_id = rebuilt_outline_id(&reopened);
        let mut current = root_outline.get("First").and_then(Object::as_reference);
        let mut previous = None;
        let mut walked = Vec::new();
        while let Some(id) = current {
            let item = reopened
                .object(id)
                .expect("outline item")
                .as_dictionary()
                .expect("item dictionary")
                .clone();
            assert_eq!(
                item.get("Parent").and_then(Object::as_reference),
                Some(outline_id)
            );
            assert_eq!(item.get("Prev").and_then(Object::as_reference), previous);
            let title = item
                .get("Title")
                .and_then(|value| match value {
                    Object::String(bytes) => Some(decode_text_string(bytes)),
                    _ => None,
                })
                .expect("title");
            if title == "Slide 1: Lantern Slide" {
                assert_eq!(item.get("Count").and_then(Object::as_i64), Some(-1));
                assert!(!item.contains_key("XI_Open"));
                let child_id = item
                    .get("First")
                    .and_then(Object::as_reference)
                    .expect("child");
                let child = reopened
                    .object(child_id)
                    .expect("child")
                    .as_dictionary()
                    .expect("child dictionary")
                    .clone();
                assert_eq!(child.get("Parent").and_then(Object::as_reference), Some(id));
            }
            previous = Some(id);
            current = item.get("Next").and_then(Object::as_reference);
            walked.push(title);
        }
        assert_eq!(walked.len(), 4);
        assert_eq!(walked.first().map(String::as_str), Some("Prelude"));
        assert_eq!(
            walked.last().map(String::as_str),
            Some("Harbor Slide (renamed)")
        );

        // PDFium has no outline traversal in this workspace's binding; opening
        // the rewritten bytes and reading each page proves the file is valid
        // for the independent renderer, while PDFXI asserts the outline above.
        let renderer = PdfiumRenderer::from_bytes(saved).expect("PDFium opens the written file");
        assert_eq!(renderer.document_meta().expect("metadata").page_count, 3);
        for page in 0..3 {
            renderer
                .page_size(RenderPageIndex::new(page))
                .expect("PDFium reads every page");
        }
    }

    fn rebuilt_outline_id(document: &Document) -> ObjectId {
        let root = document.root().expect("catalog id");
        document
            .object(root)
            .expect("catalog")
            .as_dictionary()
            .expect("catalog dictionary")
            .get("Outlines")
            .and_then(Object::as_reference)
            .expect("outline reference")
    }

    #[test]
    fn document_without_outlines_gains_one_and_preserves_pages_and_catalog() {
        let source = fixture("realworld/microsoft-excel-2019-sample.pdf");
        let mut document = Document::from_bytes(source).expect("fixture");
        assert!(!read_outline(&document).expect("read before").present);
        let before_keys = catalog_keys(&document);
        let before_pages: Vec<ObjectId> = document
            .pages()
            .expect("pages")
            .iter()
            .map(|page| page.id)
            .collect();

        let outline = Outline::new().with_root(
            Bookmark::new("Start")
                .expect("title")
                .with_destination(destination(1, FitType::Fit)),
        );
        write_outline(&mut document, &outline).expect("write on a document without outlines");
        let saved = document.save().expect("full rewrite");
        let reopened = Document::from_bytes(saved).expect("saved document parses");
        let read = read_outline(&reopened).expect("written outline reads");

        assert!(read.present);
        assert_eq!(read.outline.roots.len(), 1);
        assert_eq!(read.outline.roots[0].title, "Start");
        assert_eq!(
            read.outline.roots[0]
                .destination
                .as_ref()
                .expect("destination")
                .page
                .get(),
            1
        );
        let after_keys = catalog_keys(&reopened);
        assert!(after_keys.iter().any(|key| key == "Outlines"));
        for key in before_keys {
            assert!(
                after_keys.contains(&key),
                "catalog entry /{key} must be preserved"
            );
        }
        let after_pages: Vec<ObjectId> = reopened
            .pages()
            .expect("pages")
            .iter()
            .map(|page| page.id)
            .collect();
        assert_eq!(
            before_pages, after_pages,
            "page objects and ids are untouched"
        );
    }

    #[test]
    fn repeated_write_sees_the_previous_staged_outline() {
        let source = fixture("realworld/microsoft-excel-2019-sample.pdf");
        let mut document = Document::from_bytes(source).expect("fixture");
        let first = Outline::new().with_root(
            Bookmark::new("First")
                .expect("title")
                .with_destination(destination(0, FitType::Fit)),
        );
        write_outline(&mut document, &first).expect("first write");

        let second = Outline::new()
            .with_root(
                Bookmark::new("First")
                    .expect("title")
                    .with_destination(destination(0, FitType::Fit)),
            )
            .with_root(
                Bookmark::new("Second")
                    .expect("title")
                    .with_destination(destination(1, FitType::Fit)),
            );
        write_outline(&mut document, &second).expect("second write");

        let saved = document.save().expect("full rewrite");
        let reopened = Document::from_bytes(saved).expect("saved document parses");
        let read = read_outline(&reopened).expect("written outline reads");
        let titles: Vec<&str> = read
            .outline
            .roots
            .iter()
            .map(|bookmark| bookmark.title.as_str())
            .collect();
        assert_eq!(titles, vec!["First", "Second"]);
    }

    #[test]
    fn invalid_page_and_title_are_typed_errors_without_staging() {
        let source = fixture("realworld/microsoft-excel-2019-sample.pdf");
        let mut document = Document::from_bytes(source).expect("fixture");

        let bad_page = Outline::new().with_root(
            Bookmark::new("Bad page")
                .expect("title")
                .with_destination(destination(99, FitType::Fit)),
        );
        assert!(matches!(
            write_outline(&mut document, &bad_page),
            Err(OutlineWriteError::PageNotFound { page: 99 })
        ));
        assert!(
            !read_outline(&document).expect("still readable").present,
            "a rejected write must not create /Outlines"
        );

        let mut invalid = Bookmark::new("Valid").expect("title");
        invalid.title.clear();
        let bad_title = Outline::new().with_root(invalid);
        assert!(matches!(
            write_outline(&mut document, &bad_title),
            Err(OutlineWriteError::InvalidTitle(_))
        ));
        assert!(!read_outline(&document).expect("still readable").present);
    }

    #[test]
    fn invalid_input_leaves_document_bytes_untouched_and_later_writes_work() {
        let source = fixture("realworld/powerpoint-slides.pdf");
        let mut document = Document::from_bytes(source).expect("fixture");
        let baseline = document.save().expect("baseline rewrite");

        let bad_page = Outline::new().with_root(
            Bookmark::new("Bad page")
                .expect("title")
                .with_destination(destination(99, FitType::Fit)),
        );
        assert!(matches!(
            write_outline(&mut document, &bad_page),
            Err(OutlineWriteError::PageNotFound { page: 99 })
        ));
        assert_eq!(
            document.save().expect("save after rejected page"),
            baseline,
            "a rejected out-of-range page must not stage any object"
        );

        let bad_fit = Outline::new().with_root(
            Bookmark::new("Bad fit")
                .expect("title")
                .with_destination(destination(0, FitType::FitH { top: f32::NAN })),
        );
        assert!(matches!(
            write_outline(&mut document, &bad_fit),
            Err(OutlineWriteError::NonFiniteDestination)
        ));
        assert_eq!(
            document.save().expect("save after rejected fit"),
            baseline,
            "a rejected NaN coordinate must not stage any object"
        );

        let mut valid = Outline::new().with_root(
            Bookmark::new("Start")
                .expect("title")
                .with_destination(destination(0, FitType::Fit)),
        );
        write_outline(&mut document, &valid).expect("valid write after rejections");
        let saved = document.save().expect("save valid outline");
        let reopened = Document::from_bytes(saved).expect("valid outline parses");
        let read = read_outline(&reopened).expect("valid outline reads");
        assert_eq!(read.outline.roots.len(), 1);
        assert_eq!(read.outline.roots[0].title, "Start");

        valid
            .rename_bookmark(&BookmarkPath::root().child(0), "Renamed")
            .expect("rename");
        write_outline(&mut document, &valid).expect("second valid write");
        let saved = document.save().expect("save second outline");
        let reopened = Document::from_bytes(saved).expect("second outline parses");
        let read = read_outline(&reopened).expect("second outline reads");
        assert_eq!(read.outline.roots[0].title, "Renamed");
    }

    #[test]
    fn write_is_deterministic_for_identical_input() {
        let source = fixture("realworld/powerpoint-slides.pdf");
        let outline = Outline::new()
            .with_root(
                Bookmark::new("First")
                    .expect("title")
                    .with_destination(destination(0, FitType::Fit)),
            )
            .with_root(
                Bookmark::new("Second")
                    .expect("title")
                    .with_destination(destination(
                        2,
                        FitType::Xyz {
                            left: Some(0.0),
                            top: Some(405.0),
                            zoom: None,
                        },
                    )),
            );

        let mut first = Document::from_bytes(source.clone()).expect("fixture");
        write_outline(&mut first, &outline).expect("first write");
        let first_bytes = first.save().expect("first save");

        let mut second = Document::from_bytes(source).expect("fixture");
        write_outline(&mut second, &outline).expect("second write");
        let second_bytes = second.save().expect("second save");

        assert_eq!(first_bytes, second_bytes);
    }

    #[test]
    fn malformed_existing_outlines_is_a_typed_error() {
        let mut document = Document::from_bytes(fixture("minimal.pdf")).expect("fixture");
        let root = document.root().expect("catalog id");
        let mut catalog = document
            .object(root)
            .expect("catalog")
            .as_dictionary()
            .expect("catalog dictionary")
            .clone();
        catalog.insert("Outlines".to_owned(), Object::Integer(1));
        document
            .set_object(root, Object::Dictionary(catalog))
            .expect("stage catalog");

        let outline = Outline::new().with_root(Bookmark::new("Title").expect("title"));
        assert!(matches!(
            write_outline(&mut document, &outline),
            Err(OutlineWriteError::MalformedOutlines)
        ));
    }
}
