//! Page geometry primitives.
//!
//! Coordinates follow the PDF default user-space convention: the origin is at
//! the lower-left of the page and the axes grow rightward and upward. All
//! rectangles are validated at construction so that downstream code never has
//! to defend against inverted or non-finite bounds.

use core::fmt;

/// A point in PDF page coordinates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Point {
    /// Horizontal coordinate.
    pub x: f32,
    /// Vertical coordinate.
    pub y: f32,
}

impl Point {
    /// Creates a point from its coordinates.
    pub fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }

    /// Returns `true` when both coordinates are finite.
    pub fn is_finite(self) -> bool {
        self.x.is_finite() && self.y.is_finite()
    }
}

/// Errors produced while validating [`Rect`] and [`PageGeometry`] values.
#[derive(Debug, Clone, PartialEq)]
pub enum GeometryError {
    /// A coordinate was infinite or `NaN`.
    NonFiniteCoordinate {
        /// Name of the offending field.
        field: &'static str,
        /// The rejected value.
        value: f32,
    },
    /// A rectangle's upper corner lay below or left of its lower corner.
    InvertedRect {
        /// Lower-left x.
        x0: f32,
        /// Lower-left y.
        y0: f32,
        /// Upper-right x.
        x1: f32,
        /// Upper-right y.
        y1: f32,
    },
    /// A page index was not smaller than the document's page count.
    PageOutOfRange {
        /// The rejected zero-based index.
        index: usize,
        /// Number of pages in the document.
        page_count: usize,
    },
    /// A page rotation was not 0, 90, 180, or 270 degrees.
    InvalidRotation(u16),
    /// A crop box extended outside the media box.
    CropBoxOutsideMediaBox,
}

impl fmt::Display for GeometryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NonFiniteCoordinate { field, value } => {
                write!(f, "coordinate `{field}` must be finite, got {value}")
            }
            Self::InvertedRect { x0, y0, x1, y1 } => {
                write!(f, "invalid rect ({x0}, {y0}) - ({x1}, {y1})")
            }
            Self::PageOutOfRange { index, page_count } => {
                write!(
                    f,
                    "page index {index} is outside a document of {page_count} pages"
                )
            }
            Self::InvalidRotation(deg) => {
                write!(f, "page rotation {deg} is not 0, 90, 180, or 270 degrees")
            }
            Self::CropBoxOutsideMediaBox => {
                write!(f, "crop box is not contained within the media box")
            }
        }
    }
}

impl std::error::Error for GeometryError {}

/// A finite, axis-aligned rectangle whose upper corner is never below or left
/// of its lower corner.
///
/// Instances are created through [`Rect::new`] or [`Rect::from_points`], which
/// enforce those invariants.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
}

impl Rect {
    /// Creates a rectangle from its lower-left and upper-right corners.
    ///
    /// Returns [`GeometryError::NonFiniteCoordinate`] if any component is not
    /// finite, or [`GeometryError::InvertedRect`] if the extent is inverted.
    pub fn new(x0: f32, y0: f32, x1: f32, y1: f32) -> Result<Self, GeometryError> {
        for (field, value) in [("x0", x0), ("y0", y0), ("x1", x1), ("y1", y1)] {
            if !value.is_finite() {
                return Err(GeometryError::NonFiniteCoordinate { field, value });
            }
        }
        if x1 < x0 || y1 < y0 {
            return Err(GeometryError::InvertedRect { x0, y0, x1, y1 });
        }
        Ok(Self { x0, y0, x1, y1 })
    }

    /// Creates the smallest rectangle containing two points, in either order.
    pub fn from_points(a: Point, b: Point) -> Result<Self, GeometryError> {
        Self::new(a.x.min(b.x), a.y.min(b.y), a.x.max(b.x), a.y.max(b.y))
    }

