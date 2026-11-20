//! Generation of widget appearance streams and `/AP` dictionaries.
//!
//! This module is dependency-free and deterministic: for a given [`Field`],
//! [`WidgetAppearance`] and [`FieldValue`], [`build_appearance`] always produces
//! the same [`AppearanceDictionary`] bytes. It is a *layout and modeling* tool,
//! not a font rasteriser. It never reads font files, the network, or the
//! filesystem, and it never panics on hostile geometry or values.
//!
//! # Coordinate system
//!
//! [`WidgetAppearance::rect`] is the widget rectangle in **page points**
//! (`x`, `y` = lower-left corner; `width`, `height` = extents), matching a PDF
//! annotation `/Rect`. Appearance-stream content is emitted in the widget's own
//! local coordinate system with a bounding box of
//! `(0, 0, width, height)`: the host places that box onto the page through the
//! annotation `/Rect` and `/Matrix`. Consequently `rect.x`/`rect.y` identify the
//! widget on the page but are *not* added to the content coordinates.
//!
//! # Text layout heuristic
//!
//! Real glyph metrics are unavailable without a font dependency, so text is
//! measured with a documented **average-width heuristic**: each character
//! advances by `font.size * factor`, where [`AVERAGE_WIDTH`] is the default
//! factor, narrow punctuation uses [`NARROW_WIDTH`], and wide capitals use
//! [`WIDE_WIDTH`]. Non-ASCII characters conservatively use [`WIDE_WIDTH`].
//! This is deterministic and platform-independent; it is an approximation of
//! base-14 Helvetica, not an exact match for an arbitrary embedded font.
//!
//! Single-line fields render one line. Multiline fields greedily wrap on
//! whitespace into as many lines as fit vertically (line height =
//! `font.size * 1.2`). A line that is still too wide is **clipped** to the
//! characters that fit and terminated with an ellipsis `…` when there is room
//! for it ([`fit_with_ellipsis`] documents the exact rule). Horizontal alignment
//! is applied by offsetting each line within the available width.
//!
//! # Button and choice appearances
//!
//! Checkbox and radio widgets produce a *stateful* `/N` dictionary. A checkbox
//! uses the conventional Acrobat state names `On` and `Off`
//! ([`STATE_ON`], [`STATE_OFF`]); a radio group uses each choice name plus
//! `Off`. `On` draws a box with a simple check-mark polyline, a radio selection
//! draws a filled circle, and off states draw only the outline. Choice widgets
//! render either a single combo line or a list box whose selected rows carry a
//! highlight rectangle; every row is clipped with the same ellipsis rule.

use crate::field::{Field, FieldFlags, FieldType, FieldValue};
use std::collections::BTreeMap;
use std::fmt;

/// Default advance-width factor for an ordinary ASCII glyph.
pub const AVERAGE_WIDTH: f32 = 0.55;
/// Advance-width factor for narrow glyphs such as `i`, `l` and punctuation.
pub const NARROW_WIDTH: f32 = 0.28;
/// Advance-width factor for wide glyphs such as `M`, `W` and non-ASCII text.
pub const WIDE_WIDTH: f32 = 0.85;
/// Ratio of line height to font size used by the layout heuristic.
pub const LINE_HEIGHT_FACTOR: f32 = 1.2;
/// Horizontal inset from the border to the text, in points.
pub const HORIZONTAL_PADDING: f32 = 2.0;
/// Vertical inset from the border to the text, in points.
pub const VERTICAL_PADDING: f32 = 2.0;
/// Conventional "on" state name of a checkbox appearance (`/AP /N /On`).
pub const STATE_ON: &str = "On";
/// Conventional "off" state name of a button appearance (`/AP /N /Off`).
pub const STATE_OFF: &str = "Off";
/// PDF base font resource name used in the emitted `Tf` operator.
pub const DEFAULT_FONT_RESOURCE: &str = "/F1";

/// An RGB colour with components in the inclusive range `0.0..=1.0`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RgbColor {
    /// Red component.
    pub red: f32,
    /// Green component.
    pub green: f32,
    /// Blue component.
    pub blue: f32,
}

impl RgbColor {
    /// Creates a colour from three components.
    ///
    /// Components are *not* clamped here; [`RgbColor::is_valid`] reports whether
    /// they are finite and within range, which [`WidgetAppearance::validate`]
    /// checks before generating content.
    pub const fn new(red: f32, green: f32, blue: f32) -> Self {
        Self { red, green, blue }
    }

    /// Opaque black (`0, 0, 0`).
    pub const BLACK: Self = Self::new(0.0, 0.0, 0.0);
    /// Opaque white (`1, 1, 1`).
    pub const WHITE: Self = Self::new(1.0, 1.0, 1.0);
    /// Light grey used for selected list-box rows.
    pub const HIGHLIGHT: Self = Self::new(0.85, 0.85, 0.85);

    /// Returns the components as `[red, green, blue]`.
    pub const fn components(self) -> [f32; 3] {
        [self.red, self.green, self.blue]
    }

    /// Returns `true` when all components are finite and within `0.0..=1.0`.
    pub fn is_valid(self) -> bool {
        [self.red, self.green, self.blue]
            .iter()
            .all(|c| c.is_finite() && (0.0..=1.0).contains(c))
    }
}

/// The PDF border style of a widget (`/BS /S`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BorderStyle {
    /// Solid line (the default).
    #[default]
    Solid,
    /// Dashed line.
    Dashed,
    /// Dotted line.
    Dotted,
    /// Beveled edge.
    Beveled,
    /// Inset edge.
    Inset,
    /// Underline only.
    Underline,
}

