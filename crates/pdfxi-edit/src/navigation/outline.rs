//! Bookmark outline model and PDF dictionary round-trip.
//!
//! A [`Bookmark`] is a node in the outline tree. Children are stored inline as
//! a `Vec`, so the in-memory model is a plain tree with no possibility of a
//! cycle. The dictionary form, however, uses the PDF linked-list representation
//! (`/First`, `/Last`, `/Next`, `/Prev`, `/Parent`), which *can* be malformed.
//! Parsing therefore validates the reference graph and reports
//! [`NavigationError::Cycle`], [`NavigationError::Orphan`], and related errors
//! before building the tree.
//!
//! # Dictionary layout
//!
//! `outline_to_dict` produces an `/Outlines` dictionary:
//!
//! - `/Type /Outlines`
//! - `/Count <signed total visible count>`
//! - `/First`, `/Last` — ids of the first/last root bookmark
//! - `/Items` — array of per-item dictionaries sorted by id, each containing:
//!   - `XI_Id`, `/Title`, `/Parent`, `/First`, `/Last`, `/Next`, `/Prev`,
//!     `/Count`, `/Dest`, `/C`, `/F`, and `XI_Open`.

use std::collections::{HashMap, HashSet};

use crate::annotation::Color;
use crate::pdf::value::{optional_array, optional_bool, optional_int};
use crate::pdf::{Dict, Value};

use super::{
    Destination, NavigationError, id_value, optional_id, required_id, required_name,
    rgb_from_value, rgb_to_value, validate_name,
};

/// Bit in the `/F` flag field that marks a bookmark as italic.
const FLAG_ITALIC: u64 = 1;
/// Bit in the `/F` flag field that marks a bookmark as bold.
const FLAG_BOLD: u64 = 2;

/// A single bookmark (outline item) and its descendants.
#[derive(Clone, Debug, PartialEq)]
pub struct Bookmark {
    /// Text shown in the bookmark pane (`/Title`).
    pub title: String,
    /// Jump target, or `None` for a bookmark with no action (`/Dest`).
    pub destination: Option<Destination>,
    /// Nested child bookmarks, in display order.
    pub children: Vec<Bookmark>,
    /// Marker colour (`/C`), or `None` for the default colour.
    pub color: Option<Color>,
    /// Whether the title is rendered bold (`/F` bit 2).
    pub bold: bool,
    /// Whether the title is rendered italic (`/F` bit 1).
    pub italic: bool,
    /// Whether this bookmark's children are expanded (`/Count` sign / `XI_Open`).
    pub open: bool,
}

impl Bookmark {
    /// Creates a bookmark with the given title.
    ///
    /// The title must be non-empty and within [`super::MAX_NAME_BYTES`]. The
    /// bookmark starts with no destination, no children, no colour, regular
    /// style, and an open state.
    pub fn new(title: impl Into<String>) -> Result<Self, NavigationError> {
        let title = title.into();
        validate_name("Title", &title)?;
        Ok(Self {
            title,
            destination: None,
            children: Vec::new(),
            color: None,
            bold: false,
            italic: false,
            open: true,
        })
    }

    /// Sets the destination, returning the modified bookmark.
    pub fn with_destination(mut self, destination: Destination) -> Self {
        self.destination = Some(destination);
        self
    }

    /// Appends a child, returning the modified bookmark.
    pub fn with_child(mut self, child: Bookmark) -> Self {
        self.children.push(child);
        self
    }

    /// Appends several children, returning the modified bookmark.
    pub fn with_children(mut self, children: impl IntoIterator<Item = Bookmark>) -> Self {
        self.children.extend(children);
        self
    }

    /// Sets the marker colour, returning the modified bookmark.
    pub fn with_color(mut self, color: Color) -> Self {
        self.color = Some(color);
        self
    }

    /// Sets bold/italic style flags, returning the modified bookmark.
    pub fn with_style(mut self, bold: bool, italic: bool) -> Self {
        self.bold = bold;
        self.italic = italic;
        self
    }

    /// Sets the open/closed state, returning the modified bookmark.
    pub fn with_open(mut self, open: bool) -> Self {
        self.open = open;
        self
    }

    /// Returns the PDF `/F` style bit field for this bookmark.
    pub fn style_flags(&self) -> u64 {
        let mut flags = 0;
        if self.italic {
            flags |= FLAG_ITALIC;
        }
        if self.bold {
            flags |= FLAG_BOLD;
        }
        flags
    }

