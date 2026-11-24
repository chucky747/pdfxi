//! Comment and markup editing on a real [`pdfxi_core::Document`].
//!
//! Unlike the renderer-agnostic models elsewhere in this crate, this module
//! mutates the typed core document directly. A note or highlight becomes a real
//! annotation dictionary in the page's `/Annots` array, carrying a form XObject
//! appearance stream so another viewer (PDFium, Acrobat, a printer) draws it
//! without having to synthesise an appearance itself.
//!
//! The module also reads annotations back ([`list_annotations`]) and exchanges
//! them as XFDF ([`export_xfdf`] / [`import_xfdf`]) for the shared-review and
//! Comment & Review acceptance items.
//!
//! # Write model
//!
//! Every operation is copy-on-write: it stages objects through
//! [`pdfxi_core::Document::set_object`] and never mutates the source bytes.
//! Because the core document exposes only its original objects, each operation
//! first builds an in-memory view of the current state by appending the staged
//! changes with [`pdfxi_core::Document::save_incremental`] and reparsing that
//! buffer. This keeps repeated edits on one document correct without a private
//! core accessor; [`DocumentSession`](https://docs.rs/pdfxi-document) reparses
//! after every edit anyway, so the cost matches the session write path.
//!
//! # What a written annotation contains
//!
//! Every kind is written with `/Type /Annot`, its `/Subtype`, a normalized
//! `/Rect`, `/Contents`, `/T` (author), `/NM` (a unique identifier), `/M` (the
//! PDF date), a `/C` colour array, `/CA` opacity, `/F 4` (print) and
//! `/AP << /N <form XObject> >>`. The form carries `/BBox`, `/Resources` and a
//! content stream: a vector note icon for `/Text`, a translucent fill for
//! `/Highlight`, a stroked line under or through each quad for `/Underline` and
//! `/StrikeOut`, stroked point paths for `/Ink`, a stroked line with an open
//! arrowhead for `/Line`, a stroked or filled rectangle for `/Square`, a Bezier
//! ellipse for `/Circle`, a closed path for `/Polygon`, and a bordered,
//! lettered label for `/Stamp`. Text markup annotations also carry
//! `/QuadPoints`, an ink annotation carries `/InkList`, a line carries `/L` and
//! `/LE`, and a polygon carries `/Vertices`, as required by ISO 32000-1.
//!
//! [`DocumentSession`]: https://docs.rs/pdfxi-document

use std::time::{SystemTime, UNIX_EPOCH};

use pdfxi_core::{
    Dictionary, Document, Object, ObjectId, Rectangle, decode_text_string, encode_text_string,
};
use thiserror::Error;

use crate::annotation::AnnotationKind;
use crate::geometry::{GeometryError, Point, Rect};
use crate::header_footer::escape_literal;

/// Side length of the sticky-note icon, in default user space units.
pub const NOTE_ICON_SIZE: f32 = 24.0;

/// Maximum number of quads accepted by one text-markup annotation.
///
/// The cap bounds the `/QuadPoints` array and the appearance stream so a hostile
/// or buggy caller cannot make one annotation allocate without limit.
pub const MAX_MARKUP_QUADS: usize = 4096;

/// Maximum number of separate strokes accepted by one ink annotation.
pub const MAX_INK_PATHS: usize = 256;

/// Maximum total number of points accepted by one ink annotation.
pub const MAX_INK_POINTS: usize = 8192;

/// Minimum ink stroke width in default user space units.
pub const MIN_INK_STROKE_WIDTH: f32 = 0.1;

/// Maximum ink stroke width in default user space units.
pub const MAX_INK_STROKE_WIDTH: f32 = 32.0;

/// Stroke colour of underline, strikeout and ink annotations as an RGB triple.
pub const MARKUP_STROKE_COLOR: [f64; 3] = [0.0, 0.0, 0.0];

/// Stroke width of underline and strikeout markup, in default user space units.
pub const MARKUP_STROKE_WIDTH: f32 = 1.0;

/// Maximum number of vertices accepted by one polygon annotation.
pub const MAX_POLYGON_VERTICES: usize = 4096;

/// Maximum number of characters accepted by one stamp label.
///
/// The cap bounds the appearance content stream and the `/Contents` string so a
/// hostile caller cannot make one annotation allocate without limit.
pub const MAX_STAMP_TEXT_CHARS: usize = 200;

/// Length of one open-arrow leg drawn for a `/Line` annotation, in default user
/// space units.
pub const ARROW_HEAD_LENGTH: f32 = 8.0;

/// Colour of the stamp border and label as an RGB triple in `0.0..=1.0`.
pub const STAMP_COLOR: [f64; 3] = [0.75, 0.1, 0.1];

/// Padding between a stamp's border and its label, in default user space units.
const STAMP_PADDING: f64 = 6.0;

/// The standard `/Name` values accepted for a stamp annotation, in the casing
/// written to the document.
///
/// The list is the standard stamp set from ISO 32000-1 Table 181. A caller may
/// pass any casing; the canonical spelling is written to `/Name`.
pub const STAMP_NAMES: [&str; 14] = [
    "Approved",
    "Experimental",
    "NotApproved",
    "AsIs",
    "Expired",
    "NotForPublicRelease",
    "Confidential",
    "Final",
    "Sold",
    "Departmental",
    "ForComment",
    "TopSecret",
    "Draft",
    "ForPublicRelease",
];

/// Stroke, fill and opacity of a geometric shape annotation.
///
/// The style is plain data. [`add_line_styled`], [`add_square_styled`],
/// [`add_circle_styled`] and [`add_polygon_styled`] validate every component
/// before allocating an object or staging a change, so a non-finite or
/// out-of-range value returns [`AnnotationEditError::InvalidColorComponent`] or
/// [`AnnotationEditError::InvalidOpacity`] and leaves the document unchanged.
/// [`ShapeStyle::default`] is an opaque black outline with no interior fill.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ShapeStyle {
    /// Stroke colour written to `/C`, as three components in `0.0..=1.0`.
    pub stroke_color: [f64; 3],
    /// Interior colour written to `/IC` when [`Some`], or no interior fill.
    pub fill_color: Option<[f64; 3]>,
    /// Border width written to `/BS /W`, in default user space points.
    pub stroke_width: f32,
    /// Opacity written to `/CA`, in `0.0..=1.0`.
    pub opacity: f64,
}

impl Default for ShapeStyle {
    fn default() -> Self {
        Self {
            stroke_color: MARKUP_STROKE_COLOR,
            fill_color: None,
            stroke_width: MARKUP_STROKE_WIDTH,
            opacity: 1.0,
        }
    }
}

/// Fill colour of the note icon as an RGB triple in `0.0..=1.0`.
const NOTE_COLOR: [f64; 3] = [1.0, 0.82, 0.1];

/// Fill colour of a highlight as an RGB triple in `0.0..=1.0`.
const HIGHLIGHT_COLOR: [f64; 3] = [1.0, 1.0, 0.0];

/// Annotation opacity written to `/CA` for highlights.
const HIGHLIGHT_OPACITY: f64 = 0.5;

/// Extra slack, in points, added around an ink path before its annotation
/// rectangle is derived, so the stroked line is fully inside the appearance
/// bounding box.
const INK_RECT_PADDING: f32 = 0.5;

/// One annotation subtype this module writes and reads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AnnotationSubtype {
    /// A sticky-note text comment (`/Subtype /Text`).
    Text,
    /// A text highlight markup (`/Subtype /Highlight`).
    Highlight,
    /// Any other subtype found in an existing document.
    Other(String),
}

impl AnnotationSubtype {
    /// Map a PDF `/Subtype` name to a typed subtype.
    pub fn from_pdf_name(name: &str) -> Self {
        if name.eq_ignore_ascii_case("Text") {
            Self::Text
        } else if name.eq_ignore_ascii_case("Highlight") {
            Self::Highlight
        } else {
            Self::Other(name.to_owned())
        }
    }

    /// Return the `/Subtype` name written to the annotation dictionary.
    pub fn as_pdf_name(&self) -> &str {
        match self {
            Self::Text => "Text",
            Self::Highlight => "Highlight",
            Self::Other(name) => name,
        }
    }
}

/// A review state written to `/State` with `/StateModel /Review`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AnnotationState {
    /// The comment is accepted (resolved).
    Accepted,
    /// The comment is rejected.
    Rejected,
    /// The comment is cancelled.
    Cancelled,
    /// The comment is completed.
    Completed,
    /// The comment is marked.
    Marked,
    /// The comment is unmarked.
    Unmarked,
}

impl AnnotationState {
    /// Return the PDF name written to `/State`.
    pub fn as_pdf_name(self) -> &'static str {
        match self {
            Self::Accepted => "Accepted",
            Self::Rejected => "Rejected",
            Self::Cancelled => "Cancelled",
            Self::Completed => "Completed",
            Self::Marked => "Marked",
            Self::Unmarked => "Unmarked",
        }
    }

    /// Parse a `/State` name, returning `None` for an unknown state.
    pub fn from_pdf_name(name: &str) -> Option<Self> {
        match name {
            "Accepted" => Some(Self::Accepted),
            "Rejected" => Some(Self::Rejected),
            "Cancelled" => Some(Self::Cancelled),
            "Completed" => Some(Self::Completed),
            "Marked" => Some(Self::Marked),
            "Unmarked" => Some(Self::Unmarked),
            _ => None,
        }
    }
}

/// A reference to an annotation this module created or found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AnnotationRef {
    /// The `/NM` identifier.
    pub id: String,
    /// The indirect object holding the annotation dictionary.
    pub object: ObjectId,
    /// Zero-based page that holds the annotation.
    pub page_index: usize,
    /// The annotation subtype.
    pub subtype: AnnotationSubtype,
}

/// A read-only view of one annotation on a page.
#[derive(Clone, Debug, PartialEq)]
pub struct AnnotationView {
    /// The indirect object holding the annotation dictionary.
    pub object: ObjectId,
    /// The `/NM` identifier, or an empty string when the file omits it.
    pub id: String,
    /// The annotation subtype.
    pub subtype: AnnotationSubtype,
    /// The modelled toolset kind, when the subtype is one this crate models.
    ///
    /// Text-markup and drawing subtypes are reported as
    /// [`AnnotationSubtype::Other`] so that the enum stays compatible with the
    /// shell's existing exhaustive match; this field carries the typed kind
    /// (`Underline`, `Strikeout`, `Pencil`, `Stamp`, and so on) that a caller
    /// should branch on.
    pub kind: Option<AnnotationKind>,
    /// The `/Contents` text decoded from its PDF string.
    pub contents: String,
    /// The `/T` author decoded from its PDF string.
    pub author: String,
    /// `/NM` of the `/IRT` parent annotation, when the annotation is a reply.
    pub parent_id: Option<String>,
    /// The raw `/State` name, when present.
    pub state: Option<String>,
    /// The `/Rect` bounds in default user space.
    pub rect: Rect,
    /// The `/M` date string, when present.
    pub date: Option<String>,
    /// One rectangle per `/QuadPoints` group for a text-markup annotation.
    pub quads: Vec<Rect>,
    /// One point list per `/InkList` stroke for an ink annotation.
    pub ink_paths: Vec<Vec<Point>>,
    /// The raw `/C` colour components, when the annotation carries them.
    pub color: Option<Vec<f64>>,
    /// The `/BS /W` stroke width in default user space units, when present.
    pub stroke_width: Option<f32>,
    /// The `/L` endpoints of a `/Line` annotation, as `(from, to)`.
    pub line: Option<(Point, Point)>,
    /// The `/Vertices` of a `/Polygon` annotation, in storage order.
    pub vertices: Vec<Point>,
    /// The `/Name` of a `/Stamp` annotation, when present.
    pub stamp_name: Option<String>,
}