    /// Creates a finite, normalized rectangle without the possibility of failure.
    ///
    /// Non-finite coordinates become zero and the corner order is normalized so
    /// that the lower corner never exceeds the upper corner. This is useful for
    /// display geometry derived from already-validated annotation dictionaries.
    pub fn saturating(x0: f32, y0: f32, x1: f32, y1: f32) -> Self {
        let clean = |value: f32| if value.is_finite() { value } else { 0.0 };
        let (a, b) = (clean(x0), clean(x1));
        let (c, d) = (clean(y0), clean(y1));
        Self {
            x0: a.min(b),
            y0: c.min(d),
            x1: a.max(b),
            y1: c.max(d),
        }
    }

    /// Lower-left x coordinate.
    pub fn x0(&self) -> f32 {
        self.x0
    }

    /// Lower-left y coordinate.
    pub fn y0(&self) -> f32 {
        self.y0
    }

    /// Upper-right x coordinate.
    pub fn x1(&self) -> f32 {
        self.x1
    }

    /// Upper-right y coordinate.
    pub fn y1(&self) -> f32 {
        self.y1
    }

    /// Returns a rectangle inset by `dx` horizontally and `dy` vertically on
    /// each side, or `None` when the inset would collapse the rectangle.
    pub fn inset(&self, dx: f32, dy: f32) -> Option<Self> {
        let (x0, x1) = (self.x0 + dx, self.x1 - dx);
        let (y0, y1) = (self.y0 + dy, self.y1 - dy);
        if x0 >= x1 || y0 >= y1 {
            None
        } else {
            Some(Self { x0, y0, x1, y1 })
        }
    }

    /// Width of the rectangle.
    pub fn width(&self) -> f32 {
        self.x1 - self.x0
    }

    /// Height of the rectangle.
    pub fn height(&self) -> f32 {
        self.y1 - self.y0
    }

    /// Returns `true` when the rectangle encloses no area.
    pub fn is_empty(&self) -> bool {
        self.width() == 0.0 || self.height() == 0.0
    }

    /// Returns `true` when `point` lies on or inside the rectangle.
    pub fn contains_point(&self, point: Point) -> bool {
        point.x >= self.x0 && point.x <= self.x1 && point.y >= self.y0 && point.y <= self.y1
    }

    /// Returns `true` when `other` lies wholly inside this rectangle.
    pub fn contains_rect(&self, other: &Rect) -> bool {
        self.x0 <= other.x0 && other.x1 <= self.x1 && self.y0 <= other.y0 && other.y1 <= self.y1
    }

    /// Returns `true` when the two rectangles share a positive-area overlap.
    ///
    /// Rectangles that merely touch at an edge or corner do not intersect.
    pub fn intersects(&self, other: &Rect) -> bool {
        self.x0 < other.x1 && other.x0 < self.x1 && self.y0 < other.y1 && other.y0 < self.y1
    }

    /// Returns the positive-area overlap of the two rectangles, or `None` when
    /// they are disjoint.
    pub fn intersection(&self, other: &Rect) -> Option<Rect> {
        if !self.intersects(other) {
            return None;
        }
        let (x0, y0) = (self.x0.max(other.x0), self.y0.max(other.y0));
        let (x1, y1) = (self.x1.min(other.x1), self.y1.min(other.y1));
        // Both inputs are validated and `intersects` guarantees x1 > x0 and
        // y1 > y0, so this construction cannot fail.
        Some(Rect { x0, y0, x1, y1 })
    }

    /// Returns the smallest rectangle containing both inputs.
    pub fn union(&self, other: &Rect) -> Rect {
        Rect {
            x0: self.x0.min(other.x0),
            y0: self.y0.min(other.y0),
            x1: self.x1.max(other.x1),
            y1: self.y1.max(other.y1),
        }
    }
}

/// A zero-based page index.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PageIndex(usize);

impl PageIndex {
    /// Creates a page index from a zero-based number.
    pub fn new(value: usize) -> Self {
        Self(value)
    }

    /// Returns the zero-based page number.
    pub fn get(self) -> usize {
        self.0
    }

    /// Returns `true` when the index refers to a page in a document of the
    /// given size.
    pub fn is_valid_for(self, page_count: usize) -> bool {
        self.0 < page_count
    }