    /// Returns the number of visible descendants of this bookmark.
    ///
    /// An open bookmark exposes all of its children plus their visible
    /// descendants; a closed bookmark exposes only its immediate children
    /// (their subtrees are collapsed). This count is stored as `/Count`, with a
    /// negative sign when the bookmark is closed.
    pub fn visible_descendant_count(&self) -> i64 {
        if self.children.is_empty() {
            return 0;
        }
        if self.open {
            self.children
                .iter()
                .map(|child| 1 + child.visible_descendant_count())
                .sum()
        } else {
            self.children.len() as i64
        }
    }
}

/// The root container of an outline tree.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Outline {
    /// Top-level bookmarks, in display order.
    pub roots: Vec<Bookmark>,
}

impl Outline {
    /// Creates an empty outline.
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends a top-level bookmark, returning the modified outline.
    pub fn with_root(mut self, root: Bookmark) -> Self {
        self.roots.push(root);
        self
    }

    /// Returns the total number of visible bookmarks in the document.
    ///
    /// This is the value stored in the root `/Count`: every root is visible,
    /// plus each root's visible descendants.
    pub fn total_visible_count(&self) -> i64 {
        self.roots
            .iter()
            .map(|root| 1 + root.visible_descendant_count())
            .sum()
    }
}

/// A bookmark paired with the ids assigned during serialization.
struct FlatItem<'a> {
    id: u64,
    node: &'a Bookmark,
    parent: Option<u64>,
    first_child: Option<u64>,
    last_child: Option<u64>,
    next: Option<u64>,
    prev: Option<u64>,
}

/// Assigns ids in pre-order and records every sibling/parent link.
fn flatten<'a>(
    siblings: &'a [Bookmark],
    parent: Option<u64>,
    next_id: &mut u64,
    out: &mut Vec<FlatItem<'a>>,
) -> (Option<u64>, Option<u64>) {
    let mut first_id = None;
    let mut prev_index: Option<usize> = None;

    for node in siblings {
        let id = *next_id;
        *next_id += 1;
        let (child_first, child_last) = flatten(&node.children, Some(id), next_id, out);
        let index = out.len();
        out.push(FlatItem {
            id,
            node,
            parent,
            first_child: child_first,
            last_child: child_last,
            next: None,
            prev: None,
        });
        if let Some(previous) = prev_index {
            if let Some(item) = out.get_mut(previous) {
                item.next = Some(id);
            }
            let previous_id = id_of(out, previous);
            if let Some(item) = out.get_mut(index) {
                item.prev = previous_id;
            }
        } else {
            first_id = Some(id);
        }
        prev_index = Some(index);
    }

    let last_id = prev_index.and_then(|index| out.get(index).map(|item| item.id));
    (first_id, last_id)
}