/// Typed errors returned by the annotation library.
#[derive(Debug, Error)]
pub enum AnnotationEditError {
    /// The core parser or writer rejected the document.
    #[error(transparent)]
    Core(#[from] pdfxi_core::Error),
    /// A caller-supplied rectangle or point was invalid.
    #[error(transparent)]
    Geometry(#[from] GeometryError),
    /// The annotation identifier was empty.
    #[error("annotation identifier must not be empty")]
    EmptyId,
    /// The annotation identifier is already used on the page.
    #[error("annotation identifier '{id}' already exists on page {page}")]
    DuplicateId {
        /// The rejected identifier.
        id: String,
        /// The zero-based page index.
        page: usize,
    },
    /// The requested annotation does not exist on the page.
    #[error("annotation '{id}' was not found on page {page}")]
    AnnotationNotFound {
        /// The identifier that was searched for.
        id: String,
        /// The zero-based page index.
        page: usize,
    },
    /// A page's `/Annots` entry was not an array.
    #[error("page {page} /Annots is not an array")]
    MalformedAnnots {
        /// The zero-based page index.
        page: usize,
    },
    /// A page object was not a dictionary.
    #[error("page object {0} is not a dictionary")]
    MalformedPage(ObjectId),
    /// An annotation object was not a dictionary.
    #[error("annotation object {0} is not a dictionary")]
    MalformedAnnotation(ObjectId),
    /// A page lacked a rectangle that an annotation operation requires.
    #[error("page {0} has no valid /MediaBox")]
    MissingPageBounds(usize),
    /// A text markup was given no usable quads.
    #[error("a text markup annotation requires at least one non-empty quad")]
    EmptyQuads,
    /// An ink annotation was given no stroke with a point.
    #[error("an ink annotation requires at least one stroke with at least one point")]
    EmptyInk,
    /// A text markup exceeded the documented quad cap.
    #[error("text markup has {count} quads; the maximum is {limit}")]
    TooManyQuads {
        /// Number of quads supplied.
        count: usize,
        /// The documented cap, [`MAX_MARKUP_QUADS`].
        limit: usize,
    },
    /// An ink annotation exceeded the documented stroke cap.
    #[error("ink annotation has {count} strokes; the maximum is {limit}")]
    TooManyInkPaths {
        /// Number of strokes supplied.
        count: usize,
        /// The documented cap, [`MAX_INK_PATHS`].
        limit: usize,
    },
    /// An ink annotation exceeded the documented point cap.
    #[error("ink annotation has {count} points; the maximum is {limit}")]
    TooManyInkPoints {
        /// Number of points supplied.
        count: usize,
        /// The documented cap, [`MAX_INK_POINTS`].
        limit: usize,
    },
    /// An ink stroke width was non-finite or outside the accepted range.
    #[error("ink stroke width must be finite and in 0.1..=32.0 points, got {value}")]
    InvalidStrokeWidth {
        /// The rejected width.
        value: f32,
    },
    /// A line annotation was given identical endpoints.
    #[error("a line annotation requires two distinct endpoints")]
    DegenerateLine,
    /// A rectangle or ellipse annotation had no area.
    #[error("a shape annotation requires a rectangle with positive width and height")]
    EmptyShapeRect,
    /// A free-text annotation font size was outside the supported range.
    #[error("font size must be finite and in 4..=72 points, got {0}")]
    InvalidFontSize(f32),
    /// A free-text or callout annotation was given blank text.
    #[error("a free-text annotation requires non-blank text")]
    EmptyText,
    /// A callout anchor was not a finite point.
    #[error("the callout anchor must be a finite point")]
    InvalidAnchor,
    /// A file-attachment name was empty or contained a path separator.
    #[error("attachment name {0:?} is not a single valid file name")]
    InvalidAttachmentName(String),
    /// A file-attachment payload exceeded the documented cap.
    #[error("attachment has {bytes} bytes; the maximum is {limit}")]
    AttachmentTooLarge {
        /// Supplied payload size.
        bytes: usize,
        /// The documented cap, [`MAX_ATTACHMENT_BYTES`].
        limit: usize,
    },
    /// A polygon annotation was too small or enclosed no area.
    #[error("a polygon annotation requires at least three vertices and positive-area bounds")]
    DegeneratePolygon,
    /// A polygon annotation exceeded the documented vertex cap.
    #[error("polygon annotation has {count} vertices; the maximum is {limit}")]
    TooManyVertices {
        /// Number of vertices supplied.
        count: usize,
        /// The documented cap, [`MAX_POLYGON_VERTICES`].
        limit: usize,
    },
    /// A shape colour component was non-finite or outside `0.0..=1.0`.
    #[error("shape colour components must be finite and in 0.0..=1.0, got {value}")]
    InvalidColorComponent {
        /// The rejected component.
        value: f64,
    },
    /// A shape opacity was non-finite or outside `0.0..=1.0`.
    #[error("shape opacity must be finite and in 0.0..=1.0, got {value}")]
    InvalidOpacity {
        /// The rejected opacity.
        value: f64,
    },
    /// A stamp name was not one of the standard `/Name` values.
    #[error("'{0}' is not a standard stamp name")]
    InvalidStampName(String),
    /// A stamp label exceeded the documented character cap.
    #[error("stamp text has {chars} characters; the maximum is {limit}")]
    StampTextTooLong {
        /// Character count supplied.
        chars: usize,
        /// The documented cap, [`MAX_STAMP_TEXT_CHARS`].
        limit: usize,
    },
    /// An input coordinate lay outside the page media box.
    #[error("coordinate ({x}, {y}) lies outside the media box of page {page}")]
    CoordinateOutsidePage {
        /// The zero-based page index.
        page: usize,
        /// The rejected x coordinate.
        x: f32,
        /// The rejected y coordinate.
        y: f32,
    },
    /// The PDF object-number space is exhausted.
    #[error("the PDF object number space is exhausted")]
    ObjectSpaceExhausted,
    /// XFDF text was structurally malformed.
    #[error("XFDF parse error at byte {offset}: {detail}")]
    XfdfParse {
        /// Byte offset of the problem.
        offset: usize,
        /// Human-readable explanation.
        detail: String,
    },
    /// An XFDF annotation was missing a required attribute.
    #[error("XFDF annotation at byte {offset} is missing attribute '{name}'")]
    XfdfAttribute {
        /// Byte offset of the annotation element.
        offset: usize,
        /// The missing attribute name.
        name: &'static str,
    },
    /// An XFDF annotation attribute had an invalid value.
    #[error("XFDF annotation attribute '{name}' is invalid: {value}")]
    XfdfValue {
        /// The attribute name.
        name: &'static str,
        /// The rejected value.
        value: String,
    },
    /// An XFDF annotation used a subtype this module does not create.
    #[error("unsupported XFDF annotation subtype '{0}'")]
    UnsupportedSubtype(String),
}

// ---------------------------------------------------------------------------
// Public operations
// ---------------------------------------------------------------------------

/// Add a sticky-note comment to `page_index`.
///
/// The icon is [`NOTE_ICON_SIZE`] points square with its lower-left corner at
/// `point`. `id` becomes the `/NM` identifier and must not already be used on
/// the page. The annotation gets a vector icon appearance stream, so it is
/// visible in viewers that do not synthesise appearances.
///
/// # Errors
///
/// Returns [`AnnotationEditError::EmptyId`] for an empty identifier, a duplicate or
/// missing-page error, a geometry error for a non-finite point, or a core error
/// when the document cannot be read or staged.
pub fn add_text_note(
    document: &mut Document,
    page_index: usize,
    point: Point,
    contents: &str,
    author: &str,
    id: &str,
) -> Result<AnnotationRef, AnnotationEditError> {
    add_text_note_with_date(document, page_index, point, contents, author, id, None)
}

/// Add a highlight covering every rectangle in `quads`.
///
/// The annotation `/Rect` is the union of the quads, `/QuadPoints` carries each
/// quad, and the appearance stream fills them with translucent yellow. The
/// `/NM` identifier is derived from the allocated annotation object number.
///
/// # Errors
///
/// Returns [`AnnotationEditError::EmptyQuads`] when `quads` is empty or every quad is
/// degenerate, a missing-page error, or a core error when the document cannot
/// be read or staged.
pub fn add_highlight(
    document: &mut Document,
    page_index: usize,
    quads: &[Rect],
    contents: &str,
    author: &str,
) -> Result<AnnotationRef, AnnotationEditError> {
    add_highlight_with(document, page_index, quads, contents, author, None, None)
}

/// Add an underline text-markup annotation covering every rectangle in `quads`.
///
/// The annotation carries `/QuadPoints` and a vector appearance that strokes one
/// line just above the bottom edge of each quad, so viewers that do not
/// synthesise appearances still draw it. The `/NM` identifier is derived from the
/// allocated annotation object number.
///
/// # Errors
///
/// Returns [`AnnotationEditError::TooManyQuads`] above [`MAX_MARKUP_QUADS`],
/// [`AnnotationEditError::EmptyQuads`] when every quad is degenerate,
/// [`AnnotationEditError::CoordinateOutsidePage`] for a quad outside the page
/// media box, a missing-page error, or a core error. A rejected input leaves the
/// document unchanged.
pub fn add_underline(
    document: &mut Document,
    page_index: usize,
    quads: &[Rect],
    contents: &str,
    author: &str,
) -> Result<AnnotationRef, AnnotationEditError> {
    add_text_markup_with(
        document,
        page_index,
        quads,
        contents,
        author,
        TextMarkup::Underline,
        None,
        None,
    )
}

/// Add a strikeout text-markup annotation covering every rectangle in `quads`.
///
/// Like [`add_underline`], but the stroked line crosses the middle of each quad.
///
/// # Errors
///
/// Returns the same typed errors as [`add_underline`].
pub fn add_strikeout(
    document: &mut Document,
    page_index: usize,
    quads: &[Rect],
    contents: &str,
    author: &str,
) -> Result<AnnotationRef, AnnotationEditError> {
    add_text_markup_with(
        document,
        page_index,
        quads,
        contents,
        author,
        TextMarkup::StrikeOut,
        None,
        None,
    )
}

/// Add a squiggly text-markup annotation covering every rectangle in `quads`.
///
/// Like [`add_underline`], but the appearance strokes a small zigzag along the
/// bottom edge of each quad, which is the conventional wavy underline.
///
/// # Errors
///
/// Returns the same typed errors as [`add_underline`].
pub fn add_squiggly(
    document: &mut Document,
    page_index: usize,
    quads: &[Rect],
    contents: &str,
    author: &str,
) -> Result<AnnotationRef, AnnotationEditError> {
    add_text_markup_with(
        document,
        page_index,
        quads,
        contents,
        author,
        TextMarkup::Squiggly,
        None,
        None,
    )
}

/// Add a freehand ink annotation built from `paths`.
///
/// Each path is one stroke: at least two points draw a line, one point draws a
/// dot, and empty paths are ignored. The annotation carries `/InkList`, a
/// `/BS << /W stroke_width /S /S >>` border and a vector appearance that strokes
/// every path. Its `/Rect` is the point bounds expanded by half the stroke width
/// plus a small slack, so the stroke is fully inside the appearance bounding box.
///
/// `stroke_width` must be finite and within
/// `MIN_INK_STROKE_WIDTH..=MAX_INK_STROKE_WIDTH`.
///
/// # Errors
///
/// Returns [`AnnotationEditError::TooManyInkPaths`] above [`MAX_INK_PATHS`],
/// [`AnnotationEditError::TooManyInkPoints`] above [`MAX_INK_POINTS`],
/// [`AnnotationEditError::EmptyInk`] when no path carries a point,
/// [`AnnotationEditError::InvalidStrokeWidth`] for a bad width,
/// [`AnnotationEditError::Geometry`] for a non-finite point coordinate,
/// [`AnnotationEditError::CoordinateOutsidePage`] for a point outside the page
/// media box, a missing-page error, or a core error. A rejected input leaves the
/// document unchanged.
pub fn add_ink(
    document: &mut Document,
    page_index: usize,
    paths: &[Vec<Point>],
    stroke_width: f32,
    contents: &str,
    author: &str,
) -> Result<AnnotationRef, AnnotationEditError> {
    add_ink_with(
        document,
        page_index,
        paths,
        stroke_width,
        contents,
        author,
        None,
        None,
    )
}

/// Add a straight-line annotation from `from` to `to`.
///
/// The annotation uses `/Subtype /Line`, `/L [x1 y1 x2 y2]` and
/// `/LE [/None /OpenArrow]`; its appearance strokes the segment and an open
/// arrowhead at `to`. The annotation `/Rect` is the endpoint bounds expanded by
/// half the stroke width plus [`ARROW_HEAD_LENGTH`], so the arrowhead is inside
/// the appearance bounding box. The `/NM` identifier is derived from the
/// allocated annotation object number.
///
/// Equivalent to [`add_line_styled`] with [`ShapeStyle::default`].
///
/// # Errors
///
/// Returns [`AnnotationEditError::DegenerateLine`] for identical endpoints,
/// [`AnnotationEditError::Geometry`] for a non-finite endpoint,
/// [`AnnotationEditError::CoordinateOutsidePage`] when the endpoint or the
/// derived `/Rect` leaves the page media box, a missing-page error, or a core
/// error. A rejected input leaves the document unchanged.
pub fn add_line(
    document: &mut Document,
    page_index: usize,
    from: Point,
    to: Point,
    contents: &str,
    author: &str,
) -> Result<AnnotationRef, AnnotationEditError> {
    add_line_styled(
        document,
        page_index,
        from,
        to,
        ShapeStyle::default(),
        contents,
        author,
    )
}

/// Add a straight-line annotation with an explicit [`ShapeStyle`].
///
/// The fill component is ignored: a line has no interior. See [`add_line`] for
/// the written dictionary and appearance.
///
/// # Errors
///
/// Returns every error from [`add_line`], plus
/// [`AnnotationEditError::InvalidColorComponent`],
/// [`AnnotationEditError::InvalidOpacity`] or
/// [`AnnotationEditError::InvalidStrokeWidth`] for a style component outside its
/// accepted range.
pub fn add_line_styled(
    document: &mut Document,
    page_index: usize,
    from: Point,
    to: Point,
    style: ShapeStyle,
    contents: &str,
    author: &str,
) -> Result<AnnotationRef, AnnotationEditError> {
    add_shape(
        document,
        page_index,
        ShapeGeometry::Line { from, to },
        style,
        contents,
        author,
        None,
        None,
    )
}

/// Add a rectangle annotation covering `rect`.
///
/// The annotation uses `/Subtype /Square`; its appearance strokes the rectangle
/// (and fills it with `/IC` when the style has a fill colour). The annotation
/// `/Rect` is `rect` expanded by half the stroke width so the stroked outline is
/// inside the appearance bounding box. Equivalent to [`add_square_styled`] with
/// [`ShapeStyle::default`].
///
/// # Errors
///
/// Returns [`AnnotationEditError::EmptyShapeRect`] for a zero-width or
/// zero-height rectangle, [`AnnotationEditError::CoordinateOutsidePage`] when
/// the rectangle or the derived `/Rect` leaves the page media box, a
/// missing-page error, or a core error. A rejected input leaves the document
/// unchanged.
pub fn add_square(
    document: &mut Document,
    page_index: usize,
    rect: Rect,
    contents: &str,
    author: &str,
) -> Result<AnnotationRef, AnnotationEditError> {
    add_square_styled(
        document,
        page_index,
        rect,
        ShapeStyle::default(),
        contents,
        author,
    )
}

/// Add a rectangle annotation with an explicit [`ShapeStyle`].
///
/// See [`add_square`] for the written dictionary and appearance.
///
/// # Errors
///
/// Returns every error from [`add_square`], plus the style errors documented on
/// [`add_line_styled`].
pub fn add_square_styled(
    document: &mut Document,
    page_index: usize,
    rect: Rect,
    style: ShapeStyle,
    contents: &str,
    author: &str,
) -> Result<AnnotationRef, AnnotationEditError> {
    add_shape(
        document,
        page_index,
        ShapeGeometry::Rect {
            rect,
            outline: Outline::Square,
        },
        style,
        contents,
        author,
        None,
        None,
    )
}

/// Add an ellipse annotation inscribed in `rect`.
///
/// The annotation uses `/Subtype /Circle`; its appearance draws the ellipse with
/// four Bezier curves (and fills it with `/IC` when the style has a fill colour).
/// The annotation `/Rect` is `rect` expanded by half the stroke width.
/// Equivalent to [`add_circle_styled`] with [`ShapeStyle::default`].
///
/// # Errors
///
/// Returns the same typed errors as [`add_square`].
pub fn add_circle(
    document: &mut Document,
    page_index: usize,
    rect: Rect,
    contents: &str,
    author: &str,
) -> Result<AnnotationRef, AnnotationEditError> {
    add_circle_styled(
        document,
        page_index,
        rect,
        ShapeStyle::default(),
        contents,
        author,
    )
}

/// Add an ellipse annotation with an explicit [`ShapeStyle`].
///
/// See [`add_circle`] for the written dictionary and appearance.
///
/// # Errors
///
/// Returns every error from [`add_circle`], plus the style errors documented on
/// [`add_line_styled`].
pub fn add_circle_styled(
    document: &mut Document,
    page_index: usize,
    rect: Rect,
    style: ShapeStyle,
    contents: &str,
    author: &str,
) -> Result<AnnotationRef, AnnotationEditError> {
    add_shape(
        document,
        page_index,
        ShapeGeometry::Rect {
            rect,
            outline: Outline::Circle,
        },
        style,
        contents,
        author,
        None,
        None,
    )
}

/// Add a closed polygon annotation through `vertices`.
///
/// The annotation uses `/Subtype /Polygon` and `/Vertices [x1 y1 ...]`; its
/// appearance closes and strokes the path (and fills it with `/IC` when the
/// style has a fill colour). The annotation `/Rect` is the vertex bounds
/// expanded by half the stroke width. At least three vertices are required, and
/// the polygon must enclose a non-zero area.
///
/// # Errors
///
/// Returns [`AnnotationEditError::TooManyVertices`] above
/// [`MAX_POLYGON_VERTICES`], [`AnnotationEditError::DegeneratePolygon`] for
/// fewer than three vertices or zero-area geometry,
/// [`AnnotationEditError::Geometry`] for a non-finite vertex,
/// [`AnnotationEditError::CoordinateOutsidePage`] when a vertex or the derived
/// `/Rect` leaves the page media box, a missing-page error, or a core error. A
/// rejected input leaves the document unchanged.
pub fn add_polygon(
    document: &mut Document,
    page_index: usize,
    vertices: &[Point],
    contents: &str,
    author: &str,
) -> Result<AnnotationRef, AnnotationEditError> {
    add_polygon_styled(
        document,
        page_index,
        vertices,
        ShapeStyle::default(),
        contents,
        author,
    )
}

/// Add a closed polygon annotation with an explicit [`ShapeStyle`].
///
/// See [`add_polygon`] for the written dictionary and appearance.
///
/// # Errors
///
/// Returns every error from [`add_polygon`], plus the style errors documented on
/// [`add_line_styled`].
pub fn add_polygon_styled(
    document: &mut Document,
    page_index: usize,
    vertices: &[Point],
    style: ShapeStyle,
    contents: &str,
    author: &str,
) -> Result<AnnotationRef, AnnotationEditError> {
    add_shape(
        document,
        page_index,
        ShapeGeometry::Polygon(vertices.to_vec()),
        style,
        contents,
        author,
        None,
        None,
    )
}

/// Add a stamp annotation covering `rect`.
///
/// `name` must be one of [`STAMP_NAMES`] in any casing; the canonical spelling
/// is written to `/Name` (for example `/Approved`). `text` becomes the label
/// drawn inside a bordered vector appearance and is also stored in `/Contents`.
/// No external image is used and non-ASCII characters are replaced with `?` in
/// the appearance, while `/Contents` keeps the exact text. The annotation
/// `/Rect` is `rect` unchanged.
///
/// # Errors
///
/// Returns [`AnnotationEditError::InvalidStampName`] for an unknown name,
/// [`AnnotationEditError::StampTextTooLong`] above
/// [`MAX_STAMP_TEXT_CHARS`] characters,
/// [`AnnotationEditError::EmptyShapeRect`] for a zero-area rectangle,
/// [`AnnotationEditError::CoordinateOutsidePage`] when the rectangle leaves the
/// page media box, a missing-page error, or a core error. A rejected input
/// leaves the document unchanged.
pub fn add_stamp(
    document: &mut Document,
    page_index: usize,
    rect: Rect,
    name: &str,
    text: &str,
    author: &str,
) -> Result<AnnotationRef, AnnotationEditError> {
    add_stamp_with(document, page_index, rect, name, text, author, None, None)
}

/// Add a free-text annotation with a wrapped text appearance inside `rect`.
///
/// The annotation uses `/Subtype /FreeText`, `/DA` for the default appearance,
/// and an `/AP /N` form that draws a thin border and wrapped lines of `text`.
/// `font_size` must be finite and within `4.0..=72.0`.
///
/// # Errors
///
/// Returns [`AnnotationEditError::InvalidFontSize`] for a bad size,
/// [`AnnotationEditError::EmptyShapeRect`] for an empty rectangle,
/// [`AnnotationEditError::EmptyText`] for blank text, a page or bounds
/// error, or a core error when the change cannot be staged.
pub fn add_free_text(
    document: &mut Document,
    page_index: usize,
    rect: Rect,
    text: &str,
    font_size: f32,
    author: &str,
) -> Result<AnnotationRef, AnnotationEditError> {
    add_text_box(
        document,
        page_index,
        rect,
        text,
        font_size,
        author,
        None,
        "pdfxi-freetext-",
    )
}

/// Add a free-text callout: a text box plus a line to `anchor`.
///
/// The annotation uses `/Subtype /FreeText` with `/IT /FreeTextCallout` and
/// `/CL [anchor, box edge]`, so a viewer draws the same leader line the
/// appearance does. `anchor` must lie on the page.
///
/// # Errors
///
/// Returns the same typed errors as [`add_free_text`], plus
/// [`AnnotationEditError::CoordinateOutsidePage`] when `anchor` is off the page.
pub fn add_callout(
    document: &mut Document,
    page_index: usize,
    rect: Rect,
    anchor: Point,
    text: &str,
    font_size: f32,
    author: &str,
) -> Result<AnnotationRef, AnnotationEditError> {
    if !anchor.x.is_finite() || !anchor.y.is_finite() {
        return Err(AnnotationEditError::InvalidAnchor);
    }
    add_text_box(
        document,
        page_index,
        rect,
        text,
        font_size,
        author,
        Some(anchor),
        "pdfxi-callout-",
    )
}

/// Add a cloud border around `rect`.
///
/// The annotation uses `/Subtype /Polygon` with `/IT /PolygonCloud` and a
/// scalloped appearance: overlapping stroked circles along the rectangle
/// perimeter, which is how a cloud border reads at any zoom level.
///
/// # Errors
///
/// Returns [`AnnotationEditError::EmptyShapeRect`] for an empty rectangle, a
/// bounds error, or a core error when the change cannot be staged.
pub fn add_cloud(
    document: &mut Document,
    page_index: usize,
    rect: Rect,
    author: &str,
) -> Result<AnnotationRef, AnnotationEditError> {
    if rect.is_empty() {
        return Err(AnnotationEditError::EmptyShapeRect);
    }
    let current = effective_document(document)?;
    let page_info = current.page(page_index)?;
    ensure_rect_on_page(page_index, page_info.media_box, rect)?;
    let mut page = read_page_state(&current, page_index)?;
    let ids = allocate_ids(document, 2)?;
    let appearance_id = ids[0];
    let annotation_id = ids[1];
    let generated = format!("pdfxi-cloud-{}", annotation_id.number);
    if annotation_id_exists(&current, &page.annots, &generated)? {
        return Err(AnnotationEditError::DuplicateId {
            id: generated,
            page: page_index,
        });
    }
    let date = now_pdf_date();

    let mut dictionary = Dictionary::new();
    dictionary.insert("Type".to_owned(), pdf_name("Annot"));
    dictionary.insert("Subtype".to_owned(), pdf_name("Polygon"));
    dictionary.insert("IT".to_owned(), pdf_name("PolygonCloud"));
    dictionary.insert(
        "Rect".to_owned(),
        rect_array(
            rect.inset(-CLOUD_BUMP_RADIUS, -CLOUD_BUMP_RADIUS)
                .unwrap_or(rect),
        ),
    );
    dictionary.insert("Contents".to_owned(), pdf_text("Cloud"));
    dictionary.insert("T".to_owned(), pdf_text(author));
    dictionary.insert("NM".to_owned(), pdf_text(&generated));
    dictionary.insert("M".to_owned(), pdf_text(&date));
    dictionary.insert(
        "Vertices".to_owned(),
        Object::Array(vec![
            Object::Real(f64::from(rect.x0())),
            Object::Real(f64::from(rect.y0())),
            Object::Real(f64::from(rect.x1())),
            Object::Real(f64::from(rect.y0())),
            Object::Real(f64::from(rect.x1())),
            Object::Real(f64::from(rect.y1())),
            Object::Real(f64::from(rect.x0())),
            Object::Real(f64::from(rect.y1())),
        ]),
    );
    dictionary.insert("C".to_owned(), color_array(MARKUP_STROKE_COLOR));
    dictionary.insert(
        "BS".to_owned(),
        border_style_dictionary(MARKUP_STROKE_WIDTH),
    );
    dictionary.insert("F".to_owned(), Object::Integer(4));
    dictionary.insert("AP".to_owned(), appearance_dictionary(appearance_id));

    let appearance = cloud_appearance(rect);
    document.set_object(appearance_id, appearance)?;
    document.set_object(annotation_id, Object::Dictionary(dictionary))?;
    page.annots
        .items_mut()
        .push(Object::Reference(annotation_id));
    stage_page_state(document, page)?;

    Ok(AnnotationRef {
        id: generated,
        object: annotation_id,
        page_index,
        subtype: AnnotationSubtype::Other("Polygon".to_owned()),
    })
}

/// Shared staging for a free-text box and a callout.
#[allow(clippy::too_many_arguments)]
fn add_text_box(
    document: &mut Document,
    page_index: usize,
    rect: Rect,
    text: &str,
    font_size: f32,
    author: &str,
    anchor: Option<Point>,
    prefix: &str,
) -> Result<AnnotationRef, AnnotationEditError> {
    if !font_size.is_finite() || !(4.0..=72.0).contains(&font_size) {
        return Err(AnnotationEditError::InvalidFontSize(font_size));
    }
    if rect.is_empty() {
        return Err(AnnotationEditError::EmptyShapeRect);
    }
    if text.trim().is_empty() {
        return Err(AnnotationEditError::EmptyText);
    }

    let current = effective_document(document)?;
    let page_info = current.page(page_index)?;
    ensure_rect_on_page(page_index, page_info.media_box, rect)?;
    if let Some(anchor) = anchor {
        ensure_points_on_page(page_index, page_info.media_box, [anchor])?;
    }
    let mut page = read_page_state(&current, page_index)?;
    let ids = allocate_ids(document, 2)?;
    let appearance_id = ids[0];
    let annotation_id = ids[1];
    let generated = format!("{prefix}{}", annotation_id.number);
    if annotation_id_exists(&current, &page.annots, &generated)? {
        return Err(AnnotationEditError::DuplicateId {
            id: generated,
            page: page_index,
        });
    }
    let date = now_pdf_date();

    let mut dictionary = Dictionary::new();
    dictionary.insert("Type".to_owned(), pdf_name("Annot"));
    dictionary.insert("Subtype".to_owned(), pdf_name("FreeText"));
    dictionary.insert("Rect".to_owned(), rect_array(rect));
    dictionary.insert("Contents".to_owned(), pdf_text(text));
    dictionary.insert("T".to_owned(), pdf_text(author));
    dictionary.insert("NM".to_owned(), pdf_text(&generated));
    dictionary.insert("M".to_owned(), pdf_text(&date));
    dictionary.insert(
        "DA".to_owned(),
        pdf_text(&format!("/Helv {font_size:.3} Tf 0 g")),
    );
    dictionary.insert("F".to_owned(), Object::Integer(4));
    dictionary.insert("AP".to_owned(), appearance_dictionary(appearance_id));
    if let Some(anchor) = anchor {
        dictionary.insert("IT".to_owned(), pdf_name("FreeTextCallout"));
        let edge = nearest_box_point(rect, anchor);
        dictionary.insert(
            "CL".to_owned(),
            Object::Array(vec![
                Object::Real(f64::from(anchor.x)),
                Object::Real(f64::from(anchor.y)),
                Object::Real(f64::from(edge.x)),
                Object::Real(f64::from(edge.y)),
            ]),
        );
    }

    let appearance = free_text_appearance(rect, text, font_size, anchor);
    document.set_object(appearance_id, appearance)?;
    document.set_object(annotation_id, Object::Dictionary(dictionary))?;
    page.annots
        .items_mut()
        .push(Object::Reference(annotation_id));
    stage_page_state(document, page)?;

    Ok(AnnotationRef {
        id: generated,
        object: annotation_id,
        page_index,
        subtype: AnnotationSubtype::Other("FreeText".to_owned()),
    })
}

/// Radius of one cloud bump, in points.
const CLOUD_BUMP_RADIUS: f32 = 8.0;
/// Bezier constant for a circular arc.
const CIRCLE_KAPPA: f64 = 0.552_284_749_830_793_6;

/// The point on the rectangle border nearest to `anchor`.
fn nearest_box_point(rect: Rect, anchor: Point) -> Point {
    Point::new(
        anchor.x.clamp(rect.x0(), rect.x1()),
        anchor.y.clamp(rect.y0(), rect.y1()),
    )
}

/// Build the `/AP /N` form for a free-text box, in coordinates relative to `rect`.
fn free_text_appearance(rect: Rect, text: &str, font_size: f32, anchor: Option<Point>) -> Object {
    let width = f64::from(rect.width());
    let height = f64::from(rect.height());
    let size = f64::from(font_size);
    let padding = (size * 0.5).min(width / 4.0).max(1.0);
    let chars_per_line = (((width - 2.0 * padding) / (size * 0.5)).floor() as usize).max(1);
    let lines = wrap_text(text, chars_per_line);
    let mut data = String::from("q\n0.6 0.6 0.6 RG 0.75 w\n0 0 ");
    data.push_str(&format!("{width:.3} {height:.3} re S\nQ\n"));
    data.push_str("q\n0 0 0 rg\nBT\n");
    data.push_str(&format!("/F1 {size:.3} Tf\n"));
    let line_height = size * 1.2;
    let mut baseline = height - padding - size;
    for line in lines.iter().take(64) {
        if baseline < padding {
            break;
        }
        data.push_str(&format!(
            "1 0 0 1 {padding:.3} {baseline:.3} Tm\n({}) Tj\n",
            escape_literal(line)
        ));
        baseline -= line_height;
    }
    data.push_str("ET\nQ\n");
    if let Some(anchor) = anchor {
        let edge = nearest_box_point(rect, anchor);
        data.push_str(&format!(
            "q\n0 0 0 RG 0.75 w\n{x0:.3} {y0:.3} m {x1:.3} {y1:.3} l S\nQ\n",
            x0 = f64::from(edge.x - rect.x0()),
            y0 = f64::from(edge.y - rect.y0()),
            x1 = f64::from(anchor.x - rect.x0()),
            y1 = f64::from(anchor.y - rect.y0()),
        ));
    }

    let mut font = Dictionary::new();
    font.insert("Type".to_owned(), pdf_name("Font"));
    font.insert("Subtype".to_owned(), pdf_name("Type1"));
    font.insert("BaseFont".to_owned(), pdf_name("Helvetica"));
    font.insert("Encoding".to_owned(), pdf_name("WinAnsiEncoding"));
    let mut fonts = Dictionary::new();
    fonts.insert("F1".to_owned(), Object::Dictionary(font));
    let mut resources = Dictionary::new();
    resources.insert("Font".to_owned(), Object::Dictionary(fonts));

    let mut dictionary = form_dictionary(width, height);
    dictionary.insert("Resources".to_owned(), Object::Dictionary(resources));
    Object::Stream {
        dictionary,
        data: data.into_bytes(),
    }
}

/// Build the `/AP /N` form for a cloud border around `rect`.
fn cloud_appearance(rect: Rect) -> Object {
    let width = f64::from(rect.width());
    let height = f64::from(rect.height());
    let radius = f64::from(CLOUD_BUMP_RADIUS)
        .min(width / 4.0)
        .min(height / 4.0);
    let mut data = String::from("q\n");
    data.push_str(&format!(
        "{r:.3} {g:.3} {b:.3} RG\n{w:.3} w\n",
        r = MARKUP_STROKE_COLOR[0],
        g = MARKUP_STROKE_COLOR[1],
        b = MARKUP_STROKE_COLOR[2],
        w = f64::from(MARKUP_STROKE_WIDTH),
    ));
    let mut centre = |x: f64, y: f64| {
        data.push_str(&circle_path(x, y, radius));
    };
    let columns = ((width / (radius * 2.0)).round() as usize).max(2);
    let rows = ((height / (radius * 2.0)).round() as usize).max(2);
    for index in 0..=columns {
        let x = radius + (width - 2.0 * radius) * index as f64 / columns as f64;
        centre(x, radius);
        centre(x, height - radius);
    }
    for index in 0..=rows {
        let y = radius + (height - 2.0 * radius) * index as f64 / rows as f64;
        centre(radius, y);
        centre(width - radius, y);
    }
    data.push_str("Q\n");
    let mut dictionary = form_dictionary(width, height);
    dictionary.insert(
        "Resources".to_owned(),
        Object::Dictionary(Dictionary::new()),
    );
    Object::Stream {
        dictionary,
        data: data.into_bytes(),
    }
}

/// A closed circular path with four cubic segments.
fn circle_path(cx: f64, cy: f64, radius: f64) -> String {
    let k = radius * CIRCLE_KAPPA;
    format!(
        "{x0:.3} {cy:.3} m\n\
         {x0:.3} {y1:.3} {x1:.3} {y2:.3} {cx:.3} {y2:.3} c\n\
         {x2:.3} {y2:.3} {x3:.3} {y1:.3} {x3:.3} {cy:.3} c\n\
         {x3:.3} {y3:.3} {x2:.3} {y4:.3} {cx:.3} {y4:.3} c\n\
         {x1:.3} {y4:.3} {x0:.3} {y3:.3} {x0:.3} {cy:.3} c\nS\n",
        x0 = cx - radius,
        x1 = cx - k,
        x2 = cx + k,
        x3 = cx + radius,
        y1 = cy - k,
        y2 = cy - radius,
        y3 = cy + k,
        y4 = cy + radius,
    )
}

/// Greedy word wrap to `chars_per_line` characters.
fn wrap_text(text: &str, chars_per_line: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut current = String::new();
    for word in text.split_whitespace() {
        if current.is_empty() {
            current.push_str(word);
        } else if current.chars().count() + 1 + word.chars().count() <= chars_per_line {
            current.push(' ');
            current.push_str(word);
        } else {
            lines.push(std::mem::take(&mut current));
            current.push_str(word);
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines
}

/// Build a `/BS` border dictionary with a solid stroke.
fn border_style_dictionary(width: f32) -> Object {
    let mut border = Dictionary::new();
    border.insert("W".to_owned(), Object::Real(f64::from(width)));
    border.insert("S".to_owned(), pdf_name("S"));
    Object::Dictionary(border)
}

/// Add a file-attachment annotation that embeds `bytes`.
///
/// The annotation uses `/Subtype /FileAttachment` with a `/Paperclip` icon and
/// `/FS` pointing at a `/Filespec` whose `/EF /F` is an `/EmbeddedFile` stream
/// containing the payload. The name is validated as a single attachment
/// reference (no path separators, no control characters), and the embedded bytes
/// are capped at [`MAX_ATTACHMENT_BYTES`] so a hostile caller cannot force an
/// unbounded object into the document. The annotation is also discoverable
/// through `pdfxi_core::Document::attachments` because the `/FS` file name is
/// the attachment name.
///
/// # Errors
///
/// Returns [`AnnotationEditError::InvalidAttachmentName`] for a bad name,
/// [`AnnotationEditError::AttachmentTooLarge`] above the cap,
/// [`AnnotationEditError::EmptyShapeRect`] for an empty rectangle, a bounds
/// error, or a core error when the object cannot be staged.
pub fn add_file_attachment(
    document: &mut Document,
    page_index: usize,
    rect: Rect,
    name: &str,
    bytes: &[u8],
    author: &str,
) -> Result<AnnotationRef, AnnotationEditError> {
    if name.trim().is_empty()
        || name
            .chars()
            .any(|character| matches!(character, '/' | '\\' | '\u{0}') || character.is_control())
    {
        return Err(AnnotationEditError::InvalidAttachmentName(name.to_owned()));
    }
    if bytes.len() > MAX_ATTACHMENT_BYTES {
        return Err(AnnotationEditError::AttachmentTooLarge {
            bytes: bytes.len(),
            limit: MAX_ATTACHMENT_BYTES,
        });
    }
    if rect.is_empty() {
        return Err(AnnotationEditError::EmptyShapeRect);
    }

    let current = effective_document(document)?;
    let page_info = current.page(page_index)?;
    ensure_rect_on_page(page_index, page_info.media_box, rect)?;
    let mut page = read_page_state(&current, page_index)?;
    let ids = allocate_ids(document, 4)?;
    let appearance_id = ids[0];
    let annotation_id = ids[1];
    let stream_id = ids[2];
    let filespec_id = ids[3];
    let generated = format!("pdfxi-attachment-{}", annotation_id.number);
    if annotation_id_exists(&current, &page.annots, &generated)? {
        return Err(AnnotationEditError::DuplicateId {
            id: generated,
            page: page_index,
        });
    }
    let date = now_pdf_date();

    // Embedded file stream.
    let mut params = Dictionary::new();
    params.insert(
        "Size".to_owned(),
        Object::Integer(i64::try_from(bytes.len()).unwrap_or(i64::MAX)),
    );
    params.insert("ModDate".to_owned(), pdf_text(&date));
    let mut stream_dictionary = Dictionary::new();
    stream_dictionary.insert("Type".to_owned(), pdf_name("EmbeddedFile"));
    stream_dictionary.insert(
        "Length".to_owned(),
        Object::Integer(i64::try_from(bytes.len()).unwrap_or(i64::MAX)),
    );
    stream_dictionary.insert("Params".to_owned(), Object::Dictionary(params));
    let stream = Object::Stream {
        dictionary: stream_dictionary,
        data: bytes.to_vec(),
    };

    // File specification referenced by the annotation.
    let mut embedded = Dictionary::new();
    embedded.insert(
        "F".to_owned(),
        Object::Reference(ObjectId::new(stream_id.number, stream_id.generation)),
    );
    let mut filespec = Dictionary::new();
    filespec.insert("Type".to_owned(), pdf_name("Filespec"));
    filespec.insert("F".to_owned(), pdf_text(name));
    filespec.insert("UF".to_owned(), pdf_text(name));
    filespec.insert("EF".to_owned(), Object::Dictionary(embedded));
    filespec.insert("Desc".to_owned(), pdf_text(author));

    let mut annotation = Dictionary::new();
    annotation.insert("Type".to_owned(), pdf_name("Annot"));
    annotation.insert("Subtype".to_owned(), pdf_name("FileAttachment"));
    annotation.insert("Rect".to_owned(), rect_array(rect));
    annotation.insert("Contents".to_owned(), pdf_text(name));
    annotation.insert("T".to_owned(), pdf_text(author));
    annotation.insert("NM".to_owned(), pdf_text(&generated));
    annotation.insert("M".to_owned(), pdf_text(&date));
    annotation.insert("Name".to_owned(), pdf_name("Paperclip"));
    annotation.insert(
        "FS".to_owned(),
        Object::Reference(ObjectId::new(filespec_id.number, filespec_id.generation)),
    );
    annotation.insert("F".to_owned(), Object::Integer(4));
    annotation.insert("AP".to_owned(), appearance_dictionary(appearance_id));

    document.set_object(stream_id, stream)?;
    document.set_object(filespec_id, Object::Dictionary(filespec))?;
    document.set_object(
        appearance_id,
        note_icon_appearance(f64::from(NOTE_ICON_SIZE), ATTACHMENT_ICON_COLOR),
    )?;
    document.set_object(annotation_id, Object::Dictionary(annotation))?;
    page.annots
        .items_mut()
        .push(Object::Reference(annotation_id));
    stage_page_state(document, page)?;

    Ok(AnnotationRef {
        id: generated,
        object: annotation_id,
        page_index,
        subtype: AnnotationSubtype::Other("FileAttachment".to_owned()),
    })
}

/// Largest attachment payload a single annotation may embed, in bytes.
pub const MAX_ATTACHMENT_BYTES: usize = 64 * 1024 * 1024;

/// Icon colour for a file-attachment annotation.
const ATTACHMENT_ICON_COLOR: [f64; 3] = [0.95, 0.75, 0.15];

/// List the annotations on `page_index`, resolving each referenced dictionary.
///
/// Entries that are not indirect references or do not resolve to a dictionary
/// are skipped, so a hostile `/Annots` array cannot make the listing fail.
///
/// # Errors
///
/// Returns a missing-page or malformed-`/Annots` error, or a core error when the
/// document cannot be read.
pub fn list_annotations(
    document: &Document,
    page_index: usize,
) -> Result<Vec<AnnotationView>, AnnotationEditError> {
    let current = effective_document(document)?;
    list_on_page(&current, page_index)
}

/// Reply to the annotation whose `/NM` is `parent` on `page_index`.
///
/// The new annotation is a `/Text` note carrying `/IRT` set to the parent's
/// indirect object and `/RT /R`, which is how viewers build a comment thread.
///
/// # Errors
///
/// Returns [`AnnotationEditError::AnnotationNotFound`] when `parent` is not on the page,
/// or any error from [`add_text_note`].
pub fn reply_to(
    document: &mut Document,
    page_index: usize,
    parent: &str,
    contents: &str,
    author: &str,
) -> Result<AnnotationRef, AnnotationEditError> {
    reply_to_with_date(document, page_index, parent, contents, author, None)
}

/// Set `/State` (and `/StateModel /Review`) on the annotation `id`.
///
/// # Errors
///
/// Returns [`AnnotationEditError::AnnotationNotFound`] when the annotation is missing, or
/// a core error when the change cannot be staged.
pub fn set_state(
    document: &mut Document,
    page_index: usize,
    id: &str,
    state: AnnotationState,
) -> Result<(), AnnotationEditError> {
    let current = effective_document(document)?;
    let (_, object, mut dictionary) = find_annotation(&current, page_index, id)?;
    dictionary.insert("State".to_owned(), pdf_name(state.as_pdf_name()));
    dictionary.insert("StateModel".to_owned(), pdf_name("Review"));
    document.set_object(object, Object::Dictionary(dictionary))?;
    Ok(())
}

/// Replace the `/Contents` text of the annotation `id`.
///
/// The annotation's appearance stream is left in place: the note icon does not
/// embed its contents, and a highlight is defined by its `/QuadPoints`.
///
/// # Errors
///
/// Returns [`AnnotationEditError::AnnotationNotFound`] when the annotation is missing, or
/// a core error when the change cannot be staged.
pub fn update_contents(
    document: &mut Document,
    page_index: usize,
    id: &str,
    contents: &str,
) -> Result<(), AnnotationEditError> {
    let current = effective_document(document)?;
    let (_, object, mut dictionary) = find_annotation(&current, page_index, id)?;
    dictionary.insert("Contents".to_owned(), pdf_text(contents));
    dictionary.insert("M".to_owned(), pdf_text(&now_pdf_date()));
    document.set_object(object, Object::Dictionary(dictionary))?;
    Ok(())
}

/// Remove the annotation `id` from `page_index` and overwrite its object.
///
/// The `/Annots` entry is removed and the annotation object (plus its `/AP /N`
/// appearance stream, when it is indirect) is replaced with `null`, so a full
/// rewrite does not retain the comment text. An incremental save keeps the
/// earlier revision by design; permanent erasure is the redaction path.
///
/// # Errors
///
/// Returns [`AnnotationEditError::AnnotationNotFound`] when the annotation is missing, or
/// a core error when the change cannot be staged.
pub fn delete_annotation(
    document: &mut Document,
    page_index: usize,
    id: &str,
) -> Result<(), AnnotationEditError> {
    let current = effective_document(document)?;
    let (index, object, dictionary) = find_annotation(&current, page_index, id)?;
    let mut page = read_page_state(&current, page_index)?;
    if index < page.annots.items().len() {
        page.annots.items_mut().remove(index);
    }
    stage_page_state(document, page)?;
    document.set_object(object, Object::Null)?;
    if let Some(appearance) = appearance_object(&dictionary) {
        document.set_object(appearance, Object::Null)?;
    }
    Ok(())
}

/// Export every annotation in the document as XFDF text.
///
/// The output is a small, deterministic XFDF document whose `<annots>` block
/// holds one `<annot>` element per annotation, in page order. Attributes are
/// `page`, `subtype`, `rect` (normalized `x0,y0,x1,y1`), `author`, `date` and
/// `id`; the element text is the `/Contents` value with XML escaping. A thread
/// reply adds `replyto`, and a resolved comment adds `state`, so the exported
/// file can carry a review thread.
///
/// # Errors
///
/// Returns a missing-page or malformed-`/Annots` error, or a core error when a
/// page cannot be read.
pub fn export_xfdf(document: &Document) -> Result<String, AnnotationEditError> {
    let current = effective_document(document)?;
    let page_count = current.page_count()?;
    let mut output = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <xfdf xmlns=\"http://ns.adobe.com/xfdf/\" xml:space=\"preserve\">\n  <annots>\n",
    );
    for page_index in 0..page_count {
        for view in list_on_page(&current, page_index)? {
            output.push_str("    <annot page=\"");
            output.push_str(&page_index.to_string());
            output.push_str("\" subtype=\"");
            output.push_str(&escape_xml_text(view.subtype.as_pdf_name()));
            output.push_str("\" rect=\"");
            output.push_str(&format_rect(view.rect));
            output.push_str("\" author=\"");
            output.push_str(&escape_xml_text(&view.author));
            output.push_str("\" date=\"");
            output.push_str(&escape_xml_text(view.date.as_deref().unwrap_or("")));
            output.push_str("\" id=\"");
            output.push_str(&escape_xml_text(&view.id));
            output.push('"');
            if let Some(parent) = &view.parent_id {
                output.push_str(" replyto=\"");
                output.push_str(&escape_xml_text(parent));
                output.push('"');
            }
            if let Some(state) = &view.state {
                output.push_str(" state=\"");
                output.push_str(&escape_xml_text(state));
                output.push('"');
            }
            output.push('>');
            output.push_str(&escape_xml_text(&view.contents));
            output.push_str("</annot>\n");
        }
    }
    output.push_str("  </annots>\n</xfdf>\n");
    Ok(output)
}

/// Import the `<annots>` block of an XFDF document into `document`.
///
/// Text annotations become [`add_text_note`] notes, highlights become
/// [`add_highlight`] calls covering the exported rectangle, underline and
/// strikeout markup become [`add_underline`] / [`add_strikeout`] calls covering
/// it, and an annotation with a `replyto` attribute becomes a thread reply.
/// `author` is used when the element has no `author` attribute. The returned
/// count is the number of annotations created.
///
/// # Errors
///
/// Returns an [`AnnotationEditError::XfdfParse`] or [`AnnotationEditError::XfdfValue`] error for
/// malformed markup, [`AnnotationEditError::UnsupportedSubtype`] for a subtype this module
/// does not create, or any error from the add/state operations.
pub fn import_xfdf(
    document: &mut Document,
    xml: &str,
    author: &str,
) -> Result<usize, AnnotationEditError> {
    let annotations = parse_xfdf_annots(xml)?;
    let mut imported = 0usize;
    for raw in annotations {
        let page_index = required_attribute(&raw, "page")?
            .parse::<usize>()
            .map_err(|_| AnnotationEditError::XfdfValue {
                name: "page",
                value: required_attribute(&raw, "page").unwrap_or("").to_owned(),
            })?;
        let rect = parse_xfdf_rect(required_attribute(&raw, "rect")?)?;
        let subtype = required_attribute(&raw, "subtype")?;
        let element_author = raw
            .attribute("author")
            .filter(|value| !value.is_empty())
            .unwrap_or(author);
        let date = raw.attribute("date").filter(|value| !value.is_empty());
        let id = raw
            .attribute("id")
            .filter(|value| !value.is_empty())
            .map(str::to_owned);
        let reply_to = raw.attribute("replyto");

        let created = if subtype.eq_ignore_ascii_case("Text") {
            match reply_to {
                Some(parent) => reply_to_with_date(
                    document,
                    page_index,
                    parent,
                    &raw.contents,
                    element_author,
                    date,
                )?,
                None => {
                    let generated;
                    let id = match id {
                        Some(id) => id,
                        None => {
                            generated = format!("pdfxi-import-{imported}");
                            generated
                        }
                    };
                    add_text_note_with_date(
                        document,
                        page_index,
                        Point::new(rect.x0(), rect.y0()),
                        &raw.contents,
                        element_author,
                        &id,
                        date,
                    )?
                }
            }
        } else if subtype.eq_ignore_ascii_case("Highlight") {
            add_highlight_with(
                document,
                page_index,
                std::slice::from_ref(&rect),
                &raw.contents,
                element_author,
                date,
                id.as_deref(),
            )?
        } else if subtype.eq_ignore_ascii_case("Underline") {
            add_text_markup_with(
                document,
                page_index,
                std::slice::from_ref(&rect),
                &raw.contents,
                element_author,
                TextMarkup::Underline,
                date,
                id.as_deref(),
            )?
        } else if subtype.eq_ignore_ascii_case("StrikeOut") {
            add_text_markup_with(
                document,
                page_index,
                std::slice::from_ref(&rect),
                &raw.contents,
                element_author,
                TextMarkup::StrikeOut,
                date,
                id.as_deref(),
            )?
        } else {
            return Err(AnnotationEditError::UnsupportedSubtype(subtype.to_owned()));
        };

        if let Some(state) = raw.attribute("state")
            && let Some(state) = AnnotationState::from_pdf_name(state)
        {
            set_state(document, page_index, &created.id, state)?;
        }
        imported += 1;
    }
    Ok(imported)
}

// ---------------------------------------------------------------------------
// Annotation construction
// ---------------------------------------------------------------------------

/// Inputs shared by a text note and a reply.
struct NoteSpec<'a> {
    rect: Rect,
    contents: &'a str,
    author: &'a str,
    id: &'a str,
    date: &'a str,
    parent: Option<ObjectId>,
}

fn add_text_note_with_date(
    document: &mut Document,
    page_index: usize,
    point: Point,
    contents: &str,
    author: &str,
    id: &str,
    date: Option<&str>,
) -> Result<AnnotationRef, AnnotationEditError> {
    if id.is_empty() {
        return Err(AnnotationEditError::EmptyId);
    }
    if !point.is_finite() {
        return Err(AnnotationEditError::Geometry(
            GeometryError::NonFiniteCoordinate {
                field: "point",
                value: point.x,
            },
        ));
    }
    let rect = Rect::new(
        point.x,
        point.y,
        point.x + NOTE_ICON_SIZE,
        point.y + NOTE_ICON_SIZE,
    )?;
    add_note_with(document, page_index, rect, contents, author, id, date, None)
}

fn reply_to_with_date(
    document: &mut Document,
    page_index: usize,
    parent: &str,
    contents: &str,
    author: &str,
    date: Option<&str>,
) -> Result<AnnotationRef, AnnotationEditError> {
    let current = effective_document(document)?;
    let (_, parent_object, parent_dictionary) = find_annotation(&current, page_index, parent)?;
    let parent_rect = rect_from_dictionary(&parent_dictionary);
    let id = format!("pdfxi-reply-{}", parent_object.number);
    add_note_with(
        document,
        page_index,
        parent_rect,
        contents,
        author,
        &id,
        date,
        Some(parent_object),
    )
}

#[allow(clippy::too_many_arguments)]
fn add_note_with(
    document: &mut Document,
    page_index: usize,
    rect: Rect,
    contents: &str,
    author: &str,
    id: &str,
    date: Option<&str>,
    parent: Option<ObjectId>,
) -> Result<AnnotationRef, AnnotationEditError> {
    let current = effective_document(document)?;
    let mut page = read_page_state(&current, page_index)?;
    if annotation_id_exists(&current, &page.annots, id)? {
        return Err(AnnotationEditError::DuplicateId {
            id: id.to_owned(),
            page: page_index,
        });
    }

    let ids = allocate_ids(document, 2)?;
    let appearance_id = ids[0];
    let annotation_id = ids[1];
    let now;
    let date = match date {
        Some(date) => date,
        None => {
            now = now_pdf_date();
            now.as_str()
        }
    };
    let spec = NoteSpec {
        rect,
        contents,
        author,
        id,
        date,
        parent,
    };
    let annotation = build_note_annotation(&spec, appearance_id);
    let appearance = note_icon_appearance(f64::from(NOTE_ICON_SIZE), NOTE_COLOR);

    document.set_object(appearance_id, appearance)?;
    document.set_object(annotation_id, Object::Dictionary(annotation))?;
    page.annots
        .items_mut()
        .push(Object::Reference(annotation_id));
    stage_page_state(document, page)?;

    Ok(AnnotationRef {
        id: id.to_owned(),
        object: annotation_id,
        page_index,
        subtype: AnnotationSubtype::Text,
    })
}

fn add_highlight_with(
    document: &mut Document,
    page_index: usize,
    quads: &[Rect],
    contents: &str,
    author: &str,
    date: Option<&str>,
    id: Option<&str>,
) -> Result<AnnotationRef, AnnotationEditError> {
    let mut bounds: Option<Rect> = None;
    let mut usable: Vec<Rect> = Vec::with_capacity(quads.len());
    for quad in quads {
        if quad.is_empty() {
            continue;
        }
        usable.push(*quad);
        bounds = Some(match bounds {
            Some(current) => current.union(quad),
            None => *quad,
        });
    }
    let Some(rect) = bounds else {
        return Err(AnnotationEditError::EmptyQuads);
    };

    let current = effective_document(document)?;
    let mut page = read_page_state(&current, page_index)?;
    let ids = allocate_ids(document, 2)?;
    let appearance_id = ids[0];
    let annotation_id = ids[1];
    let generated = format!("pdfxi-highlight-{}", annotation_id.number);
    let id = match id {
        Some(id) if !id.is_empty() => id,
        _ => generated.as_str(),
    };
    if annotation_id_exists(&current, &page.annots, id)? {
        return Err(AnnotationEditError::DuplicateId {
            id: id.to_owned(),
            page: page_index,
        });
    }
    let now;
    let date = match date {
        Some(date) => date,
        None => {
            now = now_pdf_date();
            now.as_str()
        }
    };

    let annotation =
        build_highlight_annotation(rect, &usable, contents, author, id, date, appearance_id);
    let appearance = highlight_appearance(rect, &usable);

    document.set_object(appearance_id, appearance)?;
    document.set_object(annotation_id, Object::Dictionary(annotation))?;
    page.annots
        .items_mut()
        .push(Object::Reference(annotation_id));
    stage_page_state(document, page)?;

    Ok(AnnotationRef {
        id: id.to_owned(),
        object: annotation_id,
        page_index,
        subtype: AnnotationSubtype::Highlight,
    })
}

/// A text-markup subtype that strokes a line through or under its quads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TextMarkup {
    /// `/Subtype /Underline`.
    Underline,
    /// `/Subtype /StrikeOut`.
    StrikeOut,
    /// `/Subtype /Squiggly`.
    Squiggly,
}

impl TextMarkup {
    fn subtype_name(self) -> &'static str {
        match self {
            Self::Underline => "Underline",
            Self::StrikeOut => "StrikeOut",
            Self::Squiggly => "Squiggly",
        }
    }

