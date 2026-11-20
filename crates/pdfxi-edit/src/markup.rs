//! Typed options and deterministic appearance geometry for drawing annotations.
//!
//! This module completes the typed models for the drawing/markup toolset that
//! the older [`crate::pdf`] mapping keeps as opaque subtypes. It is entirely
//! standard-library only and reuses the dependency-free [`Dict`]/[`Value`]
//! intermediate and the [`PathCommand`] geometry primitives already defined in
//! `pdfxi-edit`.
//!
//! - [`FreeTextOptions`], [`CalloutOptions`], [`LineOptions`], [`ArrowOptions`],
//!   [`ShapeOptions`], and [`CloudOptions`] cover the free-text/callout/line/
//!   arrow/shape/cloud tools (REVIEW-002).
//! - [`InkOptions`], [`StampOptions`], and [`FileAttachmentOptions`] cover the
//!   pencil/stamp/file-attachment tools (REVIEW-003). The attachment model is a
//!   *reference* (a name plus icon); caller-held bytes are never embedded here.
//!
//! Every option type validates its input at construction, derives a pure
//! [`MarkupAppearance`] whose commands are emitted in a fixed order, and
//! round-trips through [`DrawingOptions::to_dict`] /
//! [`DrawingOptions::from_dict`] using a private `XI_Draw...` key namespace.
//! Equal input always produces byte-identical dictionaries and geometry.

use core::fmt;

use crate::geometry::{Point, Rect};
use crate::pdf::appearance::PathCommand;
use crate::pdf::serialize::{BorderLineStyle, BorderStyle};
use crate::pdf::value::{Dict, Value};

/// Errors produced while validating or parsing drawing options.
#[derive(Debug, Clone, PartialEq)]
pub enum MarkupError {
    /// A required dictionary key was absent.
    MissingKey(&'static str),
    /// A dictionary value had the wrong type.
    WrongType {
        /// Key that held the wrong type.
        key: String,
        /// Description of the expected type.
        expected: &'static str,
    },
    /// A numeric value was non-finite or outside its allowed range.
    InvalidNumber {
        /// Key or field name of the offending value.
        key: &'static str,
    },
    /// A name value was not recognised.
    InvalidName {
        /// Key that held the name.
        key: &'static str,
        /// The rejected name.
        value: String,
    },
    /// The `XI_DrawKind` name did not match any drawing tool.
    UnknownKind(String),
    /// An ink model had no non-empty stroke.
    EmptyInkPaths,
    /// A polygon had fewer than three vertices.
    TooFewVertices,
    /// A border style failed validation.
    InvalidBorder(String),
}

impl fmt::Display for MarkupError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingKey(key) => write!(f, "missing drawing key `{key}`"),
            Self::WrongType { key, expected } => {
                write!(f, "drawing key `{key}` must be {expected}")
            }
            Self::InvalidNumber { key } => write!(f, "drawing value `{key}` is not a valid number"),
            Self::InvalidName { key, value } => {
                write!(f, "drawing key `{key}` has unknown name `{value}`")
            }
            Self::UnknownKind(name) => write!(f, "unknown drawing kind `{name}`"),
            Self::EmptyInkPaths => f.write_str("an ink annotation needs at least one stroke"),
            Self::TooFewVertices => f.write_str("a polygon needs at least three vertices"),
            Self::InvalidBorder(reason) => write!(f, "invalid border style: {reason}"),
        }
    }
}

impl std::error::Error for MarkupError {}

/// The drawing tool a [`DrawingOptions`] value configures.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DrawingKind {
    /// A free-standing text box.
    FreeText,
    /// A text box with a leader line.
    Callout,
    /// A straight line.
    Line,
    /// A line with an arrowhead.
    Arrow,
    /// A rectangle, ellipse, or polygon.
    Shape,
    /// A scalloped cloud outline.
    Cloud,
    /// A freehand pencil (ink) stroke.
    Ink,
    /// A graphical stamp.
    Stamp,
    /// A file-attachment icon.
    FileAttachment,
}

impl DrawingKind {
    /// Stable lowercase name used in dictionaries and diagnostics.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::FreeText => "free_text",
            Self::Callout => "callout",
            Self::Line => "line",
            Self::Arrow => "arrow",
            Self::Shape => "shape",
            Self::Cloud => "cloud",
            Self::Ink => "ink",
            Self::Stamp => "stamp",
            Self::FileAttachment => "file_attachment",
        }
    }
}

/// Horizontal alignment of free text inside its box.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextAlignment {
    /// Left aligned.
    Left,
    /// Centered.
    Center,
    /// Right aligned.
    Right,
}

impl TextAlignment {
    /// Stable name used in dictionaries.
    pub fn as_name(self) -> &'static str {
        match self {
            Self::Left => "Left",
            Self::Center => "Center",
            Self::Right => "Right",
        }
    }

    /// Parses an alignment name.
    pub fn from_name(name: &str) -> Result<Self, MarkupError> {
        match name {
            "Left" => Ok(Self::Left),
            "Center" => Ok(Self::Center),
            "Right" => Ok(Self::Right),
            other => Err(MarkupError::InvalidName {
                key: "XI_DrawAlignment",
                value: other.to_string(),
            }),
        }
    }
}

/// The arrowhead style drawn at the end of an [`ArrowOptions`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArrowHead {
    /// No head (a plain line).
    None,
    /// An open (unfilled) arrowhead.
    Open,
    /// A closed (filled) arrowhead.
    Closed,
}

impl ArrowHead {
    /// Stable name used in dictionaries.
    pub fn as_name(self) -> &'static str {
        match self {
            Self::None => "None",
            Self::Open => "Open",
            Self::Closed => "Closed",
        }
    }

    /// Parses an arrowhead name.
    pub fn from_name(name: &str) -> Result<Self, MarkupError> {
        match name {
            "None" => Ok(Self::None),
            "Open" => Ok(Self::Open),
            "Closed" => Ok(Self::Closed),
            other => Err(MarkupError::InvalidName {
                key: "XI_DrawHead",
                value: other.to_string(),
            }),
        }
    }
}

