//! Deterministic appearance geometry derived from annotation dictionaries.
//!
//! This module does not rasterize anything; it produces the vector primitives
//! an appearance stream needs: border paths (solid or dashed), icon placement,
//! and the quads/lines used by text markup. Every function is pure and orders
//! its output deterministically, so equal input always yields equal geometry.
//! In particular, markup quads are sorted into a stable reading order
//! (top-to-bottom, then left-to-right) before layout is emitted.

use std::cmp::Ordering;

use crate::geometry::{Point, Rect};

use super::serialize::{BorderLineStyle, BorderStyle, PdfAnnotation, PdfSubtype, Quad, TextIcon};

/// A single drawing command in an appearance path.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PathCommand {
    /// Begin a new subpath at a point.
    MoveTo(Point),
    /// Draw a line to a point.
    LineTo(Point),
    /// Close the current subpath.
    Close,
}

/// The border path of an annotation, with its width and line style.
#[derive(Clone, Debug, PartialEq)]
pub struct BorderGeometry {
    /// Border width in page units.
    pub width: f32,
    /// Whether the border is solid or dashed.
    pub style: BorderLineStyle,
    /// Drawing commands in deterministic order.
    pub commands: Vec<PathCommand>,
}

/// Placement of a text-note icon inside an annotation rectangle.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct IconLayout {
    /// Centered square containing the icon.
    pub icon_rect: Rect,
    /// Anchor point (the center of `icon_rect`).
    pub anchor: Point,
}

/// Layout derived from one markup quad.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MarkLayout {
    /// Bounds of the quad, used as the highlight fill region.
    pub rect: Rect,
    /// Optional stroke (underline, strikeout, or squiggly baseline).
    pub stroke: Option<(Point, Point)>,
}

/// The markup subtype whose geometry is being generated.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MarkupKind {
    /// Fill each quad without a stroke.
    Highlight,
    /// Stroke the bottom edge of each quad.
    Underline,
    /// Stroke the vertical middle of each quad.
    StrikeOut,
    /// Stroke the bottom edge of each quad (wave rendering is left to the painter).
    Squiggly,
}

/// The complete appearance geometry of an annotation.
#[derive(Clone, Debug, PartialEq)]
pub struct AppearanceGeometry {
    /// Border path around the annotation rectangle.
    pub border: BorderGeometry,
    /// Constant opacity applied to the appearance.
    pub opacity: f32,
    /// Icon placement, present only for text notes.
    pub icon: Option<IconLayout>,
    /// Markup quad layouts, present only for markup subtypes.
    pub marks: Vec<MarkLayout>,
}

/// Builds the border path of `rect` using `style`.
///
/// A solid border is four edges followed by a close command. A dashed border
/// walks each edge from the lower-left corner clockwise, emitting a
/// `MoveTo`/`LineTo` pair per dash. The traversal order is fixed, so repeated
/// calls with the same inputs return byte-identical paths.
pub fn border_path(rect: &Rect, style: &BorderStyle) -> BorderGeometry {
    let corners = [
        Point::new(rect.x0(), rect.y0()),
        Point::new(rect.x1(), rect.y0()),
        Point::new(rect.x1(), rect.y1()),
        Point::new(rect.x0(), rect.y1()),
    ];
    let mut commands = Vec::new();
    if style.style == BorderLineStyle::Solid || style.dash.is_empty() {
        if let Some(first) = corners.first() {
            commands.push(PathCommand::MoveTo(*first));
            for corner in corners.iter().skip(1) {
                commands.push(PathCommand::LineTo(*corner));
            }
            commands.push(PathCommand::Close);
        }
    } else {
        for index in 0..corners.len() {
            let from = corners[index];
            let to = corners[(index + 1) % corners.len()];
            append_dashed_edge(from, to, &style.dash, &mut commands);
        }
    }
    BorderGeometry {
        width: style.width,
        style: style.style,
        commands,
    }
}

