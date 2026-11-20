//! Keyboard navigation, focus order, high-contrast tokens, and reflow metrics.
//!
//! This module is the window-free model behind the viewer's accessibility
//! navigation. It deliberately depends on no GUI toolkit: [`FocusOrder`] models a
//! deterministic Tab ring over named regions, [`map_nav_shortcut`] resolves
//! accessibility shortcuts without an `egui::Key`, [`ThemeTokens`] and
//! [`tokens`] provide testable color roles (including a WCAG high-contrast
//! palette), and [`reflow_metrics`] computes deterministic reflow layout numbers
//! from caller-supplied page and viewport sizes. A host translates these values
//! into native focus calls, widget colors, and line breaking.

use std::fmt;

// ---------------------------------------------------------------------------
// Focus order
// ---------------------------------------------------------------------------

/// A focusable region of the viewer shell.
///
/// The variant order in [`FocusOrder::default_order`] is the canonical reading
/// order; it is fixed here so focus traversal is deterministic and testable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FocusTarget {
    /// The application menu bar.
    MenuBar,
    /// The main toolbar.
    Toolbar,
    /// The open-document tab strip.
    TabStrip,
    /// The page thumbnail pane.
    ThumbnailPane,
    /// The central PDF page canvas.
    PageCanvas,
    /// The in-document find bar.
    FindBar,
    /// The optional-content layers panel.
    LayersPanel,
    /// The outline/bookmarks panel.
    BookmarksPanel,
    /// The embedded attachments panel.
    AttachmentsPanel,
    /// The link annotations panel.
    LinksPanel,
    /// The bottom status bar.
    StatusBar,
}

impl FocusTarget {
    /// A stable, human-readable label suitable for a screen-reader tooltip.
    pub fn label(self) -> &'static str {
        match self {
            Self::MenuBar => "Menu bar",
            Self::Toolbar => "Toolbar",
            Self::TabStrip => "Document tabs",
            Self::ThumbnailPane => "Page thumbnails",
            Self::PageCanvas => "PDF page",
            Self::FindBar => "Find bar",
            Self::LayersPanel => "Layers panel",
            Self::BookmarksPanel => "Bookmarks panel",
            Self::AttachmentsPanel => "Attachments panel",
            Self::LinksPanel => "Links panel",
            Self::StatusBar => "Status bar",
        }
    }
}

impl fmt::Display for FocusTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// One entry in a [`FocusOrder`]: a target plus its current availability.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Focusable {
    /// The region this entry represents.
    pub target: FocusTarget,
    /// Whether the region currently participates in Tab traversal.
    pub enabled: bool,
}

impl Focusable {
    /// Create a focusable entry.
    pub fn new(target: FocusTarget, enabled: bool) -> Self {
        Self { target, enabled }
    }

    /// Create an always-enabled entry.
    pub fn enabled(target: FocusTarget) -> Self {
        Self::new(target, true)
    }
}

/// A deterministic Tab ring over focusable regions.
///
/// Traversal skips disabled entries and wraps at both ends. The currently
/// focused index is always a valid enabled entry after construction or a
/// successful focus mutation; if no entry is enabled, navigation is a no-op
/// that returns `None`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FocusOrder {
    entries: Vec<Focusable>,
    focus: usize,
}

impl FocusOrder {
    /// Build a focus ring from explicit entries.
    ///
    /// The initial focus is the first enabled entry, or index `0` when none are
    /// enabled (in which case [`Self::focused`] is `None`).
    pub fn new(entries: Vec<Focusable>) -> Self {
        let focus = entries.iter().position(|entry| entry.enabled).unwrap_or(0);
        Self { entries, focus }
    }

    /// Build a ring where every supplied target is enabled, in the given order.
    pub fn from_targets(targets: &[FocusTarget]) -> Self {
        Self::new(targets.iter().copied().map(Focusable::enabled).collect())
    }