/// The geometric primitive used by [`ShapeOptions`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShapeKind {
    /// An axis-aligned rectangle.
    Rectangle,
    /// An ellipse inscribed in the annotation rectangle.
    Oval,
    /// A closed polygon described by explicit vertices.
    Polygon,
}

impl ShapeKind {
    /// Stable name used in dictionaries.
    pub fn as_name(self) -> &'static str {
        match self {
            Self::Rectangle => "Rectangle",
            Self::Oval => "Oval",
            Self::Polygon => "Polygon",
        }
    }

    /// Parses a shape-kind name.
    pub fn from_name(name: &str) -> Result<Self, MarkupError> {
        match name {
            "Rectangle" => Ok(Self::Rectangle),
            "Oval" => Ok(Self::Oval),
            "Polygon" => Ok(Self::Polygon),
            other => Err(MarkupError::InvalidName {
                key: "XI_DrawShapeKind",
                value: other.to_string(),
            }),
        }
    }
}

/// Standard stamp captions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StampName {
    /// "APPROVED".
    Approved,
    /// "CONFIDENTIAL".
    Confidential,
    /// "DRAFT".
    Draft,
    /// "FINAL".
    Final,
}

impl StampName {
    /// Stable name used in dictionaries and the printed caption.
    pub fn as_name(self) -> &'static str {
        match self {
            Self::Approved => "Approved",
            Self::Confidential => "Confidential",
            Self::Draft => "Draft",
            Self::Final => "Final",
        }
    }

    /// Parses a stamp name.
    pub fn from_name(name: &str) -> Result<Self, MarkupError> {
        match name {
            "Approved" => Ok(Self::Approved),
            "Confidential" => Ok(Self::Confidential),
            "Draft" => Ok(Self::Draft),
            "Final" => Ok(Self::Final),
            other => Err(MarkupError::InvalidName {
                key: "XI_DrawStamp",
                value: other.to_string(),
            }),
        }
    }
}

/// Icon drawn for a [`FileAttachmentOptions`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AttachmentIcon {
    /// A push-pin outline.
    PushPin,
    /// A paperclip outline.
    Paperclip,
    /// A small graph outline.
    Graph,
}

impl AttachmentIcon {
    /// Stable name used in dictionaries.
    pub fn as_name(self) -> &'static str {
        match self {
            Self::PushPin => "PushPin",
            Self::Paperclip => "Paperclip",
            Self::Graph => "Graph",
        }
    }

    /// Parses an attachment icon name.
    pub fn from_name(name: &str) -> Result<Self, MarkupError> {
        match name {
            "PushPin" => Ok(Self::PushPin),
            "Paperclip" => Ok(Self::Paperclip),
            "Graph" => Ok(Self::Graph),
            other => Err(MarkupError::InvalidName {
                key: "XI_DrawIcon",
                value: other.to_string(),
            }),
        }
    }
}

/// Typed options for a free-text box (REVIEW-002).
#[derive(Clone, Debug, PartialEq)]
pub struct FreeTextOptions {
    /// Text shown in the box.
    pub text: String,
    /// Font size in points; must be finite and positive.
    pub font_size: f32,
    /// Horizontal text alignment.
    pub alignment: TextAlignment,
    /// Border appearance of the box.
    pub border: BorderStyle,
}

impl FreeTextOptions {
    /// Creates validated free-text options.
    pub fn new(
        text: impl Into<String>,
        font_size: f32,
        alignment: TextAlignment,
        border: BorderStyle,
    ) -> Result<Self, MarkupError> {
        if !font_size.is_finite() || font_size <= 0.0 {
            return Err(MarkupError::InvalidNumber { key: "font_size" });
        }
        Ok(Self {
            text: text.into(),
            font_size,
            alignment,
            border,
        })
    }
}

/// Typed options for a callout with a leader line (REVIEW-002).
#[derive(Clone, Debug, PartialEq)]
pub struct CalloutOptions {
    /// Text shown in the callout box.
    pub text: String,
    /// Point the leader line points at, in page coordinates.
    pub leader_end: Point,
    /// Border appearance of the box.
    pub border: BorderStyle,
}

impl CalloutOptions {
    /// Creates validated callout options.
    pub fn new(
        text: impl Into<String>,
        leader_end: Point,
        border: BorderStyle,
    ) -> Result<Self, MarkupError> {
        if !leader_end.is_finite() {
            return Err(MarkupError::InvalidNumber { key: "leader_end" });
        }
        Ok(Self {
            text: text.into(),
            leader_end,
            border,
        })
    }
}

/// Typed options for a straight line (REVIEW-002).
#[derive(Clone, Debug, PartialEq)]
pub struct LineOptions {
    /// First endpoint.
    pub start: Point,
    /// Second endpoint.
    pub end: Point,
    /// Line appearance.
    pub border: BorderStyle,
}

impl LineOptions {
    /// Creates validated line options; the endpoints must be finite and distinct.
    pub fn new(start: Point, end: Point, border: BorderStyle) -> Result<Self, MarkupError> {
        if !start.is_finite() || !end.is_finite() {
            return Err(MarkupError::InvalidNumber { key: "line_points" });
        }
        if start == end {
            return Err(MarkupError::InvalidNumber { key: "line_points" });
        }
        Ok(Self { start, end, border })
    }
}

/// Typed options for an arrow (REVIEW-002).
#[derive(Clone, Debug, PartialEq)]
pub struct ArrowOptions {
    /// Underlying line.
    pub line: LineOptions,
    /// Arrowhead style.
    pub head: ArrowHead,
    /// Length of each arrowhead wing in page units.
    pub head_length: f32,
}