impl BorderStyle {
    /// Returns the PDF name for this style, without the leading slash.
    pub const fn pdf_name(self) -> &'static str {
        match self {
            Self::Solid => "S",
            Self::Dashed => "D",
            Self::Dotted => "D",
            Self::Beveled => "B",
            Self::Inset => "I",
            Self::Underline => "U",
        }
    }
}

/// Text alignment, corresponding to the PDF `Q` (quadding) value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Alignment {
    /// Left aligned (quadding 0, the default).
    #[default]
    Left,
    /// Centred (quadding 1).
    Center,
    /// Right aligned (quadding 2).
    Right,
}

impl Alignment {
    /// Returns the PDF quadding code (`0`, `1` or `2`).
    pub const fn quadding(self) -> i64 {
        match self {
            Self::Left => 0,
            Self::Center => 1,
            Self::Right => 2,
        }
    }
}

/// Widget rotation in degrees, normalised to right angles.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Rotation {
    /// No rotation (the default).
    #[default]
    Deg0,
    /// 90 degrees counter-clockwise.
    Deg90,
    /// 180 degrees.
    Deg180,
    /// 270 degrees counter-clockwise.
    Deg270,
}

impl Rotation {
    /// Returns the rotation in degrees.
    pub const fn degrees(self) -> i16 {
        match self {
            Self::Deg0 => 0,
            Self::Deg90 => 90,
            Self::Deg180 => 180,
            Self::Deg270 => 270,
        }
    }
}

impl TryFrom<i16> for Rotation {
    type Error = AppearanceError;

    /// Converts a right-angle degree value into a [`Rotation`].
    ///
    /// # Errors
    ///
    /// Returns [`AppearanceError::InvalidRotation`] for any value other than
    /// `0`, `90`, `180` or `270`.
    fn try_from(degrees: i16) -> Result<Self, Self::Error> {
        match degrees {
            0 => Ok(Self::Deg0),
            90 => Ok(Self::Deg90),
            180 => Ok(Self::Deg180),
            270 => Ok(Self::Deg270),
            other => Err(AppearanceError::InvalidRotation(other)),
        }
    }
}

/// A rectangle in page points, used both for widget placement and appearance
/// bounding boxes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WidgetRect {
    /// X coordinate of the lower-left corner, in page points.
    pub x: f32,
    /// Y coordinate of the lower-left corner, in page points.
    pub y: f32,
    /// Extent along the x axis, in points.
    pub width: f32,
    /// Extent along the y axis, in points.
    pub height: f32,
}

impl WidgetRect {
    /// Creates a rectangle from its lower-left corner and extents.
    pub const fn new(x: f32, y: f32, width: f32, height: f32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    /// X coordinate of the right edge (`x + width`).
    pub fn right(self) -> f32 {
        self.x + self.width
    }

    /// Y coordinate of the top edge (`y + height`).
    pub fn top(self) -> f32 {
        self.y + self.height
    }

    /// Returns `true` when either extent is zero or negative.
    pub fn is_zero_sized(self) -> bool {
        self.width <= 0.0 || self.height <= 0.0
    }

    /// Returns `true` when all four coordinates are finite.
    pub fn is_finite(self) -> bool {
        self.x.is_finite()
            && self.y.is_finite()
            && self.width.is_finite()
            && self.height.is_finite()
    }
}

/// The font used to render a widget's text.
#[derive(Debug, Clone, PartialEq)]
pub struct FontSpec {
    /// Base font resource name, for example `"Helvetica"`.
    pub name: String,
    /// Font size in points.
    pub size: f32,
    /// Text colour.
    pub color: RgbColor,
}

impl FontSpec {
    /// Creates a font specification.
    pub fn new(name: impl Into<String>, size: f32, color: RgbColor) -> Self {
        Self {
            name: name.into(),
            size,
            color,
        }
    }
}

impl Default for FontSpec {
    fn default() -> Self {
        Self::new("Helvetica", 12.0, RgbColor::BLACK)
    }
}

/// The complete visual description of one widget.
///
/// Every field is public so callers can construct a descriptor directly. Use
/// [`WidgetAppearance::new`] for the sensible defaults (Helvetica 12pt black on
/// white, a 1pt solid black border, left aligned, unrotated), then override the
/// desired properties. [`WidgetAppearance::validate`] is called automatically
/// by [`build_appearance`].
#[derive(Debug, Clone, PartialEq)]
pub struct WidgetAppearance {
    /// Widget rectangle in page points.
    pub rect: WidgetRect,
    /// Border line style.
    pub border_style: BorderStyle,
    /// Border width in points.
    pub border_width: f32,
    /// Border colour.
    pub border_color: RgbColor,
    /// Widget background colour (drawn as a filled rectangle).
    pub background: RgbColor,
    /// Font used for text and choice labels.
    pub font: FontSpec,
    /// Horizontal text alignment.
    pub alignment: Alignment,
    /// Widget rotation.
    pub rotation: Rotation,
}

impl WidgetAppearance {
    /// Creates a descriptor for `rect` with all default styling.
    pub fn new(rect: WidgetRect) -> Self {
        Self {
            rect,
            border_style: BorderStyle::Solid,
            border_width: 1.0,
            border_color: RgbColor::BLACK,
            background: RgbColor::WHITE,
            font: FontSpec::default(),
            alignment: Alignment::Left,
            rotation: Rotation::Deg0,
        }
    }