/// Computes a deterministic icon placement for a text note.
///
/// The icon is a centered square whose side is 60% of the shorter rectangle
/// side. The icon kind is accepted so callers can render distinct glyphs, but
/// placement itself is identical for every icon, keeping the layout stable.
pub fn icon_layout(rect: &Rect, icon: TextIcon) -> IconLayout {
    let side = rect.width().min(rect.height()) * 0.6;
    let inset_x = (rect.width() - side) / 2.0;
    let inset_y = (rect.height() - side) / 2.0;
    let icon_rect = rect
        .inset(inset_x, inset_y)
        .unwrap_or_else(|| Rect::saturating(rect.x0(), rect.y0(), rect.x1(), rect.y1()));
    let anchor = Point::new(
        (icon_rect.x0() + icon_rect.x1()) / 2.0,
        (icon_rect.y0() + icon_rect.y1()) / 2.0,
    );
    // `icon` is intentionally read here: distinct icons share geometry but the
    // parameter documents which glyph the caller will draw.
    let _ = icon;
    IconLayout { icon_rect, anchor }
}

/// Sorts markup quads into a deterministic reading order, in place.
///
/// Quads are ordered by descending upper y (top lines first), then by
/// ascending left x (left to right). Ties keep insertion order because the
/// slice sort is stable.
pub fn sort_quads_in_reading_order(quads: &mut [Quad]) {
    quads.sort_by(|a, b| {
        let a_bounds = a.bounds();
        let b_bounds = b.bounds();
        b_bounds
            .y1()
            .partial_cmp(&a_bounds.y1())
            .unwrap_or(Ordering::Equal)
            .then_with(|| {
                a_bounds
                    .x0()
                    .partial_cmp(&b_bounds.x0())
                    .unwrap_or(Ordering::Equal)
            })
    });
}

/// Builds mark layouts for a markup subtype from its quads.
///
/// The quads are copied and sorted before layout, so the result is independent
/// of the input order. Highlight fills the whole quad; underline and squiggly
/// stroke its bottom edge; strikeout strokes its vertical middle.
pub fn markup_geometry(quads: &[Quad], kind: MarkupKind) -> Vec<MarkLayout> {
    let mut ordered = quads.to_vec();
    sort_quads_in_reading_order(&mut ordered);
    ordered
        .into_iter()
        .map(|quad| {
            let bounds = quad.bounds();
            match kind {
                MarkupKind::Highlight => MarkLayout {
                    rect: bounds,
                    stroke: None,
                },
                MarkupKind::Underline | MarkupKind::Squiggly => {
                    let thickness = (bounds.height() * 0.05).max(0.5);
                    let line_y = bounds.y0() + thickness;
                    MarkLayout {
                        rect: Rect::saturating(bounds.x0(), bounds.y0(), bounds.x1(), line_y),
                        stroke: Some((
                            Point::new(bounds.x0(), line_y),
                            Point::new(bounds.x1(), line_y),
                        )),
                    }
                }
                MarkupKind::StrikeOut => {
                    let mid_y = (bounds.y0() + bounds.y1()) / 2.0;
                    let half = (bounds.height() * 0.025).max(0.25);
                    MarkLayout {
                        rect: Rect::saturating(
                            bounds.x0(),
                            mid_y - half,
                            bounds.x1(),
                            mid_y + half,
                        ),
                        stroke: Some((
                            Point::new(bounds.x0(), mid_y),
                            Point::new(bounds.x1(), mid_y),
                        )),
                    }
                }
            }
        })
        .collect()
}

