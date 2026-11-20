//! Window-free loupe magnifier and gesture-navigation model.
//!
//! This module is the pure model behind a PDF magnifier ("loupe"). It knows
//! nothing about `egui`, native pointer/window events, or the OS gesture
//! recognizer. Instead the host supplies drag/scroll/pinch deltas through
//! [`LoupeModel::apply`], and the model returns deterministic, clamped changes
//! to its own magnification state. All geometry is computed in two documented
//! coordinate spaces — **page space** (PDF points, `y` down) and **screen
//! space** (device units, `y` down) — so the same inputs always produce the
//! same source/destination rectangles and hit results.
//!
//! # Geometry
//!
//! The lens has a fixed screen size ([`LoupeModel::lens`]). The *destination*
//! rectangle is where the magnified image is drawn on screen; the *source*
//! rectangle is the region of the page it shows, sized as `lens / zoom` and
//! centred on the lens centre in page space. [`LoupeModel::page_to_screen`] and
//! [`LoupeModel::screen_to_page`] are exact inverses, so a source rect maps
//! round-trip onto its destination rect.

/// Smallest supported magnification factor (1x).
pub const MIN_LOUPE_ZOOM: f32 = 1.0;
/// Largest supported magnification factor (16x).
pub const MAX_LOUPE_ZOOM: f32 = 16.0;
/// Largest single gesture delta applied in one call, in input units.
pub const MAX_GESTURE_STEP: f32 = 512.0;
/// Largest accumulated drag magnitude retained by the model, in input units.
pub const MAX_GESTURE_ACCUM: f32 = 8192.0;

/// Shape of the magnifier lens.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LensShape {
    /// A rectangular lens covering its whole bounding rectangle.
    #[default]
    Rectangle,
    /// A circular lens inscribed in a square bounding rectangle.
    Circle,
}

/// Typed errors produced while configuring a loupe model.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum LoupeError {
    /// A size or position had non-positive or non-finite dimensions.
    #[error("loupe geometry must be finite and positive")]
    InvalidGeometry,
    /// A zoom value was non-finite or outside the supported bounds.
    #[error("loupe zoom {0} is outside {MIN_LOUPE_ZOOM}..={MAX_LOUPE_ZOOM}")]
    InvalidZoom(f32),
}

/// A 2D point (`x` right, `y` down) in either page or screen space.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct LoupePoint {
    /// Horizontal coordinate.
    pub x: f32,
    /// Vertical coordinate.
    pub y: f32,
}

impl LoupePoint {
    /// Create a point at `(x, y)`.
    pub const fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }
}

/// An extent (`width`, `height`) in either page or screen space.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct LoupeSize {
    /// Horizontal extent.
    pub width: f32,
    /// Vertical extent.
    pub height: f32,
}

impl LoupeSize {
    /// Create a size.
    pub const fn new(width: f32, height: f32) -> Self {
        Self { width, height }
    }

    fn is_valid(self) -> bool {
        self.width.is_finite() && self.height.is_finite() && self.width > 0.0 && self.height > 0.0
    }
}

/// An axis-aligned rectangle (`x`, `y`, `width`, `height`).
///
/// Hit testing is half-open: a point is inside when
/// `x >= left && x < right && y >= top && y < bottom`.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct LoupeRect {
    /// Left edge (inclusive).
    pub x: f32,
    /// Top edge (inclusive),
    pub y: f32,
    /// Rectangle width.
    pub width: f32,
    /// Rectangle height.
    pub height: f32,
}

