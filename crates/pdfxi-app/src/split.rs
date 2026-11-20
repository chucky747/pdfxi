//! Window-free split and tile pane model for the document viewer.
//!
//! This module describes how a *single* viewer window is divided into logical
//! panes. There are no OS window calls and no `egui`, renderer, or platform
//! dependency: a pane is a [`PaneRect`] in viewport pixels plus a
//! [`PaneViewState`] (which tab it shows and its own scroll, zoom, and page).
//! Keeping the math and state machine here makes split/tile behavior
//! deterministic and unit-testable without a GUI.
//!
//! # Single-window panes
//!
//! PDFXI does not open multiple native top-level windows. A "split" or "tile"
//! layout divides the central canvas area into non-overlapping rectangles
//! separated by fixed-thickness dividers. Each pane owns an independent view
//! of a document; two panes may be bound to the same [`TabId`] (useful for
//! comparing two places in one document) or to different tabs. The host shell
//! paints each pane's content into its rectangle — this module only computes
//! the geometry and stores the view state.
//!
//! # Geometry conventions
//!
//! - A [`SplitDirection::Horizontal`] split draws a **horizontal** divider and
//!   produces a top pane (first) and a bottom pane (second). The ratio applies
//!   to the fraction of the available *height* given to the first pane.
//! - A [`SplitDirection::Vertical`] split draws a **vertical** divider and
//!   produces a left pane (first) and a right pane (second). The ratio applies
//!   to the fraction of the available *width* given to the first pane.
//! - All rectangles are half-open (`[x, x + width) × [y, y + height)`) so a
//!   point on a boundary belongs to at most one pane, which keeps hit testing
//!   deterministic.
//! - Divider thickness and minimum pane sizes are integer pixels. A split or
//!   tile whose viewport cannot honour the minimum returns a typed
//!   [`SplitError::ViewportTooSmall`] instead of producing degenerate rects.

use crate::canvas::CanvasPoint;
use crate::commands::ShortcutModifiers;
use crate::tabs::TabId;
use crate::view::Zoom;

/// Default fraction of the split axis assigned to the first pane.
pub const DEFAULT_SPLIT_RATIO: f32 = 0.5;

/// Default divider thickness in viewport pixels.
pub const DEFAULT_DIVIDER_THICKNESS: u32 = 4;

/// Default minimum size of a pane along its split axis, in viewport pixels.
pub const DEFAULT_MINIMUM_PANE: u32 = 32;

/// A point in viewport pixels (`x` right, `y` down).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PanePoint {
    /// Horizontal coordinate.
    pub x: i32,
    /// Vertical coordinate.
    pub y: i32,
}

impl PanePoint {
    /// Create a point at `(x, y)`.
    pub const fn new(x: i32, y: i32) -> Self {
        Self { x, y }
    }
}

/// An axis-aligned rectangle of viewport pixels describing a pane or divider.
///
/// Rectangles are half-open: [`Self::contains`] reports `true` for points with
/// `x` in `[x, x + width)` and `y` in `[y, y + height)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PaneRect {
    /// Left edge in viewport pixels.
    pub x: i32,
    /// Top edge in viewport pixels.
    pub y: i32,
    /// Rectangle width in pixels.
    pub width: u32,
    /// Rectangle height in pixels.
    pub height: u32,
}

impl PaneRect {
    /// Create a rectangle from its top-left corner and size.
    pub const fn new(x: i32, y: i32, width: u32, height: u32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    /// The exclusive right edge.
    pub fn right(self) -> i32 {
        self.x.saturating_add(self.width as i32)
    }

    /// The exclusive bottom edge.
    pub fn bottom(self) -> i32 {
        self.y.saturating_add(self.height as i32)
    }

    /// Whether the rectangle has zero area.
    pub fn is_empty(self) -> bool {
        self.width == 0 || self.height == 0
    }

    /// The rectangle's area in pixels.
    pub fn area(self) -> u64 {
        u64::from(self.width) * u64::from(self.height)
    }

    /// Whether `point` lies inside this half-open rectangle.
    pub fn contains(self, point: PanePoint) -> bool {
        point.x >= self.x && point.x < self.right() && point.y >= self.y && point.y < self.bottom()
    }
}

/// Orientation of a divider line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DividerAxis {
    /// A horizontal line separating a top and bottom pane.
    Horizontal,
    /// A vertical line separating a left and right pane.
    Vertical,
}

/// A divider between two panes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Divider {
    /// Which way the divider runs.
    pub axis: DividerAxis,
    /// The divider's rectangle (its thickness lies along the split axis).
    pub rect: PaneRect,
}

/// How a viewport is divided into two panes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SplitDirection {
    /// A horizontal divider: top pane first, bottom pane second.
    Horizontal,
    /// A vertical divider: left pane first, right pane second.
    Vertical,
}

impl SplitDirection {
    /// The orientation of the divider this split produces.
    pub fn divider_axis(self) -> DividerAxis {
        match self {
            Self::Horizontal => DividerAxis::Horizontal,
            Self::Vertical => DividerAxis::Vertical,
        }
    }
}

/// Errors produced by split/tile and pane operations.
#[derive(Debug, Clone, PartialEq)]
pub enum SplitError {
    /// A split ratio was not finite or was outside the inclusive `[0, 1]` range.
    InvalidRatio(f32),
    /// A tile grid had a row or column count outside the inclusive `[1, 2]` range.
    InvalidGrid {
        /// The rejected row count.
        rows: u32,
        /// The rejected column count.
        cols: u32,
    },
    /// The viewport was too small to fit the divider and minimum pane sizes.
    ViewportTooSmall {
        /// The available extent along the failing axis, in pixels.
        available: u32,
        /// The minimum extent required along that axis, in pixels.
        minimum_total: u32,
    },
    /// The operation requires at least one open tab or pane, but none exists.
    EmptyTabSet,
    /// No pane exists with the requested index.
    PaneNotFound(usize),
    /// A placement was requested for a tab that no pane is bound to.
    TabNotBound(TabId),
}

impl std::fmt::Display for SplitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidRatio(ratio) => write!(f, "invalid split ratio {ratio} (expected 0..=1)"),
            Self::InvalidGrid { rows, cols } => {
                write!(
                    f,
                    "invalid tile grid {rows}x{cols} (each dimension must be 1 or 2)"
                )
            }
            Self::ViewportTooSmall {
                available,
                minimum_total,
            } => write!(
                f,
                "viewport extent {available}px is smaller than the required {minimum_total}px"
            ),
            Self::EmptyTabSet => f.write_str("cannot arrange panes without an open document"),
            Self::PaneNotFound(index) => write!(f, "no pane at index {index}"),
            Self::TabNotBound(id) => write!(f, "no pane is bound to tab {id}"),
        }
    }
}

impl std::error::Error for SplitError {}