    /// `/NM` prefix for an annotation whose id the caller did not supply.
    fn id_prefix(self) -> &'static str {
        match self {
            Self::Underline => "pdfxi-underline-",
            Self::StrikeOut => "pdfxi-strikeout-",
            Self::Squiggly => "pdfxi-squiggly-",
        }
    }

    /// Y offset of the stroked line inside the annotation rectangle.
    ///
    /// The underline sits half a stroke width above the bottom edge of the quad
    /// so it stays inside the appearance bounding box; the strikeout crosses
    /// the vertical middle; the squiggly follows the same baseline as the
    /// underline.
    fn line_offset(self, quad: Rect, rect: Rect) -> f64 {
        let half_stroke = f64::from(MARKUP_STROKE_WIDTH) / 2.0;
        match self {
            Self::Underline | Self::Squiggly => f64::from(quad.y0() - rect.y0()) + half_stroke,
            Self::StrikeOut => f64::from((quad.y0() + quad.y1()) / 2.0 - rect.y0()),
        }
    }

    fn subtype(self) -> AnnotationSubtype {
        AnnotationSubtype::Other(self.subtype_name().to_owned())
    }
}

#[allow(clippy::too_many_arguments)]
fn add_text_markup_with(
    document: &mut Document,
    page_index: usize,
    quads: &[Rect],
    contents: &str,
    author: &str,
    markup: TextMarkup,
    date: Option<&str>,
    id: Option<&str>,
) -> Result<AnnotationRef, AnnotationEditError> {
    if quads.len() > MAX_MARKUP_QUADS {
        return Err(AnnotationEditError::TooManyQuads {
            count: quads.len(),
            limit: MAX_MARKUP_QUADS,
        });
    }
    let mut bounds: Option<Rect> = None;
    let mut usable: Vec<Rect> = Vec::with_capacity(quads.len());
    for quad in quads {
        if quad.is_empty() {
            continue;
        }
        usable.push(*quad);
        bounds = Some(match bounds {
            Some(current) => current.union(quad),
            None => *quad,
        });
    }
    let Some(rect) = bounds else {
        return Err(AnnotationEditError::EmptyQuads);
    };

    let current = effective_document(document)?;
    let page_info = current.page(page_index)?;
    ensure_quads_on_page(page_index, page_info.media_box, &usable)?;
    let mut page = read_page_state(&current, page_index)?;
    let ids = allocate_ids(document, 2)?;
    let appearance_id = ids[0];
    let annotation_id = ids[1];
    let generated = format!("{}{}", markup.id_prefix(), annotation_id.number);
    let id = match id {
        Some(id) if !id.is_empty() => id,
        _ => generated.as_str(),
    };
    if annotation_id_exists(&current, &page.annots, id)? {
        return Err(AnnotationEditError::DuplicateId {
            id: id.to_owned(),
            page: page_index,
        });
    }
    let now;
    let date = match date {
        Some(date) => date,
        None => {
            now = now_pdf_date();
            now.as_str()
        }
    };

    let annotation = build_text_markup_annotation(
        markup,
        rect,
        &usable,
        contents,
        author,
        id,
        date,
        appearance_id,
    );
    let appearance = text_markup_appearance(rect, &usable, markup);

    document.set_object(appearance_id, appearance)?;
    document.set_object(annotation_id, Object::Dictionary(annotation))?;
    page.annots
        .items_mut()
        .push(Object::Reference(annotation_id));
    stage_page_state(document, page)?;

    Ok(AnnotationRef {
        id: id.to_owned(),
        object: annotation_id,
        page_index,
        subtype: markup.subtype(),
    })
}

