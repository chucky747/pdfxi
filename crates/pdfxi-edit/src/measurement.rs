//! Measurement annotations: distance, area, and perimeter (REVIEW-009).
//!
//! This module computes page-space measurement geometry from validated points
//! and converts the result into caller-chosen units through an explicit
//! [`MeasureScale`]. It produces a deterministic [`MeasurementAppearance`]
//! whose vector commands and one-line summary are pure functions of the
//! measurement, so equal input always yields identical output. Nothing here
//! rasterizes or mutates a document: it is a headless measurement model.
//!
//! All points are finite PDF page coordinates, all scale factors are validated,
//! and every failure is a typed [`MeasurementError`]. Non-test code uses no
//! `unwrap`, `expect`, `panic!`, or unsafe code.

use std::fmt;

use crate::geometry::Point;
use crate::pdf::appearance::PathCommand;

/// Errors produced while validating or evaluating a measurement.
#[derive(Debug, Clone, PartialEq)]
pub enum MeasurementError {
    /// A point carried a non-finite coordinate.
    NonFinitePoint,
    /// A distance measurement had identical endpoints.
    DegenerateDistance,
    /// A polygon measurement had fewer than three vertices.
    TooFewVertices(usize),
    /// All polygon vertices were identical, leaving no area or perimeter.
    DegeneratePolygon,
    /// A scale factor was non-finite, zero, or negative.
    InvalidScale,
    /// A rounding precision exceeded the supported range.
    TooManyDecimals(u8),
    /// A computed value was not finite.
    NonFiniteValue,
}

impl fmt::Display for MeasurementError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NonFinitePoint => f.write_str("measurement points must be finite"),
            Self::DegenerateDistance => {
                f.write_str("a distance measurement needs two distinct endpoints")
            }
            Self::TooFewVertices(count) => {
                write!(f, "a polygon needs at least three vertices, got {count}")
            }
            Self::DegeneratePolygon => f.write_str("polygon vertices must not all be identical"),
            Self::InvalidScale => f.write_str("measurement scale must be finite and positive"),
            Self::TooManyDecimals(value) => {
                write!(f, "measurement precision {value} exceeds the maximum of 6")
            }
            Self::NonFiniteValue => f.write_str("computed measurement value is not finite"),
        }
    }
}

impl std::error::Error for MeasurementError {}

/// Unit used to display a measurement value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MeasureUnit {
    /// PDF page points (1/72 inch).
    Point,
    /// Inches.
    Inch,
    /// Millimetres.
    Millimeter,
    /// Centimetres.
    Centimeter,
    /// Feet.
    Foot,
}

impl MeasureUnit {
    /// All units in a fixed declaration order.
    pub const ALL: [Self; 5] = [
        Self::Point,
        Self::Inch,
        Self::Millimeter,
        Self::Centimeter,
        Self::Foot,
    ];

    /// Short label used in appearance summaries.
    pub fn label(self) -> &'static str {
        match self {
            Self::Point => "pt",
            Self::Inch => "in",
            Self::Millimeter => "mm",
            Self::Centimeter => "cm",
            Self::Foot => "ft",
        }
    }

    /// Number of PDF points in one of this unit.
    pub fn points_per_unit(self) -> f64 {
        match self {
            Self::Point => 1.0,
            Self::Inch => 72.0,
            Self::Millimeter => 72.0 / 25.4,
            Self::Centimeter => 72.0 / 2.54,
            Self::Foot => 72.0 * 12.0,
        }
    }
}

/// Conversion from page points into a display unit.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MeasureScale {
    /// Unit the converted value is expressed in.
    pub unit: MeasureUnit,
    /// Multiplier applied to a point value to obtain `unit` values.
    pub units_per_point: f64,
}

impl MeasureScale {
    /// A one-to-one scale that reports raw points.
    pub fn identity() -> Self {
        Self {
            unit: MeasureUnit::Point,
            units_per_point: 1.0,
        }
    }

    /// A scale that converts points into `unit` using the standard factor.
    pub fn for_unit(unit: MeasureUnit) -> Self {
        Self {
            unit,
            units_per_point: 1.0 / unit.points_per_unit(),
        }
    }

    /// A scale built from a dots-per-inch resolution.
    pub fn from_dpi(dpi: f64) -> Result<Self, MeasurementError> {
        if !dpi.is_finite() || dpi <= 0.0 {
            return Err(MeasurementError::InvalidScale);
        }
        Ok(Self {
            unit: MeasureUnit::Inch,
            units_per_point: 1.0 / dpi,
        })
    }