    /// The canonical viewer focus order, with all regions enabled.
    pub fn default_order() -> Self {
        Self::from_targets(&[
            FocusTarget::MenuBar,
            FocusTarget::Toolbar,
            FocusTarget::TabStrip,
            FocusTarget::ThumbnailPane,
            FocusTarget::PageCanvas,
            FocusTarget::FindBar,
            FocusTarget::LayersPanel,
            FocusTarget::BookmarksPanel,
            FocusTarget::AttachmentsPanel,
            FocusTarget::LinksPanel,
            FocusTarget::StatusBar,
        ])
    }

    /// Number of entries, including disabled ones.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the ring has no entries at all.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// All entries in reading order.
    pub fn entries(&self) -> &[Focusable] {
        &self.entries
    }

    /// The index of the currently focused entry.
    pub fn focus_index(&self) -> usize {
        self.focus
    }

    /// The currently focused target, or `None` if the ring is empty or fully
    /// disabled.
    pub fn focused(&self) -> Option<FocusTarget> {
        self.entries
            .get(self.focus)
            .filter(|entry| entry.enabled)
            .map(|entry| entry.target)
    }

    /// Move focus to `target`, reporting whether the move succeeded.
    ///
    /// A disabled or absent target is rejected and leaves focus unchanged.
    pub fn set_focused(&mut self, target: FocusTarget) -> bool {
        if let Some(index) = self
            .entries
            .iter()
            .position(|e| e.target == target && e.enabled)
        {
            self.focus = index;
            true
        } else {
            false
        }
    }

    /// Move focus to the first enabled entry and return it.
    pub fn focus_first(&mut self) -> Option<FocusTarget> {
        let index = self.entries.iter().position(|entry| entry.enabled)?;
        self.focus = index;
        Some(self.entries[index].target)
    }

    /// Move focus to the last enabled entry and return it.
    pub fn focus_last(&mut self) -> Option<FocusTarget> {
        let index = self.entries.iter().rposition(|entry| entry.enabled)?;
        self.focus = index;
        Some(self.entries[index].target)
    }

    /// Move focus one enabled entry forward (wrapping) and return the new target.
    #[allow(clippy::should_implement_trait)] // focus ring step, not `Iterator::next`
    pub fn next(&mut self) -> Option<FocusTarget> {
        self.step(1)
    }

    /// Move focus one enabled entry backward (wrapping) and return the new target.
    pub fn previous(&mut self) -> Option<FocusTarget> {
        self.step(-1)
    }

    /// Tab traversal: `forward` selects Tab, otherwise Shift+Tab.
    pub fn tab(&mut self, forward: bool) -> Option<FocusTarget> {
        if forward {
            self.next()
        } else {
            self.previous()
        }
    }

    /// Enabled targets in reading order (the order a screen reader announces).
    pub fn focusable_targets(&self) -> Vec<FocusTarget> {
        self.entries
            .iter()
            .filter(|entry| entry.enabled)
            .map(|entry| entry.target)
            .collect()
    }

    fn step(&mut self, direction: isize) -> Option<FocusTarget> {
        let len = self.entries.len();
        if len == 0 || !self.entries.iter().any(|entry| entry.enabled) {
            return None;
        }
        let start = self.focus as isize;
        for offset in 1..=len {
            let index = (start + direction * offset as isize).rem_euclid(len as isize) as usize;
            if self.entries[index].enabled {
                self.focus = index;
                return Some(self.entries[index].target);
            }
        }
        None
    }
}

// ---------------------------------------------------------------------------
// Accessibility navigation shortcuts
// ---------------------------------------------------------------------------

/// A navigation command with a keyboard path in the viewer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NavCommand {
    /// Move focus to the next pane.
    NextPane,
    /// Move focus to the previous pane.
    PreviousPane,
    /// Focus the page canvas.
    FocusCanvas,
    /// Open and focus the find bar.
    FocusFind,
    /// Toggle the high-contrast theme.
    ToggleHighContrast,
    /// Toggle read-aloud playback.
    ReadAloud,
}