#[allow(clippy::too_many_arguments)]
fn add_ink_with(
    document: &mut Document,
    page_index: usize,
    paths: &[Vec<Point>],
    stroke_width: f32,
    contents: &str,
    author: &str,
    date: Option<&str>,
    id: Option<&str>,
) -> Result<AnnotationRef, AnnotationEditError> {
    if paths.len() > MAX_INK_PATHS {
        return Err(AnnotationEditError::TooManyInkPaths {
            count: paths.len(),
            limit: MAX_INK_PATHS,
        });
    }
    let total_points = paths
        .iter()
        .fold(0usize, |total, path| total.saturating_add(path.len()));
    if total_points > MAX_INK_POINTS {
        return Err(AnnotationEditError::TooManyInkPoints {
            count: total_points,
            limit: MAX_INK_POINTS,
        });
    }
    if !stroke_width.is_finite()
        || !(MIN_INK_STROKE_WIDTH..=MAX_INK_STROKE_WIDTH).contains(&stroke_width)
    {
        return Err(AnnotationEditError::InvalidStrokeWidth {
            value: stroke_width,
        });
    }

    let usable: Vec<&[Point]> = paths
        .iter()
        .filter(|path| !path.is_empty())
        .map(Vec::as_slice)
        .collect();
    if usable.is_empty() {
        return Err(AnnotationEditError::EmptyInk);
    }
    for path in &usable {
        for point in path.iter() {
            if !point.is_finite() {
                return Err(AnnotationEditError::Geometry(
                    GeometryError::NonFiniteCoordinate {
                        field: "point",
                        value: point.x,
                    },
                ));
            }
        }
    }

    let mut min_x = f32::INFINITY;
    let mut min_y = f32::INFINITY;
    let mut max_x = f32::NEG_INFINITY;
    let mut max_y = f32::NEG_INFINITY;
    for point in usable.iter().flat_map(|path| path.iter()) {
        min_x = min_x.min(point.x);
        min_y = min_y.min(point.y);
        max_x = max_x.max(point.x);
        max_y = max_y.max(point.y);
    }
    let padding = stroke_width / 2.0 + INK_RECT_PADDING;
    let rect = Rect::new(
        min_x - padding,
        min_y - padding,
        max_x + padding,
        max_y + padding,
    )?;

    let current = effective_document(document)?;
    let page_info = current.page(page_index)?;
    ensure_points_on_page(
        page_index,
        page_info.media_box,
        usable.iter().flat_map(|path| path.iter().copied()),
    )?;
    let mut page = read_page_state(&current, page_index)?;
    let ids = allocate_ids(document, 2)?;
    let appearance_id = ids[0];
    let annotation_id = ids[1];
    let generated = format!("pdfxi-ink-{}", annotation_id.number);
    let id = match id {
        Some(id) if !id.is_empty() => id,
        _ => generated.as_str(),
    };
    if annotation_id_exists(&current, &page.annots, id)? {
        return Err(AnnotationEditError::DuplicateId {
            id: id.to_owned(),
            page: page_index,
        });
    }
    let now;
    let date = match date {
        Some(date) => date,
        None => {
            now = now_pdf_date();
            now.as_str()
        }
    };

    let annotation = build_ink_annotation(
        rect,
        &usable,
        stroke_width,
        contents,
        author,
        id,
        date,
        appearance_id,
    );
    let appearance = ink_appearance(rect, &usable, stroke_width);

    document.set_object(appearance_id, appearance)?;
    document.set_object(annotation_id, Object::Dictionary(annotation))?;
    page.annots
        .items_mut()
        .push(Object::Reference(annotation_id));
    stage_page_state(document, page)?;

    Ok(AnnotationRef {
        id: id.to_owned(),
        object: annotation_id,
        page_index,
        subtype: AnnotationSubtype::Other("Ink".to_owned()),
    })
}