impl ArrowOptions {
    /// Creates validated arrow options.
    pub fn new(
        start: Point,
        end: Point,
        head: ArrowHead,
        head_length: f32,
    ) -> Result<Self, MarkupError> {
        if !head_length.is_finite() || head_length <= 0.0 {
            return Err(MarkupError::InvalidNumber { key: "head_length" });
        }
        Ok(Self {
            line: LineOptions::new(start, end, BorderStyle::default())?,
            head,
            head_length,
        })
    }
}

/// Typed options for a rectangle, oval, or polygon (REVIEW-002).
#[derive(Clone, Debug, PartialEq)]
pub struct ShapeOptions {
    /// Which primitive to draw.
    pub kind: ShapeKind,
    /// Explicit vertices, used only for polygons.
    pub vertices: Vec<Point>,
    /// Border appearance.
    pub border: BorderStyle,
}

impl ShapeOptions {
    /// Creates validated shape options.
    ///
    /// A polygon needs at least three finite vertices; rectangle and oval need
    /// none and derive their outline from the annotation rectangle instead.
    pub fn new(
        kind: ShapeKind,
        vertices: Vec<Point>,
        border: BorderStyle,
    ) -> Result<Self, MarkupError> {
        if kind == ShapeKind::Polygon {
            if vertices.len() < 3 {
                return Err(MarkupError::TooFewVertices);
            }
            if vertices.iter().any(|point| !point.is_finite()) {
                return Err(MarkupError::InvalidNumber { key: "vertices" });
            }
        }
        Ok(Self {
            kind,
            vertices,
            border,
        })
    }
}

/// Typed options for a scalloped cloud outline (REVIEW-002).
#[derive(Clone, Debug, PartialEq)]
pub struct CloudOptions {
    /// Number of bumps per side; must be at least one.
    pub bumps: u32,
    /// Outward height of each bump in page units.
    pub amplitude: f32,
    /// Border appearance.
    pub border: BorderStyle,
}

impl CloudOptions {
    /// Creates validated cloud options.
    pub fn new(bumps: u32, amplitude: f32, border: BorderStyle) -> Result<Self, MarkupError> {
        if bumps == 0 || !amplitude.is_finite() || amplitude < 0.0 {
            return Err(MarkupError::InvalidNumber { key: "bumps" });
        }
        Ok(Self {
            bumps,
            amplitude,
            border,
        })
    }
}

/// Typed options for a freehand pencil (ink) stroke (REVIEW-003).
#[derive(Clone, Debug, PartialEq)]
pub struct InkOptions {
    /// One or more non-empty strokes.
    pub paths: Vec<Vec<Point>>,
    /// Ink appearance.
    pub border: BorderStyle,
}

impl InkOptions {
    /// Creates validated ink options; at least one stroke must carry a point.
    pub fn new(paths: Vec<Vec<Point>>, border: BorderStyle) -> Result<Self, MarkupError> {
        if !paths.iter().any(|path| !path.is_empty()) {
            return Err(MarkupError::EmptyInkPaths);
        }
        if paths.iter().flatten().any(|point| !point.is_finite()) {
            return Err(MarkupError::InvalidNumber { key: "paths" });
        }
        Ok(Self { paths, border })
    }
}

/// Typed options for a graphical stamp (REVIEW-003).
#[derive(Clone, Debug, PartialEq)]
pub struct StampOptions {
    /// Caption printed inside the stamp.
    pub name: StampName,
    /// Border appearance of the stamp box.
    pub border: BorderStyle,
}

impl StampOptions {
    /// Creates stamp options for a caption.
    pub fn new(name: StampName, border: BorderStyle) -> Self {
        Self { name, border }
    }
}

/// Typed options for a file-attachment annotation (REVIEW-003).
///
/// This is a *reference* to a file the caller owns; stream bytes are never
/// copied into this model.
#[derive(Clone, Debug, PartialEq)]
pub struct FileAttachmentOptions {
    /// Name of the embedded file this annotation refers to.
    pub attachment_name: String,
    /// Icon drawn at the attachment point.
    pub icon: AttachmentIcon,
    /// Border appearance of the icon box.
    pub border: BorderStyle,
}

impl FileAttachmentOptions {
    /// Creates validated attachment-reference options.
    pub fn new(
        attachment_name: impl Into<String>,
        icon: AttachmentIcon,
        border: BorderStyle,
    ) -> Result<Self, MarkupError> {
        let attachment_name = attachment_name.into();
        if attachment_name.is_empty() {
            return Err(MarkupError::MissingKey("attachment_name"));
        }
        Ok(Self {
            attachment_name,
            icon,
            border,
        })
    }
}

/// Typed options for any drawing tool.
#[derive(Clone, Debug, PartialEq)]
pub enum DrawingOptions {
    /// Free-text box.
    FreeText(FreeTextOptions),
    /// Callout with a leader line.
    Callout(CalloutOptions),
    /// Straight line.
    Line(LineOptions),
    /// Arrow.
    Arrow(ArrowOptions),
    /// Rectangle, oval, or polygon.
    Shape(ShapeOptions),
    /// Cloud outline.
    Cloud(CloudOptions),
    /// Freehand ink stroke.
    Ink(InkOptions),
    /// Graphical stamp.
    Stamp(StampOptions),
    /// File-attachment icon.
    FileAttachment(FileAttachmentOptions),
}

impl DrawingOptions {
    /// Returns the tool these options configure.
    pub fn kind(&self) -> DrawingKind {
        match self {
            Self::FreeText(_) => DrawingKind::FreeText,
            Self::Callout(_) => DrawingKind::Callout,
            Self::Line(_) => DrawingKind::Line,
            Self::Arrow(_) => DrawingKind::Arrow,
            Self::Shape(_) => DrawingKind::Shape,
            Self::Cloud(_) => DrawingKind::Cloud,
            Self::Ink(_) => DrawingKind::Ink,
            Self::Stamp(_) => DrawingKind::Stamp,
            Self::FileAttachment(_) => DrawingKind::FileAttachment,
        }
    }

