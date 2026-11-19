//! Mapping between the [`Annotation`](crate::Annotation) model and PDF dictionaries.
//!
//! [`PdfAnnotation`] is the dictionary-backed representation. It stores every
//! common annotation entry (`/Subtype`, `/Rect`, `/Contents`, `/T`, `/NM`,
//! `/M`, `/C`, `/F`, `/AP`, `/CA`) plus per-subtype data and a small set of
//! documented extension keys that make the round-trip lossless.
//!
//! # Extension keys
//!
//! The older [`Annotation`](crate::Annotation) model carries an id and a page
//! index that standard PDF annotation dictionaries do not have. They are
//! preserved through the intermediate dictionary with private keys:
//!
//! - `XI_Id` — decimal annotation id (text).
//! - `XI_Page` — decimal zero-based page index (text).
//! - `XI_Alpha` — alpha channel of the colour, 0..=255 (integer).
//! - `XI_FontSize` — FreeText font size in points (real).
//!
//! An unrecognised `/Subtype` is never dropped: it becomes
//! [`PdfSubtype::Opaque`], which retains the subtype name and all unmodelled
//! dictionary entries. [`PdfAnnotation::to_annotation`] reports
//! [`PdfError::UnknownSubtype`] because the older model has no matching kind.

use crate::annotation::{Annotation, AnnotationId, AnnotationKind, Color};
use crate::geometry::{GeometryError, PageIndex, Point, Rect};

use super::error::PdfError;
use super::value::{
    Dict, Value, expect_array, expect_name, optional_array, optional_bool, optional_int,
    optional_name, optional_number, optional_text,
};

/// FreeText flag stored in `/F` marking read-only contents (PDF bit 9).
const FLAG_READ_ONLY: u32 = 1 << 8;
/// FreeText flag stored in `/F` marking locked contents (PDF bit 12).
const FLAG_LOCKED_CONTENTS: u32 = 1 << 11;

/// Top-level dictionary keys written by every annotation, regardless of subtype.
const COMMON_KEYS: &[&str] = &[
    "Type", "Subtype", "Rect", "Contents", "T", "NM", "M", "C", "F", "CA", "AP", "XI_Id",
    "XI_Page", "XI_Alpha",
];

/// Top-level dictionary keys that this module models for a known subtype.
///
/// For known subtypes, any other key is preserved in [`PdfAnnotation::extra`].
/// For opaque subtypes, every non-common key is preserved instead.
const MODEL_KEYS: &[&str] = &[
    "Type",
    "Subtype",
    "Rect",
    "Contents",
    "T",
    "NM",
    "M",
    "C",
    "F",
    "CA",
    "AP",
    "Name",
    "Open",
    "QuadPoints",
    "BS",
    "IC",
    "L",
    "LE",
    "InkList",
    "DA",
    "A",
    "Dest",
    "XI_Id",
    "XI_Page",
    "XI_Alpha",
    "XI_FontSize",
];

/// One quadrilateral of a text markup annotation.
///
/// PDF stores quad points as four `x y` pairs; the points are kept in the
/// order in which they appear in the dictionary so a round-trip is lossless.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Quad {
    /// First corner (upper-left by convention).
    pub p1: Point,
    /// Second corner (upper-right by convention).
    pub p2: Point,
    /// Third corner (lower-left by convention).
    pub p3: Point,
    /// Fourth corner (lower-right by convention).
    pub p4: Point,
}

impl Quad {
    /// Creates a quadrilateral from its four corners.
    pub fn new(p1: Point, p2: Point, p3: Point, p4: Point) -> Self {
        Self { p1, p2, p3, p4 }
    }

    /// Returns the eight numbers of this quad as PDF array values.
    pub fn to_values(self) -> Vec<Value> {
        let points = [self.p1, self.p2, self.p3, self.p4];
        let mut values = Vec::with_capacity(8);
        for point in points {
            values.push(Value::Real(f64::from(point.x)));
            values.push(Value::Real(f64::from(point.y)));
        }
        values
    }

    /// Parses one quad from eight consecutive array values.
    pub fn from_values(values: &[Value], key: &'static str) -> Result<Self, PdfError> {
        if values.len() != 8 {
            return Err(PdfError::InsufficientPoints {
                key,
                expected: 4,
                got: values.len() / 2,
            });
        }
        let mut numbers = [0.0f64; 8];
        for (slot, value) in numbers.iter_mut().zip(values) {
            let number = value.as_number().ok_or_else(|| PdfError::WrongType {
                key: key.to_string(),
                expected: "a number",
            })?;
            if !number.is_finite() {
                return Err(PdfError::NonFinite {
                    key: key.to_string(),
                });
            }
            *slot = number;
        }
        Ok(Self {
            p1: Point::new(numbers[0] as f32, numbers[1] as f32),
            p2: Point::new(numbers[2] as f32, numbers[3] as f32),
            p3: Point::new(numbers[4] as f32, numbers[5] as f32),
            p4: Point::new(numbers[6] as f32, numbers[7] as f32),
        })
    }

    /// Returns the axis-aligned bounds of the four corners.
    pub fn bounds(self) -> Rect {
        let xs = [self.p1.x, self.p2.x, self.p3.x, self.p4.x];
        let ys = [self.p1.y, self.p2.y, self.p3.y, self.p4.y];
        let mut x0 = xs[0];
        let mut x1 = xs[0];
        let mut y0 = ys[0];
        let mut y1 = ys[0];
        for &x in &xs[1..] {
            x0 = x0.min(x);
            x1 = x1.max(x);
        }
        for &y in &ys[1..] {
            y0 = y0.min(y);
            y1 = y1.max(y);
        }
        // Quad corners come from a validated dictionary, but a caller may
        // construct a Quad directly with inverted or non-finite corners.
        // Normalize here so downstream geometry never sees an invalid rect.
        Rect::saturating(x0, y0, x1, y1)
    }
}

/// The line style used by an annotation border.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BorderLineStyle {
    /// A continuous solid line (`/S /S`).
    Solid,
    /// A dashed line (`/S /D`).
    Dashed,
}

