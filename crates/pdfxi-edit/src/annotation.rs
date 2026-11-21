//! Annotation model and helpers.
//!
//! An [`Annotation`] is a markup or graphical object attached to a single page.
//! The model is deliberately renderer-agnostic: it stores geometry, appearance
//! metadata, and flags, while hit-testing and clamping operate on the page
//! geometry defined in [`crate::geometry`].

use core::fmt;

use crate::geometry::{GeometryError, PageGeometry, PageIndex, Point, Rect};

/// An RGBA color used for annotation appearance.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Color {
    r: u8,
    g: u8,
    b: u8,
    a: u8,
}

impl Color {
    /// Opaque black, the default annotation color.
    pub const BLACK: Color = Color {
        r: 0,
        g: 0,
        b: 0,
        a: 255,
    };

    /// Creates an opaque RGB color.
    pub fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b, a: 255 }
    }

    /// Sets the alpha channel, returning the modified color.
    pub fn with_alpha(mut self, a: u8) -> Self {
        self.a = a;
        self
    }

    /// Red channel.
    pub fn r(&self) -> u8 {
        self.r
    }

    /// Green channel.
    pub fn g(&self) -> u8 {
        self.g
    }

    /// Blue channel.
    pub fn b(&self) -> u8 {
        self.b
    }

    /// Alpha channel.
    pub fn a(&self) -> u8 {
        self.a
    }
}

/// Opaque identifier for an annotation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AnnotationId(u64);

impl AnnotationId {
    /// Creates an annotation identifier from a numeric value.
    pub fn new(value: u64) -> Self {
        Self(value)
    }

    /// Returns the numeric value of the identifier.
    pub fn get(self) -> u64 {
        self.0
    }
}

/// The kind of an annotation, covering the Acrobat XI review and drawing
/// toolset.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AnnotationKind {
    /// A sticky-note comment.
    Note,
    /// A text highlight markup.
    Highlight,
    /// An underline markup.
    Underline,
    /// A strikeout markup.
    Strikeout,
    /// A wavy-line (squiggly) markup.
    Squiggly,
    /// A free-standing text box.
    FreeText,
    /// A callout with a leader line and text box.
    Callout,
    /// A straight line.
    Line,
    /// An arrow (a line with an arrowhead).
    Arrow,
    /// A rectangle outline.
    Rectangle,
    /// An ellipse outline.
    Oval,
    /// A closed polyline.
    Polygon,
    /// A freehand cloud shape.
    Cloud,
    /// A freehand ink (pencil) stroke.
    Pencil,
    /// A graphical stamp.
    Stamp,
    /// A file attachment annotation.
    FileAttachment,
}

impl AnnotationKind {
    /// Returns `true` for markup annotations that are anchored to page text.
    pub fn is_markup(self) -> bool {
        matches!(
            self,
            Self::Highlight | Self::Underline | Self::Strikeout | Self::Squiggly
        )
    }

    /// Map a PDF annotation `/Subtype` name to the modelled toolset kind.
    ///
    /// Names are matched case-insensitively. PDF has no distinct subtype for an
    /// arrow or an oval: arrows are `/Line` annotations and ellipses are
    /// `/Circle`, so those names map to [`Self::Line`] and [`Self::Oval`].
    /// Returns `None` for a subtype this toolset does not model, such as
    /// `/Link`, `/Widget` or `/Popup`.
    pub fn from_pdf_name(name: &str) -> Option<Self> {
        let kind = if name.eq_ignore_ascii_case("Text") {
            Self::Note
        } else if name.eq_ignore_ascii_case("Highlight") {
            Self::Highlight
        } else if name.eq_ignore_ascii_case("Underline") {
            Self::Underline
        } else if name.eq_ignore_ascii_case("StrikeOut") {
            Self::Strikeout
        } else if name.eq_ignore_ascii_case("Squiggly") {
            Self::Squiggly
        } else if name.eq_ignore_ascii_case("FreeText") {
            Self::FreeText
        } else if name.eq_ignore_ascii_case("Line") {
            Self::Line
        } else if name.eq_ignore_ascii_case("Square") {
            Self::Rectangle
        } else if name.eq_ignore_ascii_case("Circle") {
            Self::Oval
        } else if name.eq_ignore_ascii_case("Polygon") {
            Self::Polygon
        } else if name.eq_ignore_ascii_case("Ink") {
            Self::Pencil
        } else if name.eq_ignore_ascii_case("Stamp") {
            Self::Stamp
        } else if name.eq_ignore_ascii_case("FileAttachment") {
            Self::FileAttachment
        } else {
            return None;
        };
        Some(kind)
    }

