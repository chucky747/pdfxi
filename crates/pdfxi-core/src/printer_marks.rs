//! Deterministic printer-mark content streams.
//!
//! This module builds a ready-to-use PDF content stream that draws crop marks,
//! registration marks, a CMYK colour bar and page information around a page's
//! TrimBox. It performs no I/O and depends only on the typed page boxes in
//! [`crate::boxes`].
//!
//! # Units and origin
//!
//! Every coordinate is in **default user space units** (1/72 inch, one point)
//! and uses the PDF convention: the origin is the lower-left corner of the
//! page, +x points right and +y points up. Coordinates are *not* adjusted for
//! the page's `/Rotate`; callers that place the stream on a rotated page must
//! apply the corresponding transform. Distances such as [`PrinterMarkOptions::mark_gap`]
//! and [`PrinterMarkOptions::mark_length`] are therefore in points as well.
//!
//! # Placement
//!
//! Crop marks are drawn just outside the four corners of the TrimBox; with the
//! default option values they sit in the gap between the TrimBox and the
//! BleedBox. Registration targets are centred on the four corners of the
//! MediaBox and intentionally straddle the sheet edge, as is conventional for
//! plate-alignment marks; they are excluded from the sheet-fit check below.
//! The colour bar and page-information text are placed below the TrimBox.
//! [`PrinterMarks::overlay_box`] reports the smallest rectangle that encloses
//! all generated marks, which can be used to size an overlay page or form
//! XObject.
//!
//! # Required trim margin
//!
//! The TrimBox must inset far enough from the MediaBox for the marks to stay
//! on the sheet:
//!
//! - every edge needs at least `mark_gap + mark_length` points for crop marks
//!   (21 pt with the defaults);
//! - the bottom edge needs at least `6 + color_bar_height` points for the
//!   colour bar (18 pt with the defaults);
//! - the bottom edge needs at least `6 + color_bar_height + 10` points when
//!   page information is emitted (28 pt with the defaults).
//!
//! [`generate_printer_marks`] enforces these limits and returns
//! [`crate::Error::PrinterMarksOutOfSheet`] with the required and available
//! margins when a layout cannot fit. The default options therefore require a
//! TrimBox inset of at least 21 pt on the left, right and top and 28 pt on the
//! bottom.
//!
//! # Determinism and precision
//!
//! Output is byte-for-byte deterministic: number formatting is canonical and
//! markup is emitted in a fixed order, so repeated calls with equal inputs
//! produce equal content streams. Numbers are written with at most four
//! decimal places; non-zero values with magnitude below `1.0e-4` are emitted
//! as `0`, and options below that floor are rejected so precision is never
//! silently lost.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use crate::boxes::PageBoxes;
use crate::object::{Dictionary, Object};
use crate::page::Rectangle;
use crate::{Error, Result};

/// Gap between the TrimBox bottom edge and the colour bar, in points.
const COLOR_BAR_GAP: f64 = 6.0;
/// Gap between the colour bar and the page-information baseline, in points.
const PAGE_INFO_GAP: f64 = 10.0;
/// Smallest non-zero magnitude that survives canonical number formatting.
const FMT_PRECISION: f64 = 1.0e-4;

/// A straight line segment in default user space.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MarkSegment {
    /// x-coordinate of the start point.
    pub x1: f64,
    /// y-coordinate of the start point.
    pub y1: f64,
    /// x-coordinate of the end point.
    pub x2: f64,
    /// y-coordinate of the end point.
    pub y2: f64,
}

impl MarkSegment {
    /// Construct a line segment from `(x1, y1)` to `(x2, y2)`.
    pub fn new(x1: f64, y1: f64, x2: f64, y2: f64) -> Self {
        Self { x1, y1, x2, y2 }
    }
}

/// A circle used by a registration mark.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MarkCircle {
    /// x-coordinate of the centre.
    pub cx: f64,
    /// y-coordinate of the centre.
    pub cy: f64,
    /// Radius in default user space units.
    pub radius: f64,
}

impl MarkCircle {
    /// Construct a circle with centre `(cx, cy)` and `radius`.
    pub fn new(cx: f64, cy: f64, radius: f64) -> Self {
        Self { cx, cy, radius }
    }
}

