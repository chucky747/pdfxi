//! `pdfxi production`: print-production planning and application.
//!
//! [`pdfxi_app::print::ProductionPlan`] resolves printer marks, bleed,
//! separations and the output-intent report into one deterministic plan. This
//! host derives the geometry from a real page, applies the resolved bleed box
//! through [`pdfxi_core::Document::set_page_boxes`], paints the requested
//! printer marks with the core API, and reparses the saved bytes before writing
//! them. No rendering or colour-management backend is involved.

use std::path::Path;

use anyhow::{Context, bail};
use pdfxi_app::print::{
    Bleed, OutputIntent, OutputIntentSubtype, PrinterMarks, ProductionOptions, ProductionPlan,
    SeparationInput, SizePt,
};
use pdfxi_core::{BoxKind, Document, PrinterMarkOptions, Rectangle};
use serde::Serialize;

/// JSON shape of a size in points.
#[derive(Debug, Clone, Copy, Serialize)]
struct SizeJson {
    /// Width in points.
    width: f32,
    /// Height in points.
    height: f32,
}

/// JSON shape of the resolved bleed rectangle.
#[derive(Debug, Clone, Copy, Serialize)]
struct BleedJson {
    /// Left edge relative to the trim box.
    x: f32,
    /// Bottom edge relative to the trim box.
    y: f32,
    /// Width including both horizontal bleeds.
    width: f32,
    /// Height including both vertical bleeds.
    height: f32,
}

/// JSON shape of `pdfxi production plan` and `apply`.
#[derive(Debug, Serialize)]
struct ProductionReport {
    /// Source PDF.
    input: String,
    /// Written file, when applying.
    out: Option<String>,
    /// Pages in the source document.
    pages: usize,
    /// Reference trim size (page 1).
    trim: SizeJson,
    /// Reference sheet size (page 1).
    sheet: SizeJson,
    /// Number of resolved printer marks.
    marks: usize,
    /// Resolved bleed rectangle.
    bleed: BleedJson,
    /// Canonical separation descriptions.
    separations: Vec<String>,
    /// Output-intent summary.
    output_intent: String,
    /// Output-intent warnings.
    warnings: Vec<String>,
    /// Plan summary.
    summary: String,
    /// Whether the written output reparsed (always true when applying).
    reparsed: bool,
}