/// Parameters describing a two-pane split.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SplitLayout {
    /// Which way the viewport is divided.
    pub direction: SplitDirection,
    /// Fraction of the split axis assigned to the first pane, in `[0, 1]`.
    pub ratio: f32,
    /// Divider thickness in pixels.
    pub divider_thickness: u32,
    /// Minimum pane size along the split axis in pixels.
    pub minimum_pane: u32,
}

impl SplitLayout {
    /// Build a split, validating that the ratio is finite and in `[0, 1]`.
    pub fn new(
        direction: SplitDirection,
        ratio: f32,
        divider_thickness: u32,
        minimum_pane: u32,
    ) -> Result<Self, SplitError> {
        if !ratio.is_finite() || !(0.0..=1.0).contains(&ratio) {
            return Err(SplitError::InvalidRatio(ratio));
        }
        Ok(Self {
            direction,
            ratio,
            divider_thickness,
            minimum_pane,
        })
    }

    /// Build a horizontal split with the default divider and minimum sizes.
    pub fn horizontal(ratio: f32) -> Result<Self, SplitError> {
        Self::new(
            SplitDirection::Horizontal,
            ratio,
            DEFAULT_DIVIDER_THICKNESS,
            DEFAULT_MINIMUM_PANE,
        )
    }

    /// Build a vertical split with the default divider and minimum sizes.
    pub fn vertical(ratio: f32) -> Result<Self, SplitError> {
        Self::new(
            SplitDirection::Vertical,
            ratio,
            DEFAULT_DIVIDER_THICKNESS,
            DEFAULT_MINIMUM_PANE,
        )
    }

    /// Resolve this split against a viewport into two pane rects and a divider.
    ///
    /// The first pane gets `round(ratio × available)` along the split axis,
    /// clamped so both panes remain at least `minimum_pane` pixels. The divider
    /// sits between them with the configured thickness.
    pub fn geometry(self, viewport: PaneRect) -> Result<SplitGeometry, SplitError> {
        let minimum_total = self
            .minimum_pane
            .saturating_mul(2)
            .saturating_add(self.divider_thickness);
        let (first, divider, second) = match self.direction {
            SplitDirection::Horizontal => {
                if viewport.height < minimum_total {
                    return Err(SplitError::ViewportTooSmall {
                        available: viewport.height,
                        minimum_total,
                    });
                }
                let available = viewport.height - self.divider_thickness;
                let ideal = (available as f32 * self.ratio).round() as u32;
                let first_len = ideal.clamp(self.minimum_pane, available - self.minimum_pane);
                let first = PaneRect::new(viewport.x, viewport.y, viewport.width, first_len);
                let divider = Divider {
                    axis: DividerAxis::Horizontal,
                    rect: PaneRect::new(
                        viewport.x,
                        viewport.y.saturating_add(first_len as i32),
                        viewport.width,
                        self.divider_thickness,
                    ),
                };
                let second = PaneRect::new(
                    viewport.x,
                    viewport
                        .y
                        .saturating_add(first_len as i32)
                        .saturating_add(self.divider_thickness as i32),
                    viewport.width,
                    available - first_len,
                );
                (first, divider, second)
            }
            SplitDirection::Vertical => {
                if viewport.width < minimum_total {
                    return Err(SplitError::ViewportTooSmall {
                        available: viewport.width,
                        minimum_total,
                    });
                }
                let available = viewport.width - self.divider_thickness;
                let ideal = (available as f32 * self.ratio).round() as u32;
                let first_len = ideal.clamp(self.minimum_pane, available - self.minimum_pane);
                let first = PaneRect::new(viewport.x, viewport.y, first_len, viewport.height);
                let divider = Divider {
                    axis: DividerAxis::Vertical,
                    rect: PaneRect::new(
                        viewport.x.saturating_add(first_len as i32),
                        viewport.y,
                        self.divider_thickness,
                        viewport.height,
                    ),
                };
                let second = PaneRect::new(
                    viewport
                        .x
                        .saturating_add(first_len as i32)
                        .saturating_add(self.divider_thickness as i32),
                    viewport.y,
                    available - first_len,
                    viewport.height,
                );
                (first, divider, second)
            }
        };
        Ok(SplitGeometry {
            first,
            second,
            divider,
        })
    }
}

/// The concrete rectangles produced by resolving a [`SplitLayout`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SplitGeometry {
    /// The first (top or left) pane.
    pub first: PaneRect,
    /// The second (bottom or right) pane.
    pub second: PaneRect,
    /// The divider between the two panes.
    pub divider: Divider,
}

/// A row-by-column tile grid, with each dimension constrained to `1..=2`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TileGrid {
    /// Number of rows (top to bottom), either 1 or 2.
    pub rows: u32,
    /// Number of columns (left to right), either 1 or 2.
    pub cols: u32,
}

impl TileGrid {
    /// A single-pane `1x1` grid.
    pub const ONE: Self = Self { rows: 1, cols: 1 };

    /// Build a grid, rejecting dimensions outside `1..=2`.
    pub fn new(rows: u32, cols: u32) -> Result<Self, SplitError> {
        if !(1..=2).contains(&rows) || !(1..=2).contains(&cols) {
            return Err(SplitError::InvalidGrid { rows, cols });
        }
        Ok(Self { rows, cols })
    }

    /// The number of panes this grid contains.
    pub fn pane_count(self) -> usize {
        (self.rows * self.cols) as usize
    }

    /// Resolve the grid against a viewport into row-major pane rectangles.
    ///
    /// Divider thickness is subtracted between adjacent cells and the
    /// remaining extent is distributed evenly; any remainder is assigned to
    /// the earliest rows/columns so the layout is deterministic and the cells
    /// exactly tile the viewport.
    pub fn geometry(
        self,
        viewport: PaneRect,
        divider_thickness: u32,
        minimum_pane: u32,
    ) -> Result<TileLayout, SplitError> {
        let gaps_x = (self.cols - 1).saturating_mul(divider_thickness);
        let gaps_y = (self.rows - 1).saturating_mul(divider_thickness);
        let minimum_w = self
            .cols
            .saturating_mul(minimum_pane)
            .saturating_add(gaps_x);
        let minimum_h = self
            .rows
            .saturating_mul(minimum_pane)
            .saturating_add(gaps_y);
        if viewport.width < minimum_w {
            return Err(SplitError::ViewportTooSmall {
                available: viewport.width,
                minimum_total: minimum_w,
            });
        }
        if viewport.height < minimum_h {
            return Err(SplitError::ViewportTooSmall {
                available: viewport.height,
                minimum_total: minimum_h,
            });
        }
        let available_w = viewport.width - gaps_x;
        let available_h = viewport.height - gaps_y;
        let col_widths = distribute(available_w, self.cols);
        let row_heights = distribute(available_h, self.rows);

        let mut panes = Vec::with_capacity(self.pane_count());
        let mut dividers = Vec::new();
        let mut y = viewport.y;
        for (row, row_height) in row_heights.iter().copied().enumerate() {
            let mut x = viewport.x;
            for (col, col_width) in col_widths.iter().copied().enumerate() {
                panes.push(PaneRect::new(x, y, col_width, row_height));
                x = x
                    .saturating_add(col_width as i32)
                    .saturating_add(divider_thickness as i32);
                if col + 1 < self.cols as usize {
                    dividers.push(Divider {
                        axis: DividerAxis::Vertical,
                        rect: PaneRect::new(
                            x - divider_thickness as i32,
                            y,
                            divider_thickness,
                            row_height,
                        ),
                    });
                }
            }
            y = y
                .saturating_add(row_height as i32)
                .saturating_add(divider_thickness as i32);
            if row + 1 < self.rows as usize {
                dividers.push(Divider {
                    axis: DividerAxis::Horizontal,
                    rect: PaneRect::new(
                        viewport.x,
                        y - divider_thickness as i32,
                        viewport.width,
                        divider_thickness,
                    ),
                });
            }
        }
        Ok(TileLayout { panes, dividers })
    }
}

