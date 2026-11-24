//! View layout, zoom, and rotation primitives for the document viewer.
//!
//! These types are pure data: they carry no `egui`, renderer, or platform
//! dependency. They describe *policy* (how pages are arranged, how zoom is
//! resolved, how pages are rotated) that the rendering/canvas layer will later
//! consume.
//!
//! # Rotation and zoom composition order
//!
//! The viewer composes rotation and zoom in a fixed, documented order so fit
//! modes and hit testing stay consistent (see [`crate::canvas::CanvasLayout`]):
//!
//! 1. **Rotation first.** A page's intrinsic `/Rotate` (from the PDF) is
//!    combined with the per-tab *view* rotation via [`Rotation::combine`] to
//!    obtain the effective rotation. Quarter turns (`90`/`270`) swap the
//!    displayed width and height ([`Rotation::swaps_dimensions`]).
//! 2. **Zoom second.** The fit modes resolve to a point-to-canvas scale against
//!    the *rotated* (effective) page dimensions, so "fit width" fits the width
//!    the user actually sees, whether the page is landscape or portrait.
//! 3. **Layout and mapping last.** Pages are placed using that scale; pointer
//!    hits are mapped back through the *inverse* rotation into page-local
//!    coordinates, and page-local overlay rectangles are mapped forward into
//!    canvas coordinates.
//!
//! The per-tab view rotation is deliberately separate from a page's own
//! `/Rotate`: changing the view rotation never mutates the document, and the
//! two compose additively.

/// Minimum permitted custom zoom scale (10%).
pub const MIN_ZOOM: f32 = 0.1;

/// Maximum permitted custom zoom scale (800%).
pub const MAX_ZOOM: f32 = 8.0;

/// Default zoom scale (100%).
pub const DEFAULT_ZOOM: f32 = 1.0;

/// How pages are arranged inside the viewer viewport.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ViewMode {
    /// Show one page at a time.
    SinglePage,
    /// Stack pages vertically and scroll continuously.
    #[default]
    Continuous,
    /// Show two pages side by side, one screen at a time.
    TwoPage,
    /// Show two pages side by side and scroll continuously.
    TwoPageContinuous,
}

impl ViewMode {
    /// Every mode in declaration order, for pickers and tests.
    pub const ALL: &'static [Self] = &[
        Self::SinglePage,
        Self::Continuous,
        Self::TwoPage,
        Self::TwoPageContinuous,
    ];

    /// A stable, human-readable label for this mode.
    pub fn label(self) -> &'static str {
        match self {
            Self::SinglePage => "Single Page",
            Self::Continuous => "Continuous",
            Self::TwoPage => "Two Page",
            Self::TwoPageContinuous => "Two Page Continuous",
        }
    }

    /// Whether this mode places two pages next to each other.
    pub fn is_two_page(self) -> bool {
        matches!(self, Self::TwoPage | Self::TwoPageContinuous)
    }

    /// Whether this mode scrolls continuously through all pages.
    pub fn is_continuous(self) -> bool {
        matches!(self, Self::Continuous | Self::TwoPageContinuous)
    }
}

impl std::fmt::Display for ViewMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

/// A quarter-turn rotation applied to page rendering.
///
/// Values are constrained to the four right angles a PDF viewer supports.
/// [`Rotation::from_degrees`] snaps arbitrary inputs to the nearest quarter
/// turn and normalizes negative and out-of-range angles.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Rotation {
    /// No rotation (0 degrees).
    #[default]
    Deg0,
    /// 90 degrees clockwise.
    Deg90,
    /// 180 degrees.
    Deg180,
    /// 270 degrees clockwise (90 degrees counterclockwise).
    Deg270,
}

impl Rotation {
    /// Build a rotation from an angle in degrees.
    ///
    /// The angle is normalized with a Euclidean remainder and snapped to the
    /// nearest quarter turn, so any `i32` input is accepted.
    pub fn from_degrees(degrees: i32) -> Self {
        let normalized = degrees.rem_euclid(360);
        let quarter = ((normalized + 45) / 90) % 4;
        match quarter {
            1 => Self::Deg90,
            2 => Self::Deg180,
            3 => Self::Deg270,
            _ => Self::Deg0,
        }
    }

    /// The rotation in degrees, always in `[0, 360)`.
    pub fn degrees(self) -> i16 {
        match self {
            Self::Deg0 => 0,
            Self::Deg90 => 90,
            Self::Deg180 => 180,
            Self::Deg270 => 270,
        }
    }