impl BorderLineStyle {
    /// Returns the PDF name for this style.
    pub fn as_name(self) -> &'static str {
        match self {
            Self::Solid => "S",
            Self::Dashed => "D",
        }
    }

    /// Parses a style from a PDF name.
    pub fn from_name(name: &str) -> Result<Self, PdfError> {
        match name {
            "S" => Ok(Self::Solid),
            "D" => Ok(Self::Dashed),
            other => Err(PdfError::InvalidName {
                key: "S",
                value: other.to_string(),
            }),
        }
    }
}

/// Border width, style, and dash pattern used by shapes and FreeText.
#[derive(Clone, Debug, PartialEq)]
pub struct BorderStyle {
    /// Border width in page units.
    pub width: f32,
    /// Whether the border is solid or dashed.
    pub style: BorderLineStyle,
    /// Dash pattern (`/D`); empty for a solid border.
    pub dash: Vec<f32>,
}

impl Default for BorderStyle {
    fn default() -> Self {
        Self {
            width: 1.0,
            style: BorderLineStyle::Solid,
            dash: Vec::new(),
        }
    }
}

impl BorderStyle {
    /// Creates a border style, validating width and dash lengths.
    pub fn new(width: f32, style: BorderLineStyle, dash: Vec<f32>) -> Result<Self, PdfError> {
        if !width.is_finite() || width < 0.0 {
            return Err(PdfError::NonFinite {
                key: "W".to_string(),
            });
        }
        for (index, &length) in dash.iter().enumerate() {
            let value = f64::from(length);
            if !value.is_finite() || value <= 0.0 {
                return Err(PdfError::InvalidDashLength { index, value });
            }
        }
        Ok(Self { width, style, dash })
    }
}

/// Data carried by a text (`/Text`) annotation.
#[derive(Clone, Debug, PartialEq)]
pub struct TextData {
    /// Icon shown for the note.
    pub icon: TextIcon,
    /// Whether the note pop-up is initially open.
    pub open: bool,
}

impl Default for TextData {
    fn default() -> Self {
        Self {
            icon: TextIcon::Note,
            open: false,
        }
    }
}

/// Standard icon names for a text annotation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextIcon {
    /// The default note icon.
    Note,
    /// A comment balloon.
    Comment,
    /// A key icon.
    Key,
    /// A help icon.
    Help,
    /// A new-paragraph icon.
    NewParagraph,
    /// A paragraph icon.
    Paragraph,
    /// An insert icon.
    Insert,
}

impl TextIcon {
    /// Returns the PDF name used in `/Name`.
    pub fn as_name(self) -> &'static str {
        match self {
            Self::Note => "Note",
            Self::Comment => "Comment",
            Self::Key => "Key",
            Self::Help => "Help",
            Self::NewParagraph => "NewParagraph",
            Self::Paragraph => "Paragraph",
            Self::Insert => "Insert",
        }
    }

    /// Parses a PDF name into an icon.
    pub fn from_name(name: &str) -> Result<Self, PdfError> {
        match name {
            "Note" => Ok(Self::Note),
            "Comment" => Ok(Self::Comment),
            "Key" => Ok(Self::Key),
            "Help" => Ok(Self::Help),
            "NewParagraph" => Ok(Self::NewParagraph),
            "Paragraph" => Ok(Self::Paragraph),
            "Insert" => Ok(Self::Insert),
            other => Err(PdfError::InvalidName {
                key: "Name",
                value: other.to_string(),
            }),
        }
    }
}

/// Data carried by markup annotations (`Highlight`, `Underline`, `StrikeOut`, `Squiggly`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MarkupData {
    /// Ordered quadrilaterals from `/QuadPoints`.
    pub quad_points: Vec<Quad>,
}

/// Data carried by square and circle annotations.
#[derive(Clone, Debug, PartialEq)]
pub struct ShapeData {
    /// Border appearance.
    pub border: BorderStyle,
    /// Optional interior colour from `/IC`.
    pub interior_color: Option<Color>,
}

/// The ending style at one end of a line annotation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineEnding {
    /// No arrowhead.
    None,
    /// A filled square.
    Square,
    /// A filled circle.
    Circle,
    /// A filled diamond.
    Diamond,
    /// An open arrow.
    OpenArrow,
    /// A closed arrow.
    ClosedArrow,
}

impl LineEnding {
    /// Returns the PDF name used in `/LE`.
    pub fn as_name(self) -> &'static str {
        match self {
            Self::None => "None",
            Self::Square => "Square",
            Self::Circle => "Circle",
            Self::Diamond => "Diamond",
            Self::OpenArrow => "OpenArrow",
            Self::ClosedArrow => "ClosedArrow",
        }
    }

    /// Parses a PDF name into a line ending.
    pub fn from_name(name: &str) -> Result<Self, PdfError> {
        match name {
            "None" => Ok(Self::None),
            "Square" => Ok(Self::Square),
            "Circle" => Ok(Self::Circle),
            "Diamond" => Ok(Self::Diamond),
            "OpenArrow" => Ok(Self::OpenArrow),
            "ClosedArrow" => Ok(Self::ClosedArrow),
            other => Err(PdfError::InvalidName {
                key: "LE",
                value: other.to_string(),
            }),
        }
    }
}

/// Data carried by a line (`/Line`) annotation.
#[derive(Clone, Debug, PartialEq)]
pub struct LineData {
    /// First endpoint from `/L`.
    pub start: Point,
    /// Second endpoint from `/L`.
    pub end: Point,
    /// Ending at [`Self::start`].
    pub start_ending: LineEnding,
    /// Ending at [`Self::end`].
    pub end_ending: LineEnding,
}

/// Data carried by an ink (`/Ink`) annotation.
#[derive(Clone, Debug, PartialEq)]
pub struct InkData {
    /// One or more strokes, each a sequence of points from `/InkList`.
    pub paths: Vec<Vec<Point>>,
}

/// Data carried by a FreeText (`/FreeText`) annotation.
#[derive(Clone, Debug, PartialEq)]
pub struct FreeTextData {
    /// Text to display (also mirrored into the common `/Contents`).
    pub text: String,
    /// Font size in points.
    pub font_size: f32,
    /// Border appearance.
    pub border: BorderStyle,
}

/// Data carried by a link (`/Link`) annotation.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LinkData {
    /// URI target from `/A /URI`.
    pub uri: Option<String>,
    /// Named destination from `/Dest`.
    pub dest: Option<String>,
}