    /// Derives the deterministic appearance geometry of these options within
    /// `rect`.
    pub fn appearance(&self, rect: &Rect) -> MarkupAppearance {
        let (commands, text_lines) = match self {
            Self::FreeText(options) => (
                crate::pdf::appearance::border_path(rect, &options.border).commands,
                wrap_text(&options.text, wrap_columns(rect, options.font_size)),
            ),
            Self::Callout(options) => {
                let mut commands =
                    crate::pdf::appearance::border_path(rect, &options.border).commands;
                let centre =
                    Point::new((rect.x0() + rect.x1()) / 2.0, (rect.y0() + rect.y1()) / 2.0);
                commands.push(PathCommand::MoveTo(centre));
                commands.push(PathCommand::LineTo(options.leader_end));
                (commands, wrap_text(&options.text, 40))
            }
            Self::Line(options) => (line_commands(options.start, options.end), Vec::new()),
            Self::Arrow(options) => {
                let mut commands = line_commands(options.line.start, options.line.end);
                if options.head != ArrowHead::None {
                    commands.extend(arrow_head_commands(
                        options.line.start,
                        options.line.end,
                        options.head_length,
                    ));
                }
                (commands, Vec::new())
            }
            Self::Shape(options) => (shape_commands(rect, options), Vec::new()),
            Self::Cloud(options) => (cloud_commands(rect, options), Vec::new()),
            Self::Ink(options) => {
                let mut commands = Vec::new();
                for path in &options.paths {
                    if let Some((first, rest)) = path.split_first() {
                        commands.push(PathCommand::MoveTo(*first));
                        for point in rest {
                            commands.push(PathCommand::LineTo(*point));
                        }
                    }
                }
                (commands, Vec::new())
            }
            Self::Stamp(options) => (
                crate::pdf::appearance::border_path(rect, &options.border).commands,
                vec![options.name.as_name().to_string()],
            ),
            Self::FileAttachment(options) => {
                let mut commands =
                    crate::pdf::appearance::border_path(rect, &options.border).commands;
                // A deterministic icon marker: a small centred cross.
                let cx = (rect.x0() + rect.x1()) / 2.0;
                let cy = (rect.y0() + rect.y1()) / 2.0;
                let half = (rect.width().min(rect.height()) / 8.0).max(0.0);
                commands.push(PathCommand::MoveTo(Point::new(cx - half, cy)));
                commands.push(PathCommand::LineTo(Point::new(cx + half, cy)));
                commands.push(PathCommand::MoveTo(Point::new(cx, cy - half)));
                commands.push(PathCommand::LineTo(Point::new(cx, cy + half)));
                let _ = options.icon; // icon variant is preserved in the dictionary
                (commands, Vec::new())
            }
        };
        MarkupAppearance {
            kind: self.kind(),
            commands,
            text_lines,
        }
    }

    /// Serializes these options into a private-namespaced dictionary.
    pub fn to_dict(&self) -> Dict {
        let mut dict = Dict::new();
        dict.insert(
            "XI_DrawKind".to_string(),
            Value::Name(self.kind().as_str().to_string()),
        );
        match self {
            Self::FreeText(options) => {
                dict.insert("XI_DrawText".to_string(), Value::Text(options.text.clone()));
                dict.insert(
                    "XI_DrawFontSize".to_string(),
                    Value::Real(f64::from(options.font_size)),
                );
                dict.insert(
                    "XI_DrawAlignment".to_string(),
                    Value::Name(options.alignment.as_name().to_string()),
                );
                write_border(&mut dict, &options.border);
            }
            Self::Callout(options) => {
                dict.insert("XI_DrawText".to_string(), Value::Text(options.text.clone()));
                dict.insert(
                    "XI_DrawLeaderEnd".to_string(),
                    point_value(options.leader_end),
                );
                write_border(&mut dict, &options.border);
            }
            Self::Line(options) => {
                dict.insert("XI_DrawStart".to_string(), point_value(options.start));
                dict.insert("XI_DrawEnd".to_string(), point_value(options.end));
                write_border(&mut dict, &options.border);
            }
            Self::Arrow(options) => {
                dict.insert("XI_DrawStart".to_string(), point_value(options.line.start));
                dict.insert("XI_DrawEnd".to_string(), point_value(options.line.end));
                dict.insert(
                    "XI_DrawHead".to_string(),
                    Value::Name(options.head.as_name().to_string()),
                );
                dict.insert(
                    "XI_DrawHeadLength".to_string(),
                    Value::Real(f64::from(options.head_length)),
                );
            }
            Self::Shape(options) => {
                dict.insert(
                    "XI_DrawShapeKind".to_string(),
                    Value::Name(options.kind.as_name().to_string()),
                );
                dict.insert(
                    "XI_DrawVertices".to_string(),
                    Value::Array(options.vertices.iter().copied().map(point_value).collect()),
                );
                write_border(&mut dict, &options.border);
            }
            Self::Cloud(options) => {
                dict.insert(
                    "XI_DrawBumps".to_string(),
                    Value::Int(i64::from(options.bumps)),
                );
                dict.insert(
                    "XI_DrawAmplitude".to_string(),
                    Value::Real(f64::from(options.amplitude)),
                );
                write_border(&mut dict, &options.border);
            }
            Self::Ink(options) => {
                dict.insert(
                    "XI_DrawPaths".to_string(),
                    Value::Array(
                        options
                            .paths
                            .iter()
                            .map(|path| {
                                Value::Array(path.iter().copied().map(point_value).collect())
                            })
                            .collect(),
                    ),
                );
                write_border(&mut dict, &options.border);
            }
            Self::Stamp(options) => {
                dict.insert(
                    "XI_DrawStamp".to_string(),
                    Value::Name(options.name.as_name().to_string()),
                );
                write_border(&mut dict, &options.border);
            }
            Self::FileAttachment(options) => {
                dict.insert(
                    "XI_DrawAttachment".to_string(),
                    Value::Text(options.attachment_name.clone()),
                );
                dict.insert(
                    "XI_DrawIcon".to_string(),
                    Value::Name(options.icon.as_name().to_string()),
                );
                write_border(&mut dict, &options.border);
            }
        }
        dict
    }