impl NavCommand {
    /// A stable shortcut label for display in menus and tooltips.
    pub fn shortcut_label(self) -> &'static str {
        match self {
            Self::NextPane => "F6",
            Self::PreviousPane => "Shift+F6",
            Self::FocusCanvas => "F3",
            Self::FocusFind => "F4",
            Self::ToggleHighContrast => "Ctrl+Shift+H",
            Self::ReadAloud => "F5",
        }
    }
}

/// A toolkit-independent key for an accessibility navigation shortcut.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NavKey {
    /// The `F3` key.
    F3,
    /// The `F4` key.
    F4,
    /// The `F5` key.
    F5,
    /// The `F6` key.
    F6,
    /// The `H` key.
    H,
}

/// Modifier keys held with a [`NavKey`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct NavModifiers {
    /// Control is held.
    pub ctrl: bool,
    /// Shift is held.
    pub shift: bool,
    /// Alt is held.
    pub alt: bool,
}

/// Resolve an accessibility navigation shortcut.
///
/// Function-key shortcuts ignore Ctrl but reject Alt; Shift disambiguates
/// previous-pane from next-pane. The high-contrast toggle is the sole
/// letter-key shortcut and requires Ctrl+Shift. Unrecognized combinations
/// return `None` so the host can leave them to focused widgets.
pub fn map_nav_shortcut(key: NavKey, modifiers: NavModifiers) -> Option<NavCommand> {
    let NavModifiers { ctrl, shift, alt } = modifiers;
    if alt {
        return None;
    }
    match key {
        NavKey::H if ctrl && shift => Some(NavCommand::ToggleHighContrast),
        NavKey::H => None,
        NavKey::F3 if !ctrl && !shift => Some(NavCommand::FocusCanvas),
        NavKey::F4 if !ctrl && !shift => Some(NavCommand::FocusFind),
        NavKey::F5 if !ctrl && !shift => Some(NavCommand::ReadAloud),
        NavKey::F6 if !ctrl => Some(if shift {
            NavCommand::PreviousPane
        } else {
            NavCommand::NextPane
        }),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// High-contrast theme tokens
// ---------------------------------------------------------------------------

/// An opaque 8-bit RGB display color.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb {
    /// Red channel.
    pub r: u8,
    /// Green channel.
    pub g: u8,
    /// Blue channel.
    pub b: u8,
}

impl Rgb {
    /// Create a color from its channels.
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }

    /// WCAG relative luminance in `0.0..=1.0`.
    pub fn luminance(self) -> f32 {
        fn channel(value: u8) -> f32 {
            let c = f32::from(value) / 255.0;
            if c <= 0.039_28 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        }
        0.2126 * channel(self.r) + 0.7152 * channel(self.g) + 0.0722 * channel(self.b)
    }
}

/// WCAG contrast ratio between two colors, in `1.0..=21.0`.
pub fn contrast_ratio(a: Rgb, b: Rgb) -> f32 {
    let (lighter, darker) = if a.luminance() >= b.luminance() {
        (a.luminance(), b.luminance())
    } else {
        (b.luminance(), a.luminance())
    };
    (lighter + 0.05) / (darker + 0.05)
}

/// Minimum WCAG AA contrast ratio for normal text.
pub const MIN_CONTRAST_RATIO: f32 = 4.5;
/// Contrast ratio the high-contrast palette targets (WCAG AAA).
pub const HIGH_CONTRAST_TARGET: f32 = 7.0;

/// Semantic color roles used by the viewer chrome and canvas.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThemeTokens {
    /// Window and panel background.
    pub background: Rgb,
    /// Primary text color.
    pub foreground: Rgb,
    /// Widget borders and separators.
    pub border: Rgb,
    /// Keyboard focus indicator.
    pub focus_ring: Rgb,
    /// Selected-row background.
    pub selection_background: Rgb,
    /// Selected-row foreground.
    pub selection_foreground: Rgb,
    /// Unvisited hyperlink color.
    pub link: Rgb,
    /// Visited hyperlink color.
    pub visited_link: Rgb,
    /// Disabled control color.
    pub disabled: Rgb,
    /// PDF canvas background.
    pub canvas_background: Rgb,
}