/// The PDF subtype of an annotation and its subtype-specific data.
#[derive(Clone, Debug, PartialEq)]
pub enum PdfSubtype {
    /// A sticky note (`/Text`).
    Text(TextData),
    /// A highlight (`/Highlight`).
    Highlight(MarkupData),
    /// An underline (`/Underline`).
    Underline(MarkupData),
    /// A strikeout (`/StrikeOut`).
    StrikeOut(MarkupData),
    /// A squiggly markup (`/Squiggly`).
    Squiggly(MarkupData),
    /// A rectangle (`/Square`).
    Square(ShapeData),
    /// An ellipse (`/Circle`).
    Circle(ShapeData),
    /// A straight line (`/Line`).
    Line(LineData),
    /// A freehand ink stroke (`/Ink`).
    Ink(InkData),
    /// A text box (`/FreeText`).
    FreeText(FreeTextData),
    /// A hyperlink (`/Link`).
    Link(LinkData),
    /// An unknown subtype, preserved verbatim instead of being dropped.
    Opaque(String),
}

impl PdfSubtype {
    /// Returns the PDF name for this subtype.
    pub fn name(&self) -> &str {
        match self {
            Self::Text(_) => "Text",
            Self::Highlight(_) => "Highlight",
            Self::Underline(_) => "Underline",
            Self::StrikeOut(_) => "StrikeOut",
            Self::Squiggly(_) => "Squiggly",
            Self::Square(_) => "Square",
            Self::Circle(_) => "Circle",
            Self::Line(_) => "Line",
            Self::Ink(_) => "Ink",
            Self::FreeText(_) => "FreeText",
            Self::Link(_) => "Link",
            Self::Opaque(raw) => raw,
        }
    }

    /// Returns `true` when this subtype is an opaque, unknown subtype.
    pub fn is_opaque(&self) -> bool {
        matches!(self, Self::Opaque(_))
    }
}

/// Appearance-stream resource names referenced by `/AP`.
#[derive(Clone, Debug, PartialEq)]
pub struct AppearanceNames {
    /// Name of the normal appearance stream (`/N`).
    pub normal: String,
    /// Name of the rollover appearance stream (`/R`).
    pub rollover: String,
    /// Name of the down appearance stream (`/D`).
    pub down: String,
}

impl Default for AppearanceNames {
    fn default() -> Self {
        Self {
            normal: "n0".to_string(),
            rollover: "r0".to_string(),
            down: "d0".to_string(),
        }
    }
}

/// A dictionary-backed annotation with all fields needed for a lossless round-trip.
#[derive(Clone, Debug, PartialEq)]
pub struct PdfAnnotation {
    /// PDF subtype and its per-type data.
    pub subtype: PdfSubtype,
    /// Normalized annotation rectangle.
    pub rect: Rect,
    /// Contents string (`/Contents`).
    pub contents: String,
    /// Author/title (`/T`).
    pub title: String,
    /// Annotation name (`/NM`).
    pub name: String,
    /// Modification date (`/M`).
    pub mod_date: String,
    /// RGBA appearance colour (`/C` plus `XI_Alpha`).
    pub color: Color,
    /// Constant opacity (`/CA`).
    pub opacity: f32,
    /// Annotation flag bit field (`/F`).
    pub flags: u32,
    /// Appearance stream names (`/AP`).
    pub appearance: AppearanceNames,
    /// Annotation id, preserved through `XI_Id`.
    pub id: AnnotationId,
    /// Page index, preserved through `XI_Page`.
    pub page: PageIndex,
    /// Unmodelled dictionary entries, sorted by key.
    pub extra: Vec<(String, Value)>,
}

impl PdfAnnotation {
    /// Builds a dictionary-backed annotation from the in-memory model.
    ///
    /// Subtypes that have no direct PDF equivalent in this module (for example
    /// `Callout` or `Stamp`) become [`PdfSubtype::Opaque`] using the kind's
    /// stable lowercase name, which keeps the round-trip lossless.
    pub fn from_annotation(annotation: &Annotation) -> Self {
        let subtype = match annotation.kind() {
            AnnotationKind::Note => PdfSubtype::Text(TextData::default()),
            AnnotationKind::Highlight => PdfSubtype::Highlight(MarkupData::default()),
            AnnotationKind::Underline => PdfSubtype::Underline(MarkupData::default()),
            AnnotationKind::Strikeout => PdfSubtype::StrikeOut(MarkupData::default()),
            AnnotationKind::Squiggly => PdfSubtype::Squiggly(MarkupData::default()),
            AnnotationKind::FreeText => PdfSubtype::FreeText(FreeTextData {
                text: annotation.contents().to_string(),
                font_size: 12.0,
                border: BorderStyle::default(),
            }),
            AnnotationKind::Callout => PdfSubtype::Opaque("callout".to_string()),
            AnnotationKind::Line => PdfSubtype::Line(LineData {
                start: Point::new(annotation.rect().x0(), annotation.rect().y0()),
                end: Point::new(annotation.rect().x1(), annotation.rect().y1()),
                start_ending: LineEnding::None,
                end_ending: LineEnding::None,
            }),
            AnnotationKind::Arrow => PdfSubtype::Opaque("arrow".to_string()),
            AnnotationKind::Rectangle => PdfSubtype::Square(ShapeData {
                border: BorderStyle::default(),
                interior_color: None,
            }),
            AnnotationKind::Oval => PdfSubtype::Circle(ShapeData {
                border: BorderStyle::default(),
                interior_color: None,
            }),
            AnnotationKind::Polygon => PdfSubtype::Opaque("polygon".to_string()),
            AnnotationKind::Cloud => PdfSubtype::Opaque("cloud".to_string()),
            AnnotationKind::Pencil => PdfSubtype::Ink(InkData {
                paths: vec![vec![
                    Point::new(annotation.rect().x0(), annotation.rect().y0()),
                    Point::new(annotation.rect().x1(), annotation.rect().y1()),
                ]],
            }),
            AnnotationKind::Stamp => PdfSubtype::Opaque("stamp".to_string()),
            AnnotationKind::FileAttachment => PdfSubtype::Opaque("file_attachment".to_string()),
        };

        let mut flags = 0u32;
        if annotation.is_locked() {
            flags |= FLAG_LOCKED_CONTENTS;
        }
        if annotation.is_read_only() {
            flags |= FLAG_READ_ONLY;
        }

        Self {
            subtype,
            rect: annotation.rect(),
            contents: annotation.contents().to_string(),
            title: annotation.author().to_string(),
            name: format!("ann-{}", annotation.id().get()),
            mod_date: "D:20260101000000Z".to_string(),
            color: annotation.color(),
            opacity: annotation.opacity(),
            flags,
            appearance: AppearanceNames::default(),
            id: annotation.id(),
            page: annotation.page(),
            extra: Vec::new(),
        }
    }