    /// Rotate 90 degrees clockwise.
    pub fn clockwise(self) -> Self {
        match self {
            Self::Deg0 => Self::Deg90,
            Self::Deg90 => Self::Deg180,
            Self::Deg180 => Self::Deg270,
            Self::Deg270 => Self::Deg0,
        }
    }

    /// Rotate 90 degrees counterclockwise.
    pub fn counterclockwise(self) -> Self {
        match self {
            Self::Deg0 => Self::Deg270,
            Self::Deg90 => Self::Deg0,
            Self::Deg180 => Self::Deg90,
            Self::Deg270 => Self::Deg180,
        }
    }

    /// Compose this rotation with `other`, returning the combined quarter turn.
    ///
    /// Addition is modulo a full turn, so `Deg270.combine(Deg90) == Deg0`. This
    /// is how a per-tab view rotation is combined with a page's intrinsic
    /// `/Rotate` to obtain the effective rotation used by the canvas.
    pub fn combine(self, other: Self) -> Self {
        Self::from_degrees(i32::from(self.degrees()) + i32::from(other.degrees()))
    }

    /// Whether quarter turns of this rotation swap page width and height.
    ///
    /// `90` and `270` degrees display a page on its side, so the effective page
    /// size used for layout and fit modes must exchange its two extents.
    pub fn swaps_dimensions(self) -> bool {
        matches!(self, Self::Deg90 | Self::Deg270)
    }
}

/// The zoom policy for a document view.
///
/// Fit modes defer the concrete scale to the canvas (which knows the viewport
/// and page sizes), while [`Zoom::Custom`] carries a clamped scale factor.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Zoom {
    /// Fit the page width to the viewport.
    FitWidth,
    /// Fit the entire page to the viewport.
    FitPage,
    /// An explicit scale factor, clamped to [`MIN_ZOOM`]..=[`MAX_ZOOM`].
    Custom(f32),
}

impl Default for Zoom {
    fn default() -> Self {
        Self::Custom(DEFAULT_ZOOM)
    }
}

impl Zoom {
    /// Construct a clamped custom zoom.
    ///
    /// `NaN` falls back to [`DEFAULT_ZOOM`], infinities clamp to the nearest
    /// bound, and finite values are clamped into the supported range.
    pub fn custom(scale: f32) -> Self {
        Self::Custom(clamp_scale(scale))
    }

    /// Return a copy whose custom scale is clamped.
    pub fn clamped(self) -> Self {
        match self {
            Self::Custom(scale) => Self::Custom(clamp_scale(scale)),
            fit => fit,
        }
    }

    /// The explicit scale for a custom zoom, or `None` for a fit mode.
    pub fn scale(self) -> Option<f32> {
        match self.clamped() {
            Self::Custom(scale) => Some(scale),
            _ => None,
        }
    }

    /// Resolve this zoom to a concrete scale.
    ///
    /// Fit modes use the corresponding caller-supplied fit scale; a custom
    /// zoom uses its own clamped scale. Every result is clamped to the
    /// supported range, which makes this function deterministic.
    pub fn effective_scale(self, fit_width: f32, fit_page: f32) -> f32 {
        match self {
            Self::FitWidth => clamp_scale(fit_width),
            Self::FitPage => clamp_scale(fit_page),
            Self::Custom(scale) => clamp_scale(scale),
        }
    }

    /// Whether this zoom defers its scale to the viewport.
    pub fn is_fit(self) -> bool {
        matches!(self, Self::FitWidth | Self::FitPage)
    }
}

impl std::fmt::Display for Zoom {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::FitWidth => f.write_str("Fit Width"),
            Self::FitPage => f.write_str("Fit Page"),
            Self::Custom(scale) => write!(f, "{}%", (clamp_scale(*scale) * 100.0).round() as i32),
        }
    }
}