    /// Builds a scale from an explicit unit and multiplier.
    pub fn new(unit: MeasureUnit, units_per_point: f64) -> Result<Self, MeasurementError> {
        if !units_per_point.is_finite() || units_per_point <= 0.0 {
            return Err(MeasurementError::InvalidScale);
        }
        Ok(Self {
            unit,
            units_per_point,
        })
    }

    /// Converts a point value into the configured unit.
    pub fn convert(self, points: f64) -> f64 {
        points * self.units_per_point
    }
}

/// What geometric quantity a [`Measurement`] reports.
#[derive(Clone, Debug, PartialEq)]
pub enum MeasurementKind {
    /// Straight-line distance between two endpoints.
    Distance {
        /// First endpoint.
        start: Point,
        /// Second endpoint.
        end: Point,
    },
    /// Absolute polygon area via the shoelace formula.
    Area {
        /// Polygon vertices in perimeter order.
        vertices: Vec<Point>,
    },
    /// Sum of polygon edge lengths, including the closing edge.
    Perimeter {
        /// Polygon vertices in perimeter order.
        vertices: Vec<Point>,
    },
}

impl MeasurementKind {
    /// Stable lowercase label for this measurement.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Distance { .. } => "distance",
            Self::Area { .. } => "area",
            Self::Perimeter { .. } => "perimeter",
        }
    }

    /// Validates the geometry of this measurement.
    pub fn validate(&self) -> Result<(), MeasurementError> {
        match self {
            Self::Distance { start, end } => {
                if !start.is_finite() || !end.is_finite() {
                    return Err(MeasurementError::NonFinitePoint);
                }
                if start == end {
                    return Err(MeasurementError::DegenerateDistance);
                }
                Ok(())
            }
            Self::Area { vertices } | Self::Perimeter { vertices } => {
                if vertices.len() < 3 {
                    return Err(MeasurementError::TooFewVertices(vertices.len()));
                }
                if vertices.iter().any(|point| !point.is_finite()) {
                    return Err(MeasurementError::NonFinitePoint);
                }
                let first = vertices[0];
                if vertices.iter().all(|point| *point == first) {
                    return Err(MeasurementError::DegeneratePolygon);
                }
                Ok(())
            }
        }
    }

    /// Raw measurement value in square/linear page points.
    fn value_in_points(&self) -> f64 {
        match self {
            Self::Distance { start, end } => distance(*start, *end),
            Self::Area { vertices } => polygon_area(vertices),
            Self::Perimeter { vertices } => polygon_perimeter(vertices),
        }
    }
}

/// A validated measurement with a display scale and rounding precision.
#[derive(Clone, Debug, PartialEq)]
pub struct Measurement {
    kind: MeasurementKind,
    scale: MeasureScale,
    decimals: u8,
}

impl Measurement {
    /// Builds and validates a measurement.
    pub fn new(
        kind: MeasurementKind,
        scale: MeasureScale,
        decimals: u8,
    ) -> Result<Self, MeasurementError> {
        if decimals > 6 {
            return Err(MeasurementError::TooManyDecimals(decimals));
        }
        if !scale.units_per_point.is_finite() || scale.units_per_point <= 0.0 {
            return Err(MeasurementError::InvalidScale);
        }
        kind.validate()?;
        Ok(Self {
            kind,
            scale,
            decimals,
        })
    }

    /// Creates a distance measurement.
    pub fn distance(
        start: Point,
        end: Point,
        scale: MeasureScale,
    ) -> Result<Self, MeasurementError> {
        Self::new(MeasurementKind::Distance { start, end }, scale, 2)
    }

    /// Creates an area measurement.
    pub fn area(vertices: Vec<Point>, scale: MeasureScale) -> Result<Self, MeasurementError> {
        Self::new(MeasurementKind::Area { vertices }, scale, 2)
    }

    /// Creates a perimeter measurement.
    pub fn perimeter(vertices: Vec<Point>, scale: MeasureScale) -> Result<Self, MeasurementError> {
        Self::new(MeasurementKind::Perimeter { vertices }, scale, 2)
    }

    /// Overrides the rounding precision (clamped to the supported range).
    pub fn with_decimals(mut self, decimals: u8) -> Self {
        if decimals <= 6 {
            self.decimals = decimals;
        }
        self
    }

    /// Measurement geometry.
    pub fn kind(&self) -> &MeasurementKind {
        &self.kind
    }

    /// Display scale.
    pub fn scale(&self) -> MeasureScale {
        self.scale
    }