    /// Converts this dictionary-backed annotation back into the in-memory model.
    ///
    /// Fails with [`PdfError::UnknownSubtype`] when the subtype is opaque and
    /// has no corresponding [`AnnotationKind`].
    pub fn to_annotation(&self) -> Result<Annotation, PdfError> {
        let kind = match &self.subtype {
            PdfSubtype::Text(_) => AnnotationKind::Note,
            PdfSubtype::Highlight(_) => AnnotationKind::Highlight,
            PdfSubtype::Underline(_) => AnnotationKind::Underline,
            PdfSubtype::StrikeOut(_) => AnnotationKind::Strikeout,
            PdfSubtype::Squiggly(_) => AnnotationKind::Squiggly,
            PdfSubtype::Square(_) => AnnotationKind::Rectangle,
            PdfSubtype::Circle(_) => AnnotationKind::Oval,
            PdfSubtype::Line(_) => AnnotationKind::Line,
            PdfSubtype::Ink(_) => AnnotationKind::Pencil,
            PdfSubtype::FreeText(_) => AnnotationKind::FreeText,
            PdfSubtype::Link(_) => {
                return Err(PdfError::UnknownSubtype("Link".to_string()));
            }
            PdfSubtype::Opaque(raw) => {
                kind_from_name(raw).ok_or_else(|| PdfError::UnknownSubtype(raw.clone()))?
            }
        };

        let locked = self.flags & FLAG_LOCKED_CONTENTS != 0;
        let read_only = self.flags & FLAG_READ_ONLY != 0;
        let annotation = Annotation::new(
            self.id,
            kind,
            self.page,
            self.rect,
            self.title.clone(),
            self.contents.clone(),
        )
        .map_err(|_| {
            PdfError::InvalidRect(GeometryError::InvertedRect {
                x0: self.rect.x0(),
                y0: self.rect.y0(),
                x1: self.rect.x1(),
                y1: self.rect.y1(),
            })
        })?
        .with_color(self.color)
        .with_locked(locked)
        .with_read_only(read_only);
        let annotation =
            annotation
                .with_opacity(self.opacity)
                .map_err(|_| PdfError::WrongType {
                    key: "CA".to_string(),
                    expected: "a finite number in 0.0..=1.0",
                })?;
        Ok(annotation)
    }

    /// Serializes this annotation into a PDF dictionary.
    pub fn to_dict(&self) -> Dict {
        let mut dict = Dict::new();
        dict.insert("Type".to_string(), Value::Name("Annot".to_string()));
        dict.insert(
            "Subtype".to_string(),
            Value::Name(self.subtype.name().to_string()),
        );
        dict.insert("Rect".to_string(), rect_to_values(self.rect));
        dict.insert("Contents".to_string(), Value::Text(self.contents.clone()));
        dict.insert("T".to_string(), Value::Text(self.title.clone()));
        dict.insert("NM".to_string(), Value::Text(self.name.clone()));
        dict.insert("M".to_string(), Value::Text(self.mod_date.clone()));
        dict.insert("C".to_string(), color_to_values(self.color));
        dict.insert("F".to_string(), Value::Int(i64::from(self.flags)));
        dict.insert("CA".to_string(), Value::Real(f64::from(self.opacity)));

        let mut ap = Dict::new();
        ap.insert("N".to_string(), Value::Name(self.appearance.normal.clone()));
        ap.insert(
            "R".to_string(),
            Value::Name(self.appearance.rollover.clone()),
        );
        ap.insert("D".to_string(), Value::Name(self.appearance.down.clone()));
        dict.insert("AP".to_string(), Value::Dictionary(ap));

        dict.insert("XI_Id".to_string(), Value::Text(self.id.get().to_string()));
        dict.insert(
            "XI_Page".to_string(),
            Value::Text(self.page.get().to_string()),
        );
        dict.insert(
            "XI_Alpha".to_string(),
            Value::Int(i64::from(self.color.a())),
        );

        self.write_subtype(&mut dict);

        for (key, value) in &self.extra {
            if !dict.contains_key(key) {
                dict.insert(key.clone(), value.clone());
            }
        }
        dict
    }