impl LoupeRect {
    /// Create a rectangle from its top-left corner and size.
    pub const fn new(x: f32, y: f32, width: f32, height: f32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    /// Build the rectangle of size `size` centred on `center`.
    pub fn centered(center: LoupePoint, size: LoupeSize) -> Self {
        Self::new(
            center.x - size.width * 0.5,
            center.y - size.height * 0.5,
            size.width,
            size.height,
        )
    }

    /// Half-open containment test.
    pub fn contains(self, point: LoupePoint) -> bool {
        point.x >= self.x
            && point.x < self.x + self.width
            && point.y >= self.y
            && point.y < self.y + self.height
    }

    /// The centre of the rectangle.
    pub fn center(self) -> LoupePoint {
        LoupePoint::new(self.x + self.width * 0.5, self.y + self.height * 0.5)
    }
}

/// A caller-supplied gesture delta. No OS gesture recognizer is involved.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum GestureDelta {
    /// A pointer drag; positive `dx` drags content right, `dy` drags it down.
    Drag {
        /// Horizontal drag in input units.
        dx: f32,
        /// Vertical drag in input units.
        dy: f32,
    },
    /// A scroll; positive `dx`/`dy` move the view right/down.
    Scroll {
        /// Horizontal scroll in input units.
        dx: f32,
        /// Vertical scroll in input units.
        dy: f32,
    },
    /// A pinch whose multiplicative factor scales the zoom (e.g. `1.1` zooms in).
    Pinch {
        /// Multiplicative zoom factor.
        factor: f32,
    },
}

/// The effect a gesture had on the model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GestureEffect {
    /// The delta was invalid or clamped to no change.
    Ignored,
    /// The lens centre in page space moved.
    Panned,
    /// The magnification changed.
    Zoomed,
}

/// A window-free loupe magnifier model.
#[derive(Debug, Clone, PartialEq)]
pub struct LoupeModel {
    shape: LensShape,
    zoom: f32,
    lens: LoupeSize,
    center_page: LoupePoint,
    center_screen: LoupePoint,
    page: LoupeSize,
    viewport: LoupeSize,
    drag: LoupePoint,
}

impl LoupeModel {
    /// Build a loupe model.
    ///
    /// `page`, `viewport`, and `lens` must have finite positive dimensions.
    /// `zoom` is clamped into [`MIN_LOUPE_ZOOM`]..=[`MAX_LOUPE_ZOOM`]. The lens
    /// starts centred in both the page and the viewport.
    pub fn new(
        page: LoupeSize,
        viewport: LoupeSize,
        shape: LensShape,
        lens: LoupeSize,
        zoom: f32,
    ) -> Result<Self, LoupeError> {
        if !page.is_valid() || !viewport.is_valid() || !lens.is_valid() {
            return Err(LoupeError::InvalidGeometry);
        }
        let center_page = LoupePoint::new(page.width * 0.5, page.height * 0.5);
        let center_screen = LoupePoint::new(viewport.width * 0.5, viewport.height * 0.5);
        let mut model = Self {
            shape,
            zoom: clamp_zoom(zoom),
            lens,
            center_page,
            center_screen,
            page,
            viewport,
            drag: LoupePoint::default(),
        };
        model.clamp_center_page();
        model.clamp_center_screen();
        Ok(model)
    }

    /// Lens shape.
    pub fn shape(&self) -> LensShape {
        self.shape
    }

    /// Current magnification, clamped to the supported range.
    pub fn zoom(&self) -> f32 {
        self.zoom
    }

    /// Screen-space size of the lens.
    pub fn lens(&self) -> LoupeSize {
        self.lens
    }

    /// Page size used for centre clamping.
    pub fn page(&self) -> LoupeSize {
        self.page
    }

    /// Screen viewport used for centre clamping.
    pub fn viewport(&self) -> LoupeSize {
        self.viewport
    }

    /// Lens centre in page space.
    pub fn center_page(&self) -> LoupePoint {
        self.center_page
    }

    /// Lens centre in screen space.
    pub fn center_screen(&self) -> LoupePoint {
        self.center_screen
    }

    /// Bounded total drag accumulated so far, in input units.
    pub fn accumulated_drag(&self) -> LoupePoint {
        self.drag
    }

    /// Change the lens shape.
    pub fn set_shape(&mut self, shape: LensShape) {
        self.shape = shape;
    }

    /// Set the zoom, clamping into range; returns the applied value.
    pub fn set_zoom(&mut self, zoom: f32) -> f32 {
        self.zoom = clamp_zoom(zoom);
        self.zoom
    }