/// Parse a `T,R,B,L` or single bleed specification.
fn parse_bleed(text: &str) -> anyhow::Result<Bleed> {
    let values: Vec<f32> = text
        .split(',')
        .map(|part| {
            part.trim()
                .parse::<f32>()
                .with_context(|| format!("bleed value {:?} is not a number", part.trim()))
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    let bleed = match values.as_slice() {
        [all] => Bleed {
            top: *all,
            right: *all,
            bottom: *all,
            left: *all,
        },
        [top, right, bottom, left] => Bleed {
            top: *top,
            right: *right,
            bottom: *bottom,
            left: *left,
        },
        _ => bail!("--bleed needs one value or four comma-separated values (T,R,B,L)"),
    };
    for value in [bleed.top, bleed.right, bleed.bottom, bleed.left] {
        if !value.is_finite() || value < 0.0 {
            bail!("--bleed values must be finite and non-negative");
        }
    }
    Ok(bleed)
}

/// Parse the printer-mark family list.
fn parse_marks(text: &str) -> anyhow::Result<PrinterMarks> {
    let mut marks = PrinterMarks::default();
    let normalised = text.trim().to_ascii_lowercase();
    if normalised == "all" {
        return Ok(PrinterMarks {
            crop_marks: true,
            registration_marks: true,
            color_bars: true,
            page_info: true,
        });
    }
    if normalised.is_empty() || normalised == "none" {
        return Ok(marks);
    }
    for name in normalised.split(',').map(str::trim) {
        match name {
            "" => {}
            "crop" | "crop-marks" => marks.crop_marks = true,
            "registration" | "registration-marks" => marks.registration_marks = true,
            "colour-bars" | "color-bars" | "colour-bar" | "color-bar" => marks.color_bars = true,
            "page-info" | "page-information" => marks.page_info = true,
            _ => bail!(
                "unknown mark family {name:?}; expected crop, registration, colour-bars, page-info, all or none"
            ),
        }
    }
    Ok(marks)
}

/// Parse `NAME:process` or `NAME:spot`.
///
/// With no explicit separation, the four process inks are planned, which is the
/// minimum a [`pdfxi_app::print::SeparationsPlan`] accepts.
fn parse_separations(values: &[String]) -> anyhow::Result<Vec<SeparationInput>> {
    if values.is_empty() {
        return Ok(["Cyan", "Magenta", "Yellow", "Black"]
            .into_iter()
            .map(|name| SeparationInput {
                name: name.to_owned(),
                process: true,
            })
            .collect());
    }
    values
        .iter()
        .map(|value| {
            let (name, kind) = value
                .rsplit_once(':')
                .with_context(|| format!("separation {value:?} needs NAME:process or NAME:spot"))?;
            let name = name.trim();
            if name.is_empty() {
                bail!("separation names must not be empty");
            }
            let process = match kind.trim().to_ascii_lowercase().as_str() {
                "process" => true,
                "spot" => false,
                other => bail!("unknown separation kind {other:?}; expected process or spot"),
            };
            Ok(SeparationInput {
                name: name.to_owned(),
                process,
            })
        })
        .collect()
}

/// Parse `SUBTYPE|NAME|REGISTRY|INTENT`.
fn parse_intent(text: Option<&str>) -> anyhow::Result<Vec<OutputIntent>> {
    let Some(text) = text else {
        return Ok(Vec::new());
    };
    let parts: Vec<&str> = text.splitn(4, '|').collect();
    let [subtype, name, registry, rendering_intent] = parts.as_slice() else {
        bail!("--intent needs SUBTYPE|NAME|REGISTRY|INTENT");
    };
    let subtype = match subtype.trim().to_ascii_lowercase().as_str() {
        "pdfx" | "gts_pdfx" | "pdf/x" => OutputIntentSubtype::PdfX,
        "pdfa" | "gts_pdfa1" | "pdf/a" => OutputIntentSubtype::PdfA,
        "icc" | "iccbased" | "icc-based" => OutputIntentSubtype::IccBased,
        other => bail!("unknown output-intent subtype {other:?}; expected pdfx, pdfa or iccbased"),
    };
    Ok(vec![OutputIntent {
        subtype,
        name: (*name).to_owned(),
        registry: (*registry).to_owned(),
        rendering_intent: (*rendering_intent).to_owned(),
    }])
}

/// Build the CLI options.
fn options(
    bleed: &str,
    marks: &str,
    separations: &[String],
    intent: Option<&str>,
) -> anyhow::Result<ProductionOptions> {
    Ok(ProductionOptions {
        marks: parse_marks(marks)?,
        bleed: parse_bleed(bleed)?,
        separations: parse_separations(separations)?,
        intents: parse_intent(intent)?,
    })
}

/// Reference page geometry: trim and sheet sizes.
fn page_sizes(document: &Document, index: usize) -> anyhow::Result<(SizePt, SizePt)> {
    let boxes = document
        .page_boxes(index)
        .map_err(|error| anyhow::anyhow!("page {} boxes: {error}", index + 1))?;
    let trim = boxes.rectangle(BoxKind::Trim);
    let media = boxes.rectangle(BoxKind::Media);
    let trim_size = SizePt::new(trim.width() as f32, trim.height() as f32)
        .map_err(|error| anyhow::anyhow!("trim size: {error}"))?;
    let sheet_size = SizePt::new(media.width() as f32, media.height() as f32)
        .map_err(|error| anyhow::anyhow!("sheet size: {error}"))?;
    Ok((trim_size, sheet_size))
}

/// Build the plan and its stable report for the reference page.
fn build(
    input: &Path,
    document: &Document,
    out: Option<&Path>,
    options: &ProductionOptions,
) -> anyhow::Result<(ProductionPlan, ProductionReport)> {
    let pages = document
        .page_count()
        .map_err(|error| anyhow::anyhow!("count pages: {error}"))?;
    if pages == 0 {
        bail!("cannot plan print production for a document with no pages");
    }
    let (trim, sheet) = page_sizes(document, 0)?;
    let plan = ProductionPlan::build(trim, sheet, options)
        .map_err(|error| anyhow::anyhow!("production plan: {error}"))?;
    let separations = plan
        .separations
        .separations
        .iter()
        .map(|separation| {
            format!(
                "{}({})",
                separation.name,
                if separation.process {
                    "process"
                } else {
                    "spot"
                }
            )
        })
        .collect();
    let report = ProductionReport {
        input: input.display().to_string(),
        out: out.map(|path| path.display().to_string()),
        pages,
        trim: SizeJson {
            width: trim.width,
            height: trim.height,
        },
        sheet: SizeJson {
            width: sheet.width,
            height: sheet.height,
        },
        marks: plan.marks.marks.len(),
        bleed: BleedJson {
            x: plan.bleed.x + 0.0,
            y: plan.bleed.y + 0.0,
            width: plan.bleed.width,
            height: plan.bleed.height,
        },
        separations,
        output_intent: plan.output_intent.summary.clone(),
        warnings: plan.output_intent.warnings.clone(),
        summary: plan.summary.clone(),
        reparsed: false,
    };
    Ok((plan, report))
}

/// Print a report as JSON or text.
fn print_report(report: &ProductionReport, json: bool) -> anyhow::Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(report)?);
        return Ok(());
    }
    println!(
        "input: {}  pages: {}  trim: {}x{}  sheet: {}x{}",
        report.input,
        report.pages,
        report.trim.width,
        report.trim.height,
        report.sheet.width,
        report.sheet.height
    );
    println!(
        "marks: {}  bleed: [{}, {}, {}, {}]",
        report.marks, report.bleed.x, report.bleed.y, report.bleed.width, report.bleed.height
    );
    println!(
        "separations: {}",
        if report.separations.is_empty() {
            "none".to_owned()
        } else {
            report.separations.join(", ")
        }
    );
    println!("output-intent: {}", report.output_intent);
    if let Some(out) = &report.out {
        println!("out: {out}  reparsed: {}", report.reparsed);
    }
    println!("summary: {}", report.summary);
    Ok(())
}