/// The concrete rectangles produced by resolving a [`TileGrid`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TileLayout {
    /// Pane rectangles in row-major order.
    pub panes: Vec<PaneRect>,
    /// Dividers separating adjacent cells.
    pub dividers: Vec<Divider>,
}

/// Distribute `total` pixels across `count` cells, giving any remainder to the
/// earliest cells. `count` is always non-zero for the grids used here.
fn distribute(total: u32, count: u32) -> Vec<u32> {
    let base = total / count;
    let remainder = total % count;
    (0..count)
        .map(|index| base + u32::from(index < remainder))
        .collect()
}

/// The view state local to one pane.
///
/// Two panes bound to the same tab still keep independent scroll, zoom, and
/// page values unless the manager is configured with [`ViewSharing::MirrorTab`].
#[derive(Debug, Clone, PartialEq)]
pub struct PaneViewState {
    tab: Option<TabId>,
    scroll: CanvasPoint,
    zoom: Zoom,
    page: usize,
}

impl PaneViewState {
    /// Create a pane state optionally bound to a tab, at the default view.
    pub fn new(tab: Option<TabId>) -> Self {
        Self {
            tab,
            scroll: CanvasPoint::default(),
            zoom: Zoom::default(),
            page: 0,
        }
    }

    /// The tab this pane is bound to, if any.
    pub fn tab(&self) -> Option<TabId> {
        self.tab
    }

    /// Bind this pane to a tab (or clear the binding with `None`).
    pub fn set_tab(&mut self, tab: Option<TabId>) {
        self.tab = tab;
    }

    /// This pane's scroll offset.
    pub fn scroll(&self) -> CanvasPoint {
        self.scroll
    }

    /// Set the scroll offset, sanitizing non-finite and negative components.
    pub fn set_scroll(&mut self, offset: CanvasPoint) {
        let sanitize = |value: f32| {
            if value.is_finite() && value > 0.0 {
                value
            } else {
                0.0
            }
        };
        self.scroll = CanvasPoint::new(sanitize(offset.x), sanitize(offset.y));
    }

    /// This pane's zoom policy.
    pub fn zoom(&self) -> Zoom {
        self.zoom
    }

    /// Set the zoom, clamping any custom scale into the supported range.
    pub fn set_zoom(&mut self, zoom: Zoom) {
        self.zoom = zoom.clamped();
    }

    /// This pane's zero-based current page.
    pub fn page(&self) -> usize {
        self.page
    }

    /// Set the current page. Call [`Self::clamp_page`] once a page count is known.
    pub fn set_page(&mut self, page: usize) {
        self.page = page;
    }

    /// Clamp the current page into the loaded page range.
    pub fn clamp_page(&mut self, page_count: usize) {
        self.page = if page_count == 0 {
            0
        } else {
            self.page.min(page_count - 1)
        };
    }
}

/// How pane view state is shared between panes bound to the same tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ViewSharing {
    /// Each pane keeps its own scroll, zoom, and page (the default).
    #[default]
    PerPane,
    /// All panes bound to the same tab mirror one shared view state.
    MirrorTab,
}

/// The layout currently displayed by a [`PaneManager`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PaneLayout {
    /// One pane covering the whole viewport.
    Single,
    /// A two-pane split.
    Split(SplitLayout),
    /// A row-by-column tile grid.
    Tile(TileGrid),
}

/// A split/tile command, independent of any GUI toolkit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaneCommand {
    /// Split horizontally (top and bottom panes).
    SplitHorizontal,
    /// Split vertically (left and right panes).
    SplitVertical,
    /// Arrange as a `1x1` grid.
    Tile1x1,
    /// Arrange as a `1x2` grid (one row, two columns).
    Tile1x2,
    /// Arrange as a `2x1` grid (two rows, one column).
    Tile2x1,
    /// Arrange as a `2x2` grid.
    Tile2x2,
    /// Focus the next pane, wrapping at the end.
    FocusNextPane,
    /// Focus the previous pane, wrapping at the start.
    FocusPreviousPane,
    /// Focus a pane by index.
    FocusPane(usize),
    /// Close the focused pane and promote a survivor.
    ClosePane,
    /// Toggle between [`ViewSharing::PerPane`] and [`ViewSharing::MirrorTab`].
    ToggleSharing,
}

impl PaneCommand {
    /// A stable, human-readable shortcut label for this command.
    pub fn shortcut_label(self) -> &'static str {
        match self {
            Self::SplitHorizontal => "Ctrl+Shift+H",
            Self::SplitVertical => "Ctrl+Shift+V",
            Self::Tile1x1 => "Ctrl+Shift+1",
            Self::Tile1x2 => "Ctrl+Shift+2",
            Self::Tile2x1 => "Ctrl+Shift+3",
            Self::Tile2x2 => "Ctrl+Shift+4",
            Self::FocusNextPane => "Ctrl+Shift+.",
            Self::FocusPreviousPane => "Ctrl+Shift+,",
            Self::FocusPane(_) => "click a pane",
            Self::ClosePane => "Ctrl+Shift+W",
            Self::ToggleSharing => "Ctrl+Shift+P",
        }
    }
}

/// Toolkit-independent key used by [`map_pane_shortcut`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaneKey {
    /// The `H` key.
    H,
    /// The `V` key.
    V,
    /// The `1` key.
    One,
    /// The `2` key.
    Two,
    /// The `3` key.
    Three,
    /// The `4` key.
    Four,
    /// The `.` (period) key, used to focus the next pane.
    Period,
    /// The `,` (comma) key, used to focus the previous pane.
    Comma,
    /// The `W` key.
    W,
    /// The `P` key.
    P,
}