/// Builds the complete appearance geometry for a dictionary-backed annotation.
pub fn build_appearance(annotation: &PdfAnnotation) -> AppearanceGeometry {
    let mut border = BorderStyle::default();
    let mut icon = None;
    let mut marks = Vec::new();

    match &annotation.subtype {
        PdfSubtype::Text(data) => icon = Some(icon_layout(&annotation.rect, data.icon)),
        PdfSubtype::Square(data) | PdfSubtype::Circle(data) => border = data.border.clone(),
        PdfSubtype::FreeText(data) => border = data.border.clone(),
        PdfSubtype::Highlight(data) => {
            marks = markup_geometry(&data.quad_points, MarkupKind::Highlight);
        }
        PdfSubtype::Underline(data) => {
            marks = markup_geometry(&data.quad_points, MarkupKind::Underline);
        }
        PdfSubtype::StrikeOut(data) => {
            marks = markup_geometry(&data.quad_points, MarkupKind::StrikeOut);
        }
        PdfSubtype::Squiggly(data) => {
            marks = markup_geometry(&data.quad_points, MarkupKind::Squiggly);
        }
        PdfSubtype::Line(_) | PdfSubtype::Ink(_) | PdfSubtype::Link(_) | PdfSubtype::Opaque(_) => {}
    }

    AppearanceGeometry {
        border: border_path(&annotation.rect, &border),
        opacity: annotation.opacity,
        icon,
        marks,
    }
}

/// Appends dash commands along a single edge from `from` to `to`.
fn append_dashed_edge(from: Point, to: Point, pattern: &[f32], out: &mut Vec<PathCommand>) {
    if pattern.is_empty() {
        return;
    }
    let pattern: Vec<f64> = if pattern.len().is_multiple_of(2) {
        pattern.iter().map(|&value| f64::from(value)).collect()
    } else {
        pattern
            .iter()
            .chain(pattern.iter())
            .map(|&value| f64::from(value))
            .collect()
    };
    if pattern.iter().all(|&length| length <= 0.0) {
        return;
    }
    let dx = f64::from(to.x) - f64::from(from.x);
    let dy = f64::from(to.y) - f64::from(from.y);
    let length = (dx * dx + dy * dy).sqrt();
    if length <= 0.0 {
        return;
    }
    let mut travelled = 0.0f64;
    let mut index = 0usize;
    while travelled < length {
        let segment = pattern[index % pattern.len()];
        if segment <= 0.0 {
            index += 1;
            continue;
        }
        let end = (travelled + segment).min(length);
        if index.is_multiple_of(2) {
            let start_fraction = travelled / length;
            let end_fraction = end / length;
            out.push(PathCommand::MoveTo(lerp(from, to, start_fraction)));
            out.push(PathCommand::LineTo(lerp(from, to, end_fraction)));
        }
        travelled = end;
        index += 1;
    }
}

