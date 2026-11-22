//! `pdfxi loupe`: window-free loupe magnifier and gesture navigation.
//!
//! [`pdfxi_app::loupe`] is the pure model behind the magnifier: lens geometry,
//! page/screen coordinate mapping, clamped panning and pinch zoom. This host
//! builds it from a real page, applies caller-supplied gesture deltas and
//! reports the resulting lens/source rectangles. It never touches a GUI or an
//! OS gesture recognizer.

use std::path::Path;

use anyhow::{Context, bail};
use pdfxi_app::loupe::{
    GestureDelta, GestureEffect, LensShape, LoupeModel, LoupePoint, LoupeRect, LoupeSize,
};
use pdfxi_core::Document;
use serde::Serialize;

/// JSON shape of a point.
#[derive(Debug, Clone, Copy, Serialize)]
struct PointJson {
    x: f32,
    y: f32,
}

/// JSON shape of a rectangle.
#[derive(Debug, Clone, Copy, Serialize)]
struct RectJson {
    x: f32,
    y: f32,
    width: f32,
    height: f32,
}

/// JSON shape of one gesture step.
#[derive(Debug, Serialize)]
struct StepJson {
    /// Input token.
    command: String,
    /// `ignored`, `panned` or `zoomed`.
    effect: String,
    /// Zoom after the step.
    zoom: f32,
    /// Page-space lens centre after the step.
    center_page: PointJson,
}

/// JSON shape of `pdfxi loupe`.
#[derive(Debug, Serialize)]
struct LoupeJson {
    /// Source PDF.
    input: String,
    /// One-based page.
    page: usize,
    /// Page width in points.
    page_width: f32,
    /// Page height in points.
    page_height: f32,
    /// Viewport width.
    viewport_width: f32,
    /// Viewport height.
    viewport_height: f32,
    /// Lens shape.
    shape: String,
    /// Requested zoom (before clamping).
    requested_zoom: f32,
    /// Applied zoom.
    zoom: f32,
    /// Lens width.
    lens_width: f32,
    /// Lens height.
    lens_height: f32,
    /// Lens centre in page space.
    center_page: PointJson,
    /// Lens centre in screen space.
    center_screen: PointJson,
    /// Where the magnified image is drawn.
    dest_rect: RectJson,
    /// Page-space region shown in the lens.
    source_rect: RectJson,
    /// Bounded accumulated drag.
    accumulated_drag: PointJson,
    /// Whether the lens contains its own screen centre.
    contains_screen_center: bool,
    /// Whether the lens contains its own page centre.
    contains_page_center: bool,
    /// `screen_to_page(page_to_screen(center))` round trip.
    round_trip: PointJson,
    /// Gesture steps.
    steps: Vec<StepJson>,
}

/// Parse `WxH` or `W,H`.
fn parse_size(text: &str, option: &str) -> anyhow::Result<LoupeSize> {
    let (width, height) = text
        .split_once(['x', 'X', ','])
        .with_context(|| format!("{option} {text:?} needs WxH"))?;
    let width = width
        .trim()
        .parse::<f32>()
        .with_context(|| format!("{option} width {width:?} is not a number"))?;
    let height = height
        .trim()
        .parse::<f32>()
        .with_context(|| format!("{option} height {height:?} is not a number"))?;
    if !width.is_finite() || !height.is_finite() || width <= 0.0 || height <= 0.0 {
        bail!("{option} needs finite positive dimensions");
    }
    Ok(LoupeSize::new(width, height))
}

/// Parse `X,Y`.
fn parse_point(text: &str, option: &str) -> anyhow::Result<LoupePoint> {
    let (x, y) = text
        .split_once([',', 'x'])
        .with_context(|| format!("{option} {text:?} needs X,Y"))?;
    let x = x
        .trim()
        .parse::<f32>()
        .with_context(|| format!("{option} x {x:?} is not a number"))?;
    let y = y
        .trim()
        .parse::<f32>()
        .with_context(|| format!("{option} y {y:?} is not a number"))?;
    if !x.is_finite() || !y.is_finite() {
        bail!("{option} needs finite coordinates");
    }
    Ok(LoupePoint::new(x, y))
}