/// Geometry of one geometric shape annotation.
enum ShapeGeometry {
    /// A straight line with an open arrowhead at `to`.
    Line {
        /// Start point in default user space.
        from: Point,
        /// End point in default user space; the arrowhead is drawn here.
        to: Point,
    },
    /// A rectangle or its inscribed ellipse.
    Rect {
        /// Bounds of the path in default user space.
        rect: Rect,
        /// Which path the appearance draws.
        outline: Outline,
    },
    /// A closed polygon in storage order.
    Polygon(Vec<Point>),
}

/// The two `/Rect`-shaped geometric outlines.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Outline {
    /// `/Subtype /Square`.
    Square,
    /// `/Subtype /Circle`.
    Circle,
}

impl ShapeGeometry {
    fn subtype_name(&self) -> &'static str {
        match self {
            Self::Line { .. } => "Line",
            Self::Rect {
                outline: Outline::Square,
                ..
            } => "Square",
            Self::Rect {
                outline: Outline::Circle,
                ..
            } => "Circle",
            Self::Polygon(_) => "Polygon",
        }
    }

    fn id_prefix(&self) -> &'static str {
        match self {
            Self::Line { .. } => "pdfxi-line-",
            Self::Rect {
                outline: Outline::Square,
                ..
            } => "pdfxi-square-",
            Self::Rect {
                outline: Outline::Circle,
                ..
            } => "pdfxi-circle-",
            Self::Polygon(_) => "pdfxi-polygon-",
        }
    }
}

/// Stage one geometric shape annotation after validating its geometry and style.
#[allow(clippy::too_many_arguments)]
fn add_shape(
    document: &mut Document,
    page_index: usize,
    geometry: ShapeGeometry,
    style: ShapeStyle,
    contents: &str,
    author: &str,
    date: Option<&str>,
    id: Option<&str>,
) -> Result<AnnotationRef, AnnotationEditError> {
    validate_shape_style(&style)?;
    let (rect, points) = shape_bounds(&geometry, style.stroke_width)?;

    let current = effective_document(document)?;
    let page_info = current.page(page_index)?;
    ensure_points_on_page(page_index, page_info.media_box, points)?;
    ensure_rect_on_page(page_index, page_info.media_box, rect)?;
    let mut page = read_page_state(&current, page_index)?;
    let ids = allocate_ids(document, 2)?;
    let appearance_id = ids[0];
    let annotation_id = ids[1];
    let generated = format!("{}{}", geometry.id_prefix(), annotation_id.number);
    let id = match id {
        Some(id) if !id.is_empty() => id,
        _ => generated.as_str(),
    };
    if annotation_id_exists(&current, &page.annots, id)? {
        return Err(AnnotationEditError::DuplicateId {
            id: id.to_owned(),
            page: page_index,
        });
    }
    let now;
    let date = match date {
        Some(date) => date,
        None => {
            now = now_pdf_date();
            now.as_str()
        }
    };

    let annotation = build_shape_annotation(
        &geometry,
        rect,
        &style,
        contents,
        author,
        id,
        date,
        appearance_id,
    );
    let appearance = shape_appearance(&geometry, rect, style);

    document.set_object(appearance_id, appearance)?;
    document.set_object(annotation_id, Object::Dictionary(annotation))?;
    page.annots
        .items_mut()
        .push(Object::Reference(annotation_id));
    stage_page_state(document, page)?;

    Ok(AnnotationRef {
        id: id.to_owned(),
        object: annotation_id,
        page_index,
        subtype: AnnotationSubtype::Other(geometry.subtype_name().to_owned()),
    })
}

/// Stage one stamp annotation after validating its name, text and rectangle.
#[allow(clippy::too_many_arguments)]
fn add_stamp_with(
    document: &mut Document,
    page_index: usize,
    rect: Rect,
    name: &str,
    text: &str,
    author: &str,
    date: Option<&str>,
    id: Option<&str>,
) -> Result<AnnotationRef, AnnotationEditError> {
    let Some(name) = known_stamp_name(name) else {
        return Err(AnnotationEditError::InvalidStampName(name.to_owned()));
    };
    let chars = text.chars().count();
    if chars > MAX_STAMP_TEXT_CHARS {
        return Err(AnnotationEditError::StampTextTooLong {
            chars,
            limit: MAX_STAMP_TEXT_CHARS,
        });
    }
    if rect.is_empty() {
        return Err(AnnotationEditError::EmptyShapeRect);
    }

    let current = effective_document(document)?;
    let page_info = current.page(page_index)?;
    ensure_rect_on_page(page_index, page_info.media_box, rect)?;
    let mut page = read_page_state(&current, page_index)?;
    let ids = allocate_ids(document, 2)?;
    let appearance_id = ids[0];
    let annotation_id = ids[1];
    let generated = format!("pdfxi-stamp-{}", annotation_id.number);
    let id = match id {
        Some(id) if !id.is_empty() => id,
        _ => generated.as_str(),
    };
    if annotation_id_exists(&current, &page.annots, id)? {
        return Err(AnnotationEditError::DuplicateId {
            id: id.to_owned(),
            page: page_index,
        });
    }
    let now;
    let date = match date {
        Some(date) => date,
        None => {
            now = now_pdf_date();
            now.as_str()
        }
    };

    let annotation = build_stamp_annotation(rect, name, text, author, id, date, appearance_id);
    let appearance = stamp_appearance(rect, text);

    document.set_object(appearance_id, appearance)?;
    document.set_object(annotation_id, Object::Dictionary(annotation))?;
    page.annots
        .items_mut()
        .push(Object::Reference(annotation_id));
    stage_page_state(document, page)?;

    Ok(AnnotationRef {
        id: id.to_owned(),
        object: annotation_id,
        page_index,
        subtype: AnnotationSubtype::Other("Stamp".to_owned()),
    })
}

/// Validate every component of a shape style before any object is allocated.
fn validate_shape_style(style: &ShapeStyle) -> Result<(), AnnotationEditError> {
    for component in style.stroke_color {
        ensure_color_component(component)?;
    }
    if let Some(fill) = style.fill_color {
        for component in fill {
            ensure_color_component(component)?;
        }
    }
    if !style.opacity.is_finite() || !(0.0..=1.0).contains(&style.opacity) {
        return Err(AnnotationEditError::InvalidOpacity {
            value: style.opacity,
        });
    }
    if !style.stroke_width.is_finite()
        || !(MIN_INK_STROKE_WIDTH..=MAX_INK_STROKE_WIDTH).contains(&style.stroke_width)
    {
        return Err(AnnotationEditError::InvalidStrokeWidth {
            value: style.stroke_width,
        });
    }
    Ok(())
}

fn ensure_color_component(value: f64) -> Result<(), AnnotationEditError> {
    if value.is_finite() && (0.0..=1.0).contains(&value) {
        Ok(())
    } else {
        Err(AnnotationEditError::InvalidColorComponent { value })
    }
}

/// Compute the annotation rectangle and the input points to page-check.
///
/// The rectangle is the shape's own bounds expanded by half the stroke width (and
/// by [`ARROW_HEAD_LENGTH`] for a line), so the whole appearance fits inside the
/// form `/BBox`. The returned points are the caller-supplied coordinates, so an
/// out-of-page input reports its own point rather than a derived corner.
fn shape_bounds(
    geometry: &ShapeGeometry,
    stroke_width: f32,
) -> Result<(Rect, Vec<Point>), AnnotationEditError> {
    let padding = stroke_width / 2.0;
    match geometry {
        ShapeGeometry::Line { from, to } => {
            ensure_finite_point(*from)?;
            ensure_finite_point(*to)?;
            if from.x == to.x && from.y == to.y {
                return Err(AnnotationEditError::DegenerateLine);
            }
            let line_padding = padding + ARROW_HEAD_LENGTH;
            let rect = Rect::new(
                from.x.min(to.x) - line_padding,
                from.y.min(to.y) - line_padding,
                from.x.max(to.x) + line_padding,
                from.y.max(to.y) + line_padding,
            )?;
            Ok((rect, vec![*from, *to]))
        }
        ShapeGeometry::Rect { rect, .. } => {
            if rect.is_empty() {
                return Err(AnnotationEditError::EmptyShapeRect);
            }
            let expanded = Rect::new(
                rect.x0() - padding,
                rect.y0() - padding,
                rect.x1() + padding,
                rect.y1() + padding,
            )?;
            Ok((expanded, rect_corners(*rect)))
        }
        ShapeGeometry::Polygon(vertices) => {
            if vertices.len() > MAX_POLYGON_VERTICES {
                return Err(AnnotationEditError::TooManyVertices {
                    count: vertices.len(),
                    limit: MAX_POLYGON_VERTICES,
                });
            }
            if vertices.len() < 3 {
                return Err(AnnotationEditError::DegeneratePolygon);
            }
            let mut min_x = f32::INFINITY;
            let mut min_y = f32::INFINITY;
            let mut max_x = f32::NEG_INFINITY;
            let mut max_y = f32::NEG_INFINITY;
            for vertex in vertices {
                ensure_finite_point(*vertex)?;
                min_x = min_x.min(vertex.x);
                min_y = min_y.min(vertex.y);
                max_x = max_x.max(vertex.x);
                max_y = max_y.max(vertex.y);
            }
            let bounds = Rect::new(min_x, min_y, max_x, max_y)?;
            if bounds.is_empty() || polygon_area(vertices).abs() < 1e-6 {
                return Err(AnnotationEditError::DegeneratePolygon);
            }
            let rect = Rect::new(
                bounds.x0() - padding,
                bounds.y0() - padding,
                bounds.x1() + padding,
                bounds.y1() + padding,
            )?;
            Ok((rect, vertices.clone()))
        }
    }
}

fn ensure_finite_point(point: Point) -> Result<(), AnnotationEditError> {
    if point.is_finite() {
        Ok(())
    } else {
        Err(AnnotationEditError::Geometry(
            GeometryError::NonFiniteCoordinate {
                field: "point",
                value: point.x,
            },
        ))
    }
}

fn rect_corners(rect: Rect) -> Vec<Point> {
    vec![
        Point::new(rect.x0(), rect.y0()),
        Point::new(rect.x1(), rect.y0()),
        Point::new(rect.x1(), rect.y1()),
        Point::new(rect.x0(), rect.y1()),
    ]
}

/// Twice the signed area of a polygon, as used for the degeneracy check.
fn polygon_area(vertices: &[Point]) -> f64 {
    let mut sum = 0.0;
    for (index, current) in vertices.iter().enumerate() {
        let next = vertices[(index + 1) % vertices.len()];
        sum += f64::from(current.x) * f64::from(next.y) - f64::from(next.x) * f64::from(current.y);
    }
    sum / 2.0
}

fn ensure_rect_on_page(
    page: usize,
    media_box: Rectangle,
    rect: Rect,
) -> Result<(), AnnotationEditError> {
    ensure_points_on_page(page, media_box, rect_corners(rect))
}

#[allow(clippy::too_many_arguments)]
fn build_shape_annotation(
    geometry: &ShapeGeometry,
    rect: Rect,
    style: &ShapeStyle,
    contents: &str,
    author: &str,
    id: &str,
    date: &str,
    appearance_id: ObjectId,
) -> Dictionary {
    let mut dictionary = Dictionary::new();
    dictionary.insert("Type".to_owned(), pdf_name("Annot"));
    dictionary.insert("Subtype".to_owned(), pdf_name(geometry.subtype_name()));
    dictionary.insert("Rect".to_owned(), rect_array(rect));
    match geometry {
        ShapeGeometry::Line { from, to } => {
            dictionary.insert("L".to_owned(), line_array(*from, *to));
            dictionary.insert(
                "LE".to_owned(),
                Object::Array(vec![pdf_name("None"), pdf_name("OpenArrow")]),
            );
        }
        ShapeGeometry::Rect { .. } => {}
        ShapeGeometry::Polygon(vertices) => {
            dictionary.insert("Vertices".to_owned(), point_array(vertices));
        }
    }
    if !matches!(geometry, ShapeGeometry::Line { .. })
        && let Some(fill) = style.fill_color
    {
        dictionary.insert("IC".to_owned(), color_array(fill));
    }
    dictionary.insert("Contents".to_owned(), pdf_text(contents));
    dictionary.insert("T".to_owned(), pdf_text(author));
    dictionary.insert("NM".to_owned(), pdf_text(id));
    dictionary.insert("M".to_owned(), pdf_text(date));
    dictionary.insert("C".to_owned(), color_array(style.stroke_color));
    dictionary.insert("CA".to_owned(), Object::Real(style.opacity));
    dictionary.insert("F".to_owned(), Object::Integer(4));
    dictionary.insert("BS".to_owned(), border_style(f64::from(style.stroke_width)));
    dictionary.insert("AP".to_owned(), appearance_dictionary(appearance_id));
    dictionary
}

fn build_stamp_annotation(
    rect: Rect,
    name: &str,
    text: &str,
    author: &str,
    id: &str,
    date: &str,
    appearance_id: ObjectId,
) -> Dictionary {
    let mut dictionary = Dictionary::new();
    dictionary.insert("Type".to_owned(), pdf_name("Annot"));
    dictionary.insert("Subtype".to_owned(), pdf_name("Stamp"));
    dictionary.insert("Rect".to_owned(), rect_array(rect));
    dictionary.insert("Name".to_owned(), pdf_name(name));
    dictionary.insert("Contents".to_owned(), pdf_text(text));
    dictionary.insert("T".to_owned(), pdf_text(author));
    dictionary.insert("NM".to_owned(), pdf_text(id));
    dictionary.insert("M".to_owned(), pdf_text(date));
    dictionary.insert("C".to_owned(), color_array(STAMP_COLOR));
    dictionary.insert("CA".to_owned(), Object::Real(1.0));
    dictionary.insert("F".to_owned(), Object::Integer(4));
    dictionary.insert("AP".to_owned(), appearance_dictionary(appearance_id));
    dictionary
}