    /// Returns a stable lowercase name for diagnostics and serialization.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Note => "note",
            Self::Highlight => "highlight",
            Self::Underline => "underline",
            Self::Strikeout => "strikeout",
            Self::Squiggly => "squiggly",
            Self::FreeText => "free_text",
            Self::Callout => "callout",
            Self::Line => "line",
            Self::Arrow => "arrow",
            Self::Rectangle => "rectangle",
            Self::Oval => "oval",
            Self::Polygon => "polygon",
            Self::Cloud => "cloud",
            Self::Pencil => "pencil",
            Self::Stamp => "stamp",
            Self::FileAttachment => "file_attachment",
        }
    }
}

impl fmt::Display for AnnotationKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Errors produced while constructing or mutating an [`Annotation`].
#[derive(Debug, Clone, PartialEq)]
pub enum AnnotationError {
    /// Opacity was outside the inclusive range `0.0..=1.0`, or was not finite.
    InvalidOpacity(f32),
    /// Two annotations referred to different pages.
    PageMismatch {
        /// Page of the first annotation.
        annotation_page: usize,
        /// Page of the other annotation.
        other_page: usize,
    },
    /// A page operation had no overlap with the annotation.
    OutsidePage(PageIndex),
    /// An underlying geometry value was invalid.
    Geometry(GeometryError),
}

impl fmt::Display for AnnotationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidOpacity(value) => {
                write!(
                    f,
                    "annotation opacity must be finite and in 0.0..=1.0, got {value}"
                )
            }
            Self::PageMismatch {
                annotation_page,
                other_page,
            } => {
                write!(
                    f,
                    "annotation page {annotation_page} differs from page {other_page}"
                )
            }
            Self::OutsidePage(index) => {
                write!(f, "annotation does not overlap page {}", index.get())
            }
            Self::Geometry(err) => write!(f, "annotation geometry error: {err}"),
        }
    }
}

impl std::error::Error for AnnotationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Geometry(err) => Some(err),
            _ => None,
        }
    }
}

/// A markup or graphical object attached to a page.
///
/// All invariants are enforced at construction: the rectangle is valid, the
/// opacity is finite and within `0.0..=1.0`, and the flags start disabled.
/// Appearance and metadata can be adjusted with the `with_*` builders.
#[derive(Clone, Debug, PartialEq)]
pub struct Annotation {
    id: AnnotationId,
    kind: AnnotationKind,
    page: PageIndex,
    rect: Rect,
    author: String,
    contents: String,
    color: Color,
    opacity: f32,
    locked: bool,
    read_only: bool,
}

impl Annotation {
    /// Creates an annotation with opaque black color, full opacity, and both
    /// flags disabled.
    pub fn new(
        id: AnnotationId,
        kind: AnnotationKind,
        page: PageIndex,
        rect: Rect,
        author: impl Into<String>,
        contents: impl Into<String>,
    ) -> Result<Self, AnnotationError> {
        Ok(Self {
            id,
            kind,
            page,
            rect,
            author: author.into(),
            contents: contents.into(),
            color: Color::BLACK,
            opacity: 1.0,
            locked: false,
            read_only: false,
        })
    }

    /// Sets the annotation color.
    pub fn with_color(mut self, color: Color) -> Self {
        self.color = color;
        self
    }

    /// Sets the opacity, rejecting values outside `0.0..=1.0` or non-finite.
    pub fn with_opacity(mut self, opacity: f32) -> Result<Self, AnnotationError> {
        if !opacity.is_finite() || !(0.0..=1.0).contains(&opacity) {
            return Err(AnnotationError::InvalidOpacity(opacity));
        }
        self.opacity = opacity;
        Ok(self)
    }

    /// Sets the author name.
    pub fn with_author(mut self, author: impl Into<String>) -> Self {
        self.author = author.into();
        self
    }

    /// Sets the comment contents.
    pub fn with_contents(mut self, contents: impl Into<String>) -> Self {
        self.contents = contents.into();
        self
    }

    /// Sets whether the annotation position is locked.
    pub fn with_locked(mut self, locked: bool) -> Self {
        self.locked = locked;
        self
    }

