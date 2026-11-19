//! Tab order for form fields: `/Tabs` policy parsing, deterministic widget
//! ordering, and next/previous navigation with a skip policy.
//!
//! The AcroForm `/Tabs` entry selects one of four orders:
//!
//! - `/R` — rows, then columns (the PDF default when `/Tabs` is absent).
//! - `/C` — columns, then rows.
//! - `/S` — the document structure tree order.
//! - `/A` — the annotation order of the page.
//!
//! [`TabPolicy`] models that choice and [`parse_tabs`] reads it from a PDF name.
//! A [`TabOrder`] orders a set of [`TabWidget`]s by page first, then by the
//! selected policy. Geometry is only used by the row/column policies; a widget
//! carries its page, annotation index and rectangle so the ordering is fully
//! deterministic without reading the document again.
//!
//! Structure order cannot be derived from geometry, so [`TabOrder::with_structure`]
//! accepts an explicit sequence of fully qualified field names supplied by a
//! host that owns the structure tree. Widgets not in that sequence fall to the
//! end of their page in annotation order. When no sequence is supplied the
//! annotation order is used as a stable fallback.
//!
//! Navigation is provided by [`TabNavigator`] and by the stateless
//! [`TabOrder::next_after`] / [`TabOrder::previous_before`] helpers. A
//! [`NavPolicy`] controls whether hidden and read-only fields are skipped and
//! whether traversal wraps at the ends. Nothing here mutates a form or depends
//! on a UI.

use crate::acroform::PdfValue;
use crate::field::Field;
use crate::form::Form;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

/// The ordering selected by the AcroForm `/Tabs` entry.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum TabPolicy {
    /// `/R` — order fields row by row, left to right within each row.
    #[default]
    Rows,
    /// `/C` — order fields column by column, top to bottom within each column.
    Columns,
    /// `/S` — follow the document structure tree order.
    Structure,
    /// `/A` — follow the annotation order on each page.
    Annotations,
}

impl TabPolicy {
    /// Maps a `/Tabs` name (`"R"`, `"C"`, `"S"` or `"A"`) to a policy.
    ///
    /// Unknown names fall back to [`TabPolicy::Rows`], matching the PDF default
    /// when `/Tabs` is absent or unrecognised.
    pub const fn from_name(name: &str) -> Self {
        match name.as_bytes() {
            b"R" => Self::Rows,
            b"C" => Self::Columns,
            b"S" => Self::Structure,
            b"A" => Self::Annotations,
            _ => Self::Rows,
        }
    }

    /// Returns the canonical `/Tabs` name for this policy.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Rows => "R",
            Self::Columns => "C",
            Self::Structure => "S",
            Self::Annotations => "A",
        }
    }

    /// Returns a stable, human-readable label.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Rows => "rows",
            Self::Columns => "columns",
            Self::Structure => "structure",
            Self::Annotations => "annotations",
        }
    }
}

impl fmt::Display for TabPolicy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// Parses the AcroForm `/Tabs` value into a [`TabPolicy`].
///
/// Only a PDF name or text value is consulted; `None`, references and other
/// value kinds fall back to [`TabPolicy::Rows`] so hostile input can never abort
/// tab-order construction.
pub fn parse_tabs(value: Option<&PdfValue>) -> TabPolicy {
    match value {
        Some(PdfValue::Name(name)) | Some(PdfValue::Text(name)) => TabPolicy::from_name(name),
        _ => TabPolicy::default(),
    }
}

/// Reads `/Tabs` from an AcroForm dictionary.
pub fn tabs_from_acroform(dict: &BTreeMap<String, PdfValue>) -> TabPolicy {
    parse_tabs(dict.get("Tabs"))
}

/// One focusable widget participating in tab order.
///
/// Page indices are zero-based. `annotation_index` is the widget's position in
/// its page's annotation array and is the tie-breaker for annotation and
/// structure orders. `rect` is `[x0, y0, x1, y1]` in default user space.
#[derive(Debug, Clone, PartialEq)]
pub struct TabWidget {
    name: String,
    page: usize,
    annotation_index: usize,
    rect: [f64; 4],
    hidden: bool,
    read_only: bool,
}