/// Build the `/AP /N` form for a shape, in coordinates relative to `rect`.
fn shape_appearance(geometry: &ShapeGeometry, rect: Rect, style: ShapeStyle) -> Object {
    let width = f64::from(rect.width());
    let height = f64::from(rect.height());
    let [red, green, blue] = style.stroke_color;
    let stroke_width = f64::from(style.stroke_width);
    let mut data = String::from("q\n");
    if let Some([fill_red, fill_green, fill_blue]) = style.fill_color {
        data.push_str(&format!(
            "{fill_red:.3} {fill_green:.3} {fill_blue:.3} rg\n"
        ));
    }
    data.push_str(&format!(
        "{red:.3} {green:.3} {blue:.3} RG\n{stroke_width:.3} w\n"
    ));

    match geometry {
        ShapeGeometry::Line { from, to } => {
            data.push_str("1 J 1 j\n");
            let x0 = f64::from(from.x - rect.x0());
            let y0 = f64::from(from.y - rect.y0());
            let x1 = f64::from(to.x - rect.x0());
            let y1 = f64::from(to.y - rect.y0());
            data.push_str(&format!("{x0:.3} {y0:.3} m {x1:.3} {y1:.3} l S\n"));
            for leg in arrow_head_legs(*from, *to).into_iter().flatten() {
                let lx = f64::from(leg.x - rect.x0());
                let ly = f64::from(leg.y - rect.y0());
                data.push_str(&format!("{lx:.3} {ly:.3} m {x1:.3} {y1:.3} l S\n"));
            }
        }
        ShapeGeometry::Rect {
            rect: path,
            outline,
        } => {
            let x = f64::from(path.x0() - rect.x0());
            let y = f64::from(path.y0() - rect.y0());
            let path_width = f64::from(path.width());
            let path_height = f64::from(path.height());
            match outline {
                Outline::Square => {
                    data.push_str(&format!(
                        "{x:.3} {y:.3} {path_width:.3} {path_height:.3} re {}\n",
                        if style.fill_color.is_some() { "B" } else { "S" }
                    ));
                }
                Outline::Circle => {
                    let cx = x + path_width / 2.0;
                    let cy = y + path_height / 2.0;
                    let rx = path_width / 2.0;
                    let ry = path_height / 2.0;
                    let kx = rx * ELLIPSE_KAPPA;
                    let ky = ry * ELLIPSE_KAPPA;
                    let right = cx + rx;
                    let left = cx - rx;
                    let up = cy + ry;
                    let down = cy - ry;
                    data.push_str(&format!(
                        "{right:.3} {cy:.3} m\n\
                         {right:.3} {c1y:.3} {c1x:.3} {up:.3} {cx:.3} {up:.3} c\n\
                         {c2x:.3} {up:.3} {left:.3} {c2y:.3} {left:.3} {cy:.3} c\n\
                         {left:.3} {c3y:.3} {c3x:.3} {down:.3} {cx:.3} {down:.3} c\n\
                         {c4x:.3} {down:.3} {right:.3} {c4y:.3} {right:.3} {cy:.3} c\n",
                        c1y = cy + ky,
                        c1x = cx + kx,
                        c2x = cx - kx,
                        c2y = cy + ky,
                        c3y = cy - ky,
                        c3x = cx - kx,
                        c4x = cx + kx,
                        c4y = cy - ky,
                    ));
                    data.push_str(if style.fill_color.is_some() {
                        "B\n"
                    } else {
                        "S\n"
                    });
                }
            }
        }
        ShapeGeometry::Polygon(vertices) => {
            let mut points = vertices.iter().map(|vertex| {
                (
                    f64::from(vertex.x - rect.x0()),
                    f64::from(vertex.y - rect.y0()),
                )
            });
            if let Some((x, y)) = points.next() {
                data.push_str(&format!("{x:.3} {y:.3} m\n"));
                for (x, y) in points {
                    data.push_str(&format!("{x:.3} {y:.3} l\n"));
                }
                data.push_str(if style.fill_color.is_some() {
                    "h B\n"
                } else {
                    "h S\n"
                });
            }
        }
    }
    data.push_str("Q\n");

    let mut dictionary = form_dictionary(width, height);
    dictionary.insert(
        "Resources".to_owned(),
        Object::Dictionary(Dictionary::new()),
    );
    Object::Stream {
        dictionary,
        data: data.into_bytes(),
    }
}

/// Build the bordered label form for a stamp, in coordinates relative to `rect`.
fn stamp_appearance(rect: Rect, text: &str) -> Object {
    let width = f64::from(rect.width());
    let height = f64::from(rect.height());
    let [red, green, blue] = STAMP_COLOR;
    let display = sanitize_stamp_text(text);
    let chars = display.chars().count().max(1) as f64;
    let available = (width - 2.0 * STAMP_PADDING).max(1.0);
    let font_size = (height * 0.35)
        .min(available / (chars * 0.55))
        .clamp(4.0, 24.0);
    let text_width = chars * font_size * 0.55;
    let text_x = ((width - text_width) / 2.0).max(STAMP_PADDING / 2.0);
    let text_y = ((height - font_size * 0.7) / 2.0).max(1.0);
    let inset = 2.0_f64.min(width / 4.0).min(height / 4.0);
    let border_width = (inset / 2.0).max(0.25);
    let escaped = escape_literal(&display);

    let data = format!(
        "q\n{red:.3} {green:.3} {blue:.3} RG\n{border_width:.3} w\n\
         {inset:.3} {inset:.3} {inner_width:.3} {inner_height:.3} re S\nQ\n\
         q\n{red:.3} {green:.3} {blue:.3} rg\nBT\n/F1 {font_size:.3} Tf\n\
         1 0 0 1 {text_x:.3} {text_y:.3} Tm\n({escaped}) Tj\nET\nQ\n",
        inner_width = (width - 2.0 * inset).max(0.0),
        inner_height = (height - 2.0 * inset).max(0.0),
    );

    let mut font = Dictionary::new();
    font.insert("Type".to_owned(), pdf_name("Font"));
    font.insert("Subtype".to_owned(), pdf_name("Type1"));
    font.insert("BaseFont".to_owned(), pdf_name("Helvetica"));
    font.insert("Encoding".to_owned(), pdf_name("WinAnsiEncoding"));
    let mut fonts = Dictionary::new();
    fonts.insert("F1".to_owned(), Object::Dictionary(font));
    let mut resources = Dictionary::new();
    resources.insert("Font".to_owned(), Object::Dictionary(fonts));

    let mut dictionary = form_dictionary(width, height);
    dictionary.insert("Resources".to_owned(), Object::Dictionary(resources));
    Object::Stream {
        dictionary,
        data: data.into_bytes(),
    }
}

/// Bezier circle constant: four cubic segments at this control distance
/// approximate a circle to within a fraction of a point.
const ELLIPSE_KAPPA: f64 = 0.552_284_749_830_793_6;

/// The two legs of an open arrowhead at `to`, or `None` for a zero-length line.
fn arrow_head_legs(from: Point, to: Point) -> Option<[Point; 2]> {
    let dx = f64::from(to.x - from.x);
    let dy = f64::from(to.y - from.y);
    let length = dx.hypot(dy);
    if !length.is_finite() || length <= f64::EPSILON {
        return None;
    }
    let ux = dx / length;
    let uy = dy / length;
    let (sin, cos) = 25.0_f64.to_radians().sin_cos();
    let head = f64::from(ARROW_HEAD_LENGTH);
    let leg = |vx: f64, vy: f64| Point::new(to.x - (vx * head) as f32, to.y - (vy * head) as f32);
    Some([
        leg(ux * cos - uy * sin, ux * sin + uy * cos),
        leg(ux * cos + uy * sin, -ux * sin + uy * cos),
    ])
}

/// Return the canonical `/Name` for a standard stamp name, in any casing.
fn known_stamp_name(name: &str) -> Option<&'static str> {
    STAMP_NAMES
        .iter()
        .copied()
        .find(|candidate| candidate.eq_ignore_ascii_case(name))
}

/// Map a stamp label to printable ASCII for the standard Helvetica appearance.
///
/// `/Contents` keeps the exact text; only the drawn label is restricted, so a
/// control character or a non-WinAnsi character can never split the content
/// stream.
fn sanitize_stamp_text(text: &str) -> String {
    let mut display = String::with_capacity(text.len());
    for character in text.chars() {
        if character.is_ascii_graphic() || character == ' ' {
            display.push(character);
        } else {
            display.push('?');
        }
    }
    display
}

fn build_note_annotation(spec: &NoteSpec<'_>, appearance_id: ObjectId) -> Dictionary {
    let mut dictionary = Dictionary::new();
    dictionary.insert("Type".to_owned(), pdf_name("Annot"));
    dictionary.insert("Subtype".to_owned(), pdf_name("Text"));
    dictionary.insert("Rect".to_owned(), rect_array(spec.rect));
    dictionary.insert("Contents".to_owned(), pdf_text(spec.contents));
    dictionary.insert("T".to_owned(), pdf_text(spec.author));
    dictionary.insert("NM".to_owned(), pdf_text(spec.id));
    dictionary.insert("M".to_owned(), pdf_text(spec.date));
    dictionary.insert("C".to_owned(), color_array(NOTE_COLOR));
    dictionary.insert("CA".to_owned(), Object::Real(1.0));
    dictionary.insert("F".to_owned(), Object::Integer(4));
    dictionary.insert("AP".to_owned(), appearance_dictionary(appearance_id));
    if let Some(parent) = spec.parent {
        dictionary.insert("IRT".to_owned(), Object::Reference(parent));
        dictionary.insert("RT".to_owned(), pdf_name("R"));
    }
    dictionary
}

fn build_highlight_annotation(
    rect: Rect,
    quads: &[Rect],
    contents: &str,
    author: &str,
    id: &str,
    date: &str,
    appearance_id: ObjectId,
) -> Dictionary {
    let mut dictionary = Dictionary::new();
    dictionary.insert("Type".to_owned(), pdf_name("Annot"));
    dictionary.insert("Subtype".to_owned(), pdf_name("Highlight"));
    dictionary.insert("Rect".to_owned(), rect_array(rect));
    dictionary.insert("QuadPoints".to_owned(), quad_points_array(quads));
    dictionary.insert("Contents".to_owned(), pdf_text(contents));
    dictionary.insert("T".to_owned(), pdf_text(author));
    dictionary.insert("NM".to_owned(), pdf_text(id));
    dictionary.insert("M".to_owned(), pdf_text(date));
    dictionary.insert("C".to_owned(), color_array(HIGHLIGHT_COLOR));
    dictionary.insert("CA".to_owned(), Object::Real(HIGHLIGHT_OPACITY));
    dictionary.insert("F".to_owned(), Object::Integer(4));
    dictionary.insert("AP".to_owned(), appearance_dictionary(appearance_id));
    dictionary
}

#[allow(clippy::too_many_arguments)]
fn build_text_markup_annotation(
    markup: TextMarkup,
    rect: Rect,
    quads: &[Rect],
    contents: &str,
    author: &str,
    id: &str,
    date: &str,
    appearance_id: ObjectId,
) -> Dictionary {
    let mut dictionary = Dictionary::new();
    dictionary.insert("Type".to_owned(), pdf_name("Annot"));
    dictionary.insert("Subtype".to_owned(), pdf_name(markup.subtype_name()));
    dictionary.insert("Rect".to_owned(), rect_array(rect));
    dictionary.insert("QuadPoints".to_owned(), quad_points_array(quads));
    dictionary.insert("Contents".to_owned(), pdf_text(contents));
    dictionary.insert("T".to_owned(), pdf_text(author));
    dictionary.insert("NM".to_owned(), pdf_text(id));
    dictionary.insert("M".to_owned(), pdf_text(date));
    dictionary.insert("C".to_owned(), color_array(MARKUP_STROKE_COLOR));
    dictionary.insert("CA".to_owned(), Object::Real(1.0));
    dictionary.insert("F".to_owned(), Object::Integer(4));
    dictionary.insert(
        "BS".to_owned(),
        border_style(f64::from(MARKUP_STROKE_WIDTH)),
    );
    dictionary.insert("AP".to_owned(), appearance_dictionary(appearance_id));
    dictionary
}

#[allow(clippy::too_many_arguments)]
fn build_ink_annotation(
    rect: Rect,
    paths: &[&[Point]],
    stroke_width: f32,
    contents: &str,
    author: &str,
    id: &str,
    date: &str,
    appearance_id: ObjectId,
) -> Dictionary {
    let mut dictionary = Dictionary::new();
    dictionary.insert("Type".to_owned(), pdf_name("Annot"));
    dictionary.insert("Subtype".to_owned(), pdf_name("Ink"));
    dictionary.insert("Rect".to_owned(), rect_array(rect));
    dictionary.insert("InkList".to_owned(), ink_list_array(paths));
    dictionary.insert("Contents".to_owned(), pdf_text(contents));
    dictionary.insert("T".to_owned(), pdf_text(author));
    dictionary.insert("NM".to_owned(), pdf_text(id));
    dictionary.insert("M".to_owned(), pdf_text(date));
    dictionary.insert("C".to_owned(), color_array(MARKUP_STROKE_COLOR));
    dictionary.insert("CA".to_owned(), Object::Real(1.0));
    dictionary.insert("F".to_owned(), Object::Integer(4));
    dictionary.insert("BS".to_owned(), border_style(f64::from(stroke_width)));
    dictionary.insert("AP".to_owned(), appearance_dictionary(appearance_id));
    dictionary
}

/// A solid border style dictionary (`/BS << /W w /S /S >>`).
fn border_style(width: f64) -> Object {
    let mut border = Dictionary::new();
    border.insert("W".to_owned(), Object::Real(width));
    border.insert("S".to_owned(), pdf_name("S"));
    Object::Dictionary(border)
}

/// `/InkList`: one array of alternating x/y coordinates per stroke.
fn ink_list_array(paths: &[&[Point]]) -> Object {
    let strokes = paths
        .iter()
        .map(|path| {
            let mut coordinates = Vec::with_capacity(path.len() * 2);
            for point in path.iter() {
                coordinates.push(Object::Real(f64::from(point.x)));
                coordinates.push(Object::Real(f64::from(point.y)));
            }
            Object::Array(coordinates)
        })
        .collect();
    Object::Array(strokes)
}

fn appearance_dictionary(appearance_id: ObjectId) -> Object {
    let mut appearance = Dictionary::new();
    appearance.insert("N".to_owned(), Object::Reference(appearance_id));
    Object::Dictionary(appearance)
}

/// Build a vector sticky-note icon as a form XObject.
fn note_icon_appearance(size: f64, color: [f64; 3]) -> Object {
    let [red, green, blue] = color;
    let inner = size - 0.75;
    let data = format!(
        "q\n\
         {red:.3} {green:.3} {blue:.3} rg\n\
         0 0 {size:.3} {size:.3} re f\n\
         0.25 0.25 0.25 RG 0.75 w\n\
         0.375 0.375 {inner:.3} {inner:.3} re S\n\
         Q\n\
         q\n\
         0.25 0.25 0.25 RG 0.75 w\n\
         4.000 6.000 m 20.000 6.000 l S\n\
         4.000 11.000 m 20.000 11.000 l S\n\
         4.000 16.000 m 15.000 16.000 l S\n\
         Q\n"
    );
    let mut dictionary = form_dictionary(size, size);
    dictionary.insert(
        "Resources".to_owned(),
        Object::Dictionary(Dictionary::new()),
    );
    Object::Stream {
        dictionary,
        data: data.into_bytes(),
    }
}

/// Build a translucent highlight form XObject whose coordinates are relative to
/// the annotation `/Rect` lower-left corner.
fn highlight_appearance(rect: Rect, quads: &[Rect]) -> Object {
    let width = f64::from(rect.width());
    let height = f64::from(rect.height());
    let mut data = String::from("q\n/GS0 gs\n1.000 1.000 0.000 rg\n");
    for quad in quads {
        let x = f64::from(quad.x0() - rect.x0());
        let y = f64::from(quad.y0() - rect.y0());
        let w = f64::from(quad.width());
        let h = f64::from(quad.height());
        data.push_str(&format!("{x:.3} {y:.3} {w:.3} {h:.3} re f\n"));
    }
    data.push_str("Q\n");

    let mut graphics_state = Dictionary::new();
    graphics_state.insert("Type".to_owned(), pdf_name("ExtGState"));
    graphics_state.insert("ca".to_owned(), Object::Real(HIGHLIGHT_OPACITY));
    graphics_state.insert("CA".to_owned(), Object::Real(HIGHLIGHT_OPACITY));
    let mut ext_gstates = Dictionary::new();
    ext_gstates.insert("GS0".to_owned(), Object::Dictionary(graphics_state));
    let mut resources = Dictionary::new();
    resources.insert("ExtGState".to_owned(), Object::Dictionary(ext_gstates));

    let mut dictionary = form_dictionary(width, height);
    dictionary.insert("Resources".to_owned(), Object::Dictionary(resources));
    Object::Stream {
        dictionary,
        data: data.into_bytes(),
    }
}