    /// Rounding precision used in the summary.
    pub fn decimals(&self) -> u8 {
        self.decimals
    }

    /// Converts the raw geometry into the display unit without rounding.
    pub fn value(&self) -> Result<f64, MeasurementError> {
        let value = self.scale.convert(self.kind.value_in_points());
        if !value.is_finite() {
            return Err(MeasurementError::NonFiniteValue);
        }
        Ok(value)
    }

    /// Derives a deterministic appearance with vector commands and a summary.
    pub fn appearance(&self) -> Result<MeasurementAppearance, MeasurementError> {
        let value = self.value()?;
        let factor = 10_f64.powi(i32::from(self.decimals));
        let rounded = (value * factor).round() / factor;
        let commands = match &self.kind {
            MeasurementKind::Distance { start, end } => {
                vec![PathCommand::MoveTo(*start), PathCommand::LineTo(*end)]
            }
            MeasurementKind::Area { vertices } | MeasurementKind::Perimeter { vertices } => {
                let mut commands = Vec::with_capacity(vertices.len() + 1);
                if let Some((first, rest)) = vertices.split_first() {
                    commands.push(PathCommand::MoveTo(*first));
                    for point in rest {
                        commands.push(PathCommand::LineTo(*point));
                    }
                    commands.push(PathCommand::Close);
                }
                commands
            }
        };
        let mut unit = self.scale.unit.label().to_string();
        if matches!(self.kind, MeasurementKind::Area { .. }) {
            unit.push('\u{00B2}');
        }
        let summary = format!(
            "{} {:.*} {}",
            self.kind.label(),
            usize::from(self.decimals),
            rounded,
            unit
        );
        Ok(MeasurementAppearance {
            kind: self.kind.label(),
            value: rounded,
            unit,
            decimals: self.decimals,
            commands,
            summary,
        })
    }
}

/// Deterministic appearance derived from a [`Measurement`].
#[derive(Clone, Debug, PartialEq)]
pub struct MeasurementAppearance {
    /// Measurement kind label.
    pub kind: &'static str,
    /// Rounded value in the display unit.
    pub value: f64,
    /// Unit label (with a squared marker for area).
    pub unit: String,
    /// Decimal places used for rounding.
    pub decimals: u8,
    /// Vector commands in a fixed order.
    pub commands: Vec<PathCommand>,
    /// Stable one-line human-readable summary.
    pub summary: String,
}

fn distance(a: Point, b: Point) -> f64 {
    let dx = f64::from(b.x) - f64::from(a.x);
    let dy = f64::from(b.y) - f64::from(a.y);
    dx.hypot(dy)
}

fn polygon_area(vertices: &[Point]) -> f64 {
    let mut twice = 0.0;
    for index in 0..vertices.len() {
        let current = vertices[index];
        let next = vertices[(index + 1) % vertices.len()];
        twice +=
            f64::from(current.x) * f64::from(next.y) - f64::from(next.x) * f64::from(current.y);
    }
    (twice * 0.5).abs()
}