impl TabWidget {
    /// Creates a widget with an empty rectangle that is neither hidden nor
    /// read-only.
    pub fn new(name: impl Into<String>, page: usize, annotation_index: usize) -> Self {
        Self {
            name: name.into(),
            page,
            annotation_index,
            rect: [0.0; 4],
            hidden: false,
            read_only: false,
        }
    }

    /// Creates a widget from a [`Field`], deriving the read-only flag from its
    /// [`crate::FieldFlags`].
    pub fn from_field(field: &Field, page: usize, annotation_index: usize) -> Self {
        Self {
            read_only: field.is_read_only(),
            ..Self::new(field.name(), page, annotation_index)
        }
    }

    /// Sets the widget rectangle.
    ///
    /// A rectangle containing a non-finite coordinate is replaced with an all-
    /// zero rectangle so ordering remains total on hostile input.
    #[must_use]
    pub fn with_rect(mut self, x0: f64, y0: f64, x1: f64, y1: f64) -> Self {
        if [x0, y0, x1, y1].iter().all(|value| value.is_finite()) {
            self.rect = [x0, y0, x1, y1];
        } else {
            self.rect = [0.0; 4];
        }
        self
    }

    /// Marks the widget as hidden (excluded by default during navigation).
    #[must_use]
    pub const fn hidden(mut self) -> Self {
        self.hidden = true;
        self
    }

    /// Marks the widget as read-only (excluded by default during navigation).
    #[must_use]
    pub const fn read_only(mut self) -> Self {
        self.read_only = true;
        self
    }

    /// Sets or clears the hidden flag.
    pub fn set_hidden(&mut self, hidden: bool) {
        self.hidden = hidden;
    }

    /// Sets or clears the read-only flag.
    pub fn set_read_only(&mut self, read_only: bool) {
        self.read_only = read_only;
    }

    /// Returns the fully qualified field name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the zero-based page index.
    pub const fn page(&self) -> usize {
        self.page
    }

    /// Returns the widget's position in its page's annotation array.
    pub const fn annotation_index(&self) -> usize {
        self.annotation_index
    }

    /// Returns the rectangle as `[x0, y0, x1, y1]`.
    pub const fn rect(&self) -> [f64; 4] {
        self.rect
    }

    /// Returns `true` if the widget is hidden.
    pub const fn is_hidden(&self) -> bool {
        self.hidden
    }

    /// Returns `true` if the widget is read-only.
    pub const fn is_read_only(&self) -> bool {
        self.read_only
    }

    /// Returns the left edge of the rectangle.
    pub fn left(&self) -> f64 {
        self.rect[0].min(self.rect[2])
    }

    /// Returns the top edge of the rectangle (the larger `y` value).
    pub fn top(&self) -> f64 {
        self.rect[1].max(self.rect[3])
    }

    /// Returns the vertical centre of the rectangle.
    pub fn vertical_center(&self) -> f64 {
        (self.rect[1] + self.rect[3]) * 0.5
    }

    /// Returns the horizontal centre of the rectangle.
    pub fn horizontal_center(&self) -> f64 {
        (self.rect[0] + self.rect[2]) * 0.5
    }
}

/// Controls which widgets are reachable during tab navigation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NavPolicy {
    /// Skip widgets flagged hidden when `true`.
    pub skip_hidden: bool,
    /// Skip widgets flagged read-only when `true`.
    pub skip_read_only: bool,
    /// Wrap from the last widget to the first (and back) when `true`.
    pub wrap: bool,
}

impl Default for NavPolicy {
    fn default() -> Self {
        Self {
            skip_hidden: true,
            skip_read_only: true,
            wrap: true,
        }
    }
}

impl NavPolicy {
    /// Creates a policy with all three defaults enabled.
    pub const fn new() -> Self {
        Self {
            skip_hidden: true,
            skip_read_only: true,
            wrap: true,
        }
    }

    /// Disables hidden-widget skipping.
    #[must_use]
    pub const fn with_hidden(mut self, skip: bool) -> Self {
        self.skip_hidden = skip;
        self
    }

