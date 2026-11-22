//! `pdfxi prepress plan`: trapping presets and transparency-flattening policy.
//!
//! [`pdfxi_render::trapping`] models the inputs to a print-production prepress
//! stage: a validated [`TrappingPreset`](pdfxi_render::trapping::TrappingPreset), a [`FlattenPolicy`](pdfxi_render::trapping::FlattenPolicy), and a
//! deterministic [`TrapPlan`](pdfxi_render::trapping::TrapPlan). This host reads a real document for its page
//! geometry, resolves the plan, and reports which requested features require a
//! native prepress backend. Painting traps and rasterizing transparency are
//! explicitly the backend's responsibility.

use std::path::Path;

use anyhow::{Context, bail};
use pdfxi_core::Document;
use pdfxi_render::trapping::{
    FlattenOverprint, FlattenPolicy, TrapColor, TrapJoinStyle, TrapPlan, TrapSliding,
    TrappingPreset, VectorRasterBalance,
};
use serde::Serialize;

/// JSON shape of `pdfxi prepress plan`.
#[derive(Debug, Serialize)]
struct PrepressReport {
    /// Source PDF.
    input: String,
    /// Pages in the document.
    pages: usize,
    /// First-page width in points.
    page_width: f64,
    /// First-page height in points.
    page_height: f64,
    /// Preset name.
    preset: String,
    /// Default trap width.
    trap_width_pt: f32,
    /// Black trap width.
    black_width_pt: f32,
    /// Black trap colour.
    black_color: String,
    /// Trap join style.
    join: String,
    /// Sliding-trap policy.
    sliding: String,
    /// Flattening resolution.
    flatten_dpi: u32,
    /// Vector/raster balance.
    balance: String,
    /// Overprint policy.
    overprint: String,
    /// Maximum flattening depth.
    max_depth: u8,
    /// Stable categorical encoding.
    policy_bits: u32,
    /// Whether a native prepress backend is required.
    needs_backend_prepress: bool,
    /// Backend requirements.
    warnings: Vec<String>,
}