    /// Checks that the descriptor can be used to generate an appearance.
    ///
    /// # Errors
    ///
    /// Returns an [`AppearanceError`] for a zero-sized or non-finite rectangle,
    /// a non-finite or negative border width, a non-positive font size, a colour
    /// outside `0.0..=1.0`, or an invalid rotation.
    pub fn validate(&self) -> Result<(), AppearanceError> {
        if !self.rect.is_finite() {
            return Err(AppearanceError::NonFiniteGeometry("rect"));
        }
        if self.rect.is_zero_sized() {
            return Err(AppearanceError::ZeroSizedWidget {
                width: self.rect.width,
                height: self.rect.height,
            });
        }
        if !self.border_width.is_finite() || self.border_width < 0.0 {
            return Err(AppearanceError::NonFiniteGeometry("border_width"));
        }
        if !self.font.size.is_finite() || self.font.size <= 0.0 {
            return Err(AppearanceError::InvalidFontSize(self.font.size));
        }
        if !self.border_color.is_valid() {
            return Err(AppearanceError::InvalidColor("border"));
        }
        if !self.background.is_valid() {
            return Err(AppearanceError::InvalidColor("background"));
        }
        if !self.font.color.is_valid() {
            return Err(AppearanceError::InvalidColor("font"));
        }
        Ok(())
    }
}

/// A single appearance stream in widget-local coordinates.
#[derive(Debug, Clone, PartialEq)]
pub struct AppearanceStream {
    /// Stream bounding box (`/BBox`) in widget-local points.
    pub bbox: WidgetRect,
    /// Raw content-stream bytes.
    pub content: Vec<u8>,
}

impl AppearanceStream {
    /// Creates an appearance stream from a bounding box and content bytes.
    pub fn new(bbox: WidgetRect, content: Vec<u8>) -> Self {
        Self { bbox, content }
    }
}

/// The `/N` normal appearance: either one stream or a map of named states.
#[derive(Debug, Clone, PartialEq)]
pub enum NormalAppearance {
    /// A single stream (text, choice and pushbutton widgets).
    Stream(AppearanceStream),
    /// Named states ordered by name (checkbox and radio widgets).
    States(BTreeMap<String, AppearanceStream>),
}

/// The generated `/AP` dictionary model for one widget.
#[derive(Debug, Clone, PartialEq)]
pub struct AppearanceDictionary {
    /// The normal appearance (`/N`).
    pub normal: NormalAppearance,
    /// The current appearance state (`/AS`), present only for stateful widgets.
    pub active_state: Option<String>,
}

impl AppearanceDictionary {
    /// Returns `true` when `/N` is a dictionary of named states.
    pub fn is_stateful(&self) -> bool {
        matches!(self.normal, NormalAppearance::States(_))
    }

    /// Returns the single normal stream when `/N` is a stream.
    pub fn normal_stream(&self) -> Option<&AppearanceStream> {
        match &self.normal {
            NormalAppearance::Stream(stream) => Some(stream),
            NormalAppearance::States(_) => None,
        }
    }

    /// Returns the named states when `/N` is a state dictionary.
    pub fn states(&self) -> Option<&BTreeMap<String, AppearanceStream>> {
        match &self.normal {
            NormalAppearance::Stream(_) => None,
            NormalAppearance::States(states) => Some(states),
        }
    }

    /// Returns the active state name (`/AS`), if any.
    pub fn active_state(&self) -> Option<&str> {
        self.active_state.as_deref()
    }
}

/// An error produced while generating a widget appearance.
#[derive(Debug, Clone, PartialEq)]
pub enum AppearanceError {
    /// The widget has a zero or negative width or height.
    ZeroSizedWidget {
        /// The supplied width.
        width: f32,
        /// The supplied height.
        height: f32,
    },
    /// A geometric quantity was not finite.
    NonFiniteGeometry(&'static str),
    /// The font size was non-finite or not positive.
    InvalidFontSize(f32),
    /// A rotation was not a right angle.
    InvalidRotation(i16),
    /// A colour component was out of range or non-finite.
    InvalidColor(&'static str),
}

impl fmt::Display for AppearanceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ZeroSizedWidget { width, height } => {
                write!(f, "widget has zero size ({width} x {height})")
            }
            Self::NonFiniteGeometry(what) => write!(f, "non-finite {what}"),
            Self::InvalidFontSize(size) => write!(f, "invalid font size {size}"),
            Self::InvalidRotation(degrees) => write!(f, "invalid rotation {degrees} degrees"),
            Self::InvalidColor(what) => write!(f, "invalid {what} colour"),
        }
    }
}

impl std::error::Error for AppearanceError {}

/// One laid-out text line, positioned in widget-local points.
#[derive(Debug, Clone, PartialEq)]
pub struct TextLine {
    /// The (possibly clipped and ellipsised) line text.
    pub text: String,
    /// X offset of the line origin from the local left edge.
    pub offset_x: f32,
    /// Text baseline Y in widget-local points.
    pub baseline_y: f32,
    /// Measured width of `text`, in points.
    pub width: f32,
}

/// The result of laying text out into a widget box.
#[derive(Debug, Clone, PartialEq)]
pub struct TextLayout {
    /// The rendered lines, top to bottom.
    pub lines: Vec<TextLine>,
    /// `true` when any line was clipped or when logical lines were dropped.
    pub clipped: bool,
}

/// Returns the advance width of one character under the documented heuristic.
fn char_width_factor(ch: char) -> f32 {
    if !ch.is_ascii() {
        return WIDE_WIDTH;
    }
    match ch {
        'i' | 'l' | 'j' | '.' | ',' | ':' | ';' | '\'' | '!' | '|' | '(' | ')' | '[' | ']'
        | ' ' | '`' => NARROW_WIDTH,
        'm' | 'w' | 'W' | 'M' | '@' => WIDE_WIDTH,
        _ => AVERAGE_WIDTH,
    }
}

/// Measures `text` with the average-width heuristic.
fn measure(text: &str, size: f32) -> f32 {
    text.chars().map(|ch| char_width_factor(ch) * size).sum()
}