/// Parse one gesture token.
fn parse_gesture(token: &str) -> anyhow::Result<Command> {
    let normalised = token.trim().to_ascii_lowercase();
    if let Some(values) = normalised.strip_prefix("drag:") {
        let (dx, dy) = parse_pair(values, "drag")?;
        return Ok(Command::Delta(GestureDelta::Drag { dx, dy }));
    }
    if let Some(values) = normalised.strip_prefix("scroll:") {
        let (dx, dy) = parse_pair(values, "scroll")?;
        return Ok(Command::Delta(GestureDelta::Scroll { dx, dy }));
    }
    if let Some(factor) = normalised.strip_prefix("pinch:") {
        let factor = factor
            .trim()
            .parse::<f32>()
            .with_context(|| format!("pinch factor {factor:?} is not a number"))?;
        return Ok(Command::Delta(GestureDelta::Pinch { factor }));
    }
    if let Some(values) = normalised.strip_prefix("center:") {
        return Ok(Command::Center(parse_point(values, "center")?));
    }
    match normalised.as_str() {
        "zoom-in" | "zoomin" => Ok(Command::ZoomIn),
        "zoom-out" | "zoomout" => Ok(Command::ZoomOut),
        "reset" | "reset-drag" => Ok(Command::ResetDrag),
        other => bail!("unknown loupe command {other:?}"),
    }
}

/// Parse `DX,DY`.
fn parse_pair(text: &str, option: &str) -> anyhow::Result<(f32, f32)> {
    let (x, y) = text
        .split_once(',')
        .with_context(|| format!("{option} {text:?} needs DX,DY"))?;
    let x = x
        .trim()
        .parse::<f32>()
        .with_context(|| format!("{option} dx {x:?} is not a number"))?;
    let y = y
        .trim()
        .parse::<f32>()
        .with_context(|| format!("{option} dy {y:?} is not a number"))?;
    Ok((x, y))
}

/// One parsed command.
enum Command {
    /// Apply a gesture delta.
    Delta(GestureDelta),
    /// Move the page-space centre.
    Center(LoupePoint),
    /// Zoom in one step.
    ZoomIn,
    /// Zoom out one step.
    ZoomOut,
    /// Clear accumulated drag.
    ResetDrag,
}

/// Stable effect label.
fn effect_label(effect: GestureEffect) -> &'static str {
    match effect {
        GestureEffect::Ignored => "ignored",
        GestureEffect::Panned => "panned",
        GestureEffect::Zoomed => "zoomed",
    }
}

/// Convert a rect.
fn rect_json(rect: LoupeRect) -> RectJson {
    RectJson {
        x: rect.x,
        y: rect.y,
        width: rect.width,
        height: rect.height,
    }
}

/// Convert a point.
fn point_json(point: LoupePoint) -> PointJson {
    PointJson {
        x: point.x,
        y: point.y,
    }
}