/// Geometry of the marks produced by [`generate_printer_marks`].
///
/// Exposed so callers can inspect or verify placement (for example, that crop
/// marks fall between the TrimBox and the BleedBox) without parsing the content
/// stream back out.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct MarkGeometry {
    /// Line segments forming the four crop marks.
    pub crop_segments: Vec<MarkSegment>,
    /// Crosshair line segments of the registration marks.
    pub registration_segments: Vec<MarkSegment>,
    /// Circles of the registration marks.
    pub registration_circles: Vec<MarkCircle>,
    /// Filled swatch rectangles of the CMYK colour bar.
    pub color_bar_rects: Vec<Rectangle>,
    /// Base point of the page-information text, when emitted.
    pub page_info_origin: Option<(f64, f64)>,
}

/// Options controlling which marks are drawn and how they are sized.
///
/// All distances are in default user space units (points). The defaults
/// produce crop marks of length 18 with a 3-point gap, which fit in a bleed of
/// at least 21 points on every side of the TrimBox. With the defaults the
/// TrimBox must inset at least 21 pt from every MediaBox edge and 28 pt from
/// its bottom edge for all marks to stay on the sheet; see the module docs.
/// Numeric values smaller than `1.0e-4` (other than a zero `mark_gap`) are
/// rejected because canonical output cannot represent them.
#[derive(Debug, Clone, PartialEq)]
pub struct PrinterMarkOptions {
    /// Whether to draw the four crop marks.
    pub crop_marks: bool,
    /// Whether to draw the four registration targets.
    pub registration_marks: bool,
    /// Whether to draw the CMYK colour bar.
    pub color_bars: bool,
    /// Whether to draw the page-information text.
    pub page_info: bool,
    /// Length of each crop-mark arm, in points. Must be positive and finite.
    pub mark_length: f64,
    /// Gap between a TrimBox edge and the start of its crop mark, in points.
    /// Must be non-negative and finite.
    pub mark_gap: f64,
    /// Stroke line width, in points. Must be positive and finite.
    pub line_width: f64,
    /// Radius of a registration target's circle, in points. Must be positive
    /// and finite.
    pub registration_radius: f64,
    /// Height of each colour-bar swatch, in points. Must be positive and
    /// finite.
    pub color_bar_height: f64,
    /// Text shown by the page-information mark.
    pub label: String,
}

impl Default for PrinterMarkOptions {
    fn default() -> Self {
        Self {
            crop_marks: true,
            registration_marks: true,
            color_bars: true,
            page_info: true,
            mark_length: 18.0,
            mark_gap: 3.0,
            line_width: 0.5,
            registration_radius: 6.0,
            color_bar_height: 12.0,
            label: "PDFXI".to_owned(),
        }
    }
}

impl PrinterMarkOptions {
    /// Check that every numeric option is finite, positive enough to be
    /// representable, and within its allowed range.
    ///
    /// Values below `FMT_PRECISION` are rejected: the canonical formatter
    /// writes at most four decimal places and would silently turn such a value
    /// into `0`. A `mark_gap` of exactly zero remains valid.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidPrinterMarkOptions`] naming the offending field.
    pub fn validate(&self) -> Result<()> {
        let positive = [
            ("mark_length", self.mark_length),
            ("line_width", self.line_width),
            ("registration_radius", self.registration_radius),
            ("color_bar_height", self.color_bar_height),
        ];
        for (name, value) in positive {
            if !value.is_finite() || value < FMT_PRECISION {
                return Err(Error::InvalidPrinterMarkOptions(name));
            }
        }
        if !self.mark_gap.is_finite()
            || self.mark_gap < 0.0
            || (self.mark_gap > 0.0 && self.mark_gap < FMT_PRECISION)
        {
            return Err(Error::InvalidPrinterMarkOptions("mark_gap"));
        }
        Ok(())
    }
}

/// The generated printer marks: content stream, overlay bounds and geometry.
#[derive(Debug, Clone)]
pub struct PrinterMarks {
    content: Vec<u8>,
    overlay_box: Rectangle,
    geometry: MarkGeometry,
}

impl PrinterMarks {
    /// Return the PDF content-stream bytes.
    pub fn content_stream(&self) -> &[u8] {
        &self.content
    }

    /// Return the smallest rectangle enclosing all generated marks.
    pub fn overlay_box(&self) -> Rectangle {
        self.overlay_box
    }