/// Measures `text` at `size` points with the documented average-width
/// heuristic.
///
/// This is the same approximation [`layout_text`] uses: each character advances
/// by `size` times a per-character factor ([`AVERAGE_WIDTH`],
/// [`NARROW_WIDTH`] or [`WIDE_WIDTH`]). It is deterministic and platform
/// independent; it is not the exact advance from an embedded font program.
pub fn measure_text(text: &str, size: f32) -> f32 {
    measure(text, size)
}

/// Formats a number compactly and deterministically for a content stream.
fn fmt_num(value: f32) -> String {
    if !value.is_finite() {
        return "0".to_owned();
    }
    let rounded = (value * 100.0).round() / 100.0;
    if rounded == 0.0 {
        return "0".to_owned();
    }
    let text = format!("{rounded:.2}");
    let text = text.trim_end_matches('0');
    let text = text.trim_end_matches('.');
    if text.is_empty() { "0" } else { text }.to_owned()
}

/// Formats a number exactly as appearance content streams do.
///
/// Flattening uses this to emit coordinate transforms that are byte-for-byte
/// consistent with the numeric literals produced by [`build_appearance`].
/// Non-finite values become `0`, values are rounded to two decimals, and
/// trailing zeroes are removed.
pub fn format_number(value: f32) -> String {
    fmt_num(value)
}

/// Text encoded for one PDF string operand.
///
/// The variant selects the operand syntax: a literal string `( ... )` or a
/// hexadecimal string `< ... >`. Hexadecimal strings are required for
/// composite (Type0) fonts, whose character codes are two or more bytes wide.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EncodedText {
    /// A literal string; the bytes are escaped by
    /// [`escape_literal_bytes`] when rendered.
    Literal(Vec<u8>),
    /// A hexadecimal string; the bytes are written as uppercase hex digits.
    Hex(Vec<u8>),
}

impl EncodedText {
    /// Renders the operand exactly as it appears in a content stream.
    pub fn render(&self) -> String {
        match self {
            Self::Literal(bytes) => format!("({})", escape_literal_bytes(bytes)),
            Self::Hex(bytes) => {
                let mut out = String::with_capacity(bytes.len() * 2 + 2);
                out.push('<');
                for byte in bytes {
                    out.push_str(&format!("{byte:02X}"));
                }
                out.push('>');
                out
            }
        }
    }
}

/// Encodes Unicode text into the character codes of one font.
///
/// [`build_appearance_with_encoder`] calls this for every displayed line, so a
/// caller can substitute a font-specific encoder (for example the `/DA` font of
/// an AcroForm field) without teaching the layout code about fonts. `None` means
/// the font cannot represent the text; the renderer then tries an ASCII
/// ellipsis and skips the line if that also fails.
pub trait TextEncoder {
    /// Encodes `text`, or returns `None` when a character has no code.
    fn encode(&self, text: &str) -> Option<EncodedText>;
}

/// The default encoder: ASCII stays a literal string, anything else becomes a
/// UTF-16BE literal with a `FE FF` byte-order mark.
///
/// This is the behaviour [`build_appearance`] has always used and the correct
/// fallback for a base-14 font read with the viewer's own encoding.
#[derive(Debug, Clone, Copy, Default)]
pub struct DefaultTextEncoder;

impl TextEncoder for DefaultTextEncoder {
    fn encode(&self, text: &str) -> Option<EncodedText> {
        let bytes = if text.is_ascii() {
            text.as_bytes().to_vec()
        } else {
            let mut bytes = vec![0xFE, 0xFF];
            for unit in text.encode_utf16() {
                bytes.extend_from_slice(&unit.to_be_bytes());
            }
            bytes
        };
        Some(EncodedText::Literal(bytes))
    }
}

/// Escapes one byte as a three-digit octal PDF literal escape.
fn push_octal(out: &mut String, byte: u8) {
    out.push('\\');
    out.push(char::from(b'0' + (byte >> 6)));
    out.push(char::from(b'0' + ((byte >> 3) & 0x07)));
    out.push(char::from(b'0' + (byte & 0x07)));
}

/// Escapes `bytes` for use inside a PDF literal string `( ... )`.
///
/// Backslash, `(` and `)` are escaped with a backslash; newlines, carriage
/// returns and tabs use short escapes; other bytes outside printable ASCII use
/// three-digit octal escapes. Unlike [`escape_literal_string`], no character
/// encoding is inferred: the caller has already encoded the text to the target
/// font's codes.
pub fn escape_literal_bytes(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() + 8);
    for &byte in bytes {
        match byte {
            b'\\' => out.push_str("\\\\"),
            b'(' => out.push_str("\\("),
            b')' => out.push_str("\\)"),
            b'\n' => out.push_str("\\n"),
            b'\r' => out.push_str("\\r"),
            b'\t' => out.push_str("\\t"),
            0x20..=0x7E => out.push(char::from(byte)),
            _ => push_octal(&mut out, byte),
        }
    }
    out
}

/// Escapes `text` for use inside a PDF literal string `( ... )`.
///
/// Backslash, `(` and `)` are escaped with a backslash; newlines, carriage
/// returns and tabs use short escapes; other control bytes use three-digit
/// octal escapes. When the text contains any non-ASCII scalar value the whole
/// string is encoded as UTF-16BE with a `FE FF` byte-order mark, and every byte
/// is emitted as an octal escape, which is the PDF-conformant way to carry such
/// text in a literal string.
pub fn escape_literal_string(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 8);
    if !text.is_ascii() {
        out.push_str("\\376\\377");
        for unit in text.encode_utf16() {
            push_octal(&mut out, (unit >> 8) as u8);
            push_octal(&mut out, (unit & 0xFF) as u8);
        }
    } else {
        for byte in text.bytes() {
            match byte {
                b'\\' => out.push_str("\\\\"),
                b'(' => out.push_str("\\("),
                b')' => out.push_str("\\)"),
                b'\n' => out.push_str("\\n"),
                b'\r' => out.push_str("\\r"),
                b'\t' => out.push_str("\\t"),
                0x20..=0x7E => out.push(char::from(byte)),
                _ => push_octal(&mut out, byte),
            }
        }
    }
    out
}