    /// Parses drawing options from a dictionary produced by [`Self::to_dict`].
    pub fn from_dict(dict: &Dict) -> Result<Self, MarkupError> {
        let kind_name = required_name(dict, "XI_DrawKind")?;
        let options = match kind_name.as_str() {
            "free_text" => Self::FreeText(FreeTextOptions::new(
                required_text(dict, "XI_DrawText")?,
                required_number(dict, "XI_DrawFontSize")? as f32,
                TextAlignment::from_name(&required_name(dict, "XI_DrawAlignment")?)?,
                read_border(dict)?,
            )?),
            "callout" => Self::Callout(CalloutOptions::new(
                required_text(dict, "XI_DrawText")?,
                read_point(dict, "XI_DrawLeaderEnd")?,
                read_border(dict)?,
            )?),
            "line" => Self::Line(LineOptions::new(
                read_point(dict, "XI_DrawStart")?,
                read_point(dict, "XI_DrawEnd")?,
                read_border(dict)?,
            )?),
            "arrow" => Self::Arrow(ArrowOptions::new(
                read_point(dict, "XI_DrawStart")?,
                read_point(dict, "XI_DrawEnd")?,
                ArrowHead::from_name(&required_name(dict, "XI_DrawHead")?)?,
                required_number(dict, "XI_DrawHeadLength")? as f32,
            )?),
            "shape" => Self::Shape(ShapeOptions::new(
                ShapeKind::from_name(&required_name(dict, "XI_DrawShapeKind")?)?,
                read_points_array(dict, "XI_DrawVertices")?,
                read_border(dict)?,
            )?),
            "cloud" => {
                let bumps = required_int(dict, "XI_DrawBumps")?;
                let bumps = u32::try_from(bumps).map_err(|_| MarkupError::InvalidNumber {
                    key: "XI_DrawBumps",
                })?;
                Self::Cloud(CloudOptions::new(
                    bumps,
                    required_number(dict, "XI_DrawAmplitude")? as f32,
                    read_border(dict)?,
                )?)
            }
            "ink" => Self::Ink(InkOptions::new(read_paths(dict)?, read_border(dict)?)?),
            "stamp" => Self::Stamp(StampOptions::new(
                StampName::from_name(&required_name(dict, "XI_DrawStamp")?)?,
                read_border(dict)?,
            )),
            "file_attachment" => Self::FileAttachment(FileAttachmentOptions::new(
                required_text(dict, "XI_DrawAttachment")?,
                AttachmentIcon::from_name(&required_name(dict, "XI_DrawIcon")?)?,
                read_border(dict)?,
            )?),
            other => return Err(MarkupError::UnknownKind(other.to_string())),
        };
        Ok(options)
    }
}

/// Deterministic appearance geometry derived from a [`DrawingOptions`].
#[derive(Clone, Debug, PartialEq)]
pub struct MarkupAppearance {
    /// Tool whose options produced this appearance.
    pub kind: DrawingKind,
    /// Drawing commands in a fixed, repeatable order.
    pub commands: Vec<PathCommand>,
    /// Text lines to render, empty for purely graphical tools.
    pub text_lines: Vec<String>,
}

fn point_value(point: Point) -> Value {
    Value::Array(vec![
        Value::Real(f64::from(point.x)),
        Value::Real(f64::from(point.y)),
    ])
}

fn write_border(dict: &mut Dict, border: &BorderStyle) {
    dict.insert(
        "XI_DrawBorderWidth".to_string(),
        Value::Real(f64::from(border.width)),
    );
    dict.insert(
        "XI_DrawBorderStyle".to_string(),
        Value::Name(border.style.as_name().to_string()),
    );
    dict.insert(
        "XI_DrawBorderDash".to_string(),
        Value::Array(
            border
                .dash
                .iter()
                .copied()
                .map(|d| Value::Real(f64::from(d)))
                .collect(),
        ),
    );
}

fn read_border(dict: &Dict) -> Result<BorderStyle, MarkupError> {
    let width = required_number(dict, "XI_DrawBorderWidth")? as f32;
    let style = BorderLineStyle::from_name(&required_name(dict, "XI_DrawBorderStyle")?)
        .map_err(|error| MarkupError::InvalidBorder(error.to_string()))?;
    let dash = match dict.get("XI_DrawBorderDash") {
        Some(Value::Array(values)) => values
            .iter()
            .map(|value| {
                value
                    .as_number()
                    .map(|number| number as f32)
                    .ok_or(MarkupError::WrongType {
                        key: "XI_DrawBorderDash".to_string(),
                        expected: "a number",
                    })
            })
            .collect::<Result<Vec<f32>, MarkupError>>()?,
        None => Vec::new(),
        Some(_) => {
            return Err(MarkupError::WrongType {
                key: "XI_DrawBorderDash".to_string(),
                expected: "an array",
            });
        }
    };
    BorderStyle::new(width, style, dash)
        .map_err(|error| MarkupError::InvalidBorder(error.to_string()))
}

fn required_name(dict: &Dict, key: &'static str) -> Result<String, MarkupError> {
    match dict.get(key) {
        Some(Value::Name(name)) => Ok(name.clone()),
        Some(_) => Err(MarkupError::WrongType {
            key: key.to_string(),
            expected: "a name",
        }),
        None => Err(MarkupError::MissingKey(key)),
    }
}