    /// Set the zoom, returning an error for out-of-range or non-finite values.
    pub fn try_set_zoom(&mut self, zoom: f32) -> Result<(), LoupeError> {
        if !zoom.is_finite() || zoom < MIN_LOUPE_ZOOM || zoom > MAX_LOUPE_ZOOM {
            return Err(LoupeError::InvalidZoom(zoom));
        }
        self.zoom = zoom;
        Ok(())
    }

    /// Multiply the zoom by `1.25`, clamped to the supported range.
    pub fn zoom_in(&mut self) -> f32 {
        self.set_zoom(self.zoom * 1.25)
    }

    /// Divide the zoom by `1.25`, clamped to the supported range.
    pub fn zoom_out(&mut self) -> f32 {
        self.set_zoom(self.zoom / 1.25)
    }

    /// Move the lens centre in page space, clamping it inside the page.
    pub fn set_center_page(&mut self, center: LoupePoint) {
        self.center_page = center;
        self.clamp_center_page();
    }

    /// Move the lens centre in screen space, clamping it inside the viewport.
    pub fn set_center_screen(&mut self, center: LoupePoint) {
        self.center_screen = center;
        self.clamp_center_screen();
    }

    /// Radius of a circular lens in screen units (half its width).
    pub fn radius(&self) -> f32 {
        self.lens.width * 0.5
    }

    /// Destination rectangle: where the magnified image is drawn on screen.
    pub fn dest_rect(&self) -> LoupeRect {
        LoupeRect::centered(self.center_screen, self.lens)
    }

    /// Source rectangle: the page region shown, in page space.
    pub fn source_rect(&self) -> LoupeRect {
        let size = LoupeSize::new(self.lens.width / self.zoom, self.lens.height / self.zoom);
        LoupeRect::centered(self.center_page, size)
    }

    /// Map a page-space point into screen space.
    pub fn page_to_screen(&self, point: LoupePoint) -> LoupePoint {
        LoupePoint::new(
            self.center_screen.x + (point.x - self.center_page.x) * self.zoom,
            self.center_screen.y + (point.y - self.center_page.y) * self.zoom,
        )
    }

    /// Map a screen-space point into page space. Inverse of
    /// [`Self::page_to_screen`].
    pub fn screen_to_page(&self, point: LoupePoint) -> LoupePoint {
        LoupePoint::new(
            self.center_page.x + (point.x - self.center_screen.x) / self.zoom,
            self.center_page.y + (point.y - self.center_screen.y) / self.zoom,
        )
    }

    /// Hit-test a screen point against the lens (shape-aware).
    pub fn contains_screen(&self, point: LoupePoint) -> bool {
        match self.shape {
            LensShape::Rectangle => self.dest_rect().contains(point),
            LensShape::Circle => {
                let dx = point.x - self.center_screen.x;
                let dy = point.y - self.center_screen.y;
                let radius = self.radius();
                dx * dx + dy * dy <= radius * radius
            }
        }
    }

    /// Hit-test a page point against the lens's source region (shape-aware).
    pub fn contains_page(&self, point: LoupePoint) -> bool {
        match self.shape {
            LensShape::Rectangle => self.source_rect().contains(point),
            LensShape::Circle => {
                let dx = point.x - self.center_page.x;
                let dy = point.y - self.center_page.y;
                let radius = self.radius() / self.zoom;
                dx * dx + dy * dy <= radius * radius
            }
        }
    }

