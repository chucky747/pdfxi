//! Affine matrices used to place an appearance stream inside a widget rectangle.
//!
//! PDF represents a 2-D affine transform with the six numbers
//! `[a b c d e f]`, applied to a point as:
//!
//! ```text
//! x' = a·x + c·y + e
//! y' = b·x + d·y + f
//! ```
//!
//! This module is a small, dependency-free model of that arithmetic. It is used
//! in two places:
//!
//! - the appearance generator writes a rotation into a generated stream's
//!   `/Matrix` so a widget rotated by `/MK /R` draws upright content inside its
//!   axis-aligned `/Rect`;
//! - flattening composes a stream's `/BBox` and `/Matrix` with the widget
//!   `/Rect` to obtain the `cm` operator that paints the appearance.
//!
//! Every operation returns `None` instead of panicking on non-finite input, and
//! [`Matrix::is_finite`] lets callers reject hostile numbers before use.
//!
//! # Rotation convention
//!
//! [`Matrix::rotation_degrees`] rotates counter-clockwise about the origin, the
//! same direction PDF uses for `/MK /R`. A widget rotated by 90° therefore has
//! its text baseline running from the bottom edge toward the top edge of the
//! page rectangle.

/// An affine transform in PDF `[a b c d e f]` order.
///
/// The fields are public because the values are also written verbatim into a
/// content stream; callers may inspect them without an accessor round trip.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Matrix {
    /// Horizontal scale for the x axis.
    pub a: f64,
    /// Vertical shear for the x axis.
    pub b: f64,
    /// Horizontal shear for the y axis.
    pub c: f64,
    /// Vertical scale for the y axis.
    pub d: f64,
    /// Horizontal translation.
    pub e: f64,
    /// Vertical translation.
    pub f: f64,
}

impl Default for Matrix {
    fn default() -> Self {
        Self::IDENTITY
    }
}

impl Matrix {
    /// The identity transform.
    pub const IDENTITY: Self = Self::new(1.0, 0.0, 0.0, 1.0, 0.0, 0.0);

    /// Tolerance under which a coefficient counts as zero for
    /// [`Matrix::is_identity`].
    pub const EPSILON: f64 = 1e-9;

    /// Creates a transform from its six PDF coefficients.
    pub const fn new(a: f64, b: f64, c: f64, d: f64, e: f64, f: f64) -> Self {
        Self { a, b, c, d, e, f }
    }

    /// Creates a transform from a `[a b c d e f]` array.
    pub const fn from_array(values: [f64; 6]) -> Self {
        Self::new(
            values[0], values[1], values[2], values[3], values[4], values[5],
        )
    }

    /// Returns the six coefficients in PDF order.
    pub const fn to_array(self) -> [f64; 6] {
        [self.a, self.b, self.c, self.d, self.e, self.f]
    }

    /// Returns a translation by `(tx, ty)`.
    pub const fn translation(tx: f64, ty: f64) -> Self {
        Self::new(1.0, 0.0, 0.0, 1.0, tx, ty)
    }

    /// Returns a scale by `(sx, sy)` about the origin.
    pub const fn scale(sx: f64, sy: f64) -> Self {
        Self::new(sx, 0.0, 0.0, sy, 0.0, 0.0)
    }

    /// Returns a counter-clockwise rotation for a right angle.
    ///
    /// Returns `None` for any value that is not `0`, `90`, `180` or `270`; the
    /// caller decides whether that is a malformed widget or a value to ignore.
    pub fn rotation_degrees(degrees: i64) -> Option<Self> {
        match degrees.rem_euclid(360) {
            0 => Some(Self::IDENTITY),
            90 => Some(Self::new(0.0, 1.0, -1.0, 0.0, 0.0, 0.0)),
            180 => Some(Self::new(-1.0, 0.0, 0.0, -1.0, 0.0, 0.0)),
            270 => Some(Self::new(0.0, -1.0, 1.0, 0.0, 0.0, 0.0)),
            _ => None,
        }
    }

    /// Returns the transform that rotation `degrees` needs so the rotated box
    /// `width × height` still occupies `width × height` in the first quadrant.
    ///
    /// `width` and `height` are the *appearance-local* extents *after* the
    /// rotation is taken into account: for 90° and 270° that means the caller
    /// passes the widget height as the local width and the widget width as the
    /// local height. The result is `None` for a non-right angle or non-finite
    /// extents.
    pub fn rotation_into_box(degrees: i64, width: f64, height: f64) -> Option<Self> {
        if !width.is_finite() || !height.is_finite() || width <= 0.0 || height <= 0.0 {
            return None;
        }
        let rotation = Self::rotation_degrees(degrees)?;
        let translate = match degrees.rem_euclid(360) {
            0 => Self::IDENTITY,
            90 => Self::translation(height, 0.0),
            180 => Self::translation(width, height),
            270 => Self::translation(0.0, width),
            _ => return None,
        };
        Some(rotation.then(translate))
    }