    /// Return the inspectable geometry of the marks.
    pub fn geometry(&self) -> &MarkGeometry {
        &self.geometry
    }

    /// Build a self-contained Form XObject carrying this stream.
    ///
    /// The returned stream object has `/Type /XObject`, `/Subtype /Form`, a
    /// `/BBox` equal to [`PrinterMarks::overlay_box`] and a small built-in font
    /// resource for the page-information text. The writer fills in `/Length`.
    pub fn to_form_xobject(&self) -> Object {
        let mut dictionary = Dictionary::new();
        dictionary.insert("Type".to_owned(), Object::Name("XObject".to_owned()));
        dictionary.insert("Subtype".to_owned(), Object::Name("Form".to_owned()));
        dictionary.insert("BBox".to_owned(), rectangle_array(self.overlay_box));

        let mut font = Dictionary::new();
        font.insert("Type".to_owned(), Object::Name("Font".to_owned()));
        font.insert("Subtype".to_owned(), Object::Name("Type1".to_owned()));
        font.insert("BaseFont".to_owned(), Object::Name("Helvetica".to_owned()));
        let mut fonts = BTreeMap::new();
        fonts.insert("F1".to_owned(), Object::Dictionary(font));
        let mut resources = Dictionary::new();
        resources.insert("Font".to_owned(), Object::Dictionary(fonts));
        dictionary.insert("Resources".to_owned(), Object::Dictionary(resources));

        Object::Stream {
            dictionary,
            data: self.content.clone(),
        }
    }
}

/// Generate printer marks for `boxes` using `options`.
///
/// # Errors
///
/// Returns [`Error::BoxNotContained`] or a rectangle error when `boxes` is not
/// a valid, canonical set of page boxes, [`Error::InvalidPrinterMarkOptions`]
/// when an option is out of range or below the precision floor, and
/// [`Error::PrinterMarksOutOfSheet`] when the TrimBox is too close to the
/// MediaBox for the requested marks.
pub fn generate_printer_marks(
    boxes: &PageBoxes,
    options: &PrinterMarkOptions,
) -> Result<PrinterMarks> {
    options.validate()?;
    boxes.validate()?;
    validate_sheet_fit(boxes, options)?;

    let trim = boxes.trim();
    let bleed = boxes.bleed();
    let media = boxes.media();
    let gap = options.mark_gap;
    let length = options.mark_length;
    let mut geometry = MarkGeometry::default();
    let mut body = String::new();

    body.push_str("q\n");
    let _ = writeln!(body, "{} w", fmt_num(options.line_width));
    body.push_str("0 0 0 RG\n");

    if options.crop_marks {
        for segment in crop_mark_segments(trim, gap, length) {
            body.push_str(&segment_operator(&segment));
            geometry.crop_segments.push(segment);
        }
    }

    if options.registration_marks {
        for (segments, circle) in registration_marks(media, options.registration_radius) {
            for segment in segments {
                body.push_str(&segment_operator(&segment));
                geometry.registration_segments.push(segment);
            }
            geometry.registration_circles.push(circle);
        }
        for circle in &geometry.registration_circles {
            body.push_str(&circle_operator(*circle));
        }
    }

    if options.color_bars {
        for (rectangle, color) in color_bar(trim, bleed, options.color_bar_height) {
            let (red, green, blue) = color;
            let _ = writeln!(
                body,
                "{} {} {} rg\n{} {} {} {} re f",
                fmt_num(red),
                fmt_num(green),
                fmt_num(blue),
                fmt_num(rectangle.left),
                fmt_num(rectangle.bottom),
                fmt_num(rectangle.width()),
                fmt_num(rectangle.height())
            );
            geometry.color_bar_rects.push(rectangle);
        }
    }

    if options.page_info {
        let origin = (
            trim.left,
            trim.bottom - COLOR_BAR_GAP - options.color_bar_height - PAGE_INFO_GAP,
        );
        let text = format!(
            "{} {}x{} pt",
            options.label,
            fmt_num(trim.width()),
            fmt_num(trim.height())
        );
        let _ = writeln!(
            body,
            "BT /F1 8 Tf {} {} Td ({}) Tj ET",
            fmt_num(origin.0),
            fmt_num(origin.1),
            escape_literal_string(&text)
        );
        geometry.page_info_origin = Some(origin);
    }

    body.push_str("Q\n");
    let overlay_box = overlay_bounds(&geometry, trim);
    Ok(PrinterMarks {
        content: body.into_bytes(),
        overlay_box,
        geometry,
    })
}