/// Clips `text` to `max_width` points, appending an ellipsis when it fits.
///
/// The rule is: if the text already fits it is returned unchanged; otherwise
/// whole characters that fit inside `max_width - ellipsis width` are kept and
/// `…` is appended. When the ellipsis itself does not fit, the clipped prefix
/// is returned without it. A non-positive width yields an empty string.
fn fit_with_ellipsis(text: &str, max_width: f32, size: f32) -> (String, bool) {
    if measure(text, size) <= max_width {
        return (text.to_owned(), false);
    }
    if max_width <= 0.0 {
        return (String::new(), !text.is_empty());
    }
    let ellipsis = "…";
    let ellipsis_width = measure(ellipsis, size);
    let budget = if ellipsis_width <= max_width {
        max_width - ellipsis_width
    } else {
        0.0
    };
    let mut out = String::new();
    let mut used = 0.0_f32;
    for ch in text.chars() {
        let width = char_width_factor(ch) * size;
        if used + width > budget {
            break;
        }
        out.push(ch);
        used += width;
    }
    if ellipsis_width <= max_width {
        out.push_str(ellipsis);
    }
    (out, true)
}

/// Splits text into logical lines on either newline character.
fn split_lines(text: &str) -> impl Iterator<Item = &str> {
    text.split(['\n', '\r']).filter(|part| !part.is_empty())
}

/// Horizontal space available for text inside the border and padding.
fn content_width(widget: &WidgetAppearance) -> f32 {
    (widget.rect.width - 2.0 * widget.border_width - 2.0 * HORIZONTAL_PADDING).max(0.0)
}

/// Greedily wraps one paragraph into `out`, breaking overlong words.
fn wrap_paragraph(paragraph: &str, max_width: f32, size: f32, out: &mut Vec<String>) {
    let mut current = String::new();
    for word in paragraph.split_whitespace() {
        let candidate = if current.is_empty() {
            word.to_owned()
        } else {
            format!("{current} {word}")
        };
        if measure(&candidate, size) <= max_width {
            current = candidate;
            continue;
        }
        if !current.is_empty() {
            out.push(std::mem::take(&mut current));
        }
        if measure(word, size) <= max_width {
            current = word.to_owned();
        } else {
            // An overlong single word is clipped to one line; its remainder is
            // deliberately dropped so the layout stays bounded.
            let (piece, _) = fit_with_ellipsis(word, max_width, size);
            out.push(piece);
        }
    }
    if !current.is_empty() {
        out.push(current);
    }
}

/// Lays `text` out into `widget`, applying wrapping, clipping and alignment.
///
/// The widget should be valid (see [`WidgetAppearance::validate`]); this pure
/// function does not re-validate, but it clamps all widths at zero so invalid
/// input cannot produce NaN or panic.
pub fn layout_text(text: &str, widget: &WidgetAppearance, multiline: bool) -> TextLayout {
    let max_width = content_width(widget);
    let line_height = widget.font.size * LINE_HEIGHT_FACTOR;
    let top = widget.rect.height - widget.border_width - VERTICAL_PADDING;
    let bottom = widget.border_width + VERTICAL_PADDING;

    let mut logical: Vec<String> = Vec::new();
    if multiline {
        for paragraph in split_lines(text) {
            wrap_paragraph(paragraph, max_width, widget.font.size, &mut logical);
        }
    } else {
        let flattened = split_lines(text).collect::<Vec<_>>().join(" ");
        logical.push(flattened);
    }

    let inner_height = (top - bottom).max(0.0);
    let usable = inner_height - widget.font.size;
    let max_lines = if multiline && line_height > 0.0 {
        if usable > 0.0 {
            (usable / line_height) as usize + 1
        } else {
            1
        }
    } else {
        1
    };

    let mut clipped = logical.len() > max_lines;
    let padding = widget.border_width + HORIZONTAL_PADDING;
    let mut lines = Vec::new();
    for (index, raw) in logical.into_iter().take(max_lines).enumerate() {
        let (display, was_clipped) = fit_with_ellipsis(&raw, max_width, widget.font.size);
        clipped |= was_clipped;
        let width = measure(&display, widget.font.size);
        let free = (max_width - width).max(0.0);
        let offset_x = padding
            + match widget.alignment {
                Alignment::Left => 0.0,
                Alignment::Center => free * 0.5,
                Alignment::Right => free,
            };
        let baseline_y = top - widget.font.size - index as f32 * line_height;
        lines.push(TextLine {
            text: display,
            offset_x,
            baseline_y,
            width,
        });
    }

    TextLayout { lines, clipped }
}