/// `pdfxi production plan`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn plan(
    input: &Path,
    bleed: &str,
    marks: &str,
    separations: &[String],
    intent: Option<&str>,
    json: bool,
) -> anyhow::Result<()> {
    let options = options(bleed, marks, separations, intent)?;
    let bytes = std::fs::read(input).with_context(|| format!("read {}", input.display()))?;
    let document = Document::from_bytes(bytes)
        .map_err(|error| anyhow::anyhow!("parse {}: {error}", input.display()))?;
    let (_, report) = build(input, &document, None, &options)?;
    print_report(&report, json)
}

/// `pdfxi production apply`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn apply(
    input: &Path,
    out: &Path,
    bleed: &str,
    marks: &str,
    separations: &[String],
    intent: Option<&str>,
    json: bool,
) -> anyhow::Result<()> {
    let options = options(bleed, marks, separations, intent)?;
    let bytes = std::fs::read(input).with_context(|| format!("read {}", input.display()))?;
    let mut document = Document::from_bytes(bytes)
        .map_err(|error| anyhow::anyhow!("parse {}: {error}", input.display()))?;
    let (_, mut report) = build(input, &document, Some(out), &options)?;
    let pages = report.pages;
    let mark_options = PrinterMarkOptions {
        crop_marks: options.marks.crop_marks,
        registration_marks: options.marks.registration_marks,
        color_bars: options.marks.color_bars,
        page_info: options.marks.page_info,
        ..PrinterMarkOptions::default()
    };
    let any_bleed = options.bleed.top > 0.0
        || options.bleed.right > 0.0
        || options.bleed.bottom > 0.0
        || options.bleed.left > 0.0;
    for index in 0..pages {
        // Resolve the plan for this page so a mixed-size document is handled
        // per page rather than with page 1's geometry.
        let (trim, sheet) = page_sizes(&document, index)?;
        let page_plan = ProductionPlan::build(trim, sheet, &options)
            .map_err(|error| anyhow::anyhow!("production plan for page {}: {error}", index + 1))?;
        if any_bleed {
            let trim_box = document
                .page_boxes(index)
                .map_err(|error| anyhow::anyhow!("page {} boxes: {error}", index + 1))?
                .rectangle(BoxKind::Trim);
            let bounds = page_plan.bleed;
            let bleed_box = Rectangle::new(
                trim_box.left + f64::from(bounds.x),
                trim_box.bottom + f64::from(bounds.y),
                trim_box.left + f64::from(bounds.x + bounds.width),
                trim_box.bottom + f64::from(bounds.y + bounds.height),
            );
            let boxes = document
                .page_boxes(index)
                .map_err(|error| anyhow::anyhow!("page {} boxes: {error}", index + 1))?
                .with_bleed(bleed_box)
                .map_err(|error| anyhow::anyhow!("page {} bleed box: {error}", index + 1))?;
            document
                .set_page_boxes(index, &boxes)
                .map_err(|error| anyhow::anyhow!("page {} bleed box: {error}", index + 1))?;
        }
        document
            .apply_printer_marks(index, &mark_options)
            .map_err(|error| anyhow::anyhow!("page {} marks: {error}", index + 1))?;
    }
    let saved = document
        .save()
        .map_err(|error| anyhow::anyhow!("save production output: {error}"))?;
    let reparsed = Document::from_bytes(saved.clone())
        .map_err(|error| anyhow::anyhow!("production output did not reparse: {error}"))?;
    let reparsed_pages = reparsed
        .page_count()
        .map_err(|error| anyhow::anyhow!("count output pages: {error}"))?;
    if reparsed_pages != pages {
        bail!("production output has {reparsed_pages} page(s), source had {pages}");
    }
    std::fs::write(out, &saved).with_context(|| format!("write {}", out.display()))?;
    report.reparsed = true;
    print_report(&report, json)
}