    /// Validates this index against a page count.
    pub fn checked_for(self, page_count: usize) -> Result<Self, GeometryError> {
        if self.is_valid_for(page_count) {
            Ok(self)
        } else {
            Err(GeometryError::PageOutOfRange {
                index: self.0,
                page_count,
            })
        }
    }
}

/// Geometry of a single page: its media box, visible crop box, and rotation.
#[derive(Clone, Debug, PartialEq)]
pub struct PageGeometry {
    index: PageIndex,
    media_box: Rect,
    crop_box: Rect,
    rotation_degrees: u16,
}

impl PageGeometry {
    /// Creates a page whose crop box equals its media box and whose rotation
    /// is zero.
    pub fn new(index: PageIndex, media_box: Rect) -> Result<Self, GeometryError> {
        Self::with_crop_box(index, media_box, media_box, 0)
    }

    /// Creates a page with an explicit crop box and rotation.
    ///
    /// The crop box must lie within the media box, and the rotation must be
    /// one of 0, 90, 180, or 270 degrees.
    pub fn with_crop_box(
        index: PageIndex,
        media_box: Rect,
        crop_box: Rect,
        rotation_degrees: u16,
    ) -> Result<Self, GeometryError> {
        if !matches!(rotation_degrees, 0 | 90 | 180 | 270) {
            return Err(GeometryError::InvalidRotation(rotation_degrees));
        }
        if !media_box.contains_rect(&crop_box) {
            return Err(GeometryError::CropBoxOutsideMediaBox);
        }
        Ok(Self {
            index,
            media_box,
            crop_box,
            rotation_degrees,
        })
    }

    /// Index of this page.
    pub fn index(&self) -> PageIndex {
        self.index
    }

    /// Full media box of the page.
    pub fn media_box(&self) -> Rect {
        self.media_box
    }

    /// Crop box that bounds visible content.
    pub fn crop_box(&self) -> Rect {
        self.crop_box
    }

    /// Page rotation in degrees.
    pub fn rotation_degrees(&self) -> u16 {
        self.rotation_degrees
    }

    /// Visible bounds of the page (the crop box).
    pub fn visible_bounds(&self) -> Rect {
        self.crop_box
    }

    /// Returns `true` when `point` lies within the visible page bounds.
    pub fn contains_point(&self, point: Point) -> bool {
        self.crop_box.contains_point(point)
    }

    /// Returns `true` when `rect` overlaps the visible page bounds.
    pub fn intersects(&self, rect: &Rect) -> bool {
        self.crop_box.intersects(rect)
    }

    /// Clamps `rect` to the visible page bounds, returning `None` when it is
    /// entirely outside the page.
    pub fn clamp_rect(&self, rect: &Rect) -> Option<Rect> {
        rect.intersection(&self.crop_box)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x0: f32, y0: f32, x1: f32, y1: f32) -> Rect {
        Rect::new(x0, y0, x1, y1).expect("test rect must be valid")
    }

    #[test]
    fn rect_saturating_normalizes_corners_and_non_finite_values() {
        let r = Rect::saturating(f32::NAN, 5.0, f32::INFINITY, 2.0);
        assert_eq!((r.x0(), r.y0(), r.x1(), r.y1()), (0.0, 2.0, 0.0, 5.0));
        let ordered = Rect::saturating(10.0, 20.0, 0.0, 5.0);
        assert_eq!(
            (ordered.x0(), ordered.y0(), ordered.x1(), ordered.y1()),
            (0.0, 5.0, 10.0, 20.0)
        );
    }

    #[test]
    fn rect_intersection_returns_overlap_and_none_for_disjoint() {
        let a = rect(0.0, 0.0, 10.0, 10.0);
        let b = rect(5.0, 5.0, 15.0, 15.0);
        let overlap = a.intersection(&b).expect("rects overlap");
        assert_eq!(
            (overlap.x0(), overlap.y0(), overlap.x1(), overlap.y1()),
            (5.0, 5.0, 10.0, 10.0)
        );
        assert!(a.intersects(&b));

        let disjoint = rect(20.0, 20.0, 30.0, 30.0);
        assert!(!a.intersects(&disjoint));
        assert!(a.intersection(&disjoint).is_none());
    }