    /// Disables read-only-widget skipping.
    #[must_use]
    pub const fn with_read_only(mut self, skip: bool) -> Self {
        self.skip_read_only = skip;
        self
    }

    /// Enables or disables end-of-order wrapping.
    #[must_use]
    pub const fn with_wrap(mut self, wrap: bool) -> Self {
        self.wrap = wrap;
        self
    }
}

/// Vertical grouping tolerance used by the row policy, in user-space units.
const ROW_TOLERANCE: f64 = 2.0;
/// Horizontal grouping tolerance used by the column policy, in user-space units.
const COLUMN_TOLERANCE: f64 = 2.0;

/// A deterministic ordering of [`TabWidget`]s by page and [`TabPolicy`].
#[derive(Debug, Clone, PartialEq)]
pub struct TabOrder {
    policy: TabPolicy,
    widgets: Vec<TabWidget>,
}

impl TabOrder {
    /// Builds an order from `widgets` under `policy`.
    ///
    /// For [`TabPolicy::Structure`] without an explicit structure sequence the
    /// annotation order is used as a stable fallback. Use
    /// [`TabOrder::with_structure`] to supply a structure order.
    pub fn new(policy: TabPolicy, widgets: Vec<TabWidget>) -> Self {
        Self::with_structure(policy, widgets, &[])
    }

    /// Builds an order, using `structure_names` for [`TabPolicy::Structure`].
    ///
    /// Widgets whose name appears in `structure_names` are ordered by their
    /// position in that sequence within each page; all others are placed after
    /// them in annotation order.
    pub fn with_structure(
        policy: TabPolicy,
        widgets: Vec<TabWidget>,
        structure_names: &[String],
    ) -> Self {
        let order = match policy {
            TabPolicy::Rows => order_rows(&widgets),
            TabPolicy::Columns => order_columns(&widgets),
            TabPolicy::Annotations => order_annotations(&widgets),
            TabPolicy::Structure => order_structure(&widgets, structure_names),
        };
        let ordered = order
            .into_iter()
            .map(|index| widgets[index].clone())
            .collect();
        Self {
            policy,
            widgets: ordered,
        }
    }

    /// Builds widget entries from every field in `form` on a single page, in
    /// field insertion order.
    pub fn from_form(form: &Form, page: usize, policy: TabPolicy) -> Self {
        let widgets = form
            .fields()
            .enumerate()
            .map(|(index, field)| TabWidget::from_field(field, page, index))
            .collect();
        Self::new(policy, widgets)
    }

    /// Returns the selected policy.
    pub const fn policy(&self) -> TabPolicy {
        self.policy
    }

    /// Returns the ordered widgets.
    pub fn widgets(&self) -> &[TabWidget] {
        &self.widgets
    }