    /// Writes subtype-specific keys into `dict`.
    fn write_subtype(&self, dict: &mut Dict) {
        match &self.subtype {
            PdfSubtype::Text(data) => {
                dict.insert(
                    "Name".to_string(),
                    Value::Name(data.icon.as_name().to_string()),
                );
                dict.insert("Open".to_string(), Value::Bool(data.open));
            }
            PdfSubtype::Highlight(data)
            | PdfSubtype::Underline(data)
            | PdfSubtype::StrikeOut(data)
            | PdfSubtype::Squiggly(data) => {
                let mut points = Vec::new();
                for quad in &data.quad_points {
                    points.extend(quad.to_values());
                }
                dict.insert("QuadPoints".to_string(), Value::Array(points));
            }
            PdfSubtype::Square(data) | PdfSubtype::Circle(data) => {
                dict.insert("BS".to_string(), border_to_values(&data.border));
                if let Some(interior) = data.interior_color {
                    dict.insert("IC".to_string(), color_to_values(interior));
                }
            }
            PdfSubtype::Line(data) => {
                dict.insert(
                    "L".to_string(),
                    Value::Array(vec![
                        Value::Real(f64::from(data.start.x)),
                        Value::Real(f64::from(data.start.y)),
                        Value::Real(f64::from(data.end.x)),
                        Value::Real(f64::from(data.end.y)),
                    ]),
                );
                dict.insert(
                    "LE".to_string(),
                    Value::Array(vec![
                        Value::Name(data.start_ending.as_name().to_string()),
                        Value::Name(data.end_ending.as_name().to_string()),
                    ]),
                );
            }
            PdfSubtype::Ink(data) => {
                let mut strokes = Vec::new();
                for path in &data.paths {
                    let mut values = Vec::with_capacity(path.len() * 2);
                    for point in path {
                        values.push(Value::Real(f64::from(point.x)));
                        values.push(Value::Real(f64::from(point.y)));
                    }
                    strokes.push(Value::Array(values));
                }
                dict.insert("InkList".to_string(), Value::Array(strokes));
            }
            PdfSubtype::FreeText(data) => {
                dict.insert("Contents".to_string(), Value::Text(data.text.clone()));
                dict.insert(
                    "DA".to_string(),
                    Value::Text(format!("0 0 0 rg /Helv {} Tf", data.font_size)),
                );
                dict.insert(
                    "XI_FontSize".to_string(),
                    Value::Real(f64::from(data.font_size)),
                );
                dict.insert("BS".to_string(), border_to_values(&data.border));
            }
            PdfSubtype::Link(data) => {
                if let Some(uri) = &data.uri {
                    let mut action = Dict::new();
                    action.insert("S".to_string(), Value::Name("URI".to_string()));
                    action.insert("URI".to_string(), Value::Text(uri.clone()));
                    dict.insert("A".to_string(), Value::Dictionary(action));
                }
                if let Some(dest) = &data.dest {
                    dict.insert("Dest".to_string(), Value::Text(dest.clone()));
                }
            }
            PdfSubtype::Opaque(_) => {}
        }
    }

    /// Parses and validates a PDF dictionary into a `PdfAnnotation`.
    pub fn from_dict(dict: &Dict) -> Result<Self, PdfError> {
        let subtype = parse_subtype(expect_name(dict, "Subtype")?, dict)?;
        let rect = parse_rect(expect_array(dict, "Rect")?)?;
        let contents = optional_text(dict, "Contents")?.unwrap_or("").to_string();
        let title = optional_text(dict, "T")?.unwrap_or("").to_string();
        let name = optional_text(dict, "NM")?.unwrap_or("").to_string();
        let mod_date = optional_text(dict, "M")?.unwrap_or("").to_string();

        let alpha = match optional_int(dict, "XI_Alpha")? {
            Some(value) if (0..=255).contains(&value) => value as u8,
            Some(_) => {
                return Err(PdfError::WrongType {
                    key: "XI_Alpha".to_string(),
                    expected: "an integer in 0..=255",
                });
            }
            None => 255,
        };
        let color = parse_color(expect_array(dict, "C")?, alpha)?;

        let opacity_number = optional_number(dict, "CA")?.unwrap_or(1.0);
        if !opacity_number.is_finite() || !(0.0..=1.0).contains(&opacity_number) {
            return Err(PdfError::WrongType {
                key: "CA".to_string(),
                expected: "a finite number in 0.0..=1.0",
            });
        }
        let opacity = opacity_number as f32;

        let flags_i64 = optional_int(dict, "F")?.unwrap_or(0);
        if flags_i64 < 0 {
            return Err(PdfError::WrongType {
                key: "F".to_string(),
                expected: "a non-negative integer",
            });
        }
        let flags = flags_i64 as u32;

        let appearance = match dict.get("AP") {
            None => AppearanceNames::default(),
            Some(value) => {
                let ap = value.as_dict().ok_or_else(|| PdfError::WrongType {
                    key: "AP".to_string(),
                    expected: "a dictionary",
                })?;
                AppearanceNames {
                    normal: optional_name(ap, "N")?.unwrap_or("").to_string(),
                    rollover: optional_name(ap, "R")?.unwrap_or("").to_string(),
                    down: optional_name(ap, "D")?.unwrap_or("").to_string(),
                }
            }
        };

        let id = parse_decimal(dict, "XI_Id")?.unwrap_or(0);
        let page = PageIndex::new(parse_decimal(dict, "XI_Page")?.unwrap_or(0) as usize);

        let extra = dict
            .iter()
            .filter(|(key, _)| {
                if matches!(subtype, PdfSubtype::Opaque(_)) {
                    !COMMON_KEYS.contains(&key.as_str())
                } else {
                    !MODEL_KEYS.contains(&key.as_str())
                }
            })
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect();

        Ok(Self {
            subtype,
            rect,
            contents,
            title,
            name,
            mod_date,
            color,
            opacity,
            flags,
            appearance,
            id: AnnotationId::new(id),
            page,
            extra,
        })
    }
}

/// Converts an [`Annotation`] directly into a PDF dictionary.
pub fn annotation_to_dict(annotation: &Annotation) -> Dict {
    PdfAnnotation::from_annotation(annotation).to_dict()
}

/// Parses a PDF dictionary into an [`Annotation`].
///
/// This is the inverse of [`annotation_to_dict`] for all supported kinds.
/// Opaque subtypes without an `AnnotationKind` equivalent are reported as
/// [`PdfError::UnknownSubtype`]; use [`PdfAnnotation::from_dict`] to retain them.
pub fn dict_to_annotation(dict: &Dict) -> Result<Annotation, PdfError> {
    PdfAnnotation::from_dict(dict)?.to_annotation()
}

/// Maps a lowercase `AnnotationKind` name back to its kind.
fn kind_from_name(name: &str) -> Option<AnnotationKind> {
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
    all.into_iter().find(|kind| kind.as_str() == name)
}

/// Normalizes four rectangle coordinates into a validated [`Rect`].
///
/// The input may list corners in any order; the lower and upper values are
/// paired before validation. A zero-width or zero-height rectangle is rejected.
fn parse_rect(values: &[Value]) -> Result<Rect, PdfError> {
    if values.len() != 4 {
        return Err(PdfError::InvalidRect(GeometryError::InvertedRect {
            x0: 0.0,
            y0: 0.0,
            x1: 0.0,
            y1: 0.0,
        }));
    }
    let mut numbers = [0.0f64; 4];
    for (slot, value) in numbers.iter_mut().zip(values) {
        let number = value.as_number().ok_or_else(|| PdfError::WrongType {
            key: "Rect".to_string(),
            expected: "a number",
        })?;
        if !number.is_finite() {
            return Err(PdfError::NonFinite {
                key: "Rect".to_string(),
            });
        }
        *slot = number;
    }
    let x0 = numbers[0].min(numbers[2]);
    let x1 = numbers[0].max(numbers[2]);
    let y0 = numbers[1].min(numbers[3]);
    let y1 = numbers[1].max(numbers[3]);
    if x0 >= x1 || y0 >= y1 {
        return Err(PdfError::DegenerateRect);
    }
    Rect::new(x0 as f32, y0 as f32, x1 as f32, y1 as f32).map_err(PdfError::InvalidRect)
}