/// Renders a [`TextLayout`] into a content stream using `BT/ET/Tf/Td/Tj`.
///
/// Each line is positioned with a `Td` relative to the previous line's origin,
/// which is correct because `Td` moves the text-line matrix rather than the
/// text matrix advanced by `Tj`. `encoder` supplies the font's character codes;
/// a line the encoder rejects is replaced by an ASCII ellipsis, and a line that
/// still cannot be encoded is skipped rather than written as invalid codes.
fn render_text_content(
    layout: &TextLayout,
    widget: &WidgetAppearance,
    encoder: &dyn TextEncoder,
) -> Vec<u8> {
    let mut out = String::new();
    out.push_str("BT\n");
    out.push_str(&format!("/F1 {} Tf\n", fmt_num(widget.font.size)));
    let mut prev_x = 0.0_f32;
    let mut prev_y = 0.0_f32;
    for line in &layout.lines {
        let Some(operand) = encode_line(encoder, &line.text) else {
            continue;
        };
        out.push_str(&format!(
            "{} {} Td\n",
            fmt_num(line.offset_x - prev_x),
            fmt_num(line.baseline_y - prev_y)
        ));
        out.push_str(&operand);
        out.push_str(" Tj\n");
        prev_x = line.offset_x;
        prev_y = line.baseline_y;
    }
    out.push_str("ET\n");
    out.into_bytes()
}

/// Returns the rendered string operand for `text`, degrading to `...`.
fn encode_line(encoder: &dyn TextEncoder, text: &str) -> Option<String> {
    if let Some(encoded) = encoder.encode(text) {
        return Some(encoded.render());
    }
    encoder.encode("...").map(|encoded| encoded.render())
}

/// Returns the named appearance states for a button field, sorted by name.
///
/// A checkbox yields `Off` and `On`; a radio group yields `Off` plus every
/// choice name; other field types yield an empty vector.
pub fn button_state_names(field: &Field) -> Vec<String> {
    match field.field_type() {
        FieldType::CheckBox => vec![STATE_OFF.to_owned(), STATE_ON.to_owned()],
        FieldType::Radio => {
            let mut names = vec![STATE_OFF.to_owned()];
            names.extend(field.choices().iter().cloned());
            names.sort();
            names.dedup();
            names
        }
        _ => Vec::new(),
    }
}

/// Emits a background fill and border rectangle for `widget`.
fn box_outline_content(widget: &WidgetAppearance) -> Vec<u8> {
    let mut out = String::new();
    let x = widget.border_width * 0.5;
    let y = widget.border_width * 0.5;
    let width = (widget.rect.width - widget.border_width).max(0.0);
    let height = (widget.rect.height - widget.border_width).max(0.0);
    let [br, bg, bb] = widget.background.components();
    let [or_, og, ob] = widget.border_color.components();
    out.push_str("q\n");
    out.push_str(&format!(
        "{} {} {} rg\n",
        fmt_num(br),
        fmt_num(bg),
        fmt_num(bb)
    ));
    out.push_str(&format!(
        "{} {} {} {} re f\n",
        fmt_num(x),
        fmt_num(y),
        fmt_num(width),
        fmt_num(height)
    ));
    out.push_str(&format!(
        "{} {} {} RG\n",
        fmt_num(or_),
        fmt_num(og),
        fmt_num(ob)
    ));
    out.push_str(&format!("{} w\n", fmt_num(widget.border_width)));
    out.push_str(&format!(
        "{} {} {} {} re S\n",
        fmt_num(x),
        fmt_num(y),
        fmt_num(width),
        fmt_num(height)
    ));
    out.push_str("Q\n");
    out.into_bytes()
}

/// Emits the on-state checkbox content: border plus a check-mark polyline.
///
/// The mark is a two-segment polyline scaled to the widget box, drawn with the
/// `m`, `l` and `S` path operators. This is a simple, clean-room approximation
/// of the conventional check mark.
pub fn check_mark_content(widget: &WidgetAppearance) -> Vec<u8> {
    let mut out = String::from_utf8_lossy(&box_outline_content(widget)).into_owned();
    let inset = widget.border_width + HORIZONTAL_PADDING;
    let width = (widget.rect.width - 2.0 * inset).max(0.0);
    let height = (widget.rect.height - 2.0 * inset).max(0.0);
    let p1 = (inset + width * 0.15, inset + height * 0.50);
    let p2 = (inset + width * 0.40, inset + height * 0.25);
    let p3 = (inset + width * 0.85, inset + height * 0.80);
    let [cr, cg, cb] = widget.border_color.components();
    out.push_str(&format!(
        "q\n{} w\n{} {} {} RG\n",
        fmt_num(widget.border_width.max(1.0)),
        fmt_num(cr),
        fmt_num(cg),
        fmt_num(cb)
    ));
    out.push_str(&format!("{} {} m\n", fmt_num(p1.0), fmt_num(p1.1)));
    out.push_str(&format!("{} {} l\n", fmt_num(p2.0), fmt_num(p2.1)));
    out.push_str(&format!("{} {} l\n S\nQ\n", fmt_num(p3.0), fmt_num(p3.1)));
    out.into_bytes()
}