    /// Apply a caller-supplied gesture delta.
    ///
    /// Each incoming delta is bounded to [`MAX_GESTURE_STEP`]; drags accumulate
    /// into a bounded total ([`MAX_GESTURE_ACCUM`]). Panning divides by zoom so
    /// a given screen movement shifts the page by the corresponding magnified
    /// amount, and the page centre is clamped to the page bounds. Pinching
    /// scales the zoom, which is clamped to the supported range.
    pub fn apply(&mut self, delta: GestureDelta) -> GestureEffect {
        match delta {
            GestureDelta::Drag { dx, dy } => {
                let (dx, dy) = match bounded_step(dx, dy) {
                    Some(step) => step,
                    None => return GestureEffect::Ignored,
                };
                self.drag.x = clamp_value(self.drag.x + dx, -MAX_GESTURE_ACCUM, MAX_GESTURE_ACCUM);
                self.drag.y = clamp_value(self.drag.y + dy, -MAX_GESTURE_ACCUM, MAX_GESTURE_ACCUM);
                if dx == 0.0 && dy == 0.0 {
                    return GestureEffect::Ignored;
                }
                self.pan(-dx / self.zoom, -dy / self.zoom);
                GestureEffect::Panned
            }
            GestureDelta::Scroll { dx, dy } => {
                let (dx, dy) = match bounded_step(dx, dy) {
                    Some(step) => step,
                    None => return GestureEffect::Ignored,
                };
                if dx == 0.0 && dy == 0.0 {
                    return GestureEffect::Ignored;
                }
                self.pan(dx / self.zoom, dy / self.zoom);
                GestureEffect::Panned
            }
            GestureDelta::Pinch { factor } => {
                if !factor.is_finite() || factor <= 0.0 {
                    return GestureEffect::Ignored;
                }
                let next = clamp_zoom(self.zoom * factor);
                if (next - self.zoom).abs() <= f32::EPSILON {
                    GestureEffect::Ignored
                } else {
                    self.zoom = next;
                    GestureEffect::Zoomed
                }
            }
        }
    }

    fn pan(&mut self, dx_page: f32, dy_page: f32) {
        self.center_page.x += dx_page;
        self.center_page.y += dy_page;
        self.clamp_center_page();
    }

    fn clamp_center_page(&mut self) {
        self.center_page.x = clamp_value(self.center_page.x, 0.0, self.page.width);
        self.center_page.y = clamp_value(self.center_page.y, 0.0, self.page.height);
    }

    fn clamp_center_screen(&mut self) {
        // If the lens is larger than the viewport, keep the viewport centre so
        // the inverted half-extent range can never produce a nonsensical value.
        let min_x = self.lens.width * 0.5;
        let max_x = self.viewport.width - min_x;
        let min_y = self.lens.height * 0.5;
        let max_y = self.viewport.height - min_y;
        self.center_screen.x = if min_x <= max_x {
            clamp_value(self.center_screen.x, min_x, max_x)
        } else {
            self.viewport.width * 0.5
        };
        self.center_screen.y = if min_y <= max_y {
            clamp_value(self.center_screen.y, min_y, max_y)
        } else {
            self.viewport.height * 0.5
        };
    }
}

fn clamp_zoom(value: f32) -> f32 {
    if value.is_nan() {
        return MIN_LOUPE_ZOOM;
    }
    if value == f32::INFINITY {
        return MAX_LOUPE_ZOOM;
    }
    if value == f32::NEG_INFINITY {
        return MIN_LOUPE_ZOOM;
    }
    value.clamp(MIN_LOUPE_ZOOM, MAX_LOUPE_ZOOM)
}

fn clamp_value(value: f32, low: f32, high: f32) -> f32 {
    if value.is_nan() {
        low
    } else {
        value.clamp(low, high)
    }
}