    /// Returns the fully qualified field names in tab order.
    pub fn names(&self) -> impl Iterator<Item = &str> + '_ {
        self.widgets.iter().map(TabWidget::name)
    }

    /// Returns the number of widgets.
    pub fn len(&self) -> usize {
        self.widgets.len()
    }

    /// Returns `true` when there are no widgets.
    pub fn is_empty(&self) -> bool {
        self.widgets.is_empty()
    }

    /// Returns the distinct page indices present, in ascending order.
    pub fn pages(&self) -> Vec<usize> {
        let pages: BTreeSet<usize> = self.widgets.iter().map(TabWidget::page).collect();
        pages.into_iter().collect()
    }

    /// Returns a copy restricted to the given page indices.
    ///
    /// Relative order of the retained widgets is preserved, so a navigator over
    /// the subset traverses only those pages.
    pub fn subset_for_pages(&self, pages: &[usize]) -> Self {
        let allowed: BTreeSet<usize> = pages.iter().copied().collect();
        let widgets = self
            .widgets
            .iter()
            .filter(|widget| allowed.contains(&widget.page()))
            .cloned()
            .collect();
        Self {
            policy: self.policy,
            widgets,
        }
    }

    /// Returns the name immediately after `name`, applying `nav`.
    ///
    /// An unknown `name` starts the search at the first widget. `None` is
    /// returned when no eligible widget exists (or when wrapping is disabled
    /// and the end of the order is reached).
    pub fn next_after<'a>(&'a self, name: &str, nav: &NavPolicy) -> Option<&'a str> {
        let len = self.widgets.len();
        if len == 0 {
            return None;
        }
        let position = self.widgets.iter().position(|widget| widget.name() == name);
        match position {
            Some(index) if nav.wrap => self.scan_next((index + 1) % len, len, nav),
            Some(index) => {
                let start = index + 1;
                self.scan_next(start, len - start, nav)
            }
            None => self.scan_next(0, len, nav),
        }
    }

    /// Returns the name immediately before `name`, applying `nav`.
    ///
    /// An unknown `name` starts the search at the last widget when wrapping is
    /// enabled, otherwise at the first widget.
    pub fn previous_before<'a>(&'a self, name: &str, nav: &NavPolicy) -> Option<&'a str> {
        let len = self.widgets.len();
        if len == 0 {
            return None;
        }
        let position = self.widgets.iter().position(|widget| widget.name() == name);
        match position {
            Some(0) if !nav.wrap => None,
            Some(index) => {
                let start = (index + len - 1) % len;
                let count = if nav.wrap { len } else { index };
                self.scan_previous(start, count, nav)
            }
            None if nav.wrap => self.scan_previous(len - 1, len, nav),
            None => self.scan_previous(0, 1, nav),
        }
    }

    fn scan_next<'a>(&'a self, start: usize, count: usize, nav: &NavPolicy) -> Option<&'a str> {
        let len = self.widgets.len();
        for offset in 0..count {
            let index = (start + offset) % len;
            if self.eligible(index, nav) {
                return Some(self.widgets[index].name());
            }
        }
        None
    }

    fn scan_previous<'a>(&'a self, start: usize, count: usize, nav: &NavPolicy) -> Option<&'a str> {
        let len = self.widgets.len();
        for offset in 0..count {
            let index = (start + len - (offset % len)) % len;
            if self.eligible(index, nav) {
                return Some(self.widgets[index].name());
            }
        }
        None
    }

    fn eligible(&self, index: usize, nav: &NavPolicy) -> bool {
        let widget = &self.widgets[index];
        (!nav.skip_hidden || !widget.is_hidden()) && (!nav.skip_read_only || !widget.is_read_only())
    }
}

/// A stateful cursor over a [`TabOrder`].
#[derive(Debug, Clone)]
pub struct TabNavigator<'a> {
    order: &'a TabOrder,
    nav: NavPolicy,
    current: Option<String>,
}

impl<'a> TabNavigator<'a> {
    /// Creates a navigator with the default [`NavPolicy`].
    pub fn new(order: &'a TabOrder) -> Self {
        Self::with_policy(order, NavPolicy::default())
    }

    /// Creates a navigator with an explicit [`NavPolicy`].
    pub fn with_policy(order: &'a TabOrder, nav: NavPolicy) -> Self {
        Self {
            order,
            nav,
            current: None,
        }
    }

    /// Places the cursor on `name` without checking eligibility.
    pub fn focus(&mut self, name: &str) {
        self.current = Some(name.to_owned());
    }

    /// Returns the current field name, if any.
    pub fn current(&self) -> Option<&str> {
        self.current.as_deref()
    }

    /// Moves to the previous eligible field and returns its name.
    pub fn previous(&mut self) -> Option<&'a str> {
        let found = self
            .order
            .previous_before(self.current.as_deref().unwrap_or(""), &self.nav);
        self.current = found.map(str::to_owned);
        found
    }
}

impl<'a> Iterator for TabNavigator<'a> {
    type Item = &'a str;

    /// Advances to the next eligible field and returns its name.
    ///
    /// This is the [`Iterator`] entry point; use [`TabNavigator::previous`] to
    /// walk backwards.
    fn next(&mut self) -> Option<Self::Item> {
        let found = self
            .order
            .next_after(self.current.as_deref().unwrap_or(""), &self.nav);
        self.current = found.map(str::to_owned);
        found
    }
}