fn required_text(dict: &Dict, key: &'static str) -> Result<String, MarkupError> {
    match dict.get(key) {
        Some(Value::Text(text)) => Ok(text.clone()),
        Some(_) => Err(MarkupError::WrongType {
            key: key.to_string(),
            expected: "a string",
        }),
        None => Err(MarkupError::MissingKey(key)),
    }
}

fn required_number(dict: &Dict, key: &'static str) -> Result<f64, MarkupError> {
    match dict.get(key) {
        Some(value) => value.as_number().ok_or(MarkupError::WrongType {
            key: key.to_string(),
            expected: "a number",
        }),
        None => Err(MarkupError::MissingKey(key)),
    }
}

fn required_int(dict: &Dict, key: &'static str) -> Result<i64, MarkupError> {
    match dict.get(key) {
        Some(Value::Int(value)) => Ok(*value),
        Some(_) => Err(MarkupError::WrongType {
            key: key.to_string(),
            expected: "an integer",
        }),
        None => Err(MarkupError::MissingKey(key)),
    }
}

fn read_point(dict: &Dict, key: &'static str) -> Result<Point, MarkupError> {
    let values = match dict.get(key) {
        Some(Value::Array(values)) if values.len() == 2 => values,
        Some(_) => {
            return Err(MarkupError::WrongType {
                key: key.to_string(),
                expected: "a two-element point array",
            });
        }
        None => return Err(MarkupError::MissingKey(key)),
    };
    let x = values[0].as_number().ok_or(MarkupError::WrongType {
        key: key.to_string(),
        expected: "a number",
    })? as f32;
    let y = values[1].as_number().ok_or(MarkupError::WrongType {
        key: key.to_string(),
        expected: "a number",
    })? as f32;
    if !x.is_finite() || !y.is_finite() {
        return Err(MarkupError::InvalidNumber { key });
    }
    Ok(Point::new(x, y))
}

fn read_points_array(dict: &Dict, key: &'static str) -> Result<Vec<Point>, MarkupError> {
    match dict.get(key) {
        None => Ok(Vec::new()),
        Some(Value::Array(values)) => values
            .iter()
            .map(|value| match value {
                Value::Array(pair) if pair.len() == 2 => {
                    let x = pair[0].as_number().ok_or(MarkupError::WrongType {
                        key: key.to_string(),
                        expected: "a number",
                    })? as f32;
                    let y = pair[1].as_number().ok_or(MarkupError::WrongType {
                        key: key.to_string(),
                        expected: "a number",
                    })? as f32;
                    if !x.is_finite() || !y.is_finite() {
                        return Err(MarkupError::InvalidNumber { key });
                    }
                    Ok(Point::new(x, y))
                }
                _ => Err(MarkupError::WrongType {
                    key: key.to_string(),
                    expected: "a two-element point array",
                }),
            })
            .collect(),
        Some(_) => Err(MarkupError::WrongType {
            key: key.to_string(),
            expected: "an array",
        }),
    }
}

fn read_paths(dict: &Dict) -> Result<Vec<Vec<Point>>, MarkupError> {
    match dict.get("XI_DrawPaths") {
        None => Ok(Vec::new()),
        Some(Value::Array(paths)) => paths
            .iter()
            .map(|path| match path {
                Value::Array(values) => values
                    .iter()
                    .map(|value| match value {
                        Value::Array(pair) if pair.len() == 2 => {
                            let x = pair[0].as_number().ok_or(MarkupError::WrongType {
                                key: "XI_DrawPaths".to_string(),
                                expected: "a number",
                            })? as f32;
                            let y = pair[1].as_number().ok_or(MarkupError::WrongType {
                                key: "XI_DrawPaths".to_string(),
                                expected: "a number",
                            })? as f32;
                            if !x.is_finite() || !y.is_finite() {
                                return Err(MarkupError::InvalidNumber {
                                    key: "XI_DrawPaths",
                                });
                            }
                            Ok(Point::new(x, y))
                        }
                        _ => Err(MarkupError::WrongType {
                            key: "XI_DrawPaths".to_string(),
                            expected: "a two-element point array",
                        }),
                    })
                    .collect(),
                _ => Err(MarkupError::WrongType {
                    key: "XI_DrawPaths".to_string(),
                    expected: "an array of strokes",
                }),
            })
            .collect(),
        Some(_) => Err(MarkupError::WrongType {
            key: "XI_DrawPaths".to_string(),
            expected: "an array",
        }),
    }
}

fn line_commands(start: Point, end: Point) -> Vec<PathCommand> {
    vec![PathCommand::MoveTo(start), PathCommand::LineTo(end)]
}

/// Builds the two wing strokes of an arrowhead at `end`.
fn arrow_head_commands(start: Point, end: Point, head_length: f32) -> Vec<PathCommand> {
    let dx = end.x - start.x;
    let dy = end.y - start.y;
    let length = (f64::from(dx) * f64::from(dx) + f64::from(dy) * f64::from(dy)).sqrt();
    if length == 0.0 {
        return Vec::new();
    }
    let ux = f64::from(dx) / length;
    let uy = f64::from(dy) / length;
    let angle = 0.5_f64; // radians, a deterministic ~29-degree wing
    let cos_a = angle.cos();
    let sin_a = angle.sin();
    let mut commands = Vec::with_capacity(4);
    for sign in [-1.0_f64, 1.0] {
        let rx = (ux * cos_a + sign * -uy * sin_a) as f32;
        let ry = (uy * cos_a + sign * ux * sin_a) as f32;
        commands.push(PathCommand::MoveTo(end));
        commands.push(PathCommand::LineTo(Point::new(
            end.x - rx * head_length,
            end.y - ry * head_length,
        )));
    }
    commands
}