    /// Sets whether the annotation contents are read-only.
    pub fn with_read_only(mut self, read_only: bool) -> Self {
        self.read_only = read_only;
        self
    }

    /// Annotation identifier.
    pub fn id(&self) -> AnnotationId {
        self.id
    }

    /// Annotation kind.
    pub fn kind(&self) -> AnnotationKind {
        self.kind
    }

    /// Page the annotation is attached to.
    pub fn page(&self) -> PageIndex {
        self.page
    }

    /// Axis-aligned bounds of the annotation.
    pub fn rect(&self) -> Rect {
        self.rect
    }

    /// Author name.
    pub fn author(&self) -> &str {
        &self.author
    }

    /// Comment contents.
    pub fn contents(&self) -> &str {
        &self.contents
    }

    /// Appearance color.
    pub fn color(&self) -> Color {
        self.color
    }

    /// Opacity in `0.0..=1.0`.
    pub fn opacity(&self) -> f32 {
        self.opacity
    }

    /// Whether the annotation position is locked.
    pub fn is_locked(&self) -> bool {
        self.locked
    }

    /// Whether the annotation contents are read-only.
    pub fn is_read_only(&self) -> bool {
        self.read_only
    }

    /// Returns `true` when `point` lies on or inside the annotation bounds.
    pub fn hit_test(&self, point: Point) -> bool {
        self.rect.contains_point(point)
    }

    /// Returns `true` when `point` lies within `tolerance` units of the bounds.
    ///
    /// Negative tolerances are clamped to zero, reducing this to [`Self::hit_test`].
    pub fn hit_test_with_tolerance(&self, point: Point, tolerance: f32) -> bool {
        let t = tolerance.max(0.0);
        point.x >= self.rect.x0() - t
            && point.x <= self.rect.x1() + t
            && point.y >= self.rect.y0() - t
            && point.y <= self.rect.y1() + t
    }

    /// Returns the annotation's bounds.
    pub fn bounds(&self) -> Rect {
        self.rect
    }

    /// Returns the portion of the annotation that lies within `page`'s visible
    /// bounds, or `None` when the annotation is entirely off-page.
    pub fn clamp_to_page(&self, page: &PageGeometry) -> Option<Rect> {
        page.clamp_rect(&self.rect)
    }

    /// Clamps the annotation to `page` in place.
    ///
    /// Fails with [`AnnotationError::PageMismatch`] if the page index differs
    /// and [`AnnotationError::OutsidePage`] if there is no overlap.
    pub fn clamp_to_page_in_place(&mut self, page: &PageGeometry) -> Result<(), AnnotationError> {
        if self.page != page.index() {
            return Err(AnnotationError::PageMismatch {
                annotation_page: self.page.get(),
                other_page: page.index().get(),
            });
        }
        match self.clamp_to_page(page) {
            Some(rect) => {
                self.rect = rect;
                Ok(())
            }
            None => Err(AnnotationError::OutsidePage(self.page)),
        }
    }
}