/// Converts a rectangle into four PDF real values.
fn rect_to_values(rect: Rect) -> Value {
    Value::Array(vec![
        Value::Real(f64::from(rect.x0())),
        Value::Real(f64::from(rect.y0())),
        Value::Real(f64::from(rect.x1())),
        Value::Real(f64::from(rect.y1())),
    ])
}

/// Validates and converts a three-component colour array.
fn parse_color(values: &[Value], alpha: u8) -> Result<Color, PdfError> {
    if values.len() != 3 {
        return Err(PdfError::WrongType {
            key: "C".to_string(),
            expected: "an array of three numbers",
        });
    }
    let mut components = [0u8; 3];
    for (index, value) in values.iter().enumerate() {
        let number = value.as_number().ok_or_else(|| PdfError::WrongType {
            key: "C".to_string(),
            expected: "a number",
        })?;
        if !number.is_finite() || !(0.0..=1.0).contains(&number) {
            return Err(PdfError::InvalidColorComponent {
                index,
                value: number,
            });
        }
        components[index] = (number * 255.0).round().clamp(0.0, 255.0) as u8;
    }
    Ok(Color::new(components[0], components[1], components[2]).with_alpha(alpha))
}

/// Converts a colour into a three-component PDF array.
fn color_to_values(color: Color) -> Value {
    Value::Array(vec![
        Value::Real(f64::from(color.r()) / 255.0),
        Value::Real(f64::from(color.g()) / 255.0),
        Value::Real(f64::from(color.b()) / 255.0),
    ])
}

/// Parses a pair of x/y coordinates into a point.
fn parse_point(x: f64, y: f64, key: &'static str) -> Result<Point, PdfError> {
    if !x.is_finite() || !y.is_finite() {
        return Err(PdfError::NonFinite {
            key: key.to_string(),
        });
    }
    Ok(Point::new(x as f32, y as f32))
}

/// Parses a flat array of x/y coordinate pairs.
fn parse_points(values: &[Value], key: &'static str) -> Result<Vec<Point>, PdfError> {
    if !values.len().is_multiple_of(2) {
        return Err(PdfError::OddPointCoordinates(key));
    }
    let mut points = Vec::with_capacity(values.len() / 2);
    for pair in values.as_chunks::<2>().0 {
        let x = pair[0].as_number().ok_or_else(|| PdfError::WrongType {
            key: key.to_string(),
            expected: "a number",
        })?;
        let y = pair[1].as_number().ok_or_else(|| PdfError::WrongType {
            key: key.to_string(),
            expected: "a number",
        })?;
        points.push(parse_point(x, y, key)?);
    }
    Ok(points)
}

/// Parses a decimal string held in an extension key.
fn parse_decimal(dict: &Dict, key: &'static str) -> Result<Option<u64>, PdfError> {
    let text = match optional_text(dict, key)? {
        Some(text) => text,
        None => return Ok(None),
    };
    text.parse::<u64>()
        .map(Some)
        .map_err(|_| PdfError::WrongType {
            key: key.to_string(),
            expected: "a decimal integer string",
        })
}

/// Parses a border dictionary, using defaults for missing keys.
fn parse_border(dict: &Dict) -> Result<BorderStyle, PdfError> {
    let width = optional_number(dict, "W")?.unwrap_or(1.0);
    if !width.is_finite() || width < 0.0 {
        return Err(PdfError::NonFinite {
            key: "W".to_string(),
        });
    }
    let style_name = optional_name(dict, "S")?.unwrap_or("S");
    let style = BorderLineStyle::from_name(style_name)?;
    let dash = match optional_array(dict, "D")? {
        None => Vec::new(),
        Some(values) => {
            let mut lengths = Vec::with_capacity(values.len());
            for (index, value) in values.iter().enumerate() {
                let length = value.as_number().ok_or_else(|| PdfError::WrongType {
                    key: "D".to_string(),
                    expected: "a number",
                })?;
                if !length.is_finite() || length <= 0.0 {
                    return Err(PdfError::InvalidDashLength {
                        index,
                        value: length,
                    });
                }
                lengths.push(length as f32);
            }
            lengths
        }
    };
    Ok(BorderStyle {
        width: width as f32,
        style,
        dash,
    })
}

/// Converts a border style into a `/BS` dictionary value.
fn border_to_values(border: &BorderStyle) -> Value {
    let mut dict = Dict::new();
    dict.insert("W".to_string(), Value::Real(f64::from(border.width)));
    dict.insert(
        "S".to_string(),
        Value::Name(border.style.as_name().to_string()),
    );
    if !border.dash.is_empty() {
        let dash = border
            .dash
            .iter()
            .map(|&length| Value::Real(f64::from(length)))
            .collect();
        dict.insert("D".to_string(), Value::Array(dash));
    }
    Value::Dictionary(dict)
}

/// Parses an optional interior colour (`/IC`).
fn parse_interior(dict: &Dict) -> Result<Option<Color>, PdfError> {
    match dict.get("IC") {
        None => Ok(None),
        Some(value) => {
            let values = value.as_array().ok_or_else(|| PdfError::WrongType {
                key: "IC".to_string(),
                expected: "an array",
            })?;
            Ok(Some(parse_color(values, 255)?))
        }
    }
}