/// Reads an id from `out` at `index`, returning `None` when out of range.
fn id_of(out: &[FlatItem<'_>], index: usize) -> Option<u64> {
    out.get(index).map(|item| item.id)
}

/// Converts one flat item into its dictionary form.
fn item_to_dict(item: &FlatItem<'_>) -> Dict {
    let mut dict = Dict::new();
    dict.insert("XI_Id".to_string(), id_value(item.id));
    dict.insert("Title".to_string(), Value::Text(item.node.title.clone()));
    if let Some(parent) = item.parent {
        dict.insert("Parent".to_string(), id_value(parent));
    }
    if let Some(id) = item.first_child {
        dict.insert("First".to_string(), id_value(id));
    }
    if let Some(id) = item.last_child {
        dict.insert("Last".to_string(), id_value(id));
    }
    if let Some(id) = item.next {
        dict.insert("Next".to_string(), id_value(id));
    }
    if let Some(id) = item.prev {
        dict.insert("Prev".to_string(), id_value(id));
    }
    let count = item.node.visible_descendant_count();
    if count != 0 {
        let signed = if item.node.open { count } else { -count };
        dict.insert("Count".to_string(), Value::Int(signed));
    }
    if let Some(destination) = &item.node.destination {
        dict.insert("Dest".to_string(), destination.to_value());
    }
    if let Some(color) = item.node.color {
        dict.insert("C".to_string(), rgb_to_value(color));
    }
    let flags = item.node.style_flags();
    if flags != 0 {
        dict.insert("F".to_string(), Value::Int(flags as i64));
    }
    dict.insert("XI_Open".to_string(), Value::Bool(item.node.open));
    dict
}

/// Serializes an outline into an `/Outlines` dictionary.
///
/// The produced dictionary is deterministic: ids are assigned in pre-order and
/// `/Items` is sorted by id, so structurally identical outlines always produce
/// byte-identical dictionaries.
pub fn outline_to_dict(outline: &Outline) -> Dict {
    let mut flat = Vec::new();
    let mut next_id = 0;
    let (root_first, root_last) = flatten(&outline.roots, None, &mut next_id, &mut flat);
    flat.sort_by_key(|item| item.id);

    let items: Vec<Value> = flat
        .iter()
        .map(|item| Value::Dictionary(item_to_dict(item)))
        .collect();

    let mut dict = Dict::new();
    dict.insert("Type".to_string(), Value::Name("Outlines".to_string()));
    dict.insert(
        "Count".to_string(),
        Value::Int(outline.total_visible_count()),
    );
    if let Some(id) = root_first {
        dict.insert("First".to_string(), id_value(id));
    }
    if let Some(id) = root_last {
        dict.insert("Last".to_string(), id_value(id));
    }
    dict.insert("Items".to_string(), Value::Array(items));
    dict
}

/// Parsed form of one outline item dictionary before graph validation.
struct RawItem {
    id: u64,
    title: String,
    parent: Option<u64>,
    first: Option<u64>,
    last: Option<u64>,
    next: Option<u64>,
    prev: Option<u64>,
    destination: Option<Destination>,
    color: Option<Color>,
    bold: bool,
    italic: bool,
    open: bool,
}

fn parse_raw_item(dict: &Dict) -> Result<RawItem, NavigationError> {
    let id = required_id(dict, "XI_Id")?;
    let title = required_name(dict, "Title")?;
    let parent = optional_id(dict, "Parent")?;
    let first = optional_id(dict, "First")?;
    let last = optional_id(dict, "Last")?;
    let next = optional_id(dict, "Next")?;
    let prev = optional_id(dict, "Prev")?;
    let count = optional_int(dict, "Count")?;

    let destination = match dict.get("Dest") {
        Some(value) => Some(Destination::from_value(value, "Dest")?),
        None => match dict.get("A") {
            Some(action) => {
                let action = action.as_dict().ok_or_else(|| NavigationError::WrongType {
                    key: "A".to_string(),
                    expected: "a dictionary",
                })?;
                let subtype = action.get("S").and_then(Value::as_name).unwrap_or("");
                if subtype == "GoTo" {
                    match action.get("D") {
                        Some(value) => Some(Destination::from_value(value, "D")?),
                        None => None,
                    }
                } else {
                    None
                }
            }
            None => None,
        },
    };

    let color = match dict.get("C") {
        Some(value) => Some(rgb_from_value(value)?),
        None => None,
    };

    let flags = optional_int(dict, "F")?.unwrap_or(0);
    let italic = flags & (FLAG_ITALIC as i64) != 0;
    let bold = flags & (FLAG_BOLD as i64) != 0;

    let open = match optional_bool(dict, "XI_Open")? {
        Some(value) => value,
        None => count.map(|value| value >= 0).unwrap_or(true),
    };

    Ok(RawItem {
        id,
        title,
        parent,
        first,
        last,
        next,
        prev,
        destination,
        color,
        bold,
        italic,
        open,
    })
}

fn index_items(items: &[RawItem]) -> Result<HashMap<u64, usize>, NavigationError> {
    let mut index = HashMap::new();
    for (position, item) in items.iter().enumerate() {
        if index.insert(item.id, position).is_some() {
            return Err(NavigationError::DuplicateId(item.id));
        }
    }
    Ok(index)
}

fn validate_references(
    items: &[RawItem],
    index: &HashMap<u64, usize>,
) -> Result<(), NavigationError> {
    for item in items {
        if let Some(parent) = item.parent
            && !index.contains_key(&parent)
        {
            return Err(NavigationError::UnknownParent {
                id: item.id,
                parent,
            });
        }
        for id in [item.first, item.last, item.next, item.prev]
            .into_iter()
            .flatten()
        {
            if !index.contains_key(&id) {
                return Err(NavigationError::MissingItem { id });
            }
        }
    }
    Ok(())
}

fn detect_cycles(items: &[RawItem], index: &HashMap<u64, usize>) -> Result<(), NavigationError> {
    let mut fully_checked = HashSet::new();
    for item in items {
        if fully_checked.contains(&item.id) {
            continue;
        }
        let mut seen = HashSet::new();
        let mut current = Some(item.id);
        while let Some(id) = current {
            if !seen.insert(id) {
                return Err(NavigationError::Cycle { id });
            }
            if fully_checked.contains(&id) {
                break;
            }
            current = index.get(&id).and_then(|&position| items[position].next);
        }
        fully_checked.extend(seen);
    }

    for item in items {
        let mut seen = HashSet::new();
        let mut current = Some(item.id);
        while let Some(id) = current {
            if !seen.insert(id) {
                return Err(NavigationError::Cycle { id });
            }
            current = index.get(&id).and_then(|&position| items[position].parent);
        }
    }
    Ok(())
}

fn get<'a>(
    id: u64,
    items: &'a [RawItem],
    index: &HashMap<u64, usize>,
) -> Result<&'a RawItem, NavigationError> {
    index
        .get(&id)
        .and_then(|&position| items.get(position))
        .ok_or(NavigationError::MissingItem { id })
}

