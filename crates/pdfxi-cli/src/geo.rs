//! `pdfxi geo`: read geospatial viewports and map PDF/geo coordinates.
//!
//! [`pdfxi_render::geospatial`] models `/VP` viewports with `/GPTS` bounds, a
//! `/Measure` scale and a `/GCS` coordinate system, and maps PDF space to
//! longitude/latitude affinely when the projection is geographic. This host
//! reads those dictionaries from a real page and exposes the mapping. It does
//! not render a map, reproject imagery or modify the document.

use std::path::Path;

use anyhow::{Context, bail};
use pdfxi_core::{Dictionary, Document, Object};
use pdfxi_render::geospatial::{
    GeospatialDictionary, GeospatialDocument, Projection, Unit, ViewportInput,
};
use serde::Serialize;

/// One viewport row.
#[derive(Serialize)]
struct ViewportRow {
    /// PDF rectangle `[x0, y0, x1, y1]`.
    pdf: [f64; 4],
    /// Geographic bounds `[lon0, lat0, lon1, lat1]`.
    gpts: [f64; 4],
    /// Measure subtype.
    subtype: String,
    /// Horizontal step.
    x_step: f64,
    /// Vertical step.
    y_step: f64,
    /// Measure unit code.
    unit: String,
}

/// JSON shape of `pdfxi geo info`.
#[derive(Serialize)]
struct GeoReport {
    /// Source PDF.
    file: String,
    /// One-based page.
    page: usize,
    /// Coordinate-system name.
    gcs: String,
    /// EPSG code, when declared.
    #[serde(skip_serializing_if = "Option::is_none")]
    epsg: Option<u32>,
    /// Projection code.
    projection: String,
    /// Whether this model can map the document.
    supported: bool,
    /// Declared viewports.
    viewports: Vec<ViewportRow>,
    /// `[x0, y0, x1, y1]` local points, when declared.
    #[serde(skip_serializing_if = "Option::is_none")]
    lpts: Option<[f64; 4]>,
    /// Stable plan descriptor.
    plan: String,
}

/// JSON shape of a coordinate mapping.
#[derive(Serialize)]
struct MappingReport {
    /// Source PDF.
    file: String,
    /// One-based page.
    page: usize,
    /// Zero-based viewport index.
    viewport: usize,
    /// Input coordinates.
    input: [f64; 2],
    /// Output coordinates.
    output: [f64; 2],
    /// Mapping direction.
    direction: &'static str,
}

/// Resolve one reference chain to an owned object.
fn resolve(document: &Document, object: &Object) -> anyhow::Result<Object> {
    let mut current = object.clone();
    for _ in 0..32 {
        match current {
            Object::Reference(id) => {
                current = document
                    .object(id)
                    .with_context(|| format!("read object {} {}", id.number, id.generation))?;
            }
            other => return Ok(other),
        }
    }
    bail!("reference chain exceeds 32 objects")
}

/// Read the page dictionary at `index`.
fn page_dictionary(document: &Document, index: usize) -> anyhow::Result<Dictionary> {
    let page = document
        .page(index)
        .map_err(|error| anyhow::anyhow!("read page {}: {error}", index + 1))?;
    match document
        .object(page.id)
        .map_err(|error| anyhow::anyhow!("read page object: {error}"))?
    {
        Object::Dictionary(dict) => Ok(dict),
        other => bail!("page object is not a dictionary: {other:?}"),
    }
}

/// Resolve a dictionary entry.
fn entry(
    document: &Document,
    dictionary: &Dictionary,
    key: &str,
) -> anyhow::Result<Option<Object>> {
    match dictionary.get(key) {
        Some(value) => Ok(Some(resolve(document, value)?)),
        None => Ok(None),
    }
}

/// Read a `/Units` value (string or name).
fn units_text(document: &Document, dictionary: &Dictionary) -> anyhow::Result<String> {
    match entry(document, dictionary, "Units")? {
        Some(Object::String(bytes)) => Ok(String::from_utf8_lossy(&bytes).into_owned()),
        Some(Object::Name(name)) => Ok(name),
        _ => Ok("m".to_owned()),
    }
}

/// Read a number array of exactly `N` finite values.
fn number_array<const N: usize>(
    document: &Document,
    dictionary: &Dictionary,
    key: &str,
) -> anyhow::Result<Option<[f64; N]>> {
    let Some(value) = entry(document, dictionary, key)? else {
        return Ok(None);
    };
    let Object::Array(items) = value else {
        bail!("{key} is not an array");
    };
    if items.len() < N {
        bail!(
            "{key} must contain at least {N} numbers, found {}",
            items.len()
        );
    }
    let mut out = [0.0; N];
    for (slot, item) in out.iter_mut().zip(items) {
        let number = item
            .as_f64()
            .with_context(|| format!("{key} contains a non-numeric value"))?;
        if !number.is_finite() {
            bail!("{key} contains a non-finite number");
        }
        *slot = number;
    }
    Ok(Some(out))
}