fn polygon_perimeter(vertices: &[Point]) -> f64 {
    let mut total = 0.0;
    for index in 0..vertices.len() {
        total += distance(vertices[index], vertices[(index + 1) % vertices.len()]);
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;

    fn point(x: f32, y: f32) -> Point {
        Point::new(x, y)
    }

    #[test]
    fn distance_uses_pythagorean_geometry_in_points() {
        let measurement =
            Measurement::distance(point(0.0, 0.0), point(3.0, 4.0), MeasureScale::identity())
                .expect("distance");
        assert_eq!(measurement.value().expect("value"), 5.0);
        let appearance = measurement.appearance().expect("appearance");
        assert_eq!(appearance.value, 5.0);
        assert_eq!(appearance.unit, "pt");
        assert_eq!(appearance.summary, "distance 5.00 pt");
        assert_eq!(appearance.kind, "distance");
    }

    #[test]
    fn measurement_units_convert_points_deterministically() {
        let measurement = Measurement::distance(
            point(0.0, 0.0),
            point(72.0, 0.0),
            MeasureScale::for_unit(MeasureUnit::Inch),
        )
        .expect("distance");
        assert!((measurement.value().expect("value") - 1.0).abs() < 1e-9);
        assert_eq!(
            measurement.appearance().expect("appearance").summary,
            "distance 1.00 in"
        );

        let metric = Measurement::distance(
            point(0.0, 0.0),
            point(72.0, 0.0),
            MeasureScale::for_unit(MeasureUnit::Millimeter),
        )
        .expect("distance");
        let value = metric.value().expect("value");
        assert!((value - 25.4).abs() < 1e-6);
        assert_eq!(metric.appearance().expect("appearance").unit, "mm");
    }

    #[test]
    fn area_and_perimeter_use_shoelace_and_edge_lengths() {
        let square = vec![
            point(0.0, 0.0),
            point(10.0, 0.0),
            point(10.0, 10.0),
            point(0.0, 10.0),
        ];
        let area = Measurement::area(square.clone(), MeasureScale::identity()).expect("area");
        assert_eq!(area.value().expect("value"), 100.0);
        let appearance = area.appearance().expect("appearance");
        assert_eq!(appearance.summary, "area 100.00 pt\u{00B2}");
        assert!(appearance.commands.contains(&PathCommand::Close));

        let perimeter =
            Measurement::perimeter(square, MeasureScale::identity()).expect("perimeter");
        assert_eq!(perimeter.value().expect("value"), 40.0);
        assert_eq!(
            perimeter.appearance().expect("appearance").summary,
            "perimeter 40.00 pt"
        );
    }

    #[test]
    fn appearance_summary_rounds_deterministically() {
        let measurement = Measurement::distance(
            point(0.0, 0.0),
            point(1.0, 0.0),
            MeasureScale::for_unit(MeasureUnit::Inch),
        )
        .expect("distance")
        .with_decimals(4);
        let first = measurement.appearance().expect("appearance");
        assert_eq!(first.summary, "distance 0.0139 in");
        assert_eq!(first, measurement.appearance().expect("appearance again"));
        assert_eq!(first.decimals, 4);
    }

    #[test]
    fn degenerate_distance_is_rejected() {
        assert_eq!(
            Measurement::distance(point(5.0, 5.0), point(5.0, 5.0), MeasureScale::identity()),
            Err(MeasurementError::DegenerateDistance)
        );
    }

    #[test]
    fn polygon_validation_rejects_too_few_non_finite_and_identical_vertices() {
        assert_eq!(
            Measurement::area(
                vec![point(0.0, 0.0), point(1.0, 1.0)],
                MeasureScale::identity()
            ),
            Err(MeasurementError::TooFewVertices(2))
        );
        assert_eq!(
            Measurement::perimeter(
                vec![point(0.0, 0.0), point(1.0, 1.0), point(f32::NAN, 2.0)],
                MeasureScale::identity()
            ),
            Err(MeasurementError::NonFinitePoint)
        );
        assert_eq!(
            Measurement::area(
                vec![point(2.0, 2.0), point(2.0, 2.0), point(2.0, 2.0)],
                MeasureScale::identity()
            ),
            Err(MeasurementError::DegeneratePolygon)
        );
    }

    #[test]
    fn scale_validation_rejects_non_positive_and_non_finite_factors() {
        for factor in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            assert_eq!(
                MeasureScale::new(MeasureUnit::Point, factor),
                Err(MeasurementError::InvalidScale)
            );
        }
        assert_eq!(
            MeasureScale::from_dpi(0.0),
            Err(MeasurementError::InvalidScale)
        );
        assert_eq!(
            MeasureScale::from_dpi(-96.0),
            Err(MeasurementError::InvalidScale)
        );
        assert!(MeasureScale::from_dpi(96.0).is_ok());
    }

    #[test]
    fn precision_above_six_decimals_is_rejected() {
        let result = Measurement::new(
            MeasurementKind::Distance {
                start: point(0.0, 0.0),
                end: point(1.0, 0.0),
            },
            MeasureScale::identity(),
            7,
        );
        assert_eq!(result, Err(MeasurementError::TooManyDecimals(7)));
    }

    #[test]
    fn distance_commands_are_deterministic() {
        let measurement =
            Measurement::distance(point(1.0, 2.0), point(4.0, 6.0), MeasureScale::identity())
                .expect("distance");
        assert_eq!(
            measurement.appearance().expect("appearance").commands,
            vec![
                PathCommand::MoveTo(point(1.0, 2.0)),
                PathCommand::LineTo(point(4.0, 6.0))
            ]
        );
    }

    #[test]
    fn unit_labels_and_error_messages_are_stable() {
        let labels: Vec<&str> = MeasureUnit::ALL.iter().map(|unit| unit.label()).collect();
        assert_eq!(labels, vec!["pt", "in", "mm", "cm", "ft"]);
        assert!(!MeasurementError::NonFiniteValue.to_string().is_empty());
        assert!(!MeasurementError::InvalidScale.to_string().is_empty());
    }
}