/// Returns the smallest rectangle containing all annotations in `annotations`,
/// or `None` when the iterator is empty.
pub fn bounds_union<'a, I>(annotations: I) -> Option<Rect>
where
    I: IntoIterator<Item = &'a Annotation>,
{
    let mut iter = annotations.into_iter();
    let first = iter.next()?.rect();
    Some(iter.fold(first, |acc, annotation| acc.union(&annotation.rect())))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x0: f32, y0: f32, x1: f32, y1: f32) -> Rect {
        Rect::new(x0, y0, x1, y1).expect("test rect must be valid")
    }

    fn note(id: u64, rect: Rect) -> Annotation {
        Annotation::new(
            AnnotationId::new(id),
            AnnotationKind::Note,
            PageIndex::new(0),
            rect,
            "alice",
            "review",
        )
        .expect("test annotation must be valid")
    }

    #[test]
    fn annotation_hit_test_within_and_outside_rect() {
        let a = note(1, rect(10.0, 20.0, 30.0, 40.0));
        assert!(a.hit_test(Point::new(15.0, 25.0)));
        assert!(a.hit_test(Point::new(10.0, 20.0)));
        assert!(!a.hit_test(Point::new(31.0, 25.0)));
        assert!(!a.hit_test(Point::new(15.0, 41.0)));
        assert!(a.hit_test_with_tolerance(Point::new(9.0, 20.0), 2.0));
        assert!(!a.hit_test_with_tolerance(Point::new(9.0, 20.0), 0.0));
    }

    #[test]
    fn annotation_bounds_union_covers_all_rects() {
        let a = note(1, rect(0.0, 0.0, 4.0, 6.0));
        let b = note(2, rect(-2.0, 3.0, 9.0, 11.0));
        let union = bounds_union([&a, &b]).expect("non-empty iterator");
        assert_eq!(
            (union.x0(), union.y0(), union.x1(), union.y1()),
            (-2.0, 0.0, 9.0, 11.0)
        );
        assert!(bounds_union(std::iter::empty::<&Annotation>()).is_none());
    }

    #[test]
    fn annotation_clamps_to_page_and_rejects_wrong_page() {
        let page = PageGeometry::with_crop_box(
            PageIndex::new(0),
            rect(0.0, 0.0, 612.0, 792.0),
            rect(100.0, 100.0, 500.0, 500.0),
            0,
        )
        .expect("page must be valid");
        let mut a = note(1, rect(50.0, 50.0, 200.0, 200.0));
        let clamped = a.clamp_to_page(&page).expect("partial overlap");
        assert_eq!(
            (clamped.x0(), clamped.y0(), clamped.x1(), clamped.y1()),
            (100.0, 100.0, 200.0, 200.0)
        );
        a.clamp_to_page_in_place(&page).expect("in-place clamp");
        assert_eq!(a.rect(), clamped);

        let other_page = PageGeometry::new(PageIndex::new(1), rect(0.0, 0.0, 100.0, 100.0))
            .expect("page must be valid");
        assert!(matches!(
            a.clamp_to_page_in_place(&other_page),
            Err(AnnotationError::PageMismatch { .. })
        ));
    }

    #[test]
    fn annotation_rejects_invalid_opacity() {
        let a = note(1, rect(0.0, 0.0, 1.0, 1.0));
        assert!(matches!(
            a.clone().with_opacity(-0.1),
            Err(AnnotationError::InvalidOpacity(_))
        ));
        assert!(matches!(
            a.clone().with_opacity(1.1),
            Err(AnnotationError::InvalidOpacity(_))
        ));
        assert!(matches!(
            a.clone().with_opacity(f32::NAN),
            Err(AnnotationError::InvalidOpacity(_))
        ));
        assert!((a.with_opacity(0.5).expect("valid opacity").opacity() - 0.5).abs() < f32::EPSILON);
    }

    #[test]
    fn annotation_kinds_cover_full_toolset() {
        let all = [
            AnnotationKind::Note,
            AnnotationKind::Highlight,
            AnnotationKind::Underline,
            AnnotationKind::Strikeout,
            AnnotationKind::Squiggly,
            AnnotationKind::FreeText,
            AnnotationKind::Callout,
            AnnotationKind::Line,
            AnnotationKind::Arrow,
            AnnotationKind::Rectangle,
            AnnotationKind::Oval,
            AnnotationKind::Polygon,
            AnnotationKind::Cloud,
            AnnotationKind::Pencil,
            AnnotationKind::Stamp,
            AnnotationKind::FileAttachment,
        ];
        assert_eq!(all.len(), 16);
        assert_eq!(AnnotationKind::Highlight.as_str(), "highlight");
        assert!(AnnotationKind::Highlight.is_markup());
        assert!(!AnnotationKind::Note.is_markup());
    }

    #[test]
    fn pdf_subtype_names_map_to_toolset_kinds() {
        assert_eq!(
            AnnotationKind::from_pdf_name("Underline"),
            Some(AnnotationKind::Underline)
        );
        assert_eq!(
            AnnotationKind::from_pdf_name("StrikeOut"),
            Some(AnnotationKind::Strikeout)
        );
        assert_eq!(
            AnnotationKind::from_pdf_name("ink"),
            Some(AnnotationKind::Pencil)
        );
        assert_eq!(
            AnnotationKind::from_pdf_name("Square"),
            Some(AnnotationKind::Rectangle)
        );
        assert_eq!(
            AnnotationKind::from_pdf_name("Circle"),
            Some(AnnotationKind::Oval)
        );
        assert_eq!(
            AnnotationKind::from_pdf_name("Stamp"),
            Some(AnnotationKind::Stamp)
        );
        assert_eq!(AnnotationKind::from_pdf_name("Link"), None);
        assert_eq!(AnnotationKind::from_pdf_name(""), None);
    }
}