/// Reject a layout whose crop marks, colour bar or page information would fall
/// outside the MediaBox.
///
/// [`PageBoxes::validate`] guarantees the TrimBox lies within the MediaBox, so
/// every available margin here is non-negative. Registration marks are exempt:
/// they are centred on MediaBox corners and deliberately straddle the edge.
fn validate_sheet_fit(boxes: &PageBoxes, options: &PrinterMarkOptions) -> Result<()> {
    let media = boxes.media();
    let trim = boxes.trim();
    if options.crop_marks {
        let required = options.mark_gap + options.mark_length;
        let margins = [
            ("left crop mark", trim.left - media.left),
            ("bottom crop mark", trim.bottom - media.bottom),
            ("right crop mark", media.right - trim.right),
            ("top crop mark", media.top - trim.top),
        ];
        for (what, available) in margins {
            if available < required {
                return Err(Error::PrinterMarksOutOfSheet {
                    what,
                    required,
                    available,
                });
            }
        }
    }
    let bottom_margin = trim.bottom - media.bottom;
    if options.color_bars {
        let required = COLOR_BAR_GAP + options.color_bar_height;
        if bottom_margin < required {
            return Err(Error::PrinterMarksOutOfSheet {
                what: "colour bar",
                required,
                available: bottom_margin,
            });
        }
    }
    if options.page_info {
        let required = COLOR_BAR_GAP + options.color_bar_height + PAGE_INFO_GAP;
        if bottom_margin < required {
            return Err(Error::PrinterMarksOutOfSheet {
                what: "page information",
                required,
                available: bottom_margin,
            });
        }
    }
    Ok(())
}

/// Compute the eight crop-mark segments for a TrimBox.
fn crop_mark_segments(trim: Rectangle, gap: f64, length: f64) -> Vec<MarkSegment> {
    let inner = trim.left - gap;
    let outer = inner - length;
    let right_inner = trim.right + gap;
    let right_outer = right_inner + length;
    let top_inner = trim.top + gap;
    let top_outer = top_inner + length;
    let bottom_inner = trim.bottom - gap;
    let bottom_outer = bottom_inner - length;
    vec![
        MarkSegment::new(outer, trim.top, inner, trim.top),
        MarkSegment::new(trim.left, top_inner, trim.left, top_outer),
        MarkSegment::new(right_inner, trim.top, right_outer, trim.top),
        MarkSegment::new(trim.right, top_inner, trim.right, top_outer),
        MarkSegment::new(outer, trim.bottom, inner, trim.bottom),
        MarkSegment::new(trim.left, bottom_outer, trim.left, bottom_inner),
        MarkSegment::new(right_inner, trim.bottom, right_outer, trim.bottom),
        MarkSegment::new(trim.right, bottom_outer, trim.right, bottom_inner),
    ]
}

/// Compute registration targets at the four corners of the MediaBox.
fn registration_marks(media: Rectangle, radius: f64) -> Vec<([MarkSegment; 2], MarkCircle)> {
    let reach = radius * 2.0;
    let centres = [
        (media.left, media.bottom),
        (media.right, media.bottom),
        (media.left, media.top),
        (media.right, media.top),
    ];
    centres
        .iter()
        .map(|&(cx, cy)| {
            let cross = [
                MarkSegment::new(cx - reach, cy, cx + reach, cy),
                MarkSegment::new(cx, cy - reach, cx, cy + reach),
            ];
            (cross, MarkCircle::new(cx, cy, radius))
        })
        .collect()
}

/// Compute the four CMYK swatches below the TrimBox.
fn color_bar(trim: Rectangle, bleed: Rectangle, height: f64) -> Vec<(Rectangle, (f64, f64, f64))> {
    let width = trim.width() / 16.0;
    let bottom = trim.bottom - COLOR_BAR_GAP - height;
    let colors = [
        (0.0, 1.0, 1.0),
        (1.0, 0.0, 1.0),
        (1.0, 1.0, 0.0),
        (0.0, 0.0, 0.0),
    ];
    let available = bleed.width();
    let stride = width.min(available / 4.0);
    colors
        .iter()
        .enumerate()
        .map(|(index, &color)| {
            let left = trim.left + stride * index as f64;
            (
                Rectangle::new(left, bottom, left + width, bottom + height),
                color,
            )
        })
        .collect()
}