/// Resolve a pane shortcut into a [`PaneCommand`].
///
/// All pane shortcuts require Ctrl+Shift and reject Alt, which keeps them
/// disjoint from the view commands (plain Ctrl) and tab commands
/// (`Ctrl+W`, `Ctrl+Tab`). Unrecognized combinations return `None` so the shell
/// can leave them to other widgets.
pub fn map_pane_shortcut(key: PaneKey, modifiers: ShortcutModifiers) -> Option<PaneCommand> {
    let ShortcutModifiers { ctrl, shift, alt } = modifiers;
    if !ctrl || !shift || alt {
        return None;
    }
    match key {
        PaneKey::H => Some(PaneCommand::SplitHorizontal),
        PaneKey::V => Some(PaneCommand::SplitVertical),
        PaneKey::One => Some(PaneCommand::Tile1x1),
        PaneKey::Two => Some(PaneCommand::Tile1x2),
        PaneKey::Three => Some(PaneCommand::Tile2x1),
        PaneKey::Four => Some(PaneCommand::Tile2x2),
        PaneKey::Period => Some(PaneCommand::FocusNextPane),
        PaneKey::Comma => Some(PaneCommand::FocusPreviousPane),
        PaneKey::W => Some(PaneCommand::ClosePane),
        PaneKey::P => Some(PaneCommand::ToggleSharing),
    }
}

/// A single-window pane manager: current layout, pane view states, and focus.
///
/// The manager is authoritative about how many panes exist. The layout describes
/// how those panes are arranged; [`Self::geometry`] truncates the layout's rects
/// to the existing pane count, so closing a tile pane can leave a vacant cell
/// rather than forcing an unrelated document to move.
#[derive(Debug, Clone, PartialEq)]
pub struct PaneManager {
    panes: Vec<PaneViewState>,
    active: usize,
    layout: PaneLayout,
    sharing: ViewSharing,
}

impl Default for PaneManager {
    fn default() -> Self {
        Self {
            panes: Vec::new(),
            active: 0,
            layout: PaneLayout::Single,
            sharing: ViewSharing::default(),
        }
    }
}

impl PaneManager {
    /// Create an empty manager with no panes and no layout.
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a manager with one pane bound to `tab`.
    pub fn single(tab: TabId) -> Self {
        Self {
            panes: vec![PaneViewState::new(Some(tab))],
            active: 0,
            layout: PaneLayout::Single,
            sharing: ViewSharing::default(),
        }
    }

    /// Create a manager whose single pane is bound to the first of `tabs`.
    ///
    /// An empty `tabs` slice is a typed [`SplitError::EmptyTabSet`].
    pub fn from_tabs(tabs: &[TabId]) -> Result<Self, SplitError> {
        let first = *tabs.first().ok_or(SplitError::EmptyTabSet)?;
        Ok(Self::single(first))
    }

    /// The number of panes currently present.
    pub fn pane_count(&self) -> usize {
        self.panes.len()
    }

    /// The focused (active) pane index.
    pub fn active_index(&self) -> usize {
        self.active
    }

    /// A reference to the focused pane, if any.
    pub fn active_pane(&self) -> Option<&PaneViewState> {
        self.panes.get(self.active)
    }

    /// A reference to a pane by index.
    pub fn pane(&self, index: usize) -> Option<&PaneViewState> {
        self.panes.get(index)
    }

    /// The current layout.
    pub fn layout(&self) -> PaneLayout {
        self.layout
    }

    /// The current view-sharing rule.
    pub fn sharing(&self) -> ViewSharing {
        self.sharing
    }

    /// Set the view-sharing rule.
    pub fn set_sharing(&mut self, sharing: ViewSharing) {
        self.sharing = sharing;
    }

    /// The distinct tabs bound to panes, in pane order.
    pub fn bound_tabs(&self) -> Vec<TabId> {
        let mut tabs = Vec::new();
        for pane in &self.panes {
            if let Some(tab) = pane.tab
                && !tabs.contains(&tab)
            {
                tabs.push(tab);
            }
        }
        tabs
    }

    /// Focus a pane by index.
    pub fn focus_pane(&mut self, index: usize) -> Result<(), SplitError> {
        if index >= self.panes.len() {
            return Err(SplitError::PaneNotFound(index));
        }
        self.active = index;
        Ok(())
    }

    /// Focus the next pane, wrapping to the first after the last.
    pub fn focus_next(&mut self) -> Result<(), SplitError> {
        if self.panes.is_empty() {
            return Err(SplitError::PaneNotFound(self.active));
        }
        self.active = (self.active + 1) % self.panes.len();
        Ok(())
    }

    /// Focus the previous pane, wrapping to the last before the first.
    pub fn focus_previous(&mut self) -> Result<(), SplitError> {
        if self.panes.is_empty() {
            return Err(SplitError::PaneNotFound(self.active));
        }
        self.active = (self.active + self.panes.len() - 1) % self.panes.len();
        Ok(())
    }

    /// Replace the layout with a two-pane split of the focused pane.
    ///
    /// The new pane is bound to the same tab as the source (when it has one)
    /// and seeded with a *copy* of the source's scroll/zoom/page; the two panes
    /// are thereafter independent under [`ViewSharing::PerPane`]. The focused
    /// pane index stays at zero. Any other panes are discarded deterministically.
    pub fn split(&mut self, direction: SplitDirection, ratio: f32) -> Result<(), SplitError> {
        if self.panes.is_empty() {
            return Err(SplitError::EmptyTabSet);
        }
        let layout = SplitLayout::new(
            direction,
            ratio,
            DEFAULT_DIVIDER_THICKNESS,
            DEFAULT_MINIMUM_PANE,
        )?;
        let source = self.panes[self.active].clone();
        let mut second = PaneViewState::new(source.tab);
        second.set_scroll(source.scroll);
        second.set_zoom(source.zoom);
        second.set_page(source.page);
        self.panes = vec![source, second];
        self.layout = PaneLayout::Split(layout);
        self.active = 0;
        Ok(())
    }

    /// Replace the layout with a `rows x cols` tile grid.
    ///
    /// Existing panes keep their local state and tab binding by index; any new
    /// panes are bound cyclically to the distinct tabs already present. An
    /// empty pane set or a set with no bound tabs is [`SplitError::EmptyTabSet`].
    pub fn tile(&mut self, rows: u32, cols: u32) -> Result<(), SplitError> {
        let grid = TileGrid::new(rows, cols)?;
        if self.panes.is_empty() {
            return Err(SplitError::EmptyTabSet);
        }
        let tabs = self.bound_tabs();
        if tabs.is_empty() {
            return Err(SplitError::EmptyTabSet);
        }
        let count = grid.pane_count();
        let old = std::mem::take(&mut self.panes);
        let mut panes = Vec::with_capacity(count);
        for index in 0..count {
            if let Some(existing) = old.get(index) {
                panes.push(existing.clone());
            } else {
                panes.push(PaneViewState::new(Some(tabs[index % tabs.len()])));
            }
        }
        self.panes = panes;
        self.layout = PaneLayout::Tile(grid);
        self.active = self.active.min(count - 1);
        Ok(())
    }