/// Build a stroked-line form XObject for underline or strikeout markup whose
/// coordinates are relative to the annotation `/Rect` lower-left corner.
fn text_markup_appearance(rect: Rect, quads: &[Rect], markup: TextMarkup) -> Object {
    let width = f64::from(rect.width());
    let height = f64::from(rect.height());
    let [red, green, blue] = MARKUP_STROKE_COLOR;
    let mut data = format!(
        "q\n{red:.3} {green:.3} {blue:.3} RG\n{:.3} w\n",
        MARKUP_STROKE_WIDTH
    );
    for quad in quads {
        let x0 = f64::from(quad.x0() - rect.x0());
        let x1 = f64::from(quad.x1() - rect.x0());
        let y = markup.line_offset(*quad, rect);
        if markup == TextMarkup::Squiggly {
            // A half-period-per-segment zigzag reads as a wavy underline at
            // normal zoom and stays a valid stroked path.
            let width = x1 - x0;
            let amplitude = f64::from(MARKUP_STROKE_WIDTH) * 1.2;
            let steps = ((width / 3.0).round() as usize).clamp(4, 96);
            let mut first = true;
            for step in 0..=steps {
                let t = step as f64 / steps as f64;
                let x = x0 + width * t;
                let wave = if step % 2 == 0 { amplitude } else { -amplitude };
                let py = y + wave;
                if first {
                    data.push_str(&format!("{x:.3} {py:.3} m\n"));
                    first = false;
                } else {
                    data.push_str(&format!("{x:.3} {py:.3} l\n"));
                }
            }
            data.push_str("S\n");
        } else {
            data.push_str(&format!("{x0:.3} {y:.3} m {x1:.3} {y:.3} l S\n"));
        }
    }
    data.push_str("Q\n");

    let mut dictionary = form_dictionary(width, height);
    dictionary.insert(
        "Resources".to_owned(),
        Object::Dictionary(Dictionary::new()),
    );
    Object::Stream {
        dictionary,
        data: data.into_bytes(),
    }
}

/// Build a stroked-path form XObject for an ink annotation whose coordinates are
/// relative to the annotation `/Rect` lower-left corner.
fn ink_appearance(rect: Rect, paths: &[&[Point]], stroke_width: f32) -> Object {
    let width = f64::from(rect.width());
    let height = f64::from(rect.height());
    let [red, green, blue] = MARKUP_STROKE_COLOR;
    // Round caps and joins give freehand ink its natural look and make a
    // single-point stroke paint a dot.
    let mut data = format!("q\n{red:.3} {green:.3} {blue:.3} RG\n{stroke_width:.3} w\n1 J 1 j\n");
    for path in paths {
        let mut points = path.iter();
        let Some(first) = points.next() else {
            continue;
        };
        let x = f64::from(first.x - rect.x0());
        let y = f64::from(first.y - rect.y0());
        data.push_str(&format!("{x:.3} {y:.3} m\n"));
        for point in points {
            let x = f64::from(point.x - rect.x0());
            let y = f64::from(point.y - rect.y0());
            data.push_str(&format!("{x:.3} {y:.3} l\n"));
        }
        data.push_str("S\n");
    }
    data.push_str("Q\n");

    let mut dictionary = form_dictionary(width, height);
    dictionary.insert(
        "Resources".to_owned(),
        Object::Dictionary(Dictionary::new()),
    );
    Object::Stream {
        dictionary,
        data: data.into_bytes(),
    }
}

fn form_dictionary(width: f64, height: f64) -> Dictionary {
    let mut dictionary = Dictionary::new();
    dictionary.insert("Type".to_owned(), pdf_name("XObject"));
    dictionary.insert("Subtype".to_owned(), pdf_name("Form"));
    dictionary.insert("FormType".to_owned(), Object::Integer(1));
    dictionary.insert(
        "BBox".to_owned(),
        Object::Array(vec![
            Object::Real(0.0),
            Object::Real(0.0),
            Object::Real(width),
            Object::Real(height),
        ]),
    );
    dictionary.insert("Matrix".to_owned(), identity_matrix());
    dictionary
}

fn identity_matrix() -> Object {
    Object::Array(vec![
        Object::Real(1.0),
        Object::Real(0.0),
        Object::Real(0.0),
        Object::Real(1.0),
        Object::Real(0.0),
        Object::Real(0.0),
    ])
}

fn rect_array(rect: Rect) -> Object {
    Object::Array(vec![
        Object::Real(f64::from(rect.x0())),
        Object::Real(f64::from(rect.y0())),
        Object::Real(f64::from(rect.x1())),
        Object::Real(f64::from(rect.y1())),
    ])
}

/// ISO 32000-1 quad order: upper-left, upper-right, lower-left, lower-right.
fn quad_points_array(quads: &[Rect]) -> Object {
    let mut points = Vec::with_capacity(quads.len() * 8);
    for quad in quads {
        points.push(Object::Real(f64::from(quad.x0())));
        points.push(Object::Real(f64::from(quad.y1())));
        points.push(Object::Real(f64::from(quad.x1())));
        points.push(Object::Real(f64::from(quad.y1())));
        points.push(Object::Real(f64::from(quad.x0())));
        points.push(Object::Real(f64::from(quad.y0())));
        points.push(Object::Real(f64::from(quad.x1())));
        points.push(Object::Real(f64::from(quad.y0())));
    }
    Object::Array(points)
}

fn color_array(color: [f64; 3]) -> Object {
    Object::Array(color.into_iter().map(Object::Real).collect())
}

/// `/L`: the two endpoints of a line annotation.
fn line_array(from: Point, to: Point) -> Object {
    Object::Array(vec![
        Object::Real(f64::from(from.x)),
        Object::Real(f64::from(from.y)),
        Object::Real(f64::from(to.x)),
        Object::Real(f64::from(to.y)),
    ])
}

/// `/Vertices`: alternating x/y coordinates of a polygon.
fn point_array(points: &[Point]) -> Object {
    let mut coordinates = Vec::with_capacity(points.len() * 2);
    for point in points {
        coordinates.push(Object::Real(f64::from(point.x)));
        coordinates.push(Object::Real(f64::from(point.y)));
    }
    Object::Array(coordinates)
}

fn pdf_name(name: &str) -> Object {
    Object::Name(name.to_owned())
}

fn pdf_text(text: &str) -> Object {
    Object::String(encode_text_string(text))
}

// ---------------------------------------------------------------------------
// Document access helpers
// ---------------------------------------------------------------------------

/// Build a reparsed view that includes every staged change.
fn effective_document(document: &Document) -> Result<Document, AnnotationEditError> {
    let bytes = document.save_incremental()?;
    Document::from_bytes(bytes).map_err(AnnotationEditError::Core)
}

/// A page dictionary together with the shape of its `/Annots` entry.
struct PageState {
    page_id: ObjectId,
    dictionary: Dictionary,
    stream_data: Option<Vec<u8>>,
    annots: Annots,
}

/// The `/Annots` value: either an inline array or an indirect array object.
enum Annots {
    Direct(Vec<Object>),
    Indirect(ObjectId, Vec<Object>),
}

impl Annots {
    fn items(&self) -> &[Object] {
        match self {
            Self::Direct(items) | Self::Indirect(_, items) => items,
        }
    }

    fn items_mut(&mut self) -> &mut Vec<Object> {
        match self {
            Self::Direct(items) | Self::Indirect(_, items) => items,
        }
    }
}

fn read_page_state(
    document: &Document,
    page_index: usize,
) -> Result<PageState, AnnotationEditError> {
    let page = document.page(page_index)?;
    let object = document.object(page.id)?;
    let (dictionary, stream_data) = match object {
        Object::Dictionary(dictionary) => (dictionary, None),
        Object::Stream { dictionary, data } => (dictionary, Some(data)),
        _ => return Err(AnnotationEditError::MalformedPage(page.id)),
    };
    let annots = match dictionary.get("Annots").cloned() {
        None | Some(Object::Null) => Annots::Direct(Vec::new()),
        Some(Object::Array(items)) => Annots::Direct(items),
        Some(Object::Reference(id)) => match document.object(id)? {
            Object::Array(items) => Annots::Indirect(id, items),
            Object::Null => Annots::Direct(Vec::new()),
            _ => {
                return Err(AnnotationEditError::MalformedAnnots { page: page_index });
            }
        },
        Some(_) => {
            return Err(AnnotationEditError::MalformedAnnots { page: page_index });
        }
    };
    Ok(PageState {
        page_id: page.id,
        dictionary,
        stream_data,
        annots,
    })
}

fn stage_page_state(document: &mut Document, page: PageState) -> Result<(), AnnotationEditError> {
    let PageState {
        page_id,
        mut dictionary,
        stream_data,
        annots,
    } = page;
    match annots {
        Annots::Direct(items) => {
            dictionary.insert("Annots".to_owned(), Object::Array(items));
        }
        Annots::Indirect(id, items) => {
            document.set_object(id, Object::Array(items))?;
        }
    }
    let object = match stream_data {
        Some(data) => Object::Stream { dictionary, data },
        None => Object::Dictionary(dictionary),
    };
    document.set_object(page_id, object)?;
    Ok(())
}

fn list_on_page(
    document: &Document,
    page_index: usize,
) -> Result<Vec<AnnotationView>, AnnotationEditError> {
    let page = read_page_state(document, page_index)?;
    let mut views = Vec::new();
    for item in page.annots.items() {
        let Some(id) = item.as_reference() else {
            continue;
        };
        let Ok(Object::Dictionary(dictionary)) = document.object(id) else {
            continue;
        };
        views.push(view_from_dictionary(document, id, &dictionary));
    }
    Ok(views)
}

fn view_from_dictionary(
    document: &Document,
    object: ObjectId,
    dictionary: &Dictionary,
) -> AnnotationView {
    let subtype = dictionary
        .get("Subtype")
        .and_then(Object::as_name)
        .map(AnnotationSubtype::from_pdf_name)
        .unwrap_or_else(|| AnnotationSubtype::Other(String::new()));
    let parent_id = dictionary
        .get("IRT")
        .and_then(Object::as_reference)
        .map(|parent| {
            document
                .object(parent)
                .ok()
                .and_then(|value| value.as_dictionary().cloned())
                .and_then(|dictionary| string_from_dictionary(&dictionary, "NM"))
                .unwrap_or_else(|| format!("{} {} R", parent.number, parent.generation))
        });
    AnnotationView {
        object,
        id: string_from_dictionary(dictionary, "NM").unwrap_or_default(),
        kind: AnnotationKind::from_pdf_name(subtype.as_pdf_name()),
        subtype,
        contents: string_from_dictionary(dictionary, "Contents").unwrap_or_default(),
        author: string_from_dictionary(dictionary, "T").unwrap_or_default(),
        parent_id,
        state: dictionary
            .get("State")
            .and_then(Object::as_name)
            .map(str::to_owned),
        rect: rect_from_dictionary(dictionary),
        date: string_from_dictionary(dictionary, "M"),
        quads: quad_rects_from_dictionary(dictionary),
        ink_paths: ink_paths_from_dictionary(dictionary),
        color: color_from_dictionary(dictionary),
        stroke_width: stroke_width_from_dictionary(dictionary),
        line: line_endpoints_from_dictionary(dictionary),
        vertices: vertices_from_dictionary(dictionary),
        stamp_name: dictionary
            .get("Name")
            .and_then(Object::as_name)
            .map(str::to_owned),
    }
}

/// Decode `/QuadPoints` into one rectangle per eight-number group.
fn quad_rects_from_dictionary(dictionary: &Dictionary) -> Vec<Rect> {
    let Some(Object::Array(items)) = dictionary.get("QuadPoints") else {
        return Vec::new();
    };
    let Some(values) = items
        .iter()
        .map(Object::as_f64)
        .collect::<Option<Vec<f64>>>()
    else {
        return Vec::new();
    };
    values
        .as_slice()
        .as_chunks::<8>()
        .0
        .iter()
        .map(|group| {
            let xs = [group[0], group[2], group[4], group[6]];
            let ys = [group[1], group[3], group[5], group[7]];
            let x0 = xs.iter().copied().fold(f64::INFINITY, f64::min);
            let x1 = xs.iter().copied().fold(f64::NEG_INFINITY, f64::max);
            let y0 = ys.iter().copied().fold(f64::INFINITY, f64::min);
            let y1 = ys.iter().copied().fold(f64::NEG_INFINITY, f64::max);
            Rect::saturating(x0 as f32, y0 as f32, x1 as f32, y1 as f32)
        })
        .collect()
}

/// Decode `/InkList` into one point list per stroke, skipping malformed entries.
fn ink_paths_from_dictionary(dictionary: &Dictionary) -> Vec<Vec<Point>> {
    let Some(Object::Array(strokes)) = dictionary.get("InkList") else {
        return Vec::new();
    };
    strokes
        .iter()
        .filter_map(|stroke| {
            let items = stroke.as_array()?;
            let values = items
                .iter()
                .map(Object::as_f64)
                .collect::<Option<Vec<f64>>>()?;
            Some(
                values
                    .as_slice()
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|pair| Point::new(pair[0] as f32, pair[1] as f32))
                    .collect(),
            )
        })
        .collect()
}

/// Decode the `/C` colour array, preserving its component count.
fn color_from_dictionary(dictionary: &Dictionary) -> Option<Vec<f64>> {
    let items = dictionary.get("C").and_then(Object::as_array)?;
    items.iter().map(Object::as_f64).collect()
}

/// Decode `/L` into the `(from, to)` endpoints of a line annotation.
fn line_endpoints_from_dictionary(dictionary: &Dictionary) -> Option<(Point, Point)> {
    let items = dictionary.get("L").and_then(Object::as_array)?;
    if items.len() != 4 {
        return None;
    }
    let values: Option<Vec<f64>> = items.iter().map(Object::as_f64).collect();
    let values = values?;
    let from = Point::new(values[0] as f32, values[1] as f32);
    let to = Point::new(values[2] as f32, values[3] as f32);
    if from.is_finite() && to.is_finite() {
        Some((from, to))
    } else {
        None
    }
}

/// Decode `/Vertices` into polygon points, skipping malformed arrays.
fn vertices_from_dictionary(dictionary: &Dictionary) -> Vec<Point> {
    let Some(items) = dictionary.get("Vertices").and_then(Object::as_array) else {
        return Vec::new();
    };
    if items.len() < 6 || !items.len().is_multiple_of(2) {
        return Vec::new();
    }
    let Some(values) = items
        .iter()
        .map(Object::as_f64)
        .collect::<Option<Vec<f64>>>()
    else {
        return Vec::new();
    };
    values
        .as_slice()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| Point::new(pair[0] as f32, pair[1] as f32))
        .collect()
}

/// Decode the `/BS /W` border width.
fn stroke_width_from_dictionary(dictionary: &Dictionary) -> Option<f32> {
    dictionary
        .get("BS")
        .and_then(Object::as_dictionary)
        .and_then(|border| border.get("W"))
        .and_then(Object::as_f64)
        .map(|width| width as f32)
}

fn find_annotation(
    document: &Document,
    page_index: usize,
    id: &str,
) -> Result<(usize, ObjectId, Dictionary), AnnotationEditError> {
    let page = read_page_state(document, page_index)?;
    for (index, item) in page.annots.items().iter().enumerate() {
        let Some(object_id) = item.as_reference() else {
            continue;
        };
        let Ok(Object::Dictionary(dictionary)) = document.object(object_id) else {
            continue;
        };
        if string_from_dictionary(&dictionary, "NM").as_deref() == Some(id) {
            return Ok((index, object_id, dictionary));
        }
    }
    Err(AnnotationEditError::AnnotationNotFound {
        id: id.to_owned(),
        page: page_index,
    })
}

fn annotation_id_exists(
    document: &Document,
    annots: &Annots,
    id: &str,
) -> Result<bool, AnnotationEditError> {
    for item in annots.items() {
        let Some(object_id) = item.as_reference() else {
            continue;
        };
        if let Object::Dictionary(dictionary) = document.object(object_id)?
            && string_from_dictionary(&dictionary, "NM").as_deref() == Some(id)
        {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Reject a quad whose corners fall outside the page media box.
fn ensure_quads_on_page(
    page: usize,
    media_box: Rectangle,
    quads: &[Rect],
) -> Result<(), AnnotationEditError> {
    for quad in quads {
        ensure_points_on_page(
            page,
            media_box,
            [
                Point::new(quad.x0(), quad.y0()),
                Point::new(quad.x1(), quad.y0()),
                Point::new(quad.x1(), quad.y1()),
                Point::new(quad.x0(), quad.y1()),
            ],
        )?;
    }
    Ok(())
}

/// Reject a point that is not finite or that falls outside the page media box.
fn ensure_points_on_page(
    page: usize,
    media_box: Rectangle,
    points: impl IntoIterator<Item = Point>,
) -> Result<(), AnnotationEditError> {
    for point in points {
        if !point.is_finite() {
            return Err(AnnotationEditError::Geometry(
                GeometryError::NonFiniteCoordinate {
                    field: "point",
                    value: point.x,
                },
            ));
        }
        let x = f64::from(point.x);
        let y = f64::from(point.y);
        if x < media_box.left || x > media_box.right || y < media_box.bottom || y > media_box.top {
            return Err(AnnotationEditError::CoordinateOutsidePage {
                page,
                x: point.x,
                y: point.y,
            });
        }
    }
    Ok(())
}

fn rect_from_dictionary(dictionary: &Dictionary) -> Rect {
    let Some(Object::Array(items)) = dictionary.get("Rect") else {
        return Rect::saturating(0.0, 0.0, 0.0, 0.0);
    };
    if items.len() != 4 {
        return Rect::saturating(0.0, 0.0, 0.0, 0.0);
    }
    let values: Option<Vec<f64>> = items.iter().map(Object::as_f64).collect();
    let Some(values) = values else {
        return Rect::saturating(0.0, 0.0, 0.0, 0.0);
    };
    Rect::saturating(
        values[0] as f32,
        values[1] as f32,
        values[2] as f32,
        values[3] as f32,
    )
}

fn string_from_dictionary(dictionary: &Dictionary, key: &str) -> Option<String> {
    match dictionary.get(key) {
        Some(Object::String(bytes)) => Some(decode_text_string(bytes)),
        _ => None,
    }
}

fn appearance_object(dictionary: &Dictionary) -> Option<ObjectId> {
    dictionary
        .get("AP")
        .and_then(Object::as_dictionary)
        .and_then(|appearance| appearance.get("N"))
        .and_then(Object::as_reference)
}

fn allocate_ids(document: &Document, count: usize) -> Result<Vec<ObjectId>, AnnotationEditError> {
    let mut candidate = document.allocate_object_number()?;
    let mut ids = Vec::with_capacity(count);
    while ids.len() < count {
        if candidate == 0 || document.is_object_number_used(candidate) {
            candidate = candidate
                .checked_add(1)
                .ok_or(AnnotationEditError::ObjectSpaceExhausted)?;
            continue;
        }
        ids.push(ObjectId::new(candidate, 0));
        candidate = candidate
            .checked_add(1)
            .ok_or(AnnotationEditError::ObjectSpaceExhausted)?;
    }
    Ok(ids)
}

// ---------------------------------------------------------------------------
// Date helpers
// ---------------------------------------------------------------------------

/// Return the current UTC time as a PDF date string (`D:YYYYMMDDHHmmSSZ`).
fn now_pdf_date() -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    pdf_date_from_unix(seconds)
}

fn pdf_date_from_unix(seconds: u64) -> String {
    let days = i64::try_from(seconds / 86_400).unwrap_or(i64::MAX);
    let remainder = seconds % 86_400;
    let (year, month, day) = civil_from_days(days);
    let hour = remainder / 3_600;
    let minute = (remainder % 3_600) / 60;
    let second = remainder % 60;
    format!("D:{year:04}{month:02}{day:02}{hour:02}{minute:02}{second:02}Z")
}

/// Convert days since the Unix epoch to a proleptic Gregorian date.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let shifted = days + 719_468;
    let era = if shifted >= 0 {
        shifted
    } else {
        shifted - 146_096
    } / 146_097;
    let day_of_era = shifted - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = if month_prime < 10 {
        month_prime + 3
    } else {
        month_prime - 9
    };
    if month <= 2 {
        year += 1;
    }
    (year, month as u32, day as u32)
}

// ---------------------------------------------------------------------------
// XFDF parsing
// ---------------------------------------------------------------------------

/// One `<annot>` element decoded from XFDF.
struct RawXfdfAnnot {
    offset: usize,
    attributes: Vec<(String, String)>,
    contents: String,
}

impl RawXfdfAnnot {
    fn attribute(&self, name: &str) -> Option<&str> {
        self.attributes
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }
}

fn required_attribute<'a>(
    annotation: &'a RawXfdfAnnot,
    name: &'static str,
) -> Result<&'a str, AnnotationEditError> {
    annotation
        .attribute(name)
        .ok_or(AnnotationEditError::XfdfAttribute {
            offset: annotation.offset,
            name,
        })
}