/// Emits a circle outline, filled when `on` is `true`, for a radio state.
fn radio_mark_content(widget: &WidgetAppearance, on: bool) -> Vec<u8> {
    let mut out = String::from_utf8_lossy(&box_outline_content(widget)).into_owned();
    let cx = widget.rect.width * 0.5;
    let cy = widget.rect.height * 0.5;
    let radius = (widget.rect.width.min(widget.rect.height) * 0.3).max(0.0);
    let k = 0.552_284_8_f32 * radius;
    let [cr, cg, cb] = widget.border_color.components();
    out.push_str(&format!(
        "q\n{} {} {} RG\n{} {} {} rg\n",
        fmt_num(cr),
        fmt_num(cg),
        fmt_num(cb),
        fmt_num(cr),
        fmt_num(cg),
        fmt_num(cb)
    ));
    out.push_str(&format!("{} {} m\n", fmt_num(cx + radius), fmt_num(cy)));
    out.push_str(&format!(
        "{} {} {} {} {} {} c\n",
        fmt_num(cx + radius),
        fmt_num(cy + k),
        fmt_num(cx + k),
        fmt_num(cy + radius),
        fmt_num(cx),
        fmt_num(cy + radius)
    ));
    out.push_str(&format!(
        "{} {} {} {} {} {} c\n",
        fmt_num(cx - k),
        fmt_num(cy + radius),
        fmt_num(cx - radius),
        fmt_num(cy + k),
        fmt_num(cx - radius),
        fmt_num(cy)
    ));
    out.push_str(&format!(
        "{} {} {} {} {} {} c\n",
        fmt_num(cx - radius),
        fmt_num(cy - k),
        fmt_num(cx - k),
        fmt_num(cy - radius),
        fmt_num(cx),
        fmt_num(cy - radius)
    ));
    out.push_str(&format!(
        "{} {} {} {} {} {} c\n",
        fmt_num(cx + k),
        fmt_num(cy - radius),
        fmt_num(cx + radius),
        fmt_num(cy - k),
        fmt_num(cx + radius),
        fmt_num(cy)
    ));
    if on {
        out.push_str("f\n");
    } else {
        out.push_str("S\n");
    }
    out.push_str("Q\n");
    out.into_bytes()
}

/// Builds the named `/N` states for a checkbox or radio field.
fn button_states(field: &Field, widget: &WidgetAppearance) -> BTreeMap<String, AppearanceStream> {
    let bbox = WidgetRect::new(0.0, 0.0, widget.rect.width, widget.rect.height);
    let mut states = BTreeMap::new();
    match field.field_type() {
        FieldType::CheckBox => {
            states.insert(
                STATE_OFF.to_owned(),
                AppearanceStream::new(bbox, box_outline_content(widget)),
            );
            states.insert(
                STATE_ON.to_owned(),
                AppearanceStream::new(bbox, check_mark_content(widget)),
            );
        }
        FieldType::Radio => {
            states.insert(
                STATE_OFF.to_owned(),
                AppearanceStream::new(bbox, radio_mark_content(widget, false)),
            );
            for choice in field.choices() {
                states.insert(
                    choice.clone(),
                    AppearanceStream::new(bbox, radio_mark_content(widget, true)),
                );
            }
        }
        _ => {}
    }
    states
}

/// Determines the active `/AS` state name for a button field and value.
fn button_active_state(field: &Field, value: &FieldValue) -> Option<String> {
    match field.field_type() {
        FieldType::CheckBox => Some(
            if value.as_bool().unwrap_or(false) {
                STATE_ON
            } else {
                STATE_OFF
            }
            .to_owned(),
        ),
        FieldType::Radio => Some(
            value
                .as_choices()
                .and_then(|selected| selected.first())
                .cloned()
                .unwrap_or_else(|| STATE_OFF.to_owned()),
        ),
        _ => None,
    }
}

/// Renders a list-box choice field with highlighted selected rows.
fn render_choice_list(
    widget: &WidgetAppearance,
    options: &[String],
    selected: &[String],
    encoder: &dyn TextEncoder,
) -> Vec<u8> {
    let size = widget.font.size;
    let line_height = size * LINE_HEIGHT_FACTOR;
    let top = widget.rect.height - widget.border_width - VERTICAL_PADDING;
    let inner_height =
        (widget.rect.height - 2.0 * (widget.border_width + VERTICAL_PADDING)).max(0.0);
    let visible = if line_height > 0.0 {
        (inner_height / line_height) as usize
    } else {
        0
    };
    let max_width = content_width(widget);
    let offset_x = widget.border_width + HORIZONTAL_PADDING;
    let [hr, hg, hb] = RgbColor::HIGHLIGHT.components();
    let [tr, tg, tb] = widget.font.color.components();

    let mut out = String::new();
    out.push_str("q\n");
    for (index, option) in options.iter().take(visible).enumerate() {
        let baseline = top - size - index as f32 * line_height;
        if selected.iter().any(|value| value == option) {
            out.push_str(&format!(
                "{} {} {} rg\n",
                fmt_num(hr),
                fmt_num(hg),
                fmt_num(hb)
            ));
            out.push_str(&format!(
                "{} {} {} {} re f\n",
                fmt_num(widget.border_width),
                fmt_num(baseline - size * 0.2),
                fmt_num(max_width),
                fmt_num(line_height * 0.9)
            ));
        }
        let (display, _) = fit_with_ellipsis(option, max_width, size);
        let Some(operand) = encode_line(encoder, &display) else {
            continue;
        };
        out.push_str("BT\n");
        out.push_str(&format!("/F1 {} Tf\n", fmt_num(size)));
        out.push_str(&format!(
            "{} {} {} rg\n",
            fmt_num(tr),
            fmt_num(tg),
            fmt_num(tb)
        ));
        out.push_str(&format!("{} {} Td\n", fmt_num(offset_x), fmt_num(baseline)));
        out.push_str(&operand);
        out.push_str(" Tj\n");
        out.push_str("ET\n");
    }
    out.push_str("Q\n");
    out.into_bytes()
}

/// Converts a field value into the text shown by a text-like widget.
fn value_to_text(value: &FieldValue) -> String {
    match value {
        FieldValue::Text(text) => text.clone(),
        FieldValue::Number(number) => fmt_num(*number as f32),
        FieldValue::Bool(flag) => if *flag { STATE_ON } else { STATE_OFF }.to_owned(),
        FieldValue::Choice(values) => values.first().cloned().unwrap_or_default(),
        FieldValue::Empty => String::new(),
    }
}