fn order_rows(widgets: &[TabWidget]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..widgets.len()).collect();
    order.sort_by(|&a, &b| {
        let left = &widgets[a];
        let right = &widgets[b];
        left.page
            .cmp(&right.page)
            .then_with(|| right.vertical_center().total_cmp(&left.vertical_center()))
            .then_with(|| left.left().total_cmp(&right.left()))
            .then_with(|| left.name().cmp(right.name()))
    });
    group_by_band(widgets, order, Axis::Vertical, ROW_TOLERANCE, false)
}

fn order_columns(widgets: &[TabWidget]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..widgets.len()).collect();
    order.sort_by(|&a, &b| {
        let left = &widgets[a];
        let right = &widgets[b];
        left.page
            .cmp(&right.page)
            .then_with(|| {
                left.horizontal_center()
                    .total_cmp(&right.horizontal_center())
            })
            .then_with(|| right.top().total_cmp(&left.top()))
            .then_with(|| left.name().cmp(right.name()))
    });
    group_by_band(widgets, order, Axis::Horizontal, COLUMN_TOLERANCE, true)
}

fn order_annotations(widgets: &[TabWidget]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..widgets.len()).collect();
    order.sort_by(|&a, &b| {
        let left = &widgets[a];
        let right = &widgets[b];
        left.page
            .cmp(&right.page)
            .then_with(|| left.annotation_index().cmp(&right.annotation_index()))
            .then_with(|| left.name().cmp(right.name()))
    });
    order
}

fn order_structure(widgets: &[TabWidget], structure_names: &[String]) -> Vec<usize> {
    let mut rank: BTreeMap<&str, usize> = BTreeMap::new();
    for (index, name) in structure_names.iter().enumerate() {
        rank.entry(name.as_str()).or_insert(index);
    }
    let mut order: Vec<usize> = (0..widgets.len()).collect();
    order.sort_by(|&a, &b| {
        let left = &widgets[a];
        let right = &widgets[b];
        let left_rank = rank.get(left.name()).copied().unwrap_or(usize::MAX);
        let right_rank = rank.get(right.name()).copied().unwrap_or(usize::MAX);
        left.page
            .cmp(&right.page)
            .then_with(|| left_rank.cmp(&right_rank))
            .then_with(|| left.annotation_index().cmp(&right.annotation_index()))
            .then_with(|| left.name().cmp(right.name()))
    });
    order
}

#[derive(Clone, Copy)]
enum Axis {
    Vertical,
    Horizontal,
}

fn band_center(widget: &TabWidget, axis: Axis) -> f64 {
    match axis {
        Axis::Vertical => widget.vertical_center(),
        Axis::Horizontal => widget.horizontal_center(),
    }
}

fn within_band(widget: &TabWidget, axis: Axis, tolerance: f64, center: f64) -> bool {
    (band_center(widget, axis) - center).abs() <= tolerance
}

fn sort_within_band(widgets: &[TabWidget], band: &mut [usize], axis: Axis, descending: bool) {
    band.sort_by(|&a, &b| {
        let left = &widgets[a];
        let right = &widgets[b];
        let (primary_a, primary_b) = match axis {
            Axis::Vertical => (left.left(), right.left()),
            Axis::Horizontal => (left.top(), right.top()),
        };
        let primary = if descending {
            primary_b.total_cmp(&primary_a)
        } else {
            primary_a.total_cmp(&primary_b)
        };
        primary.then_with(|| left.name().cmp(right.name()))
    });
}