/// Emit the PDF operators for one line segment.
fn segment_operator(segment: &MarkSegment) -> String {
    format!(
        "{} {} m\n{} {} l\nS\n",
        fmt_num(segment.x1),
        fmt_num(segment.y1),
        fmt_num(segment.x2),
        fmt_num(segment.y2)
    )
}

/// Emit four cubic Bézier arcs approximating a circle as PDF operators.
fn circle_operator(circle: MarkCircle) -> String {
    let k = 0.5523f64 * circle.radius;
    let r = circle.radius;
    let (cx, cy) = (circle.cx, circle.cy);
    let mut out = format!("{} {} m\n", fmt_num(cx + r), fmt_num(cy));
    for (c1x, c1y, c2x, c2y, ex, ey) in [
        (cx + r, cy + k, cx + k, cy + r, cx, cy + r),
        (cx - k, cy + r, cx - r, cy + k, cx - r, cy),
        (cx - r, cy - k, cx - k, cy - r, cx, cy - r),
        (cx + k, cy - r, cx + r, cy - k, cx + r, cy),
    ] {
        let _ = writeln!(
            out,
            "{} {} {} {} {} {} c",
            fmt_num(c1x),
            fmt_num(c1y),
            fmt_num(c2x),
            fmt_num(c2y),
            fmt_num(ex),
            fmt_num(ey)
        );
    }
    out.push_str("S\n");
    out
}

/// Escape the three characters that are special inside a PDF literal string.
fn escape_literal_string(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '\\' | '(' | ')' => out.push('\\'),
            _ => {}
        }
        out.push(ch);
    }
    out
}

/// Format a finite number canonically: integers without a decimal point,
/// otherwise up to four decimal places with trailing zeros removed.
///
/// This establishes a precision floor at `FMT_PRECISION` (`1.0e-4`): any value
/// whose magnitude is smaller, including `-0.0`, is emitted as `0`. Because the
/// formatter is deterministic but lossy below that floor,
/// [`PrinterMarkOptions::validate`] rejects option values in that range rather
/// than letting them disappear from the stream.
fn fmt_num(value: f64) -> String {
    if value.abs() < FMT_PRECISION {
        return "0".to_owned();
    }
    if value.fract() == 0.0 && value.abs() < 1.0e12 {
        return (value as i64).to_string();
    }
    let mut text = format!("{value:.4}");
    while text.ends_with('0') {
        text.pop();
    }
    if text.ends_with('.') {
        text.pop();
    }
    text
}

/// Convert a rectangle into a four-number PDF array object.
fn rectangle_array(rectangle: Rectangle) -> Object {
    let number = |value: f64| {
        if value.fract() == 0.0 {
            Object::Integer(value as i64)
        } else {
            Object::Real(value)
        }
    };
    Object::Array(vec![
        number(rectangle.left),
        number(rectangle.bottom),
        number(rectangle.right),
        number(rectangle.top),
    ])
}