fn child_ids(
    item: &RawItem,
    items: &[RawItem],
    index: &HashMap<u64, usize>,
) -> Result<Vec<u64>, NavigationError> {
    let mut ids = Vec::new();
    let mut current = item.first;
    let mut guard = HashSet::new();
    while let Some(id) = current {
        if !guard.insert(id) {
            return Err(NavigationError::Cycle { id });
        }
        let child = get(id, items, index)?;
        ids.push(id);
        current = child.next;
    }
    Ok(ids)
}

fn validate_structure(
    items: &[RawItem],
    index: &HashMap<u64, usize>,
    root_first: Option<u64>,
    root_last: Option<u64>,
) -> Result<Vec<u64>, NavigationError> {
    let mut roots = Vec::new();
    let mut current = root_first;
    let mut previous: Option<u64> = None;
    while let Some(id) = current {
        let item = get(id, items, index)?;
        if item.parent.is_some() {
            return Err(NavigationError::BrokenSiblingLink { id });
        }
        if item.prev != previous {
            return Err(NavigationError::BrokenSiblingLink { id });
        }
        roots.push(id);
        previous = Some(id);
        current = item.next;
    }
    if let Some(last) = root_last
        && previous != Some(last)
    {
        return Err(NavigationError::BrokenSiblingLink { id: last });
    }

    let mut reached: HashSet<u64> = roots.iter().copied().collect();
    for parent in items {
        let children = child_ids(parent, items, index)?;
        let mut expected_prev: Option<u64> = None;
        for child_id in &children {
            let child = get(*child_id, items, index)?;
            if child.parent != Some(parent.id) {
                return Err(NavigationError::BrokenSiblingLink { id: *child_id });
            }
            if child.prev != expected_prev {
                return Err(NavigationError::BrokenSiblingLink { id: *child_id });
            }
            expected_prev = Some(*child_id);
            reached.insert(*child_id);
        }
        if parent.last != expected_prev && !children.is_empty() {
            return Err(NavigationError::BrokenSiblingLink { id: parent.id });
        }
        if children.is_empty() && (parent.first.is_some() || parent.last.is_some()) {
            return Err(NavigationError::BrokenSiblingLink { id: parent.id });
        }
    }

    for item in items {
        if !reached.contains(&item.id) {
            return Err(NavigationError::Orphan { id: item.id });
        }
    }
    Ok(roots)
}

fn build_subtree(
    id: u64,
    items: &[RawItem],
    index: &HashMap<u64, usize>,
    visiting: &mut HashSet<u64>,
) -> Result<Bookmark, NavigationError> {
    if !visiting.insert(id) {
        return Err(NavigationError::Cycle { id });
    }
    let raw = get(id, items, index)?;
    let children = {
        let mut built = Vec::new();
        for child_id in child_ids(raw, items, index)? {
            built.push(build_subtree(child_id, items, index, visiting)?);
        }
        built
    };
    visiting.remove(&id);
    Ok(Bookmark {
        title: raw.title.clone(),
        destination: raw.destination.clone(),
        children,
        color: raw.color,
        bold: raw.bold,
        italic: raw.italic,
        open: raw.open,
    })
}