/// A named viewer theme.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Theme {
    /// The default light theme.
    Standard,
    /// The high-contrast (black background) theme.
    HighContrast,
}

/// Return the color tokens for `theme`.
///
/// Both palettes keep primary text and selection pairs at WCAG AA or better;
/// the high-contrast palette targets AAA (`>= 7:1`) using only pure and near-
/// pure hues so the mapping remains deterministic across platforms.
pub fn tokens(theme: Theme) -> ThemeTokens {
    match theme {
        Theme::Standard => ThemeTokens {
            background: Rgb::new(250, 250, 248),
            foreground: Rgb::new(28, 28, 30),
            border: Rgb::new(120, 124, 130),
            focus_ring: Rgb::new(0, 84, 168),
            selection_background: Rgb::new(198, 220, 250),
            selection_foreground: Rgb::new(16, 24, 40),
            link: Rgb::new(0, 78, 168),
            visited_link: Rgb::new(104, 32, 140),
            disabled: Rgb::new(112, 114, 118),
            canvas_background: Rgb::new(236, 236, 234),
        },
        Theme::HighContrast => ThemeTokens {
            background: Rgb::new(0, 0, 0),
            foreground: Rgb::new(255, 255, 255),
            border: Rgb::new(255, 255, 255),
            focus_ring: Rgb::new(255, 255, 0),
            selection_background: Rgb::new(255, 255, 0),
            selection_foreground: Rgb::new(0, 0, 0),
            link: Rgb::new(0, 255, 255),
            visited_link: Rgb::new(255, 128, 255),
            disabled: Rgb::new(160, 160, 160),
            canvas_background: Rgb::new(0, 0, 0),
        },
    }
}

/// The five text/background pairs that must meet [`MIN_CONTRAST_RATIO`].
///
/// Returned as `(name, foreground, background)` triples so a host or test can
/// audit every pair uniformly.
pub fn audited_contrast_pairs(t: ThemeTokens) -> [(&'static str, Rgb, Rgb); 5] {
    [
        ("foreground/background", t.foreground, t.background),
        (
            "selection_foreground/selection_background",
            t.selection_foreground,
            t.selection_background,
        ),
        ("link/background", t.link, t.background),
        ("visited_link/background", t.visited_link, t.background),
        ("focus_ring/background", t.focus_ring, t.background),
    ]
}

// ---------------------------------------------------------------------------
// Reflow layout metrics
// ---------------------------------------------------------------------------

/// Smallest allowed reflow scale factor.
pub const MIN_REFLOW_SCALE: f32 = 0.5;
/// Largest allowed reflow scale factor.
pub const MAX_REFLOW_SCALE: f32 = 4.0;

/// Inputs to a single-column text reflow pass, all in PDF points.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ReflowRequest {
    /// Width of the unreflowed source page.
    pub page_width: f32,
    /// Available viewport width.
    pub viewport_width: f32,
    /// Available viewport height.
    pub viewport_height: f32,
    /// Source font size.
    pub font_size: f32,
    /// Line-height multiplier (for example `1.2`).
    pub line_spacing: f32,
    /// Uniform content margin.
    pub margin: f32,
}

/// Deterministic metrics describing one reflowed page.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ReflowMetrics {
    /// Usable width after margins.
    pub content_width: f32,
    /// Usable height after margins.
    pub content_height: f32,
    /// Scale applied to source text to fit the viewport width.
    pub scale: f32,
    /// Reflowed line height in points.
    pub line_height: f32,
    /// Approximate number of average-width glyphs per line.
    pub approx_chars_per_line: usize,
    /// Maximum number of whole lines that fit on one viewport page.
    pub max_lines_per_page: usize,
}