    /// Set the focused pane's scroll offset.
    ///
    /// Under [`ViewSharing::MirrorTab`] every pane bound to the same tab is
    /// updated; under [`ViewSharing::PerPane`] only the focused pane changes.
    pub fn set_scroll(&mut self, offset: CanvasPoint) -> Result<(), SplitError> {
        let active = self.active;
        let tab = self.panes.get(active).and_then(|pane| pane.tab);
        if tab.is_none() {
            return Err(SplitError::PaneNotFound(active));
        }
        for (index, pane) in self.panes.iter_mut().enumerate() {
            if index == active
                || (self.sharing == ViewSharing::MirrorTab
                    && tab.is_some_and(|t| pane.tab == Some(t)))
            {
                pane.set_scroll(offset);
            }
        }
        Ok(())
    }

    /// Set the focused pane's zoom, honouring the sharing rule.
    pub fn set_zoom(&mut self, zoom: Zoom) -> Result<(), SplitError> {
        let active = self.active;
        let tab = self.panes.get(active).and_then(|pane| pane.tab);
        if tab.is_none() {
            return Err(SplitError::PaneNotFound(active));
        }
        for (index, pane) in self.panes.iter_mut().enumerate() {
            if index == active
                || (self.sharing == ViewSharing::MirrorTab
                    && tab.is_some_and(|t| pane.tab == Some(t)))
            {
                pane.set_zoom(zoom);
            }
        }
        Ok(())
    }

    /// Set the focused pane's page, honouring the sharing rule.
    pub fn set_page(&mut self, page: usize) -> Result<(), SplitError> {
        let active = self.active;
        let tab = self.panes.get(active).and_then(|pane| pane.tab);
        if tab.is_none() {
            return Err(SplitError::PaneNotFound(active));
        }
        for (index, pane) in self.panes.iter_mut().enumerate() {
            if index == active
                || (self.sharing == ViewSharing::MirrorTab
                    && tab.is_some_and(|t| pane.tab == Some(t)))
            {
                pane.set_page(page);
            }
        }
        Ok(())
    }

    /// Bind a specific pane to a tab.
    pub fn bind_pane(&mut self, index: usize, tab: TabId) -> Result<(), SplitError> {
        self.panes
            .get_mut(index)
            .ok_or(SplitError::PaneNotFound(index))?
            .set_tab(Some(tab));
        Ok(())
    }

    /// Close the focused pane, discarding its pane-local state.
    ///
    /// The first surviving pane is promoted to focus (for a two-pane split this
    /// is the other pane). A surviving single pane reverts the layout to
    /// [`PaneLayout::Single`]. The removed state is returned to the caller, who
    /// must not write it back to the tab — closing always clears pane-local
    /// view state.
    pub fn close_pane(&mut self) -> Result<PaneViewState, SplitError> {
        if self.panes.is_empty() {
            return Err(SplitError::PaneNotFound(self.active));
        }
        let removed = self.panes.remove(self.active);
        if self.panes.is_empty() {
            self.active = 0;
            self.layout = PaneLayout::Single;
        } else {
            self.active = 0;
            if self.panes.len() == 1 {
                self.layout = PaneLayout::Single;
            }
        }
        Ok(removed)
    }

    /// Remove every pane bound to `tab`, returning the number removed.
    ///
    /// This is the deterministic tab-close cleanup path: if the focused pane
    /// is removed the first survivor is promoted, and a lone survivor reverts
    /// the layout to [`PaneLayout::Single`]. Closing a tab never writes pane
    /// view state back to the tab.
    pub fn close_tab(&mut self, tab: TabId) -> Result<usize, SplitError> {
        if self.panes.is_empty() {
            return Err(SplitError::EmptyTabSet);
        }
        if !self.bound_tabs().contains(&tab) {
            return Err(SplitError::TabNotBound(tab));
        }
        let before = self.panes.len();
        self.panes.retain(|pane| pane.tab != Some(tab));
        let removed = before - self.panes.len();
        if self.panes.is_empty() {
            self.active = 0;
            self.layout = PaneLayout::Single;
        } else {
            if self.active >= self.panes.len() {
                self.active = 0;
            }
            if self.panes.len() == 1 {
                self.layout = PaneLayout::Single;
            }
        }
        Ok(removed)
    }

    /// Resolve the current layout into one rectangle per existing pane.
    ///
    /// Returns an empty vector when there are no panes. A viewport that is too
    /// small for the layout's dividers and minimum sizes produces a typed
    /// [`SplitError::ViewportTooSmall`].
    pub fn geometry(&self, viewport: PaneRect) -> Result<Vec<PaneRect>, SplitError> {
        if self.panes.is_empty() {
            return Ok(Vec::new());
        }
        let mut rects = match self.layout {
            PaneLayout::Single => vec![viewport],
            PaneLayout::Split(layout) => {
                let split = layout.geometry(viewport)?;
                vec![split.first, split.second]
            }
            PaneLayout::Tile(grid) => {
                grid.geometry(viewport, DEFAULT_DIVIDER_THICKNESS, DEFAULT_MINIMUM_PANE)?
                    .panes
            }
        };
        rects.truncate(self.panes.len().max(1));
        Ok(rects)
    }

    /// Resolve the current layout's dividers against a viewport.
    pub fn dividers(&self, viewport: PaneRect) -> Result<Vec<Divider>, SplitError> {
        match self.layout {
            PaneLayout::Single => Ok(Vec::new()),
            PaneLayout::Split(layout) => Ok(vec![layout.geometry(viewport)?.divider]),
            PaneLayout::Tile(grid) => Ok(grid
                .geometry(viewport, DEFAULT_DIVIDER_THICKNESS, DEFAULT_MINIMUM_PANE)?
                .dividers),
        }
    }