/// Clamp a scale into the supported range, sanitizing non-finite values.
fn clamp_scale(value: f32) -> f32 {
    if value.is_nan() {
        return DEFAULT_ZOOM;
    }
    if value == f32::INFINITY {
        return MAX_ZOOM;
    }
    if value == f32::NEG_INFINITY {
        return MIN_ZOOM;
    }
    value.clamp(MIN_ZOOM, MAX_ZOOM)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zoom_custom_clamps_to_bounds() {
        assert_eq!(Zoom::custom(0.01).scale(), Some(MIN_ZOOM));
        assert_eq!(Zoom::custom(42.0).scale(), Some(MAX_ZOOM));
        assert_eq!(Zoom::custom(1.25).scale(), Some(1.25));
        // A directly constructed variant is clamped too.
        assert_eq!(Zoom::Custom(-3.0).clamped().scale(), Some(MIN_ZOOM));
    }

    #[test]
    fn zoom_sanitizes_non_finite_values() {
        assert_eq!(Zoom::custom(f32::NAN).scale(), Some(DEFAULT_ZOOM));
        assert_eq!(Zoom::custom(f32::INFINITY).scale(), Some(MAX_ZOOM));
        assert_eq!(Zoom::custom(f32::NEG_INFINITY).scale(), Some(MIN_ZOOM));
    }

    #[test]
    fn zoom_fit_modes_resolve_to_provided_scales() {
        assert_eq!(Zoom::FitWidth.effective_scale(0.5, 0.25), 0.5);
        assert_eq!(Zoom::FitPage.effective_scale(0.5, 0.25), 0.25);
        assert_eq!(Zoom::custom(2.0).effective_scale(0.5, 0.25), 2.0);
        assert_eq!(Zoom::FitWidth.scale(), None);
        assert!(Zoom::FitPage.is_fit());
        assert!(!Zoom::custom(1.0).is_fit());
        // Fit scales are clamped as well.
        assert_eq!(Zoom::FitWidth.effective_scale(100.0, 1.0), MAX_ZOOM);
    }

    #[test]
    fn rotation_normalizes_and_turns() {
        assert_eq!(Rotation::from_degrees(0), Rotation::Deg0);
        assert_eq!(Rotation::from_degrees(90), Rotation::Deg90);
        assert_eq!(Rotation::from_degrees(-90), Rotation::Deg270);
        assert_eq!(Rotation::from_degrees(450), Rotation::Deg90);
        assert_eq!(Rotation::from_degrees(44).degrees(), 0);
        assert_eq!(Rotation::from_degrees(46).degrees(), 90);
        assert_eq!(Rotation::Deg0.clockwise(), Rotation::Deg90);
        assert_eq!(Rotation::Deg0.counterclockwise(), Rotation::Deg270);
        assert_eq!(Rotation::Deg270.clockwise(), Rotation::Deg0);
        assert_eq!(Rotation::Deg90.clockwise().clockwise(), Rotation::Deg270);
    }

    #[test]
    fn rotation_cycles_through_four_quarter_turns() {
        // Clockwise cycling returns to the start after exactly four steps.
        let mut r = Rotation::Deg0;
        let mut seen = Vec::new();
        for _ in 0..4 {
            r = r.clockwise();
            seen.push(r);
        }
        assert_eq!(
            seen,
            vec![
                Rotation::Deg90,
                Rotation::Deg180,
                Rotation::Deg270,
                Rotation::Deg0,
            ]
        );
        // Counterclockwise is the exact inverse of clockwise.
        for start in [
            Rotation::Deg0,
            Rotation::Deg90,
            Rotation::Deg180,
            Rotation::Deg270,
        ] {
            assert_eq!(start.clockwise().counterclockwise(), start);
        }
    }

    #[test]
    fn rotation_compose_and_dimension_swap() {
        assert_eq!(Rotation::Deg270.combine(Rotation::Deg90), Rotation::Deg0);
        assert_eq!(Rotation::Deg90.combine(Rotation::Deg90), Rotation::Deg180);
        assert_eq!(Rotation::Deg0.combine(Rotation::Deg270), Rotation::Deg270);
        assert!(Rotation::Deg90.swaps_dimensions());
        assert!(Rotation::Deg270.swaps_dimensions());
        assert!(!Rotation::Deg0.swaps_dimensions());
        assert!(!Rotation::Deg180.swaps_dimensions());
    }

    #[test]
    fn view_mode_classification_is_consistent() {
        assert!(ViewMode::Continuous.is_continuous());
        assert!(ViewMode::TwoPageContinuous.is_continuous());
        assert!(!ViewMode::SinglePage.is_continuous());
        assert!(ViewMode::TwoPage.is_two_page());
        assert!(ViewMode::TwoPageContinuous.is_two_page());
        assert!(!ViewMode::Continuous.is_two_page());
        assert_eq!(ViewMode::SinglePage.label(), "Single Page");
    }
}