fn bounded_step(dx: f32, dy: f32) -> Option<(f32, f32)> {
    if !dx.is_finite() || !dy.is_finite() {
        return None;
    }
    Some((
        dx.clamp(-MAX_GESTURE_STEP, MAX_GESTURE_STEP),
        dy.clamp(-MAX_GESTURE_STEP, MAX_GESTURE_STEP),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model(shape: LensShape) -> LoupeModel {
        LoupeModel::new(
            LoupeSize::new(612.0, 792.0),
            LoupeSize::new(1000.0, 800.0),
            shape,
            LoupeSize::new(200.0, 200.0),
            4.0,
        )
        .expect("valid geometry")
    }

    fn approx(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-3
    }

    #[test]
    fn new_validates_geometry_and_centres_the_lens() {
        assert_eq!(
            LoupeModel::new(
                LoupeSize::new(0.0, 10.0),
                LoupeSize::new(10.0, 10.0),
                LensShape::Circle,
                LoupeSize::new(2.0, 2.0),
                2.0
            ),
            Err(LoupeError::InvalidGeometry)
        );
        assert_eq!(
            LoupeModel::new(
                LoupeSize::new(10.0, 10.0),
                LoupeSize::new(f32::NAN, 10.0),
                LensShape::Circle,
                LoupeSize::new(2.0, 2.0),
                2.0
            ),
            Err(LoupeError::InvalidGeometry)
        );
        let loupe = model(LensShape::Circle);
        assert_eq!(loupe.center_page(), LoupePoint::new(306.0, 396.0));
        assert_eq!(loupe.center_screen(), LoupePoint::new(500.0, 400.0));
        assert_eq!(loupe.zoom(), 4.0);
    }

    #[test]
    fn zoom_is_bounded_and_sanitized() {
        let mut loupe = model(LensShape::Rectangle);
        assert_eq!(loupe.set_zoom(100.0), MAX_LOUPE_ZOOM);
        assert_eq!(loupe.set_zoom(0.001), MIN_LOUPE_ZOOM);
        assert_eq!(loupe.set_zoom(f32::NAN), MIN_LOUPE_ZOOM);
        assert_eq!(loupe.set_zoom(f32::INFINITY), MAX_LOUPE_ZOOM);
        assert_eq!(loupe.set_zoom(2.0), 2.0);
        loupe.set_zoom(1.0);
        assert_eq!(loupe.zoom_in(), 1.25);
        assert_eq!(loupe.zoom_out(), 1.0);
        assert_eq!(
            loupe.try_set_zoom(MAX_LOUPE_ZOOM + 1.0),
            Err(LoupeError::InvalidZoom(MAX_LOUPE_ZOOM + 1.0))
        );
        assert_eq!(loupe.try_set_zoom(3.5), Ok(()));
        assert_eq!(loupe.zoom(), 3.5);
    }

    #[test]
    fn page_to_screen_and_screen_to_page_round_trip() {
        let loupe = model(LensShape::Rectangle);
        let point = LoupePoint::new(400.0, 500.0);
        let screen = loupe.page_to_screen(point);
        let back = loupe.screen_to_page(screen);
        assert!(approx(back.x, point.x) && approx(back.y, point.y));
    }

    #[test]
    fn source_rect_maps_round_trip_to_dest_rect() {
        let loupe = model(LensShape::Rectangle);
        let source = loupe.source_rect();
        let dest = loupe.dest_rect();
        // Source size is lens size divided by zoom.
        assert!(approx(source.width, 200.0 / 4.0));
        assert!(approx(source.height, 200.0 / 4.0));
        let top_left = loupe.page_to_screen(LoupePoint::new(source.x, source.y));
        let bottom_right = loupe.page_to_screen(LoupePoint::new(
            source.x + source.width,
            source.y + source.height,
        ));
        assert!(approx(top_left.x, dest.x) && approx(top_left.y, dest.y));
        assert!(approx(bottom_right.x, dest.x + dest.width));
        assert!(approx(bottom_right.y, dest.y + dest.height));
    }

    #[test]
    fn lens_hit_testing_is_shape_aware_and_deterministic() {
        let rect = model(LensShape::Rectangle);
        let circle = model(LensShape::Circle);
        // A point at the corner of the destination rect is inside the rectangle
        // but outside the circle of the same bounding box.
        let corner = LoupePoint::new(rect.dest_rect().x + 1.0, rect.dest_rect().y + 1.0);
        assert!(rect.contains_screen(corner));
        assert!(!circle.contains_screen(corner));
        assert!(circle.contains_screen(circle.center_screen()));
        let far = LoupePoint::new(0.0, 0.0);
        assert!(!rect.contains_screen(far));
        assert!(!circle.contains_screen(far));
        // Page-space hit testing mirrors the screen-space result.
        assert!(rect.contains_page(rect.center_page()));
        assert!(circle.contains_page(circle.center_page()));
        assert_eq!(rect.contains_screen(corner), rect.contains_screen(corner));
    }

    #[test]
    fn drag_and_scroll_pan_and_clamp_to_page() {
        let mut loupe = model(LensShape::Rectangle);
        assert_eq!(
            loupe.apply(GestureDelta::Drag { dx: 40.0, dy: 20.0 }),
            GestureEffect::Panned
        );
        // Dragging right/down moves the page centre left/up by delta/zoom.
        assert!(approx(loupe.center_page().x, 306.0 - 10.0));
        assert!(approx(loupe.center_page().y, 396.0 - 5.0));
        // A single huge scroll is bounded to MAX_GESTURE_STEP before dividing
        // by zoom, so its effect is finite and predictable.
        loupe.apply(GestureDelta::Scroll {
            dx: 100_000.0,
            dy: -100_000.0,
        });
        assert!(approx(
            loupe.center_page().x,
            296.0 + MAX_GESTURE_STEP / 4.0
        ));
        assert!(approx(
            loupe.center_page().y,
            391.0 - MAX_GESTURE_STEP / 4.0
        ));
        // Repeated scrolls clamp the centre to the page bounds.
        loupe.set_zoom(MIN_LOUPE_ZOOM);
        for _ in 0..4 {
            loupe.apply(GestureDelta::Scroll {
                dx: 100_000.0,
                dy: -100_000.0,
            });
        }
        assert_eq!(loupe.center_page().x, 612.0);
        assert_eq!(loupe.center_page().y, 0.0);
    }

    #[test]
    fn gesture_accumulation_is_bounded() {
        let mut loupe = model(LensShape::Rectangle);
        for _ in 0..100 {
            loupe.apply(GestureDelta::Drag {
                dx: MAX_GESTURE_STEP,
                dy: MAX_GESTURE_STEP,
            });
        }
        assert_eq!(loupe.accumulated_drag().x, MAX_GESTURE_ACCUM);
        assert_eq!(loupe.accumulated_drag().y, MAX_GESTURE_ACCUM);
    }

    #[test]
    fn pinch_clamps_zoom_and_invalid_deltas_are_ignored() {
        let mut loupe = model(LensShape::Circle);
        assert_eq!(
            loupe.apply(GestureDelta::Pinch { factor: 100.0 }),
            GestureEffect::Zoomed
        );
        assert_eq!(loupe.zoom(), MAX_LOUPE_ZOOM);
        assert_eq!(
            loupe.apply(GestureDelta::Pinch { factor: 0.0001 }),
            GestureEffect::Zoomed
        );
        assert_eq!(loupe.zoom(), MIN_LOUPE_ZOOM);
        assert_eq!(
            loupe.apply(GestureDelta::Pinch { factor: 1.0 }),
            GestureEffect::Ignored
        );
        assert_eq!(
            loupe.apply(GestureDelta::Pinch { factor: f32::NAN }),
            GestureEffect::Ignored
        );
        assert_eq!(
            loupe.apply(GestureDelta::Drag {
                dx: f32::INFINITY,
                dy: 0.0
            }),
            GestureEffect::Ignored
        );
    }

    #[test]
    fn identical_gesture_sequences_are_deterministic() {
        let gestures = [
            GestureDelta::Drag {
                dx: 30.0,
                dy: -12.0,
            },
            GestureDelta::Scroll { dx: -8.0, dy: 16.0 },
            GestureDelta::Pinch { factor: 1.5 },
            GestureDelta::Drag {
                dx: 100.0,
                dy: 100.0,
            },
        ];
        let mut a = model(LensShape::Circle);
        let mut b = model(LensShape::Circle);
        for gesture in gestures {
            assert_eq!(a.apply(gesture), b.apply(gesture));
        }
        assert_eq!(a, b);
    }

    #[test]
    fn center_clamping_keeps_the_lens_inside_the_viewport() {
        let mut loupe = model(LensShape::Rectangle);
        loupe.set_center_screen(LoupePoint::new(-1000.0, 5000.0));
        // Lens half-extent is 100, so the centre stays within [100, 900] x
        // [100, 700].
        assert_eq!(loupe.center_screen().x, 100.0);
        assert_eq!(loupe.center_screen().y, 700.0);
    }
}