    /// Apply a typed [`PaneCommand`] to this manager.
    pub fn apply_command(&mut self, command: PaneCommand) -> Result<(), SplitError> {
        match command {
            PaneCommand::SplitHorizontal => {
                self.split(SplitDirection::Horizontal, DEFAULT_SPLIT_RATIO)
            }
            PaneCommand::SplitVertical => self.split(SplitDirection::Vertical, DEFAULT_SPLIT_RATIO),
            PaneCommand::Tile1x1 => self.tile(1, 1),
            PaneCommand::Tile1x2 => self.tile(1, 2),
            PaneCommand::Tile2x1 => self.tile(2, 1),
            PaneCommand::Tile2x2 => self.tile(2, 2),
            PaneCommand::FocusNextPane => self.focus_next(),
            PaneCommand::FocusPreviousPane => self.focus_previous(),
            PaneCommand::FocusPane(index) => self.focus_pane(index),
            PaneCommand::ClosePane => self.close_pane().map(|_| ()),
            PaneCommand::ToggleSharing => {
                self.set_sharing(match self.sharing {
                    ViewSharing::PerPane => ViewSharing::MirrorTab,
                    ViewSharing::MirrorTab => ViewSharing::PerPane,
                });
                Ok(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tabs::TabId;

    fn tab(n: u64) -> TabId {
        TabId::from_u64(n)
    }

    fn viewport(width: u32, height: u32) -> PaneRect {
        PaneRect::new(0, 0, width, height)
    }

    // ---- geometry: horizontal / vertical split ---------------------------

    #[test]
    fn horizontal_split_produces_top_bottom_panes_and_divider() {
        let layout = SplitLayout::horizontal(0.5).expect("valid ratio");
        let geometry = layout.geometry(viewport(100, 200)).expect("roomy viewport");
        // available height = 196; 0.5 -> 98, clamped stays 98.
        assert_eq!(geometry.first, PaneRect::new(0, 0, 100, 98));
        assert_eq!(
            geometry.divider,
            Divider {
                axis: DividerAxis::Horizontal,
                rect: PaneRect::new(0, 98, 100, 4),
            }
        );
        assert_eq!(geometry.second, PaneRect::new(0, 102, 100, 98));
        assert_eq!(
            geometry.divider.axis,
            SplitDirection::Horizontal.divider_axis()
        );
        // A point on the divider line is not in either pane.
        assert!(!geometry.first.contains(PanePoint::new(10, 98)));
        assert!(!geometry.second.contains(PanePoint::new(10, 101)));
    }

    #[test]
    fn vertical_split_produces_left_right_panes_and_divider() {
        let layout = SplitLayout::vertical(0.25).expect("valid ratio");
        let geometry = layout.geometry(viewport(400, 100)).expect("roomy viewport");
        // available width = 396; 0.25 -> 99.
        assert_eq!(geometry.first, PaneRect::new(0, 0, 99, 100));
        assert_eq!(
            geometry.divider,
            Divider {
                axis: DividerAxis::Vertical,
                rect: PaneRect::new(99, 0, 4, 100),
            }
        );
        assert_eq!(geometry.second, PaneRect::new(103, 0, 297, 100));
        assert_eq!(
            geometry.divider.axis,
            SplitDirection::Vertical.divider_axis()
        );
        assert!(geometry.first.contains(PanePoint::new(0, 0)));
        assert!(geometry.second.contains(PanePoint::new(399, 99)));
        assert!(!geometry.second.contains(PanePoint::new(400, 0)));
    }

    #[test]
    fn split_geometry_panes_and_divider_tile_the_viewport() {
        for direction in [SplitDirection::Horizontal, SplitDirection::Vertical] {
            let layout = SplitLayout::new(direction, 0.37, 6, 10).expect("valid");
            let geometry = layout.geometry(viewport(320, 240)).expect("roomy");
            let covered = geometry.first.area()
                + geometry.second.area()
                + u64::from(geometry.divider.rect.width) * u64::from(geometry.divider.rect.height);
            assert_eq!(covered, 320 * 240, "no gaps or overlaps for {direction:?}");
        }
    }

    // ---- minimum-size clamping and viewport errors ------------------------

    #[test]
    fn extreme_ratios_clamp_to_the_minimum_pane_size() {
        let tiny = SplitLayout::new(SplitDirection::Vertical, 0.0, 4, 30).expect("valid");
        let geometry = tiny.geometry(viewport(200, 100)).expect("roomy");
        assert_eq!(geometry.first.width, 30);
        assert_eq!(geometry.second.width, 200 - 4 - 30);

        let huge = SplitLayout::new(SplitDirection::Vertical, 1.0, 4, 30).expect("valid");
        let geometry = huge.geometry(viewport(200, 100)).expect("roomy");
        assert_eq!(geometry.first.width, 200 - 4 - 30);
        assert_eq!(geometry.second.width, 30);
    }

    #[test]
    fn too_small_viewport_is_a_typed_error() {
        let layout = SplitLayout::new(SplitDirection::Horizontal, 0.5, 4, 32).expect("valid");
        // Needs 2*32 + 4 = 68px of height; 60 is too small.
        assert_eq!(
            layout.geometry(viewport(100, 60)),
            Err(SplitError::ViewportTooSmall {
                available: 60,
                minimum_total: 68,
            })
        );
    }

    // ---- invalid input ----------------------------------------------------

    #[test]
    fn invalid_split_ratios_are_rejected() {
        for ratio in [f32::INFINITY, f32::NEG_INFINITY, -0.1, 1.1] {
            assert_eq!(
                SplitLayout::horizontal(ratio).err(),
                Some(SplitError::InvalidRatio(ratio)),
                "ratio {ratio} must be rejected"
            );
        }
        // NaN is not equal to itself, so assert on the variant instead.
        assert!(matches!(
            SplitLayout::horizontal(f32::NAN),
            Err(SplitError::InvalidRatio(value)) if value.is_nan()
        ));
        assert!(SplitLayout::horizontal(0.0).is_ok());
        assert!(SplitLayout::horizontal(1.0).is_ok());
    }

    #[test]
    fn invalid_tile_grids_are_rejected() {
        for (rows, cols) in [(0, 1), (1, 0), (3, 1), (2, 3), (0, 0)] {
            assert_eq!(
                TileGrid::new(rows, cols).err(),
                Some(SplitError::InvalidGrid { rows, cols })
            );
        }
        assert!(TileGrid::new(1, 1).is_ok());
        assert!(TileGrid::new(2, 2).is_ok());
        assert_eq!(TileGrid::new(2, 2).expect("valid").pane_count(), 4);
    }

    // ---- tile grid layouts ------------------------------------------------

    #[test]
    fn tile_grid_one_by_two_splits_the_width_evenly() {
        let tile = TileGrid::new(1, 2).expect("valid");
        let layout = tile
            .geometry(viewport(204, 100), 4, 32)
            .expect("roomy viewport");
        assert_eq!(
            layout.panes,
            vec![
                PaneRect::new(0, 0, 100, 100),
                PaneRect::new(104, 0, 100, 100)
            ]
        );
        assert_eq!(
            layout.dividers,
            vec![Divider {
                axis: DividerAxis::Vertical,
                rect: PaneRect::new(100, 0, 4, 100),
            }]
        );
    }

    #[test]
    fn tile_grid_two_by_one_splits_the_height_evenly() {
        let tile = TileGrid::new(2, 1).expect("valid");
        let layout = tile
            .geometry(viewport(100, 204), 4, 32)
            .expect("roomy viewport");
        assert_eq!(
            layout.panes,
            vec![
                PaneRect::new(0, 0, 100, 100),
                PaneRect::new(0, 104, 100, 100)
            ]
        );
        assert_eq!(layout.dividers[0].axis, DividerAxis::Horizontal);
    }

    #[test]
    fn tile_grid_two_by_two_is_row_major_and_deterministic() {
        let tile = TileGrid::new(2, 2).expect("valid");
        let layout = tile
            .geometry(viewport(204, 204), 4, 32)
            .expect("roomy viewport");
        assert_eq!(
            layout.panes,
            vec![
                PaneRect::new(0, 0, 100, 100),
                PaneRect::new(104, 0, 100, 100),
                PaneRect::new(0, 104, 100, 100),
                PaneRect::new(104, 104, 100, 100),
            ]
        );
        // One vertical divider per row plus one full-width horizontal divider.
        assert_eq!(layout.dividers.len(), 3);
        assert_eq!(
            layout
                .dividers
                .iter()
                .filter(|divider| divider.axis == DividerAxis::Vertical)
                .count(),
            2
        );
        assert_eq!(
            layout
                .dividers
                .iter()
                .filter(|divider| divider.axis == DividerAxis::Horizontal)
                .count(),
            1
        );
        assert_eq!(
            layout,
            tile.geometry(viewport(204, 204), 4, 32).expect("repeat")
        );
        // No gaps or overlaps.
        let covered: u64 = layout.panes.iter().map(|pane| pane.area()).sum::<u64>()
            + layout
                .dividers
                .iter()
                .map(|d| u64::from(d.rect.width) * u64::from(d.rect.height))
                .sum::<u64>();
        assert_eq!(covered, 204 * 204);
    }

    #[test]
    fn tile_grid_minimum_size_and_viewport_error() {
        let tile = TileGrid::new(2, 2).expect("valid");
        // Needs 2*32 + 4 = 68 in each axis.
        assert_eq!(
            tile.geometry(viewport(100, 60), 4, 32),
            Err(SplitError::ViewportTooSmall {
                available: 60,
                minimum_total: 68,
            })
        );
        let layout = tile
            .geometry(viewport(140, 140), 4, 32)
            .expect("roomy viewport");
        assert!(
            layout
                .panes
                .iter()
                .all(|pane| pane.width >= 32 && pane.height >= 32)
        );
    }

    // ---- per-pane state, sharing, and focus --------------------------------

    #[test]
    fn per_pane_scroll_and_zoom_are_isolated() {
        let mut manager = PaneManager::single(tab(1));
        assert_eq!(manager.split(SplitDirection::Horizontal, 0.5), Ok(()));
        assert_eq!(manager.pane_count(), 2);
        // The second pane is seeded from the first *at split time* (default
        // scroll), then each pane evolves independently under PerPane sharing.
        assert_eq!(
            manager.pane(1).expect("second").scroll(),
            CanvasPoint::new(0.0, 0.0)
        );
        manager
            .set_scroll(CanvasPoint::new(10.0, 20.0))
            .expect("active pane");
        assert_eq!(
            manager.pane(0).expect("first").scroll(),
            CanvasPoint::new(10.0, 20.0)
        );
        assert_eq!(
            manager.pane(1).expect("second").scroll(),
            CanvasPoint::new(0.0, 0.0)
        );

        manager.focus_pane(1).expect("second pane exists");
        manager
            .set_scroll(CanvasPoint::new(99.0, 88.0))
            .expect("active pane");
        manager.set_zoom(Zoom::custom(2.0)).expect("active pane");
        assert_eq!(
            manager.pane(0).expect("first").scroll(),
            CanvasPoint::new(10.0, 20.0)
        );
        assert_eq!(
            manager.pane(1).expect("second").scroll(),
            CanvasPoint::new(99.0, 88.0)
        );
        assert_eq!(manager.pane(0).expect("first").zoom(), Zoom::default());
        assert_eq!(manager.pane(1).expect("second").zoom().scale(), Some(2.0));
    }

    #[test]
    fn mirror_tab_shares_view_state_across_panes_on_the_same_tab() {
        let mut manager = PaneManager::single(tab(7));
        assert_eq!(manager.split(SplitDirection::Vertical, 0.5), Ok(()));
        assert_eq!(manager.bound_tabs(), vec![tab(7)]);
        manager.set_sharing(ViewSharing::MirrorTab);

        manager.focus_pane(1).expect("second pane exists");
        manager.set_zoom(Zoom::custom(1.75)).expect("active pane");
        manager.set_page(5).expect("active pane");
        assert_eq!(manager.pane(0).expect("first").zoom().scale(), Some(1.75));
        assert_eq!(manager.pane(0).expect("first").page(), 5);
        assert_eq!(manager.pane(1).expect("second").zoom().scale(), Some(1.75));
        assert_eq!(manager.pane(1).expect("second").page(), 5);
    }

    #[test]
    fn panes_bound_to_different_tabs_do_not_share_even_in_mirror_mode() {
        let mut manager = PaneManager::single(tab(1));
        assert_eq!(manager.split(SplitDirection::Vertical, 0.5), Ok(()));
        assert_eq!(manager.bind_pane(1, tab(2)), Ok(()));
        assert_eq!(manager.bound_tabs(), vec![tab(1), tab(2)]);
        manager.set_sharing(ViewSharing::MirrorTab);
        manager.set_zoom(Zoom::custom(3.0)).expect("active pane");
        assert_eq!(manager.pane(0).expect("first").zoom().scale(), Some(3.0));
        assert_eq!(manager.pane(1).expect("second").zoom(), Zoom::default());
    }

    #[test]
    fn focus_next_and_previous_wrap_and_invalid_focus_is_typed() {
        let mut manager = PaneManager::single(tab(1));
        assert_eq!(manager.split(SplitDirection::Horizontal, 0.5), Ok(()));
        assert_eq!(manager.active_index(), 0);
        assert_eq!(manager.focus_next(), Ok(()));
        assert_eq!(manager.active_index(), 1);
        assert_eq!(manager.focus_next(), Ok(()));
        assert_eq!(manager.active_index(), 0);
        assert_eq!(manager.focus_previous(), Ok(()));
        assert_eq!(manager.active_index(), 1);
        assert_eq!(manager.focus_pane(2), Err(SplitError::PaneNotFound(2)));
        assert_eq!(
            manager.active_index(),
            1,
            "failed focus leaves focus unchanged"
        );
    }

    #[test]
    fn pane_view_state_clamps_scroll_and_page() {
        let mut pane = PaneViewState::new(Some(tab(1)));
        pane.set_scroll(CanvasPoint::new(f32::NAN, -5.0));
        assert_eq!(pane.scroll(), CanvasPoint::new(0.0, 0.0));
        pane.set_scroll(CanvasPoint::new(12.5, 30.0));
        assert_eq!(pane.scroll(), CanvasPoint::new(12.5, 30.0));
        pane.set_zoom(Zoom::custom(100.0));
        assert_eq!(pane.zoom().scale(), Some(crate::view::MAX_ZOOM));
        pane.set_page(9);
        pane.clamp_page(4);
        assert_eq!(pane.page(), 3);
        pane.clamp_page(0);
        assert_eq!(pane.page(), 0);
    }

    // ---- close-pane promotion and tab-close cleanup ------------------------

    #[test]
    fn closing_a_pane_promotes_the_survivor_and_discards_local_state() {
        let mut manager = PaneManager::single(tab(1));
        assert_eq!(manager.split(SplitDirection::Horizontal, 0.5), Ok(()));
        manager.focus_pane(1).expect("second pane exists");
        manager
            .set_scroll(CanvasPoint::new(77.0, 88.0))
            .expect("active pane");

        let removed = manager.close_pane().expect("a pane exists");
        assert_eq!(removed.scroll(), CanvasPoint::new(77.0, 88.0));
        assert_eq!(manager.pane_count(), 1);
        assert_eq!(manager.active_index(), 0, "survivor is promoted");
        assert_eq!(manager.layout(), PaneLayout::Single);
        assert_eq!(
            manager.active_pane().expect("survivor").scroll(),
            CanvasPoint::new(0.0, 0.0)
        );
    }

    #[test]
    fn tab_close_removes_all_its_panes_deterministically() {
        let mut manager = PaneManager::single(tab(1));
        assert_eq!(manager.split(SplitDirection::Vertical, 0.5), Ok(()));
        assert_eq!(manager.bind_pane(1, tab(2)), Ok(()));
        assert_eq!(manager.focus_pane(1), Ok(()));

        assert_eq!(manager.close_tab(tab(2)), Ok(1));
        assert_eq!(manager.pane_count(), 1);
        assert_eq!(manager.active_index(), 0, "focus promoted to survivor");
        assert_eq!(manager.layout(), PaneLayout::Single);
        assert_eq!(manager.pane(0).and_then(|p| p.tab), Some(tab(1)));

        // Closing the last tab empties the manager; further edits are typed errors.
        assert_eq!(manager.close_tab(tab(1)), Ok(1));
        assert_eq!(manager.pane_count(), 0);
        assert_eq!(manager.active_pane(), None);
        assert_eq!(
            manager.set_zoom(Zoom::FitPage),
            Err(SplitError::PaneNotFound(0))
        );
        assert_eq!(manager.close_tab(tab(1)), Err(SplitError::EmptyTabSet));
    }

    #[test]
    fn empty_tab_set_is_a_typed_error() {
        assert_eq!(PaneManager::from_tabs(&[]), Err(SplitError::EmptyTabSet));
        let mut empty = PaneManager::new();
        assert_eq!(
            empty.split(SplitDirection::Horizontal, 0.5),
            Err(SplitError::EmptyTabSet)
        );
        assert_eq!(empty.tile(2, 2), Err(SplitError::EmptyTabSet));
        assert_eq!(empty.geometry(viewport(100, 100)), Ok(Vec::new()));
    }

    // ---- determinism and commands -----------------------------------------

    #[test]
    fn layouts_and_managers_are_deterministic() {
        let mut a = PaneManager::single(tab(1));
        let mut b = PaneManager::single(tab(1));
        for manager in [&mut a, &mut b] {
            assert_eq!(manager.split(SplitDirection::Horizontal, 0.4), Ok(()));
            assert_eq!(manager.focus_pane(1), Ok(()));
            manager.set_zoom(Zoom::custom(1.5)).expect("active pane");
            manager
                .set_scroll(CanvasPoint::new(5.0, 9.0))
                .expect("active pane");
        }
        assert_eq!(
            a, b,
            "identical operation sequences yield identical managers"
        );
        assert_eq!(
            a.geometry(viewport(200, 200)),
            b.geometry(viewport(200, 200))
        );
    }

    #[test]
    fn tile_command_rebinds_new_panes_to_distinct_tabs() {
        let mut manager = PaneManager::single(tab(1));
        assert_eq!(manager.split(SplitDirection::Vertical, 0.5), Ok(()));
        assert_eq!(manager.bind_pane(1, tab(2)), Ok(()));
        assert_eq!(manager.apply_command(PaneCommand::Tile2x2), Ok(()));
        assert_eq!(
            manager.layout(),
            PaneLayout::Tile(TileGrid::new(2, 2).expect("valid"))
        );
        assert_eq!(manager.pane_count(), 4);
        assert_eq!(manager.bound_tabs(), vec![tab(1), tab(2)]);
        assert_eq!(manager.pane(2).and_then(|p| p.tab), Some(tab(1)));
        assert_eq!(manager.pane(3).and_then(|p| p.tab), Some(tab(2)));
        assert_eq!(
            manager.geometry(viewport(204, 204)).expect("roomy").len(),
            4
        );
    }

    #[test]
    fn shortcut_map_and_labels_are_stable() {
        let mods = |ctrl: bool, shift: bool, alt: bool| ShortcutModifiers { ctrl, shift, alt };
        let cs = mods(true, true, false);
        assert_eq!(
            map_pane_shortcut(PaneKey::H, cs),
            Some(PaneCommand::SplitHorizontal)
        );
        assert_eq!(
            map_pane_shortcut(PaneKey::V, cs),
            Some(PaneCommand::SplitVertical)
        );
        assert_eq!(
            map_pane_shortcut(PaneKey::One, cs),
            Some(PaneCommand::Tile1x1)
        );
        assert_eq!(
            map_pane_shortcut(PaneKey::Two, cs),
            Some(PaneCommand::Tile1x2)
        );
        assert_eq!(
            map_pane_shortcut(PaneKey::Three, cs),
            Some(PaneCommand::Tile2x1)
        );
        assert_eq!(
            map_pane_shortcut(PaneKey::Four, cs),
            Some(PaneCommand::Tile2x2)
        );
        assert_eq!(
            map_pane_shortcut(PaneKey::Period, cs),
            Some(PaneCommand::FocusNextPane)
        );
        assert_eq!(
            map_pane_shortcut(PaneKey::Comma, cs),
            Some(PaneCommand::FocusPreviousPane)
        );
        assert_eq!(
            map_pane_shortcut(PaneKey::W, cs),
            Some(PaneCommand::ClosePane)
        );
        assert_eq!(
            map_pane_shortcut(PaneKey::P, cs),
            Some(PaneCommand::ToggleSharing)
        );
        // Pane shortcuts are disjoint from view (Ctrl) and tab (Ctrl+W/Tab) paths.
        assert_eq!(
            map_pane_shortcut(PaneKey::H, mods(false, true, false)),
            None
        );
        assert_eq!(
            map_pane_shortcut(PaneKey::H, mods(true, false, false)),
            None
        );
        assert_eq!(map_pane_shortcut(PaneKey::H, mods(true, true, true)), None);
        for command in [
            PaneCommand::SplitHorizontal,
            PaneCommand::SplitVertical,
            PaneCommand::Tile1x1,
            PaneCommand::Tile2x2,
            PaneCommand::FocusNextPane,
            PaneCommand::ClosePane,
            PaneCommand::ToggleSharing,
        ] {
            assert!(!command.shortcut_label().is_empty());
        }
    }
}