/// Parses an `/Outlines` dictionary into an [`Outline`] tree.
///
/// Reference cycles, dangling ids, and orphaned children are rejected with a
/// structured [`NavigationError`] rather than producing a malformed tree.
pub fn dict_to_outline(dict: &Dict) -> Result<Outline, NavigationError> {
    if let Some(value) = dict.get("Type") {
        let name = value.as_name().ok_or_else(|| NavigationError::WrongType {
            key: "Type".to_string(),
            expected: "a name",
        })?;
        if name != "Outlines" {
            return Err(NavigationError::InvalidName {
                key: "Type",
                value: name.to_string(),
            });
        }
    }

    let values = optional_array(dict, "Items")?.ok_or(NavigationError::MissingKey("Items"))?;
    let items: Vec<RawItem> = values
        .iter()
        .map(|value| {
            let item = value.as_dict().ok_or_else(|| NavigationError::WrongType {
                key: "Items".to_string(),
                expected: "an array of dictionaries",
            })?;
            parse_raw_item(item)
        })
        .collect::<Result<_, _>>()?;

    let index = index_items(&items)?;
    validate_references(&items, &index)?;
    detect_cycles(&items, &index)?;
    let roots = validate_structure(
        &items,
        &index,
        optional_id(dict, "First")?,
        optional_id(dict, "Last")?,
    )?;

    let mut visiting = HashSet::new();
    let mut built_roots = Vec::new();
    for id in roots {
        built_roots.push(build_subtree(id, &items, &index, &mut visiting)?);
    }
    Ok(Outline { roots: built_roots })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::PageIndex;
    use crate::navigation::FitType;

    fn simple(title: &str) -> Bookmark {
        Bookmark::new(title).expect("test title must be valid")
    }

    fn sample_outline() -> Outline {
        Outline::new()
            .with_root(
                simple("Chapter 1")
                    .with_child(
                        simple("Section 1.1")
                            .with_child(simple("Figure 1.1.1"))
                            .with_child(simple("Figure 1.1.2")),
                    )
                    .with_child(simple("Section 1.2")),
            )
            .with_root(simple("Chapter 2").with_child(simple("Section 2.1")))
            .with_root(simple("Appendix"))
    }

    fn item_index(items: &[Value]) -> HashMap<u64, &Dict> {
        let mut map = HashMap::new();
        for value in items {
            let dict = value.as_dict().expect("item must be a dictionary");
            let id = dict.get("XI_Id").and_then(Value::as_int).expect("id") as u64;
            map.insert(id, dict);
        }
        map
    }

    fn items_of(dict: &Dict) -> Vec<Value> {
        dict.get("Items")
            .and_then(Value::as_array)
            .expect("Items array")
            .to_vec()
    }

    #[test]
    fn outline_round_trip_three_levels_with_sibling_order() {
        let original = sample_outline();
        let dict = outline_to_dict(&original);
        let restored = dict_to_outline(&dict).expect("valid outline must parse");
        assert_eq!(restored, original);

        // Verify the linked-list shape directly: the first root points to the
        // second, whose `Prev` points back, and each parent's `First` chain
        // matches the child order.
        let items = items_of(&dict);
        let by_id = item_index(&items);
        assert_eq!(dict.get("Count").and_then(Value::as_int), Some(8));
        let first_root = dict.get("First").and_then(Value::as_int).expect("First") as u64;
        let first_dict = by_id.get(&first_root).expect("first item");
        assert_eq!(
            first_dict.get("Title").and_then(Value::as_text),
            Some("Chapter 1")
        );
        let second_root = first_dict
            .get("Next")
            .and_then(Value::as_int)
            .expect("Next") as u64;
        let second_dict = by_id.get(&second_root).expect("second item");
        assert_eq!(
            second_dict.get("Prev").and_then(Value::as_int),
            Some(first_root as i64)
        );
        assert_eq!(
            second_dict.get("Title").and_then(Value::as_text),
            Some("Chapter 2")
        );
    }

    #[test]
    fn open_and_closed_counts_are_signed() {
        let open = sample_outline();
        let open_dict = outline_to_dict(&open);
        assert_eq!(open_dict.get("Count").and_then(Value::as_int), Some(8));
        assert_eq!(dict_to_outline(&open_dict).expect("open outline"), open);

        let mut closed = sample_outline();
        if let Some(chapter) = closed.roots.get_mut(0) {
            chapter.open = false;
        }
        let closed_dict = outline_to_dict(&closed);
        // Chapter 1 now exposes only its two immediate children.
        assert_eq!(closed_dict.get("Count").and_then(Value::as_int), Some(6));
        let items = items_of(&closed_dict);
        let by_id = item_index(&items);
        let closed_item = by_id
            .values()
            .find(|d| d.get("Title").and_then(Value::as_text) == Some("Chapter 1"))
            .expect("chapter item");
        assert_eq!(closed_item.get("Count").and_then(Value::as_int), Some(-2));
        assert_eq!(
            closed_item.get("XI_Open").and_then(Value::as_bool),
            Some(false)
        );
        assert_eq!(
            dict_to_outline(&closed_dict).expect("closed outline"),
            closed
        );
    }

    #[test]
    fn cycle_in_next_chain_is_rejected() {
        let mut dict = outline_to_dict(&sample_outline());
        let items = dict
            .get_mut("Items")
            .and_then(Value::as_array_mut)
            .expect("items");
        // Make item 0 point its `Next` back at itself, forming a cycle.
        if let Some(Value::Dictionary(first)) = items.first_mut() {
            first.insert("Next".to_string(), Value::Int(0));
        }
        assert!(matches!(
            dict_to_outline(&dict),
            Err(NavigationError::Cycle { .. })
        ));
    }

    #[test]
    fn cycle_in_parent_chain_is_rejected() {
        let mut dict = outline_to_dict(&sample_outline());
        let items = dict
            .get_mut("Items")
            .and_then(Value::as_array_mut)
            .expect("items");
        // Item 0 is a root; give it a parent pointing at a child (id 1), and
        // ensure item 1's parent is item 0, closing a parent cycle.
        if let Some(Value::Dictionary(first)) = items.first_mut() {
            first.insert("Parent".to_string(), Value::Int(1));
        }
        assert!(matches!(
            dict_to_outline(&dict),
            Err(NavigationError::Cycle { .. })
        ));
    }

    #[test]
    fn orphan_child_is_rejected() {
        let mut dict = outline_to_dict(&sample_outline());
        let items = dict
            .get_mut("Items")
            .and_then(Value::as_array_mut)
            .expect("items");
        // Re-point child id 1 at a wrong parent (id 5) so it is not reachable
        // from any sibling chain.
        if let Some(Value::Dictionary(child)) = items.get_mut(1) {
            child.insert("Parent".to_string(), Value::Int(5));
        }
        assert!(matches!(
            dict_to_outline(&dict),
            Err(NavigationError::BrokenSiblingLink { .. }) | Err(NavigationError::Orphan { .. })
        ));
    }

    #[test]
    fn missing_reference_is_rejected() {
        let mut dict = outline_to_dict(&sample_outline());
        let items = dict
            .get_mut("Items")
            .and_then(Value::as_array_mut)
            .expect("items");
        if let Some(Value::Dictionary(first)) = items.first_mut() {
            first.insert("First".to_string(), Value::Int(999));
        }
        assert!(matches!(
            dict_to_outline(&dict),
            Err(NavigationError::MissingItem { .. })
        ));
    }

    #[test]
    fn bookmark_destination_style_and_colour_round_trip() {
        let bookmark = simple("Styled")
            .with_destination(Destination::new(
                PageIndex::new(4),
                FitType::FitR {
                    left: 1.0,
                    bottom: 2.0,
                    right: 3.0,
                    top: 4.0,
                },
            ))
            .with_color(Color::new(10, 20, 30))
            .with_style(true, true)
            .with_open(false);
        let outline = Outline::new().with_root(bookmark);
        let dict = outline_to_dict(&outline);
        let items = items_of(&dict);
        let item = items[0].as_dict().expect("item dict");
        assert_eq!(item.get("F").and_then(Value::as_int), Some(3));
        assert!(item.contains_key("Dest"));
        assert!(item.contains_key("C"));
        assert_eq!(dict_to_outline(&dict).expect("parse"), outline);
    }

    #[test]
    fn empty_outline_round_trip() {
        let outline = Outline::new();
        let dict = outline_to_dict(&outline);
        assert_eq!(dict.get("Count").and_then(Value::as_int), Some(0));
        assert!(!dict.contains_key("First"));
        assert_eq!(dict_to_outline(&dict).expect("empty outline"), outline);
    }

    #[test]
    fn serialization_is_deterministic() {
        let outline = sample_outline();
        assert_eq!(outline_to_dict(&outline), outline_to_dict(&outline));
    }

    #[test]
    fn duplicate_ids_are_rejected() {
        let mut dict = outline_to_dict(&sample_outline());
        let items = dict
            .get_mut("Items")
            .and_then(Value::as_array_mut)
            .expect("items");
        let duplicate_id = items
            .first()
            .and_then(Value::as_dict)
            .and_then(|d| d.get("XI_Id").cloned())
            .expect("id");
        if let Some(Value::Dictionary(b)) = items.get_mut(1) {
            b.insert("XI_Id".to_string(), duplicate_id);
        }
        assert!(matches!(
            dict_to_outline(&dict),
            Err(NavigationError::DuplicateId(_))
        ));
    }
}