/// Parse the black trap colour.
fn parse_black_color(text: &str) -> anyhow::Result<TrapColor> {
    let normalised = text.trim().to_ascii_lowercase();
    match normalised.as_str() {
        "auto" => return Ok(TrapColor::Auto),
        "knockout" => return Ok(TrapColor::Knockout),
        _ => {}
    }
    let values: Vec<f32> = normalised
        .split(',')
        .map(|part| {
            part.trim()
                .parse::<f32>()
                .with_context(|| format!("colour component {:?} is not a number", part.trim()))
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    match values.as_slice() {
        [c, m, y, k] => Ok(TrapColor::CustomCmyk {
            c: *c,
            m: *m,
            y: *y,
            k: *k,
        }),
        _ => bail!("--black-color needs auto, knockout or four CMYK components (0..=1)"),
    }
}

/// Parse the sliding-trap policy.
fn parse_sliding(text: &str) -> anyhow::Result<TrapSliding> {
    let normalised = text.trim().to_ascii_lowercase();
    match normalised.as_str() {
        "none" | "off" | "disabled" => Ok(TrapSliding::Disabled),
        "auto" | "automatic" => Ok(TrapSliding::Automatic),
        _ => {
            let max_slide_pt = normalised
                .parse::<f32>()
                .with_context(|| format!("--slide {text:?} needs none, auto or a point value"))?;
            Ok(TrapSliding::Limited { max_slide_pt })
        }
    }
}

/// Describe the black trap colour.
fn describe_color(color: TrapColor) -> String {
    match color {
        TrapColor::Auto => "auto".to_owned(),
        TrapColor::Knockout => "knockout".to_owned(),
        TrapColor::CustomCmyk { c, m, y, k } => format!("cmyk({c},{m},{y},{k})"),
    }
}

/// Describe the sliding policy.
fn describe_sliding(sliding: TrapSliding) -> String {
    match sliding {
        TrapSliding::Disabled => "none".to_owned(),
        TrapSliding::Automatic => "auto".to_owned(),
        TrapSliding::Limited { max_slide_pt } => format!("limited({max_slide_pt})"),
    }
}

/// `pdfxi prepress plan`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn plan(
    input: &Path,
    preset_name: &str,
    trap_width: f32,
    black_width: f32,
    black_color: &str,
    join: &str,
    sliding: &str,
    dpi: u32,
    balance: &str,
    overprint: &str,
    max_depth: u8,
    json: bool,
) -> anyhow::Result<()> {
    let bytes = std::fs::read(input).with_context(|| format!("read {}", input.display()))?;
    let document = Document::from_bytes(bytes)
        .map_err(|error| anyhow::anyhow!("parse {}: {error}", input.display()))?;
    let pages = document
        .page_count()
        .map_err(|error| anyhow::anyhow!("count pages: {error}"))?;
    if pages == 0 {
        bail!("cannot plan prepress for a document with no pages");
    }
    let frame = document
        .page_frame(0)
        .map_err(|error| anyhow::anyhow!("page 1 frame: {error}"))?;
    let preset = TrappingPreset::new(
        preset_name,
        trap_width,
        black_width,
        parse_black_color(black_color)?,
        TrapJoinStyle::parse(join)
            .map_err(|_| anyhow::anyhow!("--join must be miter, round or bevel"))?,
        parse_sliding(sliding)?,
    )
    .map_err(|error| anyhow::anyhow!("trapping preset: {error}"))?;
    let policy = FlattenPolicy::with_max_depth(
        dpi,
        VectorRasterBalance::parse(balance)
            .map_err(|_| anyhow::anyhow!("--balance must be vector, balanced or raster"))?,
        FlattenOverprint::parse(overprint)
            .map_err(|_| anyhow::anyhow!("--overprint must be preserve, simulate or knockout"))?,
        max_depth,
    )
    .map_err(|error| anyhow::anyhow!("flattening policy: {error}"))?;
    let plan =
        TrapPlan::build(&preset, &policy).map_err(|error| anyhow::anyhow!("trap plan: {error}"))?;
    let report = PrepressReport {
        input: input.display().to_string(),
        pages,
        page_width: frame.media[2] - frame.media[0],
        page_height: frame.media[3] - frame.media[1],
        preset: plan.preset_name.clone(),
        trap_width_pt: plan.trap_width_pt,
        black_width_pt: plan.black_width_pt,
        black_color: describe_color(plan.black_color),
        join: plan.join_style.name().to_owned(),
        sliding: describe_sliding(plan.sliding),
        flatten_dpi: plan.flatten_resolution_dpi,
        balance: plan.vector_raster_balance.name().to_owned(),
        overprint: plan.flatten_overprint.name().to_owned(),
        max_depth: plan.flatten_max_depth,
        policy_bits: plan.policy_bits,
        needs_backend_prepress: plan.needs_backend_prepress,
        warnings: plan.warnings.clone(),
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }
    println!(
        "input: {}  pages: {}  page: {}x{} pt",
        report.input, report.pages, report.page_width, report.page_height
    );
    println!(
        "preset: {}  trap: {} pt  black: {} pt {}  join: {}  sliding: {}",
        report.preset,
        report.trap_width_pt,
        report.black_width_pt,
        report.black_color,
        report.join,
        report.sliding
    );
    println!(
        "flatten: {} dpi  balance: {}  overprint: {}  depth: {}  bits: {}",
        report.flatten_dpi, report.balance, report.overprint, report.max_depth, report.policy_bits
    );
    println!(
        "needs-backend-prepress: {}  warnings: {}",
        report.needs_backend_prepress,
        report.warnings.len()
    );
    for warning in &report.warnings {
        println!("warning: {warning}");
    }
    Ok(())
}