fn shape_commands(rect: &Rect, options: &ShapeOptions) -> Vec<PathCommand> {
    match options.kind {
        ShapeKind::Rectangle => crate::pdf::appearance::border_path(rect, &options.border).commands,
        ShapeKind::Oval => {
            const SEGMENTS: usize = 16;
            let cx = (rect.x0() + rect.x1()) / 2.0;
            let cy = (rect.y0() + rect.y1()) / 2.0;
            let rx = rect.width() / 2.0;
            let ry = rect.height() / 2.0;
            let mut commands = Vec::with_capacity(SEGMENTS + 1);
            for index in 0..SEGMENTS {
                let angle = 2.0 * core::f32::consts::PI * index as f32 / SEGMENTS as f32;
                let point = Point::new(cx + rx * angle.cos(), cy + ry * angle.sin());
                if index == 0 {
                    commands.push(PathCommand::MoveTo(point));
                } else {
                    commands.push(PathCommand::LineTo(point));
                }
            }
            commands.push(PathCommand::Close);
            commands
        }
        ShapeKind::Polygon => {
            let mut commands = Vec::new();
            if let Some((first, rest)) = options.vertices.split_first() {
                commands.push(PathCommand::MoveTo(*first));
                for point in rest {
                    commands.push(PathCommand::LineTo(*point));
                }
                commands.push(PathCommand::Close);
            }
            commands
        }
    }
}

fn cloud_commands(rect: &Rect, options: &CloudOptions) -> Vec<PathCommand> {
    // Walk the four sides clockwise from the lower-left corner, emitting a
    // deterministic scallop pattern. Odd sample points are pushed outward.
    let segments = options.bumps.saturating_mul(2).max(2) as usize;
    let mut commands = Vec::new();
    let sides = [
        (
            Point::new(rect.x0(), rect.y0()),
            Point::new(rect.x1(), rect.y0()),
            (0.0, -1.0),
        ),
        (
            Point::new(rect.x1(), rect.y0()),
            Point::new(rect.x1(), rect.y1()),
            (1.0, 0.0),
        ),
        (
            Point::new(rect.x1(), rect.y1()),
            Point::new(rect.x0(), rect.y1()),
            (0.0, 1.0),
        ),
        (
            Point::new(rect.x0(), rect.y1()),
            Point::new(rect.x0(), rect.y0()),
            (-1.0, 0.0),
        ),
    ];
    let mut first = true;
    for (from, to, normal) in sides {
        for index in 0..=segments {
            let fraction = index as f32 / segments as f32;
            let bump = if index % 2 == 1 {
                options.amplitude
            } else {
                0.0
            };
            let point = Point::new(
                from.x + (to.x - from.x) * fraction + normal.0 * bump,
                from.y + (to.y - from.y) * fraction + normal.1 * bump,
            );
            if first {
                commands.push(PathCommand::MoveTo(point));
                first = false;
            } else {
                commands.push(PathCommand::LineTo(point));
            }
        }
    }
    commands.push(PathCommand::Close);
    commands
}

fn wrap_columns(rect: &Rect, font_size: f32) -> usize {
    let width = (rect.width() / (font_size * 0.5)).floor() as usize;
    width.clamp(1, 512)
}