    /// Returns `true` when all six coefficients are finite.
    pub fn is_finite(self) -> bool {
        [self.a, self.b, self.c, self.d, self.e, self.f]
            .iter()
            .all(|value| value.is_finite())
    }

    /// Returns `true` when the transform is the identity within
    /// [`Matrix::EPSILON`].
    pub fn is_identity(self) -> bool {
        self.is_finite()
            && (self.a - 1.0).abs() <= Self::EPSILON
            && self.b.abs() <= Self::EPSILON
            && self.c.abs() <= Self::EPSILON
            && (self.d - 1.0).abs() <= Self::EPSILON
            && self.e.abs() <= Self::EPSILON
            && self.f.abs() <= Self::EPSILON
    }

    /// Applies the transform to a point.
    pub fn apply(self, x: f64, y: f64) -> (f64, f64) {
        (
            self.a * x + self.c * y + self.e,
            self.b * x + self.d * y + self.f,
        )
    }

    /// Returns the transform that applies `self` first and `next` second.
    pub fn then(self, next: Self) -> Self {
        Self::new(
            next.a * self.a + next.c * self.b,
            next.b * self.a + next.d * self.b,
            next.a * self.c + next.c * self.d,
            next.b * self.c + next.d * self.d,
            next.a * self.e + next.c * self.f + next.e,
            next.b * self.e + next.d * self.f + next.f,
        )
    }

    /// Returns the inverse transform, or `None` when the transform is singular
    /// or non-finite.
    pub fn inverse(self) -> Option<Self> {
        if !self.is_finite() {
            return None;
        }
        let determinant = self.a * self.d - self.b * self.c;
        if determinant.abs() <= Self::EPSILON {
            return None;
        }
        let inverse = 1.0 / determinant;
        Some(Self::new(
            self.d * inverse,
            -self.b * inverse,
            -self.c * inverse,
            self.a * inverse,
            (self.c * self.f - self.d * self.e) * inverse,
            (self.b * self.e - self.a * self.f) * inverse,
        ))
    }

    /// Transforms an axis-aligned rectangle and returns its new axis-aligned
    /// bounds as `(min_x, min_y, max_x, max_y)`.
    ///
    /// Returns `None` when the transform is not finite or the bounds are not
    /// finite.
    pub fn transform_bbox(
        self,
        min_x: f64,
        min_y: f64,
        max_x: f64,
        max_y: f64,
    ) -> Option<(f64, f64, f64, f64)> {
        if !self.is_finite()
            || ![min_x, min_y, max_x, max_y]
                .iter()
                .all(|value| value.is_finite())
        {
            return None;
        }
        let (left, bottom) = (min_x.min(max_x), min_y.min(max_y));
        let (right, top) = (min_x.max(max_x), min_y.max(max_y));
        let corners = [
            self.apply(left, bottom),
            self.apply(right, bottom),
            self.apply(right, top),
            self.apply(left, top),
        ];
        let mut bounds = (
            f64::INFINITY,
            f64::INFINITY,
            f64::NEG_INFINITY,
            f64::NEG_INFINITY,
        );
        for (x, y) in corners {
            if !x.is_finite() || !y.is_finite() {
                return None;
            }
            bounds.0 = bounds.0.min(x);
            bounds.1 = bounds.1.min(y);
            bounds.2 = bounds.2.max(x);
            bounds.3 = bounds.3.max(y);
        }
        Some(bounds)
    }

    /// Returns the `cm` transform that maps an appearance box through
    /// `appearance` into a target rectangle.
    ///
    /// `bbox` is `(min_x, min_y, max_x, max_y)` in appearance space. The
    /// appearance matrix is applied first, the resulting axis-aligned bounds are
    /// then scaled *non-uniformly* to the target extents and translated to
    /// `(left, bottom)`. This is the placement rule of ISO 32000-1 §12.5.5 for
    /// annotation appearances, and the same rule viewers use when a widget's
    /// `/Rect` has a different aspect ratio from its `/BBox`.
    ///
    /// Returns `None` for non-finite values or a degenerate transformed box.
    pub fn fit_bbox(
        appearance: Self,
        bbox: (f64, f64, f64, f64),
        left: f64,
        bottom: f64,
        width: f64,
        height: f64,
    ) -> Option<Self> {
        if !width.is_finite() || !height.is_finite() || width <= 0.0 || height <= 0.0 {
            return None;
        }
        let (x0, y0, x1, y1) = appearance.transform_bbox(bbox.0, bbox.1, bbox.2, bbox.3)?;
        let box_width = x1 - x0;
        let box_height = y1 - y0;
        if box_width <= Self::EPSILON || box_height <= Self::EPSILON {
            return None;
        }
        let scale_x = width / box_width;
        let scale_y = height / box_height;
        Some(
            appearance
                .then(Self::translation(-x0, -y0))
                .then(Self::scale(scale_x, scale_y))
                .then(Self::translation(left, bottom)),
        )
    }
}

#[cfg(test)]
mod tests;