    #[test]
    fn edge_touching_rects_do_not_intersect() {
        let a = rect(0.0, 0.0, 10.0, 10.0);
        let touching = rect(10.0, 0.0, 20.0, 10.0);
        assert!(!a.intersects(&touching));
        assert!(a.intersection(&touching).is_none());
    }

    #[test]
    fn rect_union_covers_both_inputs() {
        let a = rect(0.0, 0.0, 4.0, 6.0);
        let b = rect(-2.0, 3.0, 9.0, 11.0);
        let u = a.union(&b);
        assert_eq!((u.x0(), u.y0(), u.x1(), u.y1()), (-2.0, 0.0, 9.0, 11.0));
        assert!(u.contains_rect(&a));
        assert!(u.contains_rect(&b));
    }

    #[test]
    fn rect_contains_point_is_inclusive() {
        let r = rect(0.0, 0.0, 10.0, 10.0);
        assert!(r.contains_point(Point::new(0.0, 0.0)));
        assert!(r.contains_point(Point::new(10.0, 10.0)));
        assert!(r.contains_point(Point::new(5.0, 2.0)));
        assert!(!r.contains_point(Point::new(11.0, 5.0)));
    }

    #[test]
    fn invalid_rects_are_rejected() {
        assert!(matches!(
            Rect::new(10.0, 0.0, 0.0, 10.0),
            Err(GeometryError::InvertedRect { .. })
        ));
        assert!(matches!(
            Rect::new(0.0, 0.0, f32::NAN, 10.0),
            Err(GeometryError::NonFiniteCoordinate { .. })
        ));
        assert!(matches!(
            Rect::new(0.0, 0.0, f32::INFINITY, 10.0),
            Err(GeometryError::NonFiniteCoordinate { .. })
        ));
    }

    #[test]
    fn page_geometry_validates_crop_box_and_rotation() {
        let media = rect(0.0, 0.0, 612.0, 792.0);
        let crop = rect(10.0, 10.0, 600.0, 700.0);
        let page = PageGeometry::with_crop_box(PageIndex::new(0), media, crop, 90)
            .expect("page must be valid");
        assert_eq!(page.rotation_degrees(), 90);
        assert_eq!(page.visible_bounds(), crop);

        let outside = rect(0.0, 0.0, 700.0, 792.0);
        assert_eq!(
            PageGeometry::with_crop_box(PageIndex::new(0), media, outside, 0),
            Err(GeometryError::CropBoxOutsideMediaBox)
        );
        assert_eq!(
            PageGeometry::with_crop_box(PageIndex::new(0), media, crop, 45),
            Err(GeometryError::InvalidRotation(45))
        );
    }

    #[test]
    fn page_geometry_clamps_rect_to_visible_bounds() {
        let media = rect(0.0, 0.0, 612.0, 792.0);
        let crop = rect(100.0, 100.0, 500.0, 500.0);
        let page = PageGeometry::with_crop_box(PageIndex::new(2), media, crop, 0)
            .expect("page must be valid");

        let partial = rect(50.0, 50.0, 200.0, 200.0);
        let clamped = page.clamp_rect(&partial).expect("partial overlap");
        assert_eq!(
            (clamped.x0(), clamped.y0(), clamped.x1(), clamped.y1()),
            (100.0, 100.0, 200.0, 200.0)
        );

        let outside = rect(0.0, 0.0, 50.0, 50.0);
        assert!(page.clamp_rect(&outside).is_none());
    }

    #[test]
    fn page_index_range_check() {
        assert!(PageIndex::new(1).is_valid_for(3));
        assert!(!PageIndex::new(3).is_valid_for(3));
        assert_eq!(
            PageIndex::new(3).checked_for(3),
            Err(GeometryError::PageOutOfRange {
                index: 3,
                page_count: 3
            })
        );
    }
}