/// Wraps `text` into deterministic lines of at most `columns` characters.
fn wrap_text(text: &str, columns: usize) -> Vec<String> {
    let mut lines = Vec::new();
    for paragraph in text.split('\n') {
        let mut current = String::new();
        for word in paragraph.split_whitespace() {
            if current.is_empty() {
                current.push_str(word);
            } else if current.chars().count() + 1 + word.chars().count() <= columns {
                current.push(' ');
                current.push_str(word);
            } else {
                lines.push(core::mem::take(&mut current));
                current.push_str(word);
            }
        }
        lines.push(current);
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::Rect;

    fn rect() -> Rect {
        Rect::new(0.0, 0.0, 100.0, 50.0).expect("test rect must be valid")
    }

    fn point(x: f32, y: f32) -> Point {
        Point::new(x, y)
    }

    fn border() -> BorderStyle {
        BorderStyle::default()
    }

    // REVIEW-002 ---------------------------------------------------------

    #[test]
    fn free_text_options_validate_and_round_trip() {
        let options = DrawingOptions::FreeText(
            FreeTextOptions::new("hello world", 12.0, TextAlignment::Center, border())
                .expect("valid free text"),
        );
        let dict = options.to_dict();
        assert_eq!(
            dict.get("XI_DrawKind").and_then(Value::as_name),
            Some("free_text")
        );
        assert_eq!(DrawingOptions::from_dict(&dict).expect("parse"), options);
        assert!(matches!(
            FreeTextOptions::new("x", -1.0, TextAlignment::Left, border()),
            Err(MarkupError::InvalidNumber { .. })
        ));
    }

    #[test]
    fn callout_appearance_has_a_leader_line_from_the_centre() {
        let leader = point(200.0, 30.0);
        let options = DrawingOptions::Callout(
            CalloutOptions::new("note", leader, border()).expect("valid callout"),
        );
        let appearance = options.appearance(&rect());
        assert_eq!(appearance.kind, DrawingKind::Callout);
        assert_eq!(
            appearance.commands.last(),
            Some(&PathCommand::LineTo(leader))
        );
        assert_eq!(appearance.text_lines, vec!["note".to_string()]);
    }

    #[test]
    fn line_and_arrow_appearance_is_deterministic() {
        let line = DrawingOptions::Line(
            LineOptions::new(point(0.0, 0.0), point(10.0, 10.0), border()).expect("valid line"),
        );
        assert_eq!(
            line.appearance(&rect()).commands,
            vec![
                PathCommand::MoveTo(point(0.0, 0.0)),
                PathCommand::LineTo(point(10.0, 10.0))
            ]
        );
        let arrow = DrawingOptions::Arrow(
            ArrowOptions::new(point(0.0, 0.0), point(10.0, 0.0), ArrowHead::Open, 3.0)
                .expect("valid arrow"),
        );
        let first = arrow.appearance(&rect());
        assert_eq!(first.commands.len(), 6);
        assert_eq!(first, arrow.appearance(&rect()));
        assert_eq!(
            DrawingOptions::from_dict(&arrow.to_dict()).expect("parse"),
            arrow
        );
    }

    #[test]
    fn shape_options_cover_rectangle_oval_and_polygon() {
        let rectangle = DrawingOptions::Shape(
            ShapeOptions::new(ShapeKind::Rectangle, Vec::new(), border()).expect("rectangle"),
        );
        let oval = DrawingOptions::Shape(
            ShapeOptions::new(ShapeKind::Oval, Vec::new(), border()).expect("oval"),
        );
        let polygon = DrawingOptions::Shape(
            ShapeOptions::new(
                ShapeKind::Polygon,
                vec![point(0.0, 0.0), point(10.0, 0.0), point(5.0, 10.0)],
                border(),
            )
            .expect("polygon"),
        );
        let rectangle_commands = rectangle.appearance(&rect()).commands;
        let oval_commands = oval.appearance(&rect()).commands;
        assert_eq!(
            rectangle_commands.first(),
            Some(&PathCommand::MoveTo(point(0.0, 0.0)))
        );
        assert_eq!(oval_commands.last(), Some(&PathCommand::Close));
        assert_ne!(rectangle_commands, oval_commands);
        assert!(
            polygon
                .appearance(&rect())
                .commands
                .contains(&PathCommand::Close)
        );
        assert_eq!(
            DrawingOptions::from_dict(&polygon.to_dict()).expect("parse"),
            polygon
        );
        assert!(matches!(
            ShapeOptions::new(ShapeKind::Polygon, vec![point(0.0, 0.0)], border()),
            Err(MarkupError::TooFewVertices)
        ));
    }

    #[test]
    fn cloud_outline_is_deterministic_and_scales_with_bumps() {
        let few = DrawingOptions::Cloud(CloudOptions::new(2, 1.0, border()).expect("few"));
        let many = DrawingOptions::Cloud(CloudOptions::new(5, 1.0, border()).expect("many"));
        let few_commands = few.appearance(&rect()).commands;
        assert_eq!(few_commands, few.appearance(&rect()).commands);
        assert!(many.appearance(&rect()).commands.len() > few_commands.len());
        assert!(few_commands.contains(&PathCommand::Close));
        assert_eq!(
            DrawingOptions::from_dict(&few.to_dict()).expect("parse"),
            few
        );
        assert!(matches!(
            CloudOptions::new(0, 1.0, border()),
            Err(MarkupError::InvalidNumber { .. })
        ));
    }

    // REVIEW-003 ---------------------------------------------------------

    #[test]
    fn ink_options_reject_empty_paths_and_round_trip() {
        assert!(matches!(
            InkOptions::new(vec![Vec::new()], border()),
            Err(MarkupError::EmptyInkPaths)
        ));
        let ink = DrawingOptions::Ink(
            InkOptions::new(
                vec![
                    vec![point(1.0, 2.0), point(3.0, 4.0)],
                    vec![point(5.0, 6.0)],
                ],
                border(),
            )
            .expect("valid ink"),
        );
        let appearance = ink.appearance(&rect());
        assert_eq!(appearance.kind, DrawingKind::Ink);
        assert_eq!(appearance.commands.len(), 3);
        assert_eq!(
            DrawingOptions::from_dict(&ink.to_dict()).expect("parse"),
            ink
        );
    }

    #[test]
    fn stamp_caption_and_attachment_reference_round_trip() {
        let stamp = DrawingOptions::Stamp(StampOptions::new(StampName::Approved, border()));
        assert_eq!(
            stamp.appearance(&rect()).text_lines,
            vec!["Approved".to_string()]
        );
        assert_eq!(
            DrawingOptions::from_dict(&stamp.to_dict()).expect("parse"),
            stamp
        );

        let attachment = DrawingOptions::FileAttachment(
            FileAttachmentOptions::new("report.txt", AttachmentIcon::Paperclip, border())
                .expect("valid attachment"),
        );
        let dict = attachment.to_dict();
        assert_eq!(
            dict.get("XI_DrawAttachment").and_then(Value::as_text),
            Some("report.txt")
        );
        assert_eq!(DrawingOptions::from_dict(&dict).expect("parse"), attachment);
        assert!(matches!(
            FileAttachmentOptions::new("", AttachmentIcon::Graph, border()),
            Err(MarkupError::MissingKey(_))
        ));
    }

    #[test]
    fn dictionary_round_trip_preserves_border_style_and_is_stable() {
        let options = DrawingOptions::Cloud(
            CloudOptions::new(
                3,
                2.0,
                BorderStyle::new(2.0, BorderLineStyle::Dashed, vec![3.0, 1.0])
                    .expect("valid border"),
            )
            .expect("valid cloud"),
        );
        let once = options.to_dict();
        assert_eq!(once, options.to_dict());
        assert_eq!(DrawingOptions::from_dict(&once).expect("parse"), options);
    }

    #[test]
    fn malformed_dictionaries_return_typed_errors() {
        assert!(matches!(
            DrawingOptions::from_dict(&Dict::new()),
            Err(MarkupError::MissingKey("XI_DrawKind"))
        ));
        let mut dict = Dict::new();
        dict.insert("XI_DrawKind".to_string(), Value::Name("bogus".to_string()));
        assert!(matches!(
            DrawingOptions::from_dict(&dict),
            Err(MarkupError::UnknownKind(_))
        ));
    }
}