fn parse_xfdf_annots(xml: &str) -> Result<Vec<RawXfdfAnnot>, AnnotationEditError> {
    let Some(open) = find_element_open(xml, "annots", 0) else {
        return Ok(Vec::new());
    };
    let open_end = find_tag_end(xml, open)?;
    let body_start = open_end + 1;
    let Some(close_rel) = xml[body_start..].find("</annots>") else {
        return Err(AnnotationEditError::XfdfParse {
            offset: open,
            detail: "missing </annots> close tag".to_owned(),
        });
    };
    let body_end = body_start + close_rel;

    let mut annotations = Vec::new();
    let mut search = body_start;
    while search < body_end {
        let Some(relative) = xml[search..body_end].find("<annot") else {
            break;
        };
        let tag_start = search + relative;
        let after_name = tag_start + "<annot".len();
        let next = xml[after_name..].chars().next();
        let is_annot = matches!(
            next,
            Some(character)
                if character.is_ascii_whitespace() || character == '>' || character == '/'
        );
        if !is_annot {
            search = after_name;
            continue;
        }
        let tag_end = find_tag_end(xml, tag_start)?;
        let attributes = parse_attributes(&xml[after_name..tag_end], after_name)?;
        let self_closing = xml[tag_start..tag_end].trim_end().ends_with('/');
        let (contents, next_search) = if self_closing {
            (String::new(), tag_end + 1)
        } else {
            let Some(close) = xml[tag_end + 1..body_end].find("</annot>") else {
                return Err(AnnotationEditError::XfdfParse {
                    offset: tag_start,
                    detail: "missing </annot> close tag".to_owned(),
                });
            };
            let text_start = tag_end + 1;
            let text_end = text_start + close;
            (
                decode_entities(xml[text_start..text_end].trim()),
                text_end + "</annot>".len(),
            )
        };
        annotations.push(RawXfdfAnnot {
            offset: tag_start,
            attributes,
            contents,
        });
        search = next_search;
    }
    Ok(annotations)
}

/// Find the byte offset of `<name` followed by whitespace, `>` or `/`.
fn find_element_open(xml: &str, name: &str, from: usize) -> Option<usize> {
    let mut search = from;
    while search < xml.len() {
        let relative = xml[search..].find('<')?;
        let at = search + relative;
        let after = at + 1;
        if xml[after..].starts_with(name) {
            let next = xml[after + name.len()..].chars().next();
            if matches!(
                next,
                Some(character)
                    if character.is_ascii_whitespace() || character == '>' || character == '/'
            ) {
                return Some(at);
            }
        }
        search = at + 1;
    }
    None
}

/// Find the `>` that closes the tag starting at `tag_start`, ignoring quoted `>`.
fn find_tag_end(xml: &str, tag_start: usize) -> Result<usize, AnnotationEditError> {
    let bytes = xml.as_bytes();
    let mut quote: Option<u8> = None;
    let mut position = tag_start + 1;
    while position < bytes.len() {
        let byte = bytes[position];
        match quote {
            Some(active) => {
                if byte == active {
                    quote = None;
                }
            }
            None => match byte {
                b'"' | b'\'' => quote = Some(byte),
                b'>' => return Ok(position),
                _ => {}
            },
        }
        position += 1;
    }
    Err(AnnotationEditError::XfdfParse {
        offset: tag_start,
        detail: "unterminated tag".to_owned(),
    })
}

/// Parse `name="value"` pairs from the inside of an element's open tag.
fn parse_attributes(
    text: &str,
    offset: usize,
) -> Result<Vec<(String, String)>, AnnotationEditError> {
    let bytes = text.as_bytes();
    let mut attributes = Vec::new();
    let mut position = 0usize;
    while position < bytes.len() {
        while position < bytes.len() && is_xml_space(bytes[position]) {
            position += 1;
        }
        if position >= bytes.len() || bytes[position] == b'/' {
            break;
        }
        let name_start = position;
        while position < bytes.len() && bytes[position] != b'=' && !is_xml_space(bytes[position]) {
            position += 1;
        }
        if name_start == position {
            position += 1;
            continue;
        }
        let name = &text[name_start..position];
        while position < bytes.len() && is_xml_space(bytes[position]) {
            position += 1;
        }
        if position >= bytes.len() || bytes[position] != b'=' {
            return Err(AnnotationEditError::XfdfParse {
                offset: offset + name_start,
                detail: format!("attribute '{name}' has no value"),
            });
        }
        position += 1;
        while position < bytes.len() && is_xml_space(bytes[position]) {
            position += 1;
        }
        if position >= bytes.len() || !matches!(bytes[position], b'"' | b'\'') {
            return Err(AnnotationEditError::XfdfParse {
                offset: offset + position,
                detail: format!("attribute '{name}' value must be quoted"),
            });
        }
        let quote = bytes[position];
        position += 1;
        let value_start = position;
        while position < bytes.len() && bytes[position] != quote {
            position += 1;
        }
        if position >= bytes.len() {
            return Err(AnnotationEditError::XfdfParse {
                offset: offset + value_start,
                detail: format!("attribute '{name}' value is unterminated"),
            });
        }
        let value = decode_entities(&text[value_start..position]);
        position += 1;
        attributes.push((name.to_owned(), value));
    }
    Ok(attributes)
}

fn is_xml_space(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\r' | b'\n')
}

fn parse_xfdf_rect(value: &str) -> Result<Rect, AnnotationEditError> {
    let parts: Vec<&str> = value.split(',').collect();
    if parts.len() != 4 {
        return Err(AnnotationEditError::XfdfValue {
            name: "rect",
            value: value.to_owned(),
        });
    }
    let mut numbers = [0.0f32; 4];
    for (slot, part) in numbers.iter_mut().zip(parts.iter()) {
        *slot = part
            .trim()
            .parse::<f32>()
            .map_err(|_| AnnotationEditError::XfdfValue {
                name: "rect",
                value: value.to_owned(),
            })?;
    }
    Rect::new(numbers[0], numbers[1], numbers[2], numbers[3]).map_err(|error| match error {
        GeometryError::InvertedRect { .. } => AnnotationEditError::XfdfValue {
            name: "rect",
            value: value.to_owned(),
        },
        other => AnnotationEditError::Geometry(other),
    })
}

fn format_rect(rect: Rect) -> String {
    format!("{},{},{},{}", rect.x0(), rect.y0(), rect.x1(), rect.y1())
}

fn escape_xml_text(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&apos;"),
            other => escaped.push(other),
        }
    }
    escaped
}

fn decode_entities(text: &str) -> String {
    let mut decoded = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(ampersand) = rest.find('&') {
        decoded.push_str(&rest[..ampersand]);
        let after = &rest[ampersand + 1..];
        let Some(semicolon) = after.find(';').filter(|index| *index <= 12) else {
            decoded.push('&');
            rest = after;
            continue;
        };
        let entity = &after[..semicolon];
        let replacement = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            _ => entity
                .strip_prefix("#x")
                .or_else(|| entity.strip_prefix("#X"))
                .and_then(|hex| u32::from_str_radix(hex, 16).ok())
                .or_else(|| {
                    entity
                        .strip_prefix('#')
                        .and_then(|decimal| decimal.parse::<u32>().ok())
                })
                .and_then(char::from_u32),
        };
        match replacement {
            Some(character) => decoded.push(character),
            None => {
                decoded.push('&');
                decoded.push_str(entity);
                decoded.push(';');
            }
        }
        rest = &after[semicolon + 1..];
    }
    decoded.push_str(rest);
    decoded
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x0: f32, y0: f32, x1: f32, y1: f32) -> Rect {
        Rect::new(x0, y0, x1, y1).expect("test rectangle")
    }

    #[test]
    fn pdf_date_is_utc_and_zero_padded() {
        assert_eq!(pdf_date_from_unix(0), "D:19700101000000Z");
        assert_eq!(pdf_date_from_unix(1_700_000_000), "D:20231114221320Z");
    }

    #[test]
    fn xml_entities_round_trip() {
        let original = "a & b < c > d \"e\" 'f'";
        assert_eq!(decode_entities(&escape_xml_text(original)), original);
        assert_eq!(decode_entities("&#65;&#x42;"), "AB");
        assert_eq!(decode_entities("broken & entity"), "broken & entity");
    }

    #[test]
    fn quad_points_follow_pdf_order() {
        let points = quad_points_array(&[rect(10.0, 20.0, 30.0, 40.0)]);
        let values: Vec<f64> = points
            .as_array()
            .expect("array")
            .iter()
            .filter_map(Object::as_f64)
            .collect();
        assert_eq!(values, vec![10.0, 40.0, 30.0, 40.0, 10.0, 20.0, 30.0, 20.0]);
    }

    #[test]
    fn attributes_are_parsed_with_entities_and_quotes() {
        let attributes =
            parse_attributes(" page=\"1\" author=\"A &amp; B\" id='x'", 0).expect("attributes");
        assert_eq!(attributes[0], ("page".to_owned(), "1".to_owned()));
        assert_eq!(attributes[1], ("author".to_owned(), "A & B".to_owned()));
        assert_eq!(attributes[2], ("id".to_owned(), "x".to_owned()));
    }

    #[test]
    fn quad_points_decode_back_into_rectangles() {
        let quads = [rect(10.0, 20.0, 30.0, 40.0), rect(50.0, 60.0, 70.0, 80.0)];
        let mut dictionary = Dictionary::new();
        dictionary.insert("QuadPoints".to_owned(), quad_points_array(&quads));
        assert_eq!(quad_rects_from_dictionary(&dictionary), quads.to_vec());
    }

    #[test]
    fn ink_list_round_trips_each_stroke() {
        let first = [
            Point::new(1.0, 2.0),
            Point::new(3.0, 4.0),
            Point::new(5.0, 6.0),
        ];
        let second = [Point::new(7.0, 8.0)];
        let paths: Vec<&[Point]> = vec![&first, &second];
        let mut dictionary = Dictionary::new();
        dictionary.insert("InkList".to_owned(), ink_list_array(&paths));
        assert_eq!(
            ink_paths_from_dictionary(&dictionary),
            vec![first.to_vec(), second.to_vec()]
        );
    }

    #[test]
    fn text_markup_lines_stay_inside_the_annotation_box() {
        let quad = rect(10.0, 20.0, 30.0, 40.0);
        let bounds = rect(10.0, 20.0, 30.0, 40.0);
        let underline = TextMarkup::Underline.line_offset(quad, bounds);
        assert!(
            underline > 0.0 && underline < f64::from(bounds.height()),
            "underline offset {underline} must stay inside the box"
        );
        let strikeout = TextMarkup::StrikeOut.line_offset(quad, bounds);
        assert!((strikeout - f64::from(bounds.height()) / 2.0).abs() < 1e-6);
    }

    #[test]
    fn markup_annotations_name_the_expected_pdf_subtypes() {
        assert_eq!(TextMarkup::Underline.subtype_name(), "Underline");
        assert_eq!(TextMarkup::StrikeOut.subtype_name(), "StrikeOut");
        assert_eq!(
            TextMarkup::Underline.subtype(),
            AnnotationSubtype::Other("Underline".to_owned())
        );
    }

    #[test]
    fn shape_style_validation_is_fail_closed() {
        assert!(validate_shape_style(&ShapeStyle::default()).is_ok());

        assert!(matches!(
            validate_shape_style(&ShapeStyle {
                opacity: 1.5,
                ..ShapeStyle::default()
            }),
            Err(AnnotationEditError::InvalidOpacity { .. })
        ));
        assert!(matches!(
            validate_shape_style(&ShapeStyle {
                stroke_color: [0.0, f64::NAN, 0.0],
                ..ShapeStyle::default()
            }),
            Err(AnnotationEditError::InvalidColorComponent { .. })
        ));
        assert!(matches!(
            validate_shape_style(&ShapeStyle {
                fill_color: Some([0.0, 0.0, 2.0]),
                ..ShapeStyle::default()
            }),
            Err(AnnotationEditError::InvalidColorComponent { .. })
        ));
        assert!(matches!(
            validate_shape_style(&ShapeStyle {
                stroke_width: 0.0,
                ..ShapeStyle::default()
            }),
            Err(AnnotationEditError::InvalidStrokeWidth { .. })
        ));
    }

    #[test]
    fn line_vertices_and_stamp_name_decode_from_the_dictionary() {
        let from = Point::new(10.0, 20.0);
        let to = Point::new(30.0, 40.0);
        let vertices = [
            Point::new(1.0, 2.0),
            Point::new(3.0, 4.0),
            Point::new(5.0, 6.0),
        ];
        let mut dictionary = Dictionary::new();
        dictionary.insert("L".to_owned(), line_array(from, to));
        dictionary.insert("Vertices".to_owned(), point_array(&vertices));
        dictionary.insert("Name".to_owned(), pdf_name("Approved"));

        assert_eq!(
            line_endpoints_from_dictionary(&dictionary),
            Some((from, to))
        );
        assert_eq!(vertices_from_dictionary(&dictionary), vertices.to_vec());
        assert_eq!(
            dictionary.get("Name").and_then(Object::as_name),
            Some("Approved")
        );

        dictionary.insert(
            "L".to_owned(),
            Object::Array(vec![Object::Real(1.0), Object::Real(2.0)]),
        );
        assert_eq!(line_endpoints_from_dictionary(&dictionary), None);
        dictionary.insert("Vertices".to_owned(), Object::Array(Vec::new()));
        assert!(vertices_from_dictionary(&dictionary).is_empty());
    }

    #[test]
    fn open_arrow_legs_sit_behind_the_endpoint() {
        let from = Point::new(10.0, 10.0);
        let to = Point::new(110.0, 10.0);
        let legs = arrow_head_legs(from, to).expect("a non-degenerate line has an arrowhead");
        for leg in legs {
            assert!(leg.x < to.x, "arrow legs point back along the line");
            assert!(
                (leg.y - to.y).abs() > 1.0,
                "the legs open away from the axis"
            );
        }
        assert!(arrow_head_legs(from, from).is_none());
    }

    #[test]
    fn stamp_text_is_sanitized_and_escaped_for_the_content_stream() {
        assert_eq!(sanitize_stamp_text("a(b)\\c\n\u{e9}"), "a(b)\\c??");

        let appearance = stamp_appearance(rect(10.0, 10.0, 210.0, 60.0), "x) Tj ET 0 0 m (y");
        let Object::Stream { data, .. } = appearance else {
            panic!("stamp appearance must be a stream");
        };
        let drawing = String::from_utf8(data).expect("appearance content is ASCII");
        assert!(
            drawing.contains("x\\) Tj ET 0 0 m \\(y"),
            "the label must be escaped: {drawing:?}"
        );
        assert_eq!(
            drawing.matches("Tj\nET").count(),
            1,
            "only the real text operator ends the literal: {drawing:?}"
        );
        assert!(drawing.contains("/F1"));
        assert!(drawing.contains(" re S"));
    }

    #[test]
    fn polygon_area_detects_degenerate_outlines() {
        let triangle = [
            Point::new(0.0, 0.0),
            Point::new(10.0, 0.0),
            Point::new(0.0, 10.0),
        ];
        assert!((polygon_area(&triangle) - 50.0).abs() < 1e-6);
        let collinear = [
            Point::new(0.0, 0.0),
            Point::new(5.0, 5.0),
            Point::new(10.0, 10.0),
        ];
        assert!(polygon_area(&collinear).abs() < 1e-6);
    }
}