/// `pdfxi loupe`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn inspect(
    input: &Path,
    page: usize,
    zoom: f32,
    lens: &str,
    shape: &str,
    viewport: Option<&str>,
    center: Option<&str>,
    commands: &str,
    json: bool,
) -> anyhow::Result<()> {
    if page == 0 {
        bail!("--page is one-based and must be at least 1");
    }
    if !zoom.is_finite() || zoom <= 0.0 {
        bail!("--zoom must be finite and positive");
    }
    let lens = parse_size(lens, "--lens")?;
    let shape = match shape.trim().to_ascii_lowercase().as_str() {
        "rectangle" | "rect" => LensShape::Rectangle,
        "circle" | "round" => LensShape::Circle,
        other => bail!("unknown lens shape {other:?}; expected rectangle or circle"),
    };
    let bytes = std::fs::read(input).with_context(|| format!("read {}", input.display()))?;
    let document = Document::from_bytes(bytes)
        .map_err(|error| anyhow::anyhow!("parse {}: {error}", input.display()))?;
    let pages = document
        .page_count()
        .map_err(|error| anyhow::anyhow!("count pages: {error}"))?;
    if page > pages {
        bail!("--page {page} is outside 1..={pages}");
    }
    let frame = document
        .page_frame(page - 1)
        .map_err(|error| anyhow::anyhow!("page {page} frame: {error}"))?;
    let page_size = LoupeSize::new(
        (frame.media[2] - frame.media[0]) as f32,
        (frame.media[3] - frame.media[1]) as f32,
    );
    let viewport = match viewport {
        Some(text) => parse_size(text, "--viewport")?,
        None => page_size,
    };
    let mut model = LoupeModel::new(page_size, viewport, shape, lens, zoom)
        .map_err(|error| anyhow::anyhow!("loupe: {error}"))?;
    if let Some(text) = center {
        model.set_center_page(parse_point(text, "--center")?);
    }
    let tokens: Vec<&str> = commands
        .split(';')
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .collect();
    let mut steps = Vec::new();
    for token in tokens {
        let (effect, command) = match parse_gesture(token)? {
            Command::Delta(delta) => (model.apply(delta), token),
            Command::Center(point) => {
                model.set_center_page(point);
                (GestureEffect::Panned, token)
            }
            Command::ZoomIn => {
                let before = model.zoom();
                model.zoom_in();
                (
                    if (model.zoom() - before).abs() > f32::EPSILON {
                        GestureEffect::Zoomed
                    } else {
                        GestureEffect::Ignored
                    },
                    token,
                )
            }
            Command::ZoomOut => {
                let before = model.zoom();
                model.zoom_out();
                (
                    if (model.zoom() - before).abs() > f32::EPSILON {
                        GestureEffect::Zoomed
                    } else {
                        GestureEffect::Ignored
                    },
                    token,
                )
            }
            Command::ResetDrag => {
                model.reset_accumulated_drag();
                (GestureEffect::Ignored, token)
            }
        };
        steps.push(StepJson {
            command: command.to_owned(),
            effect: effect_label(effect).to_owned(),
            zoom: model.zoom(),
            center_page: point_json(model.center_page()),
        });
    }
    let round_trip = model.screen_to_page(model.page_to_screen(model.center_page()));
    let report = LoupeJson {
        input: input.display().to_string(),
        page,
        page_width: page_size.width,
        page_height: page_size.height,
        viewport_width: viewport.width,
        viewport_height: viewport.height,
        shape: match shape {
            LensShape::Rectangle => "rectangle".to_owned(),
            LensShape::Circle => "circle".to_owned(),
        },
        requested_zoom: zoom,
        zoom: model.zoom(),
        lens_width: model.lens().width,
        lens_height: model.lens().height,
        center_page: point_json(model.center_page()),
        center_screen: point_json(model.center_screen()),
        dest_rect: rect_json(model.dest_rect()),
        source_rect: rect_json(model.source_rect()),
        accumulated_drag: point_json(model.accumulated_drag()),
        contains_screen_center: model.contains_screen(model.center_screen()),
        contains_page_center: model.contains_page(model.center_page()),
        round_trip: point_json(round_trip),
        steps,
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }
    println!(
        "input: {}  page: {}  page: {}x{}  viewport: {}x{}  shape: {}",
        report.input,
        report.page,
        report.page_width,
        report.page_height,
        report.viewport_width,
        report.viewport_height,
        report.shape
    );
    println!(
        "zoom: {} (requested {})  lens: {}x{}",
        report.zoom, report.requested_zoom, report.lens_width, report.lens_height
    );
    println!(
        "center-page: ({}, {})  center-screen: ({}, {})",
        report.center_page.x, report.center_page.y, report.center_screen.x, report.center_screen.y
    );
    println!(
        "dest: [{}, {}, {}, {}]  source: [{}, {}, {}, {}]",
        report.dest_rect.x,
        report.dest_rect.y,
        report.dest_rect.width,
        report.dest_rect.height,
        report.source_rect.x,
        report.source_rect.y,
        report.source_rect.width,
        report.source_rect.height
    );
    for step in &report.steps {
        println!(
            "{} -> {}  zoom {}  center ({}, {})",
            step.command, step.effect, step.zoom, step.center_page.x, step.center_page.y
        );
    }
    Ok(())
}