/// Compute reflow metrics, or `None` for invalid geometry.
///
/// All fields must be finite; `page_width`, `viewport_width`,
/// `viewport_height`, `font_size`, and `line_spacing` must be positive, and the
/// margins must leave a positive content area. The resulting scale is clamped
/// to [`MIN_REFLOW_SCALE`]..=[`MAX_REFLOW_SCALE`]. An average glyph is assumed
/// to be half an em, giving a deterministic, backend-free line estimate.
pub fn reflow_metrics(request: ReflowRequest) -> Option<ReflowMetrics> {
    let ReflowRequest {
        page_width,
        viewport_width,
        viewport_height,
        font_size,
        line_spacing,
        margin,
    } = request;
    let values = [
        page_width,
        viewport_width,
        viewport_height,
        font_size,
        line_spacing,
        margin,
    ];
    if !values.iter().all(|value| value.is_finite())
        || page_width <= 0.0
        || viewport_width <= 0.0
        || viewport_height <= 0.0
        || font_size <= 0.0
        || line_spacing <= 0.0
        || margin < 0.0
    {
        return None;
    }
    let content_width = viewport_width - 2.0 * margin;
    let content_height = viewport_height - 2.0 * margin;
    if content_width <= 0.0 || content_height <= 0.0 {
        return None;
    }
    let scale = (content_width / page_width).clamp(MIN_REFLOW_SCALE, MAX_REFLOW_SCALE);
    let effective_font = font_size * scale;
    let line_height = effective_font * line_spacing;
    if !line_height.is_finite() || line_height <= 0.0 {
        return None;
    }
    let glyph_width = effective_font * 0.5;
    let approx_chars_per_line = (content_width / glyph_width).floor() as usize;
    let max_lines_per_page = (content_height / line_height).floor() as usize;
    Some(ReflowMetrics {
        content_width,
        content_height,
        scale,
        line_height,
        approx_chars_per_line,
        max_lines_per_page,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mods(ctrl: bool, shift: bool, alt: bool) -> NavModifiers {
        NavModifiers { ctrl, shift, alt }
    }

    #[test]
    fn focus_order_default_starts_at_menu_bar_and_walks_in_order() {
        let mut order = FocusOrder::default_order();
        assert_eq!(order.focused(), Some(FocusTarget::MenuBar));
        assert_eq!(order.focus_index(), 0);
        let expected = [
            FocusTarget::Toolbar,
            FocusTarget::TabStrip,
            FocusTarget::ThumbnailPane,
            FocusTarget::PageCanvas,
        ];
        for target in expected {
            assert_eq!(order.next(), Some(target));
        }
    }

    #[test]
    fn focus_order_wraps_in_both_directions() {
        let mut order = FocusOrder::from_targets(&[FocusTarget::Toolbar, FocusTarget::PageCanvas]);
        assert_eq!(order.previous(), Some(FocusTarget::PageCanvas));
        assert_eq!(order.next(), Some(FocusTarget::Toolbar));
        assert_eq!(order.tab(true), Some(FocusTarget::PageCanvas));
        assert_eq!(order.tab(false), Some(FocusTarget::Toolbar));
    }

    #[test]
    fn focus_order_skips_disabled_entries_and_rejects_focusing_them() {
        let mut order = FocusOrder::new(vec![
            Focusable::enabled(FocusTarget::Toolbar),
            Focusable::new(FocusTarget::FindBar, false),
            Focusable::enabled(FocusTarget::PageCanvas),
        ]);
        assert_eq!(
            order.focusable_targets(),
            vec![FocusTarget::Toolbar, FocusTarget::PageCanvas]
        );
        assert!(!order.set_focused(FocusTarget::FindBar));
        assert_eq!(order.focused(), Some(FocusTarget::Toolbar));
        assert_eq!(order.next(), Some(FocusTarget::PageCanvas));
        assert_eq!(order.next(), Some(FocusTarget::Toolbar));
    }

    #[test]
    fn focus_order_handles_empty_and_fully_disabled_rings() {
        let mut empty = FocusOrder::new(Vec::new());
        assert!(empty.is_empty());
        assert_eq!(empty.next(), None);
        assert_eq!(empty.previous(), None);
        assert_eq!(empty.focused(), None);

        let mut disabled = FocusOrder::new(vec![Focusable::new(FocusTarget::Toolbar, false)]);
        assert_eq!(disabled.next(), None);
        assert_eq!(disabled.focus_first(), None);
        assert_eq!(disabled.focus_last(), None);
    }

    #[test]
    fn focus_order_first_last_and_set_focused_are_deterministic() {
        let mut order = FocusOrder::default_order();
        assert_eq!(order.focus_last(), Some(FocusTarget::StatusBar));
        assert_eq!(order.focus_first(), Some(FocusTarget::MenuBar));
        assert!(order.set_focused(FocusTarget::FindBar));
        assert_eq!(order.focused(), Some(FocusTarget::FindBar));
        assert_eq!(order.focus_index(), 5);
    }

    #[test]
    fn shortcut_map_resolves_all_navigation_commands() {
        assert_eq!(
            map_nav_shortcut(NavKey::F6, mods(false, false, false)),
            Some(NavCommand::NextPane)
        );
        assert_eq!(
            map_nav_shortcut(NavKey::F6, mods(false, true, false)),
            Some(NavCommand::PreviousPane)
        );
        assert_eq!(
            map_nav_shortcut(NavKey::F3, mods(false, false, false)),
            Some(NavCommand::FocusCanvas)
        );
        assert_eq!(
            map_nav_shortcut(NavKey::F4, mods(false, false, false)),
            Some(NavCommand::FocusFind)
        );
        assert_eq!(
            map_nav_shortcut(NavKey::F5, mods(false, false, false)),
            Some(NavCommand::ReadAloud)
        );
        assert_eq!(
            map_nav_shortcut(NavKey::H, mods(true, true, false)),
            Some(NavCommand::ToggleHighContrast)
        );
    }

    #[test]
    fn shortcut_map_rejects_invalid_modifier_combinations() {
        assert_eq!(map_nav_shortcut(NavKey::H, mods(true, false, false)), None);
        assert_eq!(map_nav_shortcut(NavKey::H, mods(false, true, false)), None);
        assert_eq!(map_nav_shortcut(NavKey::F6, mods(true, false, false)), None);
        assert_eq!(map_nav_shortcut(NavKey::F3, mods(false, false, true)), None);
        assert_eq!(map_nav_shortcut(NavKey::F3, mods(false, true, false)), None);
    }

    #[test]
    fn every_nav_command_has_a_stable_shortcut_label() {
        let commands = [
            NavCommand::NextPane,
            NavCommand::PreviousPane,
            NavCommand::FocusCanvas,
            NavCommand::FocusFind,
            NavCommand::ToggleHighContrast,
            NavCommand::ReadAloud,
        ];
        for command in commands {
            assert!(!command.shortcut_label().is_empty());
        }
        assert_eq!(NavCommand::PreviousPane.shortcut_label(), "Shift+F6");
        assert_eq!(
            NavCommand::ToggleHighContrast.shortcut_label(),
            "Ctrl+Shift+H"
        );
    }

    #[test]
    fn high_contrast_tokens_meet_aaa_on_every_audited_pair() {
        let t = tokens(Theme::HighContrast);
        for (name, fg, bg) in audited_contrast_pairs(t) {
            let ratio = contrast_ratio(fg, bg);
            assert!(
                ratio >= HIGH_CONTRAST_TARGET,
                "{name} ratio {ratio} < {HIGH_CONTRAST_TARGET}"
            );
        }
        assert!((contrast_ratio(Rgb::new(0, 0, 0), Rgb::new(255, 255, 255)) - 21.0).abs() < 0.001);
    }

    #[test]
    fn standard_tokens_meet_aa_on_every_audited_pair() {
        let t = tokens(Theme::Standard);
        for (name, fg, bg) in audited_contrast_pairs(t) {
            let ratio = contrast_ratio(fg, bg);
            assert!(
                ratio >= MIN_CONTRAST_RATIO,
                "{name} ratio {ratio} < {MIN_CONTRAST_RATIO}"
            );
        }
    }

    #[test]
    fn themes_are_distinct_and_contrast_ratio_is_symmetric() {
        assert_ne!(tokens(Theme::Standard), tokens(Theme::HighContrast));
        let a = Rgb::new(12, 80, 200);
        let b = Rgb::new(240, 240, 240);
        assert!((contrast_ratio(a, b) - contrast_ratio(b, a)).abs() < f32::EPSILON);
    }

    #[test]
    fn reflow_metrics_are_deterministic_and_bounded() {
        let request = ReflowRequest {
            page_width: 612.0,
            viewport_width: 800.0,
            viewport_height: 600.0,
            font_size: 11.0,
            line_spacing: 1.2,
            margin: 20.0,
        };
        let metrics = reflow_metrics(request).expect("valid request");
        assert_eq!(metrics, reflow_metrics(request).expect("valid request"));
        assert_eq!(metrics.content_width, 760.0);
        assert_eq!(metrics.content_height, 560.0);
        assert!(metrics.scale >= MIN_REFLOW_SCALE && metrics.scale <= MAX_REFLOW_SCALE);
        assert!(metrics.line_height > 0.0);
        assert!(metrics.approx_chars_per_line > 0);
        assert!(metrics.max_lines_per_page > 0);
    }

    #[test]
    fn reflow_metrics_clamp_extreme_scales() {
        let narrow = ReflowRequest {
            page_width: 612.0,
            viewport_width: 40.0,
            viewport_height: 600.0,
            font_size: 11.0,
            line_spacing: 1.2,
            margin: 0.0,
        };
        assert_eq!(
            reflow_metrics(narrow).expect("valid").scale,
            MIN_REFLOW_SCALE
        );
        let wide = ReflowRequest {
            viewport_width: 100_000.0,
            ..narrow
        };
        assert_eq!(reflow_metrics(wide).expect("valid").scale, MAX_REFLOW_SCALE);
    }

    #[test]
    fn reflow_metrics_reject_invalid_geometry() {
        let base = ReflowRequest {
            page_width: 612.0,
            viewport_width: 800.0,
            viewport_height: 600.0,
            font_size: 11.0,
            line_spacing: 1.2,
            margin: 20.0,
        };
        assert_eq!(
            reflow_metrics(ReflowRequest {
                page_width: 0.0,
                ..base
            }),
            None
        );
        assert_eq!(
            reflow_metrics(ReflowRequest {
                font_size: -1.0,
                ..base
            }),
            None
        );
        assert_eq!(
            reflow_metrics(ReflowRequest {
                line_spacing: 0.0,
                ..base
            }),
            None
        );
        assert_eq!(
            reflow_metrics(ReflowRequest {
                margin: -1.0,
                ..base
            }),
            None
        );
        assert_eq!(
            reflow_metrics(ReflowRequest {
                viewport_width: f32::NAN,
                ..base
            }),
            None
        );
        assert_eq!(
            reflow_metrics(ReflowRequest {
                viewport_width: f32::INFINITY,
                ..base
            }),
            None
        );
        assert_eq!(
            reflow_metrics(ReflowRequest {
                viewport_width: 10.0,
                margin: 20.0,
                ..base
            }),
            None
        );
    }

    #[test]
    fn focus_target_labels_are_stable() {
        let targets = [
            FocusTarget::MenuBar,
            FocusTarget::Toolbar,
            FocusTarget::TabStrip,
            FocusTarget::ThumbnailPane,
            FocusTarget::PageCanvas,
            FocusTarget::FindBar,
            FocusTarget::LayersPanel,
            FocusTarget::BookmarksPanel,
            FocusTarget::AttachmentsPanel,
            FocusTarget::LinksPanel,
            FocusTarget::StatusBar,
        ];
        let mut labels = std::collections::BTreeSet::new();
        for target in targets {
            assert!(!target.label().is_empty());
            assert_eq!(target.to_string(), target.label());
            assert!(
                labels.insert(target.label()),
                "duplicate label: {}",
                target.label()
            );
        }
    }
}
