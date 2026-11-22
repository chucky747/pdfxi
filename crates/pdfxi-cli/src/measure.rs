//! `pdfxi measure`: page-space distance, area and perimeter measurements.
//!
//! [`pdfxi_edit::measurement`] computes validated measurement geometry and a
//! deterministic appearance summary, but takes caller-supplied points. This
//! host supplies them from the command line, validates them against the real
//! page box read from the PDF, and selects the display unit or DPI scale.

use std::path::Path;

use anyhow::{Context, bail};
use pdfxi_core::Document;
use pdfxi_edit::{MeasureScale, MeasureUnit, Measurement, Point};
use serde::Serialize;

/// One measurement result.
#[derive(Serialize)]
struct ResultRow {
    /// Measurement kind.
    kind: &'static str,
    /// Rounded value in the display unit.
    value: f64,
    /// Unit label (`in`, `mm`, ...).
    unit: String,
    /// Stable one-line summary.
    summary: String,
    /// Number of vector commands in the appearance.
    commands: usize,
}

/// JSON shape of `pdfxi measure`.
#[derive(Serialize)]
struct MeasureReport {
    /// Input PDF.
    file: String,
    /// One-based page.
    page: usize,
    /// Page media box as `[left, bottom, right, top]`.
    page_box: [f64; 4],
    /// Points supplied.
    points: usize,
    /// Display unit label.
    unit: String,
    /// Points-to-unit multiplier.
    units_per_point: f64,
    /// Rounding precision.
    decimals: u8,
    /// Distance, area or perimeter results.
    results: Vec<ResultRow>,
    /// Deterministic summaries in result order.
    summaries: Vec<String>,
}

/// Parse a unit label.
fn parse_unit(label: &str) -> anyhow::Result<MeasureUnit> {
    match label.to_ascii_lowercase().as_str() {
        "pt" | "point" | "points" => Ok(MeasureUnit::Point),
        "in" | "inch" | "inches" => Ok(MeasureUnit::Inch),
        "mm" | "millimeter" | "millimetre" => Ok(MeasureUnit::Millimeter),
        "cm" | "centimeter" | "centimetre" => Ok(MeasureUnit::Centimeter),
        "ft" | "foot" | "feet" => Ok(MeasureUnit::Foot),
        other => bail!("unknown --unit {other:?}; expected pt, in, mm, cm or ft"),
    }
}

/// Parse `x,y;x,y;...` into finite page points.
fn parse_points(text: &str) -> anyhow::Result<Vec<Point>> {
    let mut points = Vec::new();
    for pair in text.split(';') {
        let pair = pair.trim();
        if pair.is_empty() {
            continue;
        }
        let (x, y) = pair
            .split_once(',')
            .with_context(|| format!("point {pair:?} needs X,Y"))?;
        let x: f32 = x
            .trim()
            .parse()
            .with_context(|| format!("point {pair:?} has an invalid x"))?;
        let y: f32 = y
            .trim()
            .parse()
            .with_context(|| format!("point {pair:?} has an invalid y"))?;
        if !x.is_finite() || !y.is_finite() {
            bail!("point {pair:?} is not finite");
        }
        points.push(Point::new(x, y));
    }
    if points.len() < 2 {
        bail!("pass at least two points as --points \"x,y;x,y\"");
    }
    Ok(points)
}

/// Build the display scale from `--unit` or `--dpi`.
fn build_scale(unit: Option<&str>, dpi: Option<f64>) -> anyhow::Result<MeasureScale> {
    match (unit, dpi) {
        (Some(_), Some(_)) => bail!("--unit and --dpi are mutually exclusive"),
        (None, Some(dpi)) => {
            MeasureScale::from_dpi(dpi).map_err(|error| anyhow::anyhow!("invalid --dpi: {error}"))
        }
        (Some(unit), None) => Ok(MeasureScale::for_unit(parse_unit(unit)?)),
        (None, None) => Ok(MeasureScale::identity()),
    }
}

/// Convert one measurement into a report row.
fn row(measurement: &Measurement) -> anyhow::Result<ResultRow> {
    let appearance = measurement
        .appearance()
        .map_err(|error| anyhow::anyhow!("measurement appearance: {error}"))?;
    Ok(ResultRow {
        kind: appearance.kind,
        value: appearance.value,
        unit: appearance.unit,
        summary: appearance.summary,
        commands: appearance.commands.len(),
    })
}

/// `pdfxi measure`.
pub(crate) fn run_measure(
    file: &Path,
    page: usize,
    points_text: &str,
    unit: Option<&str>,
    dpi: Option<f64>,
    decimals: u8,
    json: bool,
) -> anyhow::Result<()> {
    if page == 0 {
        bail!("--page is one-based and must be at least 1");
    }
    if decimals > 6 {
        bail!("--decimals must be between 0 and 6");
    }
    let points = parse_points(points_text)?;
    let scale = build_scale(unit, dpi)?;
    let bytes = std::fs::read(file).with_context(|| format!("read {}", file.display()))?;
    let document = Document::from_bytes(bytes)
        .map_err(|error| anyhow::anyhow!("parse {}: {error}", file.display()))?;
    let count = document
        .page_count()
        .map_err(|error| anyhow::anyhow!("count pages: {error}"))?;
    if page > count {
        bail!(
            "page {page} does not exist; {} has {count} page(s)",
            file.display()
        );
    }
    let info = document
        .page(page - 1)
        .map_err(|error| anyhow::anyhow!("read page {page}: {error}"))?;
    let left = info.media_box.left;
    let bottom = info.media_box.bottom;
    let right = info.media_box.right;
    let top = info.media_box.top;
    for point in &points {
        let x = f64::from(point.x);
        let y = f64::from(point.y);
        if x < left || x > right || y < bottom || y > top {
            bail!("point ({x}, {y}) lies outside the page box [{left}, {bottom}, {right}, {top}]");
        }
    }
    let mut results = Vec::new();
    if points.len() == 2 {
        let measurement = Measurement::distance(points[0], points[1], scale)
            .map_err(|error| anyhow::anyhow!("distance: {error}"))?
            .with_decimals(decimals);
        results.push(row(&measurement)?);
    } else {
        let area = Measurement::area(points.clone(), scale)
            .map_err(|error| anyhow::anyhow!("area: {error}"))?
            .with_decimals(decimals);
        let perimeter = Measurement::perimeter(points.clone(), scale)
            .map_err(|error| anyhow::anyhow!("perimeter: {error}"))?
            .with_decimals(decimals);
        results.push(row(&area)?);
        results.push(row(&perimeter)?);
    }
    let report = MeasureReport {
        file: file.display().to_string(),
        page,
        page_box: [left, bottom, right, top],
        points: points.len(),
        unit: scale.unit.label().to_owned(),
        units_per_point: scale.units_per_point,
        decimals,
        summaries: results
            .iter()
            .map(|result| result.summary.clone())
            .collect(),
        results,
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }
    println!("file: {}", report.file);
    println!("page: {}", report.page);
    println!(
        "page_box: {} {} {} {}",
        report.page_box[0], report.page_box[1], report.page_box[2], report.page_box[3]
    );
    println!(
        "unit: {}  units_per_point: {}",
        report.unit, report.units_per_point
    );
    println!("points: {}", report.points);
    for result in &report.results {
        println!("{}: {}", result.kind, result.summary);
    }
    Ok(())
}