/// Compute the smallest rectangle enclosing the generated geometry.
fn overlay_bounds(geometry: &MarkGeometry, fallback: Rectangle) -> Rectangle {
    let mut min_x = f64::INFINITY;
    let mut min_y = f64::INFINITY;
    let mut max_x = f64::NEG_INFINITY;
    let mut max_y = f64::NEG_INFINITY;
    let mut include = |x: f64, y: f64| {
        min_x = min_x.min(x);
        min_y = min_y.min(y);
        max_x = max_x.max(x);
        max_y = max_y.max(y);
    };
    for segment in &geometry.crop_segments {
        include(segment.x1, segment.y1);
        include(segment.x2, segment.y2);
    }
    for segment in &geometry.registration_segments {
        include(segment.x1, segment.y1);
        include(segment.x2, segment.y2);
    }
    for circle in &geometry.registration_circles {
        include(circle.cx - circle.radius, circle.cy - circle.radius);
        include(circle.cx + circle.radius, circle.cy + circle.radius);
    }
    for rectangle in &geometry.color_bar_rects {
        include(rectangle.left, rectangle.bottom);
        include(rectangle.right, rectangle.top);
    }
    if let Some((x, y)) = geometry.page_info_origin {
        include(x, y);
    }
    if min_x.is_finite() && min_y.is_finite() && max_x.is_finite() && max_y.is_finite() {
        Rectangle::new(min_x, min_y, max_x, max_y)
    } else {
        fallback
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::boxes::{BoxKind, PageBoxes};

    fn sample_boxes() -> PageBoxes {
        let media = Rectangle::new(0.0, 0.0, 612.0, 792.0);
        let crop = Rectangle::new(2.0, 2.0, 610.0, 790.0);
        let bleed = Rectangle::new(5.0, 5.0, 607.0, 787.0);
        let trim = Rectangle::new(30.0, 30.0, 582.0, 762.0);
        PageBoxes::from_media(media)
            .expect("media")
            .with_crop(crop)
            .expect("crop")
            .with_bleed(bleed)
            .expect("bleed")
            .with_trim(trim)
            .expect("trim")
    }

    #[test]
    fn crop_mark_geometry_matches_trim_corners() {
        let marks = generate_printer_marks(&sample_boxes(), &PrinterMarkOptions::default())
            .expect("generate");
        let segments = &marks.geometry().crop_segments;
        assert_eq!(segments.len(), 8, "two arms per trim corner");
        // Top-left horizontal arm ends 3pt left of the trim edge.
        assert_eq!(
            *segments.first().expect("first"),
            MarkSegment::new(9.0, 762.0, 27.0, 762.0)
        );
        // Top-right horizontal arm starts 3pt right of the trim edge.
        assert_eq!(segments[2], MarkSegment::new(585.0, 762.0, 603.0, 762.0));
    }

    #[test]
    fn registration_mark_geometry_uses_media_corners() {
        let marks = generate_printer_marks(&sample_boxes(), &PrinterMarkOptions::default())
            .expect("generate");
        let geometry = marks.geometry();
        assert_eq!(geometry.registration_circles.len(), 4);
        assert_eq!(
            geometry.registration_segments.len(),
            8,
            "crosshairs have two arms"
        );
        let expected = [
            MarkCircle::new(0.0, 0.0, 6.0),
            MarkCircle::new(612.0, 0.0, 6.0),
            MarkCircle::new(0.0, 792.0, 6.0),
            MarkCircle::new(612.0, 792.0, 6.0),
        ];
        assert_eq!(geometry.registration_circles, expected);
    }

    #[test]
    fn stream_bytes_are_deterministic() {
        let boxes = sample_boxes();
        let options = PrinterMarkOptions::default();
        let first = generate_printer_marks(&boxes, &options).expect("first");
        let second = generate_printer_marks(&boxes, &options).expect("second");
        assert_eq!(first.content_stream(), second.content_stream());
        assert_eq!(first.overlay_box(), second.overlay_box());
        assert!(!first.content_stream().is_empty());
        let text = String::from_utf8(first.content_stream().to_vec()).expect("ASCII stream");
        assert!(text.contains("c\n"), "registration circles use bezier arcs");
        assert!(text.contains("Tj"), "page info text is emitted");
    }

    #[test]
    fn crop_marks_lie_outside_trim_but_inside_bleed() {
        let boxes = sample_boxes();
        let trim = boxes.rectangle(BoxKind::Trim);
        let bleed = boxes.rectangle(BoxKind::Bleed);
        let marks =
            generate_printer_marks(&boxes, &PrinterMarkOptions::default()).expect("generate");
        for segment in &marks.geometry().crop_segments {
            for (x, y) in [(segment.x1, segment.y1), (segment.x2, segment.y2)] {
                let outside_trim =
                    x < trim.left || x > trim.right || y < trim.bottom || y > trim.top;
                let inside_bleed =
                    x >= bleed.left && x <= bleed.right && y >= bleed.bottom && y <= bleed.top;
                assert!(outside_trim, "mark point ({x}, {y}) is inside trim");
                assert!(inside_bleed, "mark point ({x}, {y}) is outside bleed");
            }
        }
    }

    #[test]
    fn invalid_options_and_boxes_are_rejected() {
        let boxes = sample_boxes();
        let options = PrinterMarkOptions {
            mark_length: -1.0,
            ..Default::default()
        };
        assert!(matches!(
            generate_printer_marks(&boxes, &options),
            Err(Error::InvalidPrinterMarkOptions("mark_length"))
        ));

        let media = Rectangle::new(0.0, 0.0, 100.0, 100.0);
        let bad = PageBoxes::from_media(media)
            .expect("media")
            .with_crop(Rectangle::new(-5.0, 0.0, 100.0, 100.0))
            .expect("crop");
        assert!(matches!(
            generate_printer_marks(&bad, &PrinterMarkOptions::default()),
            Err(Error::BoxNotContained { .. })
        ));
    }

    #[test]
    fn page_info_label_escapes_literal_string_specials() {
        let options = PrinterMarkOptions {
            label: "A(B)\\C".to_owned(),
            ..Default::default()
        };
        let marks = generate_printer_marks(&sample_boxes(), &options).expect("generate");
        let text = String::from_utf8(marks.content_stream().to_vec()).expect("ASCII stream");
        assert!(
            text.contains(r"A\(B\)\\C 552x732 pt"),
            "label specials were not escaped: {text}"
        );
    }

    #[test]
    fn marks_stay_on_sheet_at_the_documented_minimum_margin() {
        let media = Rectangle::new(0.0, 0.0, 612.0, 792.0);
        // 21 pt on left/right/top and 28 pt on the bottom: the documented
        // minimum for the default options.
        let trim = Rectangle::new(21.0, 28.0, 591.0, 771.0);
        let boxes = PageBoxes::from_media(media)
            .expect("media")
            .with_crop(trim)
            .expect("crop")
            .with_bleed(trim)
            .expect("bleed")
            .with_trim(trim)
            .expect("trim");
        let marks =
            generate_printer_marks(&boxes, &PrinterMarkOptions::default()).expect("generate");
        let sheet = boxes.media();
        let within = |x: f64, y: f64| {
            x >= sheet.left && x <= sheet.right && y >= sheet.bottom && y <= sheet.top
        };

        for segment in &marks.geometry().crop_segments {
            assert!(within(segment.x1, segment.y1), "crop start off sheet");
            assert!(within(segment.x2, segment.y2), "crop end off sheet");
        }
        for rectangle in &marks.geometry().color_bar_rects {
            assert!(
                within(rectangle.left, rectangle.bottom),
                "swatch start off sheet"
            );
            assert!(
                within(rectangle.right, rectangle.top),
                "swatch end off sheet"
            );
        }
        let (x, y) = marks.geometry().page_info_origin.expect("page info");
        assert!(within(x, y), "page information baseline off sheet");
        assert_eq!(y, sheet.bottom, "page info sits exactly on the bottom edge");
    }

    #[test]
    fn tight_trim_margins_are_rejected_with_a_typed_error() {
        let media = Rectangle::new(0.0, 0.0, 612.0, 792.0);
        // 30 pt on the sides/top fits crop marks; 24 pt on the bottom fits the
        // colour bar (18 pt) but not the page information (28 pt).
        let trim = Rectangle::new(30.0, 24.0, 582.0, 762.0);
        let boxes = PageBoxes::from_media(media)
            .expect("media")
            .with_crop(trim)
            .expect("crop")
            .with_bleed(trim)
            .expect("bleed")
            .with_trim(trim)
            .expect("trim");
        match generate_printer_marks(&boxes, &PrinterMarkOptions::default()) {
            Err(Error::PrinterMarksOutOfSheet {
                what,
                required,
                available,
            }) => {
                assert_eq!(what, "page information");
                assert_eq!(required, 28.0);
                assert_eq!(available, 24.0);
            }
            other => panic!("expected an out-of-sheet error, got {other:?}"),
        }
    }

    #[test]
    fn sub_precision_options_are_rejected() {
        let options = PrinterMarkOptions {
            line_width: 1.0e-5,
            ..Default::default()
        };
        assert!(matches!(
            generate_printer_marks(&sample_boxes(), &options),
            Err(Error::InvalidPrinterMarkOptions("line_width"))
        ));
        let options = PrinterMarkOptions {
            mark_gap: 1.0e-5,
            ..Default::default()
        };
        assert!(matches!(
            generate_printer_marks(&sample_boxes(), &options),
            Err(Error::InvalidPrinterMarkOptions("mark_gap"))
        ));
    }

    /// Assemble a one-page classic-table PDF whose content stream is `stream`.
    fn pdf_with_content_stream(stream: &[u8]) -> Vec<u8> {
        let mut out = String::from("%PDF-1.7\n%PDFXI\n");
        let mut bodies = vec![
            "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 612 792] >>".to_owned(),
            "<< /Type /Page /Parent 2 0 R /Contents 4 0 R >>".to_owned(),
        ];
        let mut stream_body = format!("<< /Length {} >>\nstream\n", stream.len());
        stream_body.push_str(&String::from_utf8_lossy(stream));
        stream_body.push_str("\nendstream");
        bodies.push(stream_body);

        let mut offsets = Vec::with_capacity(bodies.len());
        for (index, body) in bodies.iter().enumerate() {
            offsets.push(out.len());
            out.push_str(&format!("{} 0 obj\n{}\nendobj\n", index + 1, body));
        }
        let xref = out.len();
        out.push_str(&format!("xref\n0 {}\n", bodies.len() + 1));
        out.push_str("0000000000 65535 f \n");
        for offset in &offsets {
            out.push_str(&format!("{offset:010} 00000 n \n"));
        }
        out.push_str(&format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            bodies.len() + 1
        ));
        out.into_bytes()
    }

    #[test]
    fn marks_form_xobject_survives_save_and_reparse() {
        let marks = generate_printer_marks(&sample_boxes(), &PrinterMarkOptions::default())
            .expect("generate");
        let form = marks.to_form_xobject();
        let (dictionary, data) = match &form {
            Object::Stream { dictionary, data } => (dictionary, data),
            _ => panic!("test: a form XObject must be a stream"),
        };
        assert_eq!(data.as_slice(), marks.content_stream());
        assert_eq!(
            dictionary
                .get("BBox")
                .and_then(Object::as_array)
                .map(|items| items.len()),
            Some(4)
        );

        let original = pdf_with_content_stream(marks.content_stream());
        let document = crate::Document::from_bytes(original).expect("fixture parses");
        let saved = document.save().expect("save");
        let reparsed = crate::Document::from_bytes(saved).expect("saved parses");
        assert_eq!(reparsed.page_count().expect("page count"), 1);
        let reloaded = reparsed
            .object(crate::ObjectId::new(4, 0))
            .expect("content stream object");
        match reloaded {
            Object::Stream { data, .. } => {
                assert_eq!(
                    data.as_slice(),
                    marks.content_stream(),
                    "mark bytes survived a save/reparse round-trip"
                );
            }
            _ => panic!("test: object 4 must still be a stream"),
        }
    }

    #[test]
    fn color_bar_has_four_ordered_cmyk_swatches() {
        let marks = generate_printer_marks(&sample_boxes(), &PrinterMarkOptions::default())
            .expect("generate");
        let swatches = &marks.geometry().color_bar_rects;
        assert_eq!(swatches.len(), 4, "CMYK has four swatches");
        for pair in swatches.windows(2) {
            assert!(
                pair[0].right <= pair[1].left,
                "swatches must be in left-to-right order without overlap"
            );
            assert_eq!(pair[0].bottom, pair[1].bottom);
        }
        let text = String::from_utf8(marks.content_stream().to_vec()).expect("ASCII stream");
        assert_eq!(text.matches(" re f").count(), 4, "four filled rectangles");
    }

    #[test]
    fn disabled_mark_families_produce_empty_geometry() {
        let options = PrinterMarkOptions {
            crop_marks: false,
            registration_marks: false,
            color_bars: false,
            page_info: false,
            ..Default::default()
        };
        let marks = generate_printer_marks(&sample_boxes(), &options).expect("generate");
        assert!(marks.geometry().crop_segments.is_empty());
        assert!(marks.geometry().registration_segments.is_empty());
        assert!(marks.geometry().registration_circles.is_empty());
        assert!(marks.geometry().color_bar_rects.is_empty());
        assert_eq!(marks.geometry().page_info_origin, None);
        let text = String::from_utf8(marks.content_stream().to_vec()).expect("ASCII stream");
        assert!(!text.contains("Tj"), "no page-information text");
        assert!(!text.contains(" re f"), "no colour-bar swatches");
        assert!(!text.contains(" c\n"), "no registration circles");
        assert!(
            !marks.overlay_box().width().is_nan(),
            "overlay box is bounded"
        );
    }
}