fn group_by_band(
    widgets: &[TabWidget],
    order: Vec<usize>,
    axis: Axis,
    tolerance: f64,
    descending: bool,
) -> Vec<usize> {
    let mut result = Vec::with_capacity(order.len());
    let mut index = 0;
    while index < order.len() {
        let page = widgets[order[index]].page;
        let center = band_center(&widgets[order[index]], axis);
        let mut band = Vec::new();
        band.push(order[index]);
        let mut next = index + 1;
        while next < order.len()
            && widgets[order[next]].page == page
            && within_band(&widgets[order[next]], axis, tolerance, center)
        {
            band.push(order[next]);
            next += 1;
        }
        sort_within_band(widgets, &mut band, axis, descending);
        result.extend(band);
        index = next;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::field::FieldType;

    fn widget(name: &str, page: usize, index: usize, x: f64, y: f64) -> TabWidget {
        TabWidget::new(name, page, index).with_rect(x, y, x + 20.0, y + 10.0)
    }

    fn names(order: &TabOrder) -> Vec<&str> {
        order.names().collect()
    }

    #[test]
    fn tabs_policy_parses_all_names_and_defaults() {
        assert_eq!(TabPolicy::from_name("R"), TabPolicy::Rows);
        assert_eq!(TabPolicy::from_name("C"), TabPolicy::Columns);
        assert_eq!(TabPolicy::from_name("S"), TabPolicy::Structure);
        assert_eq!(TabPolicy::from_name("A"), TabPolicy::Annotations);
        assert_eq!(TabPolicy::from_name("?"), TabPolicy::Rows);
        assert_eq!(parse_tabs(None), TabPolicy::Rows);
        assert_eq!(
            parse_tabs(Some(&PdfValue::Name("C".to_owned()))),
            TabPolicy::Columns
        );

        let mut dict = BTreeMap::new();
        dict.insert("Tabs".to_owned(), PdfValue::Name("A".to_owned()));
        assert_eq!(tabs_from_acroform(&dict), TabPolicy::Annotations);
    }

    #[test]
    fn rows_order_is_top_to_bottom_then_left_to_right() {
        let widgets = vec![
            widget("bottom_left", 0, 0, 10.0, 0.0),
            widget("top_right", 0, 1, 200.0, 100.0),
            widget("top_left", 0, 2, 10.0, 100.0),
            widget("bottom_right", 0, 3, 200.0, 0.0),
        ];
        let order = TabOrder::new(TabPolicy::Rows, widgets);
        assert_eq!(
            names(&order),
            ["top_left", "top_right", "bottom_left", "bottom_right"]
        );
    }

    #[test]
    fn columns_order_is_left_to_right_then_top_to_bottom() {
        let widgets = vec![
            widget("bottom_left", 0, 0, 10.0, 0.0),
            widget("top_right", 0, 1, 200.0, 100.0),
            widget("top_left", 0, 2, 10.0, 100.0),
            widget("bottom_right", 0, 3, 200.0, 0.0),
        ];
        let order = TabOrder::new(TabPolicy::Columns, widgets);
        assert_eq!(
            names(&order),
            ["top_left", "bottom_left", "top_right", "bottom_right"]
        );
    }

    #[test]
    fn annotation_order_follows_annotation_index() {
        let widgets = vec![
            widget("third", 0, 2, 10.0, 0.0),
            widget("first", 0, 0, 200.0, 100.0),
            widget("second", 0, 1, 10.0, 100.0),
        ];
        let order = TabOrder::new(TabPolicy::Annotations, widgets);
        assert_eq!(names(&order), ["first", "second", "third"]);
    }

    #[test]
    fn structure_order_follows_supplied_names() {
        let widgets = vec![
            widget("c", 0, 0, 10.0, 0.0),
            widget("a", 0, 1, 200.0, 100.0),
            widget("b", 0, 2, 10.0, 100.0),
        ];
        let sequence = vec!["a".to_owned(), "b".to_owned(), "c".to_owned()];
        let order = TabOrder::with_structure(TabPolicy::Structure, widgets, &sequence);
        assert_eq!(names(&order), ["a", "b", "c"]);

        // Without a sequence, structure falls back to annotation order.
        let widgets = vec![
            widget("c", 0, 0, 10.0, 0.0),
            widget("a", 0, 1, 200.0, 100.0),
        ];
        let order = TabOrder::new(TabPolicy::Structure, widgets);
        assert_eq!(names(&order), ["c", "a"]);
    }

    #[test]
    fn next_and_previous_wrap_at_the_ends() {
        let widgets = vec![
            widget("a", 0, 0, 10.0, 100.0),
            widget("b", 0, 1, 60.0, 100.0),
            widget("c", 0, 2, 10.0, 0.0),
        ];
        let order = TabOrder::new(TabPolicy::Rows, widgets);
        let nav = NavPolicy::default();

        assert_eq!(order.next_after("b", &nav), Some("c"));
        assert_eq!(order.next_after("c", &nav), Some("a"));
        assert_eq!(order.previous_before("a", &nav), Some("c"));
        assert_eq!(order.previous_before("b", &nav), Some("a"));
    }

    #[test]
    fn hidden_and_read_only_widgets_are_skipped() {
        let widgets = vec![
            widget("a", 0, 0, 10.0, 100.0),
            widget("hidden", 0, 1, 60.0, 100.0).hidden(),
            widget("locked", 0, 2, 110.0, 100.0).read_only(),
            widget("d", 0, 3, 160.0, 100.0),
        ];
        let order = TabOrder::new(TabPolicy::Annotations, widgets);
        let nav = NavPolicy::default();

        assert_eq!(order.next_after("a", &nav), Some("d"));
        assert_eq!(order.previous_before("d", &nav), Some("a"));

        let include_all = NavPolicy::new().with_hidden(false).with_read_only(false);
        assert_eq!(order.next_after("a", &include_all), Some("hidden"));
    }

    #[test]
    fn no_wrap_stops_at_the_end() {
        let widgets = vec![widget("a", 0, 0, 10.0, 0.0), widget("b", 0, 1, 60.0, 0.0)];
        let order = TabOrder::new(TabPolicy::Rows, widgets);
        let nav = NavPolicy::default().with_wrap(false);
        assert_eq!(order.next_after("b", &nav), None);
        assert_eq!(order.previous_before("a", &nav), None);
    }

    #[test]
    fn page_subset_traverses_only_selected_pages() {
        let widgets = vec![
            widget("p0a", 0, 0, 10.0, 0.0),
            widget("p1a", 1, 0, 10.0, 0.0),
            widget("p1b", 1, 1, 60.0, 0.0),
        ];
        let order = TabOrder::new(TabPolicy::Annotations, widgets);
        assert_eq!(order.pages(), [0, 1]);

        let subset = order.subset_for_pages(&[1]);
        assert_eq!(names(&subset), ["p1a", "p1b"]);
        let nav = NavPolicy::default();
        assert_eq!(subset.next_after("p1a", &nav), Some("p1b"));
        assert_eq!(subset.next_after("p1b", &nav), Some("p1a"));
        assert!(subset.next_after("p0a", &nav).is_some());
    }

    #[test]
    fn navigator_tracks_current_field() {
        let widgets = vec![widget("a", 0, 0, 10.0, 0.0), widget("b", 0, 1, 60.0, 0.0)];
        let order = TabOrder::new(TabPolicy::Annotations, widgets);
        let mut navigator = TabNavigator::new(&order);
        assert_eq!(navigator.next(), Some("a"));
        assert_eq!(navigator.current(), Some("a"));
        assert_eq!(navigator.next(), Some("b"));
        assert_eq!(navigator.previous(), Some("a"));
        navigator.focus("b");
        assert_eq!(navigator.current(), Some("b"));
    }

    #[test]
    fn from_form_derives_read_only_flag() {
        let form = Form::from_fields([
            Field::new("open", FieldType::Text),
            Field::new("frozen", FieldType::Text).with_flags(crate::FieldFlags::READ_ONLY),
        ])
        .unwrap();
        let order = TabOrder::from_form(&form, 0, TabPolicy::Annotations);
        assert_eq!(order.len(), 2);
        assert!(!order.widgets()[0].is_read_only());
        assert!(order.widgets()[1].is_read_only());
        let nav = NavPolicy::default();
        assert_eq!(order.next_after("open", &nav), Some("open"));
    }

    #[test]
    fn empty_order_has_no_navigation() {
        let order = TabOrder::new(TabPolicy::Rows, Vec::new());
        assert!(order.is_empty());
        let nav = NavPolicy::default();
        assert_eq!(order.next_after("anything", &nav), None);
        assert_eq!(order.previous_before("anything", &nav), None);
    }
}
