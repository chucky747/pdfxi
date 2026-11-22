//! `pdfxi annotate`: add and list drawing annotations.
//!
//! The document-level writers live in `pdfxi-edit::document_annotations`; this
//! module is the CLI host. `add` stages one annotation, saves a standalone
//! document and reports its identifier; `list` prints the annotations already
//! on a page, which is how a script confirms what was written.

use std::path::Path;

use anyhow::{Context, bail};
use pdfxi_core::Document;
use pdfxi_edit::{Point, Rect, add_callout, add_cloud, add_free_text, list_annotations};
use serde::Serialize;

/// Which drawing annotation `annotate add` creates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub(crate) enum AnnotationKind {
    /// Free text in a box (`/Subtype /FreeText`).
    FreeText,
    /// Free text plus a leader line to `--anchor` (`/IT /FreeTextCallout`).
    Callout,
    /// A scalloped cloud border (`/Subtype /Polygon`, `/IT /PolygonCloud`).
    Cloud,
}

/// One annotation row in the JSON report.
#[derive(Serialize)]
struct AnnotationRow {
    /// `/NM` identifier.
    id: String,
    /// PDF subtype name.
    subtype: String,
    /// `/Contents` text.
    contents: String,
    /// `/T` author.
    author: String,
    /// `/Rect` in points.
    rect: [f32; 4],
    /// `/M` date, when present.
    date: Option<String>,
}

/// JSON shape of `pdfxi annotate` output.
#[derive(Serialize)]
struct AnnotateReport {
    /// Input file.
    file: String,
    /// Output file for `add`; omitted for `list`.
    #[serde(skip_serializing_if = "Option::is_none")]
    out: Option<String>,
    /// One-based page the annotation belongs to.
    page: usize,
    /// Annotations in page order.
    annotations: Vec<AnnotationRow>,
}

fn load(file: &Path) -> anyhow::Result<Document> {
    let bytes = std::fs::read(file).with_context(|| format!("read {}", file.display()))?;
    Document::from_bytes(bytes)
        .map_err(|error| anyhow::anyhow!("parse {}: {error}", file.display()))
}

/// Parse `x0,y0,x1,y1` into a rectangle.
fn parse_rect(text: &str) -> anyhow::Result<Rect> {
    let values: Vec<f32> = text
        .split(',')
        .map(|part| {
            part.trim()
                .parse::<f32>()
                .with_context(|| format!("rect value {:?} is not a number", part.trim()))
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    if values.len() != 4 {
        bail!("--rect needs four comma-separated points (x0,y0,x1,y1)");
    }
    let rect = Rect::new(values[0], values[1], values[2], values[3])
        .with_context(|| format!("invalid rectangle {text:?}"))?;
    if rect.is_empty() {
        bail!("--rect must have positive width and height");
    }
    Ok(rect)
}

/// Parse `x,y` into a point.
fn parse_point(text: &str) -> anyhow::Result<Point> {
    let values: Vec<f32> = text
        .split(',')
        .map(|part| {
            part.trim()
                .parse::<f32>()
                .with_context(|| format!("point value {:?} is not a number", part.trim()))
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    if values.len() != 2 {
        bail!("--anchor needs two comma-separated numbers (x,y)");
    }
    if !values[0].is_finite() || !values[1].is_finite() {
        bail!("--anchor must be a finite point");
    }
    Ok(Point::new(values[0], values[1]))
}

/// Add one annotation and save the document.
#[allow(clippy::too_many_arguments)]
pub(crate) fn run_add(
    file: &Path,
    out: &Path,
    page: usize,
    kind: AnnotationKind,
    rect: &str,
    text: Option<&str>,
    anchor: Option<&str>,
    font_size: f32,
    author: &str,
    json: bool,
) -> anyhow::Result<()> {
    if page == 0 {
        bail!("--page is one-based and must be at least 1");
    }
    let rect = parse_rect(rect)?;
    let mut document = load(file)?;
    let index = page - 1;
    let identifier = match kind {
        AnnotationKind::FreeText => {
            let text = text.context("--text is required for a free-text annotation")?;
            add_free_text(&mut document, index, rect, text, font_size, author)?
        }
        AnnotationKind::Callout => {
            let text = text.context("--text is required for a callout")?;
            let anchor = parse_point(anchor.context("--anchor is required for a callout")?)?;
            add_callout(&mut document, index, rect, anchor, text, font_size, author)?
        }
        AnnotationKind::Cloud => add_cloud(&mut document, index, rect, author)?,
    };

    let bytes = document.save().context("save the annotated document")?;
    std::fs::write(out, &bytes).with_context(|| format!("write {}", out.display()))?;

    let reopened = Document::from_bytes(bytes).context("reopen the annotated document")?;
    let annotations = rows(&reopened, index)?;
    let report = AnnotateReport {
        file: file.display().to_string(),
        out: Some(out.display().to_string()),
        page,
        annotations,
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!("file: {}", report.file);
        if let Some(out) = &report.out {
            println!("out:  {out}");
        }
        println!("page: {}", report.page);
        println!("added: {}", identifier.id);
        println!("annotations: {}", report.annotations.len());
    }
    Ok(())
}

/// List the annotations on one page.
pub(crate) fn run_list(file: &Path, page: usize, json: bool) -> anyhow::Result<()> {
    if page == 0 {
        bail!("--page is one-based and must be at least 1");
    }
    let document = load(file)?;
    let annotations = rows(&document, page - 1)?;
    let report = AnnotateReport {
        file: file.display().to_string(),
        out: None,
        page,
        annotations,
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        for row in &report.annotations {
            println!(
                "{}  {}  \"{}\"  by {}  rect: {} {} {} {}",
                row.id,
                row.subtype,
                row.contents,
                row.author,
                row.rect[0],
                row.rect[1],
                row.rect[2],
                row.rect[3]
            );
        }
        println!("annotations: {}", report.annotations.len());
    }
    Ok(())
}

/// Collect the annotation rows of one page.
fn rows(document: &Document, page_index: usize) -> anyhow::Result<Vec<AnnotationRow>> {
    let views = list_annotations(document, page_index)
        .with_context(|| format!("list annotations on page {}", page_index + 1))?;
    Ok(views
        .into_iter()
        .map(|view| AnnotationRow {
            id: view.id,
            subtype: view.subtype.as_pdf_name().to_owned(),
            contents: view.contents,
            author: view.author,
            rect: [
                view.rect.x0(),
                view.rect.y0(),
                view.rect.x1(),
                view.rect.y1(),
            ],
            date: view.date,
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::{parse_point, parse_rect};

    #[test]
    fn rectangles_and_points_parse_and_validate() {
        let rect = parse_rect("72, 600, 320, 660").expect("rect");
        assert_eq!(
            (rect.x0(), rect.y0(), rect.x1(), rect.y1()),
            (72.0, 600.0, 320.0, 660.0)
        );
        let point = parse_point("60,470").expect("point");
        assert_eq!((point.x, point.y), (60.0, 470.0));

        assert!(parse_rect("1,2,3").is_err(), "three values is not a rect");
        assert!(parse_rect("1,2,3,4,5").is_err());
        assert!(parse_rect("a,b,c,d").is_err());
        assert!(parse_rect("10,10,10,20").is_err(), "degenerate rect");
        assert!(parse_point("1").is_err());
        assert!(parse_point("NaN,2").is_err());
    }
}