/// Linearly interpolates between two points.
fn lerp(from: Point, to: Point, fraction: f64) -> Point {
    let t = fraction as f32;
    Point::new(from.x + (to.x - from.x) * t, from.y + (to.y - from.y) * t)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::Rect;
    use crate::pdf::serialize::{InkData, LineData, LineEnding, MarkupData};

    fn rect() -> Rect {
        Rect::new(0.0, 0.0, 40.0, 20.0).expect("test rect must be valid")
    }

    fn quads() -> Vec<Quad> {
        vec![
            Quad::new(
                Point::new(10.0, 10.0),
                Point::new(20.0, 10.0),
                Point::new(10.0, 12.0),
                Point::new(20.0, 12.0),
            ),
            Quad::new(
                Point::new(0.0, 18.0),
                Point::new(8.0, 18.0),
                Point::new(0.0, 20.0),
                Point::new(8.0, 20.0),
            ),
        ]
    }

    #[test]
    fn solid_border_path_is_five_deterministic_commands() {
        let style = BorderStyle::default();
        let first = border_path(&rect(), &style);
        let second = border_path(&rect(), &style);
        assert_eq!(first, second);
        assert_eq!(first.style, BorderLineStyle::Solid);
        assert_eq!(first.commands.len(), 5);
        assert!(matches!(first.commands[0], PathCommand::MoveTo(_)));
        assert!(matches!(first.commands[4], PathCommand::Close));
    }

    #[test]
    fn dashed_border_path_is_deterministic_and_non_empty() {
        let style = BorderStyle::new(1.0, BorderLineStyle::Dashed, vec![4.0, 2.0])
            .expect("valid dashed border");
        let first = border_path(&rect(), &style);
        let second = border_path(&rect(), &style);
        assert_eq!(first, second);
        assert!(first.commands.len() > 5);
        assert_eq!(first.style, BorderLineStyle::Dashed);
    }

    #[test]
    fn markup_quad_ordering_is_deterministic_and_does_not_mutate_input() {
        let input = quads();
        let mut scrambled = vec![input[0], input[1]];
        scrambled.reverse();
        let a = markup_geometry(&input, MarkupKind::Highlight);
        let b = markup_geometry(&scrambled, MarkupKind::Highlight);
        assert_eq!(a, b);
        // Reading order: the higher quad (y = 18..20) comes first.
        assert_eq!(a[0].rect.y0(), 18.0);
        assert_eq!(a[1].rect.y0(), 10.0);
        // The source annotations and quads remain untouched.
        assert_eq!(
            scrambled,
            vec![input[0], input[1]]
                .into_iter()
                .rev()
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn markup_strokes_are_deterministic() {
        for kind in [
            MarkupKind::Underline,
            MarkupKind::StrikeOut,
            MarkupKind::Squiggly,
        ] {
            let first = markup_geometry(&quads(), kind);
            let second = markup_geometry(&quads(), kind);
            assert_eq!(first, second);
            assert!(first.iter().all(|layout| layout.stroke.is_some()));
        }
        assert!(
            markup_geometry(&quads(), MarkupKind::Highlight)
                .iter()
                .all(|layout| layout.stroke.is_none())
        );
    }

    #[test]
    fn build_appearance_covers_icon_border_and_marks() {
        use super::super::serialize::{PdfAnnotation, PdfSubtype, ShapeData, TextData};
        use crate::annotation::{Annotation, AnnotationId, AnnotationKind, Color};
        use crate::geometry::PageIndex;

        let base = Annotation::new(
            AnnotationId::new(1),
            AnnotationKind::Note,
            PageIndex::new(0),
            rect(),
            "",
            "",
        )
        .expect("valid annotation");

        let mut note = PdfAnnotation::from_annotation(&base);
        note.subtype = PdfSubtype::Text(TextData::default());
        let note_geometry = build_appearance(&note);
        assert!(note_geometry.icon.is_some());
        assert_eq!(note_geometry, build_appearance(&note));

        note.subtype = PdfSubtype::Square(ShapeData {
            border: BorderStyle::new(1.0, BorderLineStyle::Solid, vec![]).expect("valid border"),
            interior_color: Some(Color::new(1, 2, 3)),
        });
        let shape_geometry = build_appearance(&note);
        assert!(shape_geometry.icon.is_none());
        assert_eq!(shape_geometry.border.width, 1.0);

        note.subtype = PdfSubtype::Highlight(MarkupData {
            quad_points: quads(),
        });
        assert_eq!(build_appearance(&note).marks.len(), 2);

        // Ink, line, and link subtypes still produce a deterministic border.
        note.subtype = PdfSubtype::Line(LineData {
            start: Point::new(0.0, 0.0),
            end: Point::new(1.0, 1.0),
            start_ending: LineEnding::None,
            end_ending: LineEnding::None,
        });
        assert_eq!(build_appearance(&note), build_appearance(&note));
        note.subtype = PdfSubtype::Ink(InkData {
            paths: vec![vec![Point::new(0.0, 0.0)]],
        });
        assert_eq!(build_appearance(&note), build_appearance(&note));
    }

    #[test]
    fn opacity_is_carried_into_appearance() {
        use super::super::serialize::PdfAnnotation;
        use crate::annotation::{Annotation, AnnotationId, AnnotationKind};
        use crate::geometry::PageIndex;

        let base = Annotation::new(
            AnnotationId::new(2),
            AnnotationKind::Note,
            PageIndex::new(0),
            rect(),
            "",
            "",
        )
        .expect("valid annotation");
        let pdf = PdfAnnotation::from_annotation(&base);
        let geometry = build_appearance(&pdf);
        assert!((geometry.opacity - pdf.opacity).abs() < f32::EPSILON);
    }
}