/// Read the `/GCS` dictionary name, type and EPSG code.
fn gcs(document: &Document, measure: &Dictionary) -> anyhow::Result<(String, Option<u32>)> {
    let Some(Object::Dictionary(dict)) = entry(document, measure, "GCS")? else {
        return Ok(("geographic".to_owned(), None));
    };
    let epsg = match dict.get("EPSG") {
        Some(Object::Integer(value)) if (0..=u32::MAX as i64).contains(value) => {
            Some(*value as u32)
        }
        _ => None,
    };
    let kind = match dict.get("Type") {
        Some(Object::Name(name)) => Some(name.clone()),
        _ => None,
    };
    let name = match (&kind, epsg) {
        (_, Some(code)) => format!("EPSG:{code}"),
        (Some(kind), None) if kind.eq_ignore_ascii_case("GEOGCS") => "geographic".to_owned(),
        (Some(kind), None) => kind.clone(),
        (None, None) => "geographic".to_owned(),
    };
    Ok((name, epsg))
}

/// Build the geospatial model for one page.
fn load(file: &Path, page: Option<usize>) -> anyhow::Result<(Document, usize, GeospatialDocument)> {
    let bytes = std::fs::read(file).with_context(|| format!("read {}", file.display()))?;
    let document = Document::from_bytes(bytes)
        .map_err(|error| anyhow::anyhow!("parse {}: {error}", file.display()))?;
    let pages = document
        .page_count()
        .map_err(|error| anyhow::anyhow!("count pages: {error}"))?;
    let index = page.unwrap_or(1);
    if index == 0 || index > pages {
        bail!("--page {index} is outside 1..={pages}");
    }
    let dict = page_dictionary(&document, index - 1)?;
    let Some(viewport_value) = entry(&document, &dict, "VP")? else {
        bail!("page {index} declares no /VP viewports");
    };
    let Object::Array(viewport_entries) = viewport_value else {
        bail!("page {index} /VP is not an array");
    };
    let media = document
        .page(index - 1)
        .map_err(|error| anyhow::anyhow!("read page {index}: {error}"))?
        .media_box;
    let default_rect = [media.left, media.bottom, media.right, media.top];
    let mut viewports = Vec::with_capacity(viewport_entries.len());
    let mut gcs_name = String::from("geographic");
    let mut epsg = None;
    let mut lpts = None;
    for viewport in &viewport_entries {
        let Object::Dictionary(viewport) = resolve(&document, viewport)? else {
            bail!("a /VP entry is not a dictionary");
        };
        let rect = number_array::<4>(&document, &viewport, "BBox")?
            .or(number_array::<4>(&document, &viewport, "Rect")?)
            .unwrap_or(default_rect);
        let Some(Object::Dictionary(measure)) = entry(&document, &viewport, "Measure")? else {
            bail!("a /VP entry has no /Measure dictionary");
        };
        let subtype = match measure.get("Subtype") {
            Some(Object::Name(name)) => name.clone(),
            _ => "GEO".to_owned(),
        };
        let gpts = number_array::<4>(&document, &measure, "GPTS")?
            .context("a /Measure dictionary needs four /GPTS numbers")?;
        let x_step = match measure.get("XStep") {
            Some(value) => value.as_f64().unwrap_or(1.0),
            None => 1.0,
        };
        let y_step = match measure.get("YStep") {
            Some(value) => value.as_f64().unwrap_or(1.0),
            None => 1.0,
        };
        let unit = units_text(&document, &measure)?;
        let (name, code) = gcs(&document, &measure)?;
        gcs_name = name;
        epsg = code;
        if let Some(values) = number_array::<4>(&document, &measure, "LPTS")? {
            lpts = Some(values);
        }
        viewports.push(ViewportInput {
            pdf_rect: rect,
            gpts,
            measure_subtype: subtype,
            x_step,
            y_step,
            measure_unit: unit,
        });
    }
    let model = GeospatialDocument::parse(GeospatialDictionary {
        viewports,
        gcs_name,
        gcs_epsg: epsg,
        lpts,
    })
    .map_err(|error| anyhow::anyhow!("geospatial dictionaries: {error}"))?;
    Ok((document, index, model))
}

/// Label a projection.
fn projection_label(projection: &Projection) -> String {
    projection.code()
}