/// Parses subtype-specific data from a dictionary.
fn parse_subtype(raw: &str, dict: &Dict) -> Result<PdfSubtype, PdfError> {
    match raw {
        "Text" => {
            let icon_name = optional_name(dict, "Name")?.unwrap_or("Note");
            Ok(PdfSubtype::Text(TextData {
                icon: TextIcon::from_name(icon_name)?,
                open: optional_bool(dict, "Open")?.unwrap_or(false),
            }))
        }
        "Highlight" | "Underline" | "StrikeOut" | "Squiggly" => {
            let quads = match optional_array(dict, "QuadPoints")? {
                None => Vec::new(),
                Some(values) => {
                    if !values.len().is_multiple_of(8) {
                        return Err(PdfError::OddPointCoordinates("QuadPoints"));
                    }
                    values
                        .as_chunks::<8>()
                        .0
                        .iter()
                        .map(|chunk| Quad::from_values(chunk, "QuadPoints"))
                        .collect::<Result<Vec<_>, _>>()?
                }
            };
            let data = MarkupData { quad_points: quads };
            Ok(match raw {
                "Highlight" => PdfSubtype::Highlight(data),
                "Underline" => PdfSubtype::Underline(data),
                "StrikeOut" => PdfSubtype::StrikeOut(data),
                _ => PdfSubtype::Squiggly(data),
            })
        }
        "Square" | "Circle" => {
            let border = match dict.get("BS") {
                None => BorderStyle::default(),
                Some(value) => {
                    let bs = value.as_dict().ok_or_else(|| PdfError::WrongType {
                        key: "BS".to_string(),
                        expected: "a dictionary",
                    })?;
                    parse_border(bs)?
                }
            };
            let shape = ShapeData {
                border,
                interior_color: parse_interior(dict)?,
            };
            Ok(if raw == "Square" {
                PdfSubtype::Square(shape)
            } else {
                PdfSubtype::Circle(shape)
            })
        }
        "Line" => {
            let values = match optional_array(dict, "L")? {
                None => return Err(PdfError::MissingPoints("L")),
                Some(values) => values,
            };
            let points = parse_points(values, "L")?;
            if points.len() != 2 {
                return Err(PdfError::InsufficientPoints {
                    key: "L",
                    expected: 2,
                    got: points.len(),
                });
            }
            let endings = match optional_array(dict, "LE")? {
                None => vec![LineEnding::None, LineEnding::None],
                Some(values) => values
                    .iter()
                    .map(|value| {
                        value
                            .as_name()
                            .ok_or_else(|| PdfError::WrongType {
                                key: "LE".to_string(),
                                expected: "a name",
                            })
                            .and_then(LineEnding::from_name)
                    })
                    .collect::<Result<Vec<_>, _>>()?,
            };
            let start_ending = *endings.first().unwrap_or(&LineEnding::None);
            let end_ending = *endings.get(1).unwrap_or(&LineEnding::None);
            Ok(PdfSubtype::Line(LineData {
                start: points[0],
                end: points[1],
                start_ending,
                end_ending,
            }))
        }
        "Ink" => {
            let strokes = match optional_array(dict, "InkList")? {
                None => return Err(PdfError::MissingPoints("InkList")),
                Some([]) => return Err(PdfError::MissingPoints("InkList")),
                Some(strokes) => strokes,
            };
            let mut paths = Vec::with_capacity(strokes.len());
            for stroke in strokes {
                let values = stroke.as_array().ok_or_else(|| PdfError::WrongType {
                    key: "InkList".to_string(),
                    expected: "an array of point arrays",
                })?;
                let points = parse_points(values, "InkList")?;
                if points.is_empty() {
                    return Err(PdfError::InsufficientPoints {
                        key: "InkList",
                        expected: 1,
                        got: 0,
                    });
                }
                paths.push(points);
            }
            Ok(PdfSubtype::Ink(InkData { paths }))
        }
        "FreeText" => {
            let font_size = match optional_number(dict, "XI_FontSize")? {
                Some(size) if size.is_finite() && size > 0.0 => size as f32,
                _ => 12.0,
            };
            let border = match dict.get("BS") {
                None => BorderStyle::default(),
                Some(value) => {
                    let bs = value.as_dict().ok_or_else(|| PdfError::WrongType {
                        key: "BS".to_string(),
                        expected: "a dictionary",
                    })?;
                    parse_border(bs)?
                }
            };
            Ok(PdfSubtype::FreeText(FreeTextData {
                text: optional_text(dict, "Contents")?.unwrap_or("").to_string(),
                font_size,
                border,
            }))
        }
        "Link" => {
            let uri = match dict.get("A") {
                None => None,
                Some(value) => {
                    let action = value.as_dict().ok_or_else(|| PdfError::WrongType {
                        key: "A".to_string(),
                        expected: "a dictionary",
                    })?;
                    optional_text(action, "URI")?.map(str::to_string)
                }
            };
            Ok(PdfSubtype::Link(LinkData {
                uri,
                dest: optional_text(dict, "Dest")?.map(str::to_string),
            }))
        }
        other => Ok(PdfSubtype::Opaque(other.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::Rect;

    fn rect(x0: f32, y0: f32, x1: f32, y1: f32) -> Rect {
        Rect::new(x0, y0, x1, y1).expect("test rect must be valid")
    }

    fn annotation(id: u64, kind: AnnotationKind) -> Annotation {
        Annotation::new(
            AnnotationId::new(id),
            kind,
            PageIndex::new(3),
            rect(10.0, 20.0, 110.0, 70.0),
            "alice",
            "hello",
        )
        .expect("test annotation must be valid")
        .with_color(Color::new(255, 0, 128).with_alpha(200))
        .with_opacity(0.75)
        .expect("valid opacity")
        .with_locked(true)
        .with_read_only(true)
    }

    #[test]
    fn annotation_round_trip_all_kinds() {
        let kinds = [
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
        for (index, kind) in kinds.into_iter().enumerate() {
            let original = annotation(index as u64 + 1, kind);
            let dict = annotation_to_dict(&original);
            let restored = dict_to_annotation(&dict).expect("round-trip must succeed");
            assert_eq!(restored, original, "round-trip failed for {kind}");
        }
    }

    #[test]
    fn pdf_annotation_round_trip_preserves_per_type_fields() {
        let mut source = PdfAnnotation::from_annotation(&annotation(7, AnnotationKind::Rectangle));
        source.subtype = PdfSubtype::Square(ShapeData {
            border: BorderStyle::new(2.0, BorderLineStyle::Dashed, vec![3.0, 1.0, 0.5, 1.0])
                .expect("valid border"),
            interior_color: Some(Color::new(10, 20, 30)),
        });
        let shaped = source.to_dict();
        assert_eq!(shaped, source.to_dict());
        source.subtype = PdfSubtype::Highlight(MarkupData {
            quad_points: vec![Quad::new(
                Point::new(1.0, 2.0),
                Point::new(3.0, 2.0),
                Point::new(1.0, 4.0),
                Point::new(3.0, 4.0),
            )],
        });
        source.extra = vec![("Custom".to_string(), Value::Int(42))];

        let dict = source.to_dict();
        let restored = PdfAnnotation::from_dict(&dict).expect("valid dict");
        assert_eq!(restored, source);
    }

    #[test]
    fn per_type_dictionary_keys_cover_the_full_toolset() {
        let cases: [(AnnotationKind, &str); 10] = [
            (AnnotationKind::Note, "Name"),
            (AnnotationKind::Highlight, "QuadPoints"),
            (AnnotationKind::Underline, "QuadPoints"),
            (AnnotationKind::Strikeout, "QuadPoints"),
            (AnnotationKind::Rectangle, "BS"),
            (AnnotationKind::Oval, "BS"),
            (AnnotationKind::Line, "L"),
            (AnnotationKind::Pencil, "InkList"),
            (AnnotationKind::FreeText, "DA"),
            (AnnotationKind::Note, "Open"),
        ];
        for (kind, key) in cases {
            let dict = annotation_to_dict(&annotation(1, kind));
            assert!(dict.contains_key(key), "missing /{key} for {kind}");
        }
        let mut link = PdfAnnotation::from_annotation(&annotation(1, AnnotationKind::Note));
        link.subtype = PdfSubtype::Link(LinkData {
            uri: Some("https://example.test".to_string()),
            dest: None,
        });
        let dict = link.to_dict();
        assert!(dict.contains_key("A"));
    }

    #[test]
    fn opaque_unknown_subtype_is_preserved() {
        let mut dict = annotation_to_dict(&annotation(9, AnnotationKind::Note));
        dict.insert("Subtype".to_string(), Value::Name("Widget".to_string()));
        dict.insert("Custom".to_string(), Value::Bool(true));

        let parsed = PdfAnnotation::from_dict(&dict).expect("opaque dict parses");
        assert!(parsed.subtype.is_opaque());
        assert_eq!(parsed.subtype.name(), "Widget");
        assert!(parsed.extra.iter().any(|(key, _)| key == "Custom"));

        let reserialized = parsed.to_dict();
        assert_eq!(reserialized, dict);
        assert!(matches!(
            dict_to_annotation(&dict),
            Err(PdfError::UnknownSubtype(_))
        ));
    }

    #[test]
    fn rectangle_is_normalized_and_degenerate_rects_rejected() {
        let mut dict = annotation_to_dict(&annotation(1, AnnotationKind::Note));
        dict.insert(
            "Rect".to_string(),
            Value::Array(vec![
                Value::Real(100.0),
                Value::Real(70.0),
                Value::Real(10.0),
                Value::Real(20.0),
            ]),
        );
        let parsed = PdfAnnotation::from_dict(&dict).expect("normalized rect");
        assert_eq!(
            (
                parsed.rect.x0(),
                parsed.rect.y0(),
                parsed.rect.x1(),
                parsed.rect.y1()
            ),
            (10.0, 20.0, 100.0, 70.0)
        );

        dict.insert(
            "Rect".to_string(),
            Value::Array(vec![
                Value::Real(5.0),
                Value::Real(5.0),
                Value::Real(5.0),
                Value::Real(10.0),
            ]),
        );
        assert_eq!(
            PdfAnnotation::from_dict(&dict),
            Err(PdfError::DegenerateRect)
        );

        dict.insert(
            "Rect".to_string(),
            Value::Array(vec![
                Value::Real(f64::NAN),
                Value::Real(0.0),
                Value::Real(10.0),
                Value::Real(10.0),
            ]),
        );
        assert!(matches!(
            PdfAnnotation::from_dict(&dict),
            Err(PdfError::NonFinite { .. })
        ));
    }

    #[test]
    fn invalid_color_components_are_rejected() {
        let mut dict = annotation_to_dict(&annotation(1, AnnotationKind::Note));
        dict.insert(
            "C".to_string(),
            Value::Array(vec![Value::Real(0.5), Value::Real(1.5), Value::Real(0.0)]),
        );
        assert_eq!(
            PdfAnnotation::from_dict(&dict),
            Err(PdfError::InvalidColorComponent {
                index: 1,
                value: 1.5
            })
        );
        dict.insert(
            "C".to_string(),
            Value::Array(vec![
                Value::Real(0.5),
                Value::Real(f64::NAN),
                Value::Real(0.0),
            ]),
        );
        assert!(matches!(
            PdfAnnotation::from_dict(&dict),
            Err(PdfError::InvalidColorComponent { .. })
        ));
    }

    #[test]
    fn missing_or_insufficient_points_are_rejected() {
        let mut dict = annotation_to_dict(&annotation(1, AnnotationKind::Line));
        dict.remove("L");
        assert_eq!(
            PdfAnnotation::from_dict(&dict),
            Err(PdfError::MissingPoints("L"))
        );
        dict.insert(
            "L".to_string(),
            Value::Array(vec![Value::Real(1.0), Value::Real(2.0)]),
        );
        assert_eq!(
            PdfAnnotation::from_dict(&dict),
            Err(PdfError::InsufficientPoints {
                key: "L",
                expected: 2,
                got: 1
            })
        );

        let mut ink = annotation_to_dict(&annotation(1, AnnotationKind::Pencil));
        ink.insert("InkList".to_string(), Value::Array(vec![]));
        assert_eq!(
            PdfAnnotation::from_dict(&ink),
            Err(PdfError::MissingPoints("InkList"))
        );
        ink.insert(
            "InkList".to_string(),
            Value::Array(vec![Value::Array(vec![])]),
        );
        assert_eq!(
            PdfAnnotation::from_dict(&ink),
            Err(PdfError::InsufficientPoints {
                key: "InkList",
                expected: 1,
                got: 0
            })
        );
    }
}