/// Returns `true` when a text field lays its value out on multiple lines.
fn is_multiline(field: &Field) -> bool {
    field.field_type() == FieldType::Text
        && field.flags().contains(FieldFlags::MULTILINE)
        && !field.flags().contains(FieldFlags::PASSWORD)
}

/// Returns the text laid out by a widget, in widget-local points.
///
/// The extent is the union of the line boxes produced by the same layout that
/// [`build_appearance`] uses, with a nominal glyph height of `0.8 × font size`
/// above and `0.2 × font size` below each baseline. It is clamped to the widget
/// rectangle. `None` means the widget draws no text (buttons, list boxes, or an
/// empty value).
///
/// This is what a caller needs to prove a generated appearance still lands
/// inside a transformed widget rectangle.
pub fn text_extent(
    field: &Field,
    widget: &WidgetAppearance,
    value: &FieldValue,
) -> Option<WidgetRect> {
    let layout = match field.field_type() {
        FieldType::Text | FieldType::Signature | FieldType::Unknown => {
            layout_text(&value_to_text(value), widget, is_multiline(field))
        }
        FieldType::Choice if field.flags().contains(FieldFlags::COMBO) => {
            let selected: &[String] = value.as_choices().unwrap_or(&[]);
            let label = selected.first().map(String::as_str).unwrap_or("");
            layout_text(label, widget, false)
        }
        FieldType::CheckBox | FieldType::Radio | FieldType::Choice | FieldType::Button => {
            return None;
        }
    };
    let size = widget.font.size;
    let mut bounds: Option<(f32, f32, f32, f32)> = None;
    for line in &layout.lines {
        if line.text.is_empty() {
            continue;
        }
        let left = line.offset_x;
        let right = line.offset_x + line.width;
        let bottom = line.baseline_y - size * 0.2;
        let top = line.baseline_y + size * 0.8;
        bounds = Some(match bounds {
            None => (left, bottom, right, top),
            Some((l, b, r, t)) => (l.min(left), b.min(bottom), r.max(right), t.max(top)),
        });
    }
    let (left, bottom, right, top) = bounds?;
    let clamp = |value: f32, max: f32| value.clamp(0.0, max.max(0.0));
    Some(WidgetRect::new(
        clamp(left, widget.rect.width),
        clamp(bottom, widget.rect.height),
        (clamp(right, widget.rect.width) - clamp(left, widget.rect.width)).max(0.0),
        (clamp(top, widget.rect.height) - clamp(bottom, widget.rect.height)).max(0.0),
    ))
}

/// Builds the appearance dictionary for one widget.
///
/// `field` supplies the type, flags and choice options, `widget` supplies the
/// visual styling and rectangle, and `value` is the *current* value to render
/// (which may differ from `field.value()`, for example when previewing an edit).
///
/// The result is deterministic: repeated calls with equal inputs produce equal
/// [`AppearanceStream`] bytes and equal state maps. Text fields and choice
/// fields produce a single normal stream; checkboxes and radio groups produce
/// named states plus an active `/AS` name.
///
/// # Errors
///
/// Returns an [`AppearanceError`] when `widget` fails
/// [`WidgetAppearance::validate`].
pub fn build_appearance(
    field: &Field,
    widget: &WidgetAppearance,
    value: &FieldValue,
) -> Result<AppearanceDictionary, AppearanceError> {
    build_appearance_with_encoder(field, widget, value, &DefaultTextEncoder)
}

/// Builds the appearance dictionary using a font-specific text encoder.
///
/// This is [`build_appearance`] with the string operands produced by `encoder`
/// instead of the default ASCII/UTF-16 literal. The layout, geometry, colours
/// and button drawings are identical; only the `Tj` operand bytes differ. Use
/// it when the appearance must reference the font named by a field's `/DA`
/// rather than a base-14 substitute.
///
/// # Errors
///
/// Returns an [`AppearanceError`] when `widget` fails
/// [`WidgetAppearance::validate`].
pub fn build_appearance_with_encoder(
    field: &Field,
    widget: &WidgetAppearance,
    value: &FieldValue,
    encoder: &dyn TextEncoder,
) -> Result<AppearanceDictionary, AppearanceError> {
    widget.validate()?;
    let bbox = WidgetRect::new(0.0, 0.0, widget.rect.width, widget.rect.height);

    match field.field_type() {
        FieldType::CheckBox | FieldType::Radio => {
            let states = button_states(field, widget);
            let active_state = button_active_state(field, value);
            Ok(AppearanceDictionary {
                normal: NormalAppearance::States(states),
                active_state,
            })
        }
        FieldType::Choice => {
            let combo = field.flags().contains(FieldFlags::COMBO);
            let selected: &[String] = value.as_choices().unwrap_or(&[]);
            let content = if combo {
                let label = selected.first().map(String::as_str).unwrap_or("");
                let layout = layout_text(label, widget, false);
                render_text_content(&layout, widget, encoder)
            } else {
                render_choice_list(widget, field.choices(), selected, encoder)
            };
            Ok(AppearanceDictionary {
                normal: NormalAppearance::Stream(AppearanceStream::new(bbox, content)),
                active_state: None,
            })
        }
        FieldType::Text | FieldType::Signature | FieldType::Unknown => {
            let text = value_to_text(value);
            let layout = layout_text(&text, widget, is_multiline(field));
            let content = render_text_content(&layout, widget, encoder);
            Ok(AppearanceDictionary {
                normal: NormalAppearance::Stream(AppearanceStream::new(bbox, content)),
                active_state: None,
            })
        }
        FieldType::Button => Ok(AppearanceDictionary {
            normal: NormalAppearance::Stream(AppearanceStream::new(
                bbox,
                box_outline_content(widget),
            )),
            active_state: None,
        }),
    }
}

#[cfg(test)]
mod tests;