/// `pdfxi geo info`.
pub(crate) fn info(file: &Path, page: Option<usize>, json: bool) -> anyhow::Result<()> {
    let (_, index, model) = load(file, page)?;
    let plan = model.resolve_plan();
    let report = GeoReport {
        file: file.display().to_string(),
        page: index,
        gcs: model.gcs().name().to_owned(),
        epsg: model.gcs().epsg(),
        projection: projection_label(model.gcs().projection()),
        supported: plan.supported(),
        viewports: model
            .viewports()
            .iter()
            .map(|viewport| ViewportRow {
                pdf: [
                    viewport.pdf().x0(),
                    viewport.pdf().y0(),
                    viewport.pdf().x1(),
                    viewport.pdf().y1(),
                ],
                gpts: [
                    viewport.gpts().lon0(),
                    viewport.gpts().lat0(),
                    viewport.gpts().lon1(),
                    viewport.gpts().lat1(),
                ],
                subtype: match viewport.measure().subtype() {
                    pdfxi_render::geospatial::MeasureSubtype::Linear => "linear".to_owned(),
                    pdfxi_render::geospatial::MeasureSubtype::Angular => "angular".to_owned(),
                },
                x_step: viewport.measure().x_step(),
                y_step: viewport.measure().y_step(),
                unit: unit_label(viewport.measure().unit()),
            })
            .collect(),
        lpts: model
            .lpts()
            .map(|lpts| [lpts.x0(), lpts.y0(), lpts.x1(), lpts.y1()]),
        plan: plan.descriptor(),
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }
    println!(
        "file: {}  page: {}  gcs: {}  epsg: {}  projection: {}",
        report.file,
        report.page,
        report.gcs,
        report
            .epsg
            .map_or_else(|| "-".to_owned(), |code| code.to_string()),
        report.projection
    );
    println!(
        "viewports: {}  supported: {}  plan: {}",
        report.viewports.len(),
        report.supported,
        report.plan
    );
    for (index, row) in report.viewports.iter().enumerate() {
        println!(
            "  {index}: pdf {:?}  geo lon {:.6}..{:.6} lat {:.6}..{:.6}  {} {:?} steps {}/{}",
            row.pdf,
            row.gpts[0].min(row.gpts[2]),
            row.gpts[0].max(row.gpts[2]),
            row.gpts[1].min(row.gpts[3]),
            row.gpts[1].max(row.gpts[3]),
            row.subtype,
            row.unit,
            row.x_step,
            row.y_step
        );
    }
    if let Some(lpts) = report.lpts {
        println!("lpts: {lpts:?}");
    }
    Ok(())
}

/// Label a unit by its stable code.
fn unit_label(unit: &Unit) -> String {
    unit.code()
}

/// `pdfxi geo to-geo`.
pub(crate) fn to_geo(
    file: &Path,
    page: Option<usize>,
    x: f64,
    y: f64,
    viewport: Option<usize>,
    json: bool,
) -> anyhow::Result<()> {
    let (_, index, model) = load(file, page)?;
    let viewport = viewport.unwrap_or(0);
    let (lon, lat) = model
        .pdf_to_geo(viewport, x, y)
        .map_err(|error| anyhow::anyhow!("map ({x}, {y}) to geo: {error}"))?;
    let report = MappingReport {
        file: file.display().to_string(),
        page: index,
        viewport,
        input: [x, y],
        output: [lon, lat],
        direction: "pdf-to-geo",
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }
    println!(
        "pdf ({x}, {y}) on page {} viewport {viewport} -> lon {lon:.9}, lat {lat:.9}",
        report.page
    );
    Ok(())
}

/// `pdfxi geo to-pdf`.
pub(crate) fn to_pdf(
    file: &Path,
    page: Option<usize>,
    lon: f64,
    lat: f64,
    viewport: Option<usize>,
    json: bool,
) -> anyhow::Result<()> {
    let (_, index, model) = load(file, page)?;
    let viewport = match viewport {
        Some(index) => index,
        None => model
            .viewport_for_geo(lon, lat)
            .map_err(|error| anyhow::anyhow!("locate ({lon}, {lat}): {error}"))?,
    };
    let (x, y) = model
        .geo_to_pdf(viewport, lon, lat)
        .map_err(|error| anyhow::anyhow!("map ({lon}, {lat}) to PDF: {error}"))?;
    let report = MappingReport {
        file: file.display().to_string(),
        page: index,
        viewport,
        input: [lon, lat],
        output: [x, y],
        direction: "geo-to-pdf",
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }
    println!(
        "lon {lon:.9}, lat {lat:.9} -> PDF ({x:.3}, {y:.3}) on page {} viewport {viewport}",
        report.page
    );
    Ok(())
}
