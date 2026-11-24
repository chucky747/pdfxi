//! `pdfxi create scan`: assemble real scanned images into a PDF.
//!
//! [`pdfxi_convert::scan_capture`] validates caller-supplied scan geometry,
//! orders simplex/duplex sheets, and reuses the print-capture assembly plan, but
//! carries no pixel buffers. This host reads real PNG files, describes each
//! page, verifies the plan, embeds each raster with `create_from_image`, and
//! merges the pages in plan order. `--device` acquires those PNG pages from an
//! attached WIA scanner through the bundled `scripts/wia-scan.ps1` sidecar;
//! `--scan` remains the path for pages scanned another way. A host with no
//! scanner reports that as a typed error, not an empty document.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, bail};
use pdfxi_convert::{
    CreateImageFormat, CreateOptions, DuplexMode, Margins, PageSize, ScanOptions, ScanSide,
    ScanSource, build_scan_plan, create_from_image,
};
use pdfxi_core::{Document, PagePlan};
use serde::{Deserialize, Serialize};

/// Maximum image file size read into memory for one scan page.
const MAX_SCAN_BYTES: u64 = 1_073_741_824;

/// The WIA acquisition sidecar embedded at build time.
const WIA_SCAN_SCRIPT: &str = include_str!("../../../scripts/wia-scan.ps1");

/// The sidecar's JSON report.
#[derive(Debug, Deserialize)]
struct WiaReport {
    /// Device name reported by WIA.
    #[allow(dead_code)]
    device: String,
    /// Acquired PNG paths in scan order.
    pages: Vec<PathBuf>,
}

/// One scanned page row.
#[derive(Serialize)]
struct ScanRow {
    /// One-based sheet ordinal.
    sheet: usize,
    /// `front` or `back`.
    side: &'static str,
    /// Plan label.
    label: String,
    /// Source file.
    file: String,
    /// `png`.
    format: &'static str,
    /// Raster width in pixels.
    width_px: u32,
    /// Raster height in pixels.
    height_px: u32,
    /// Page width in points.
    page_width: f64,
    /// Page height in points.
    page_height: f64,
}

/// JSON shape of `pdfxi create scan`.
#[derive(Serialize)]
struct ScanPdfReport {
    /// Destination PDF.
    out: String,
    /// Declared resolution.
    dpi: f64,
    /// Transport mode.
    duplex: &'static str,
    /// Pages written.
    pages: usize,
    /// Front-side pages.
    front: usize,
    /// Back-side pages.
    back: usize,
    /// Scan report summary.
    summary: String,
    /// Reused print-capture summary.
    capture: String,
    /// Ordered pages.
    rows: Vec<ScanRow>,
    /// Reopened page count.
    verified_pages: usize,
    /// Bytes written.
    bytes: usize,
}

/// Parse the duplex label.
fn parse_duplex(text: &str) -> anyhow::Result<DuplexMode> {
    match text.to_ascii_lowercase().as_str() {
        "simplex" => Ok(DuplexMode::Simplex),
        "front-first" | "duplex-front-first" => Ok(DuplexMode::DuplexFrontFirst),
        "back-first" | "duplex-back-first" => Ok(DuplexMode::DuplexBackFirst),
        other => bail!("unknown --duplex {other:?}; expected simplex, front-first or back-first"),
    }
}

/// Read width and height from a PNG header without decoding pixels.
fn image_dimensions(bytes: &[u8]) -> anyhow::Result<(u32, u32)> {
    if bytes.len() < 24 || &bytes[..8] != b"\x89PNG\r\n\x1a\n" {
        bail!("not a PNG file");
    }
    if &bytes[12..16] != b"IHDR" {
        bail!("PNG has no IHDR chunk");
    }
    let width = u32::from_be_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]);
    let height = u32::from_be_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]);
    Ok((width, height))
}
/// Classify a path's image format by extension.
///
/// Scans are limited to PNG here because the scan model's raster formats and the
/// image embedder intersect on PNG; a TWAIN/WIA adapter can emit PNG.
fn image_format(path: &Path) -> anyhow::Result<CreateImageFormat> {
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if extension == "png" {
        Ok(CreateImageFormat::Png)
    } else {
        bail!(
            "unsupported scan image {extension:?} for {}; expected png",
            path.display()
        )
    }
}

/// One loaded page image.
struct LoadedScan {
    label: String,
    path: PathBuf,
    bytes: Vec<u8>,
    format: CreateImageFormat,
}

/// Acquire pages from an attached WIA scanner.
///
/// `device` is empty for the first attached scanner or a WIA device name. The
/// sidecar writes PNG pages into a private temporary directory and reports
/// their paths. A machine with no scanner (sidecar exit 3) is reported as a
/// typed hardware error.
fn acquire_wia(device: &str) -> anyhow::Result<Vec<PathBuf>> {
    let out_dir = std::env::temp_dir().join(format!("pdfxi-scan-{}", std::process::id()));
    if out_dir.exists() {
        std::fs::remove_dir_all(&out_dir)
            .with_context(|| format!("clear the scan directory {}", out_dir.display()))?;
    }
    std::fs::create_dir_all(&out_dir)
        .with_context(|| format!("create the scan directory {}", out_dir.display()))?;

    // The test seam lets the acquisition path run with a synthetic device.
    let script = match std::env::var_os("PDFXI_WIA_SCAN_SCRIPT") {
        Some(path) => PathBuf::from(path),
        None => {
            let path = out_dir.join("wia-scan.ps1");
            std::fs::write(&path, WIA_SCAN_SCRIPT)
                .with_context(|| format!("write the WIA sidecar to {}", path.display()))?;
            path
        }
    };
    let mut command = Command::new("powershell");
    command
        .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
        .arg(&script)
        .arg("-OutDir")
        .arg(&out_dir);
    if !device.is_empty() {
        command.arg("-DeviceName").arg(device);
    }
    let output = command
        .output()
        .with_context(|| "start the WIA acquisition sidecar (PowerShell is required)")?;
    if !output.status.success() {
        let error = String::from_utf8_lossy(&output.stderr);
        let code = output.status.code().unwrap_or(-1);
        if code == 3 {
            bail!(
                "no scanner is available: {}",
                error.trim().trim_start_matches("no WIA scanner: ")
            );
        }
        bail!("the WIA sidecar failed ({code}): {}", error.trim());
    }
    let report: WiaReport =
        serde_json::from_slice(&output.stdout).context("parse the WIA sidecar output")?;
    if report.pages.is_empty() {
        bail!("the scanner returned no pages");
    }
    Ok(report.pages)
}

/// `pdfxi create scan`.
pub(crate) fn create_scan(
    scans: &[PathBuf],
    out: &Path,
    device: Option<&str>,
    dpi: f64,
    duplex_text: &str,
    json: bool,
) -> anyhow::Result<()> {
    let acquired;
    let inputs: &[PathBuf] = if let Some(name) = device {
        acquired = acquire_wia(name)?;
        acquired.as_slice()
    } else {
        scans
    };
    if inputs.is_empty() {
        bail!("pass at least one --scan IMAGE or use --device to acquire pages");
    }
    let duplex = parse_duplex(duplex_text)?;
    let options = ScanOptions::new().with_duplex(duplex);
    let mut loaded = Vec::with_capacity(inputs.len());
    let mut sources = Vec::with_capacity(inputs.len());
    for (index, path) in inputs.iter().enumerate() {
        let metadata = std::fs::metadata(path)
            .with_context(|| format!("read scan image {}", path.display()))?;
        if metadata.len() > MAX_SCAN_BYTES {
            bail!(
                "scan image {} is {} bytes; the limit is {MAX_SCAN_BYTES}",
                path.display(),
                metadata.len()
            );
        }
        let bytes =
            std::fs::read(path).with_context(|| format!("read scan image {}", path.display()))?;
        let format = image_format(path)?;
        let (width, height) = image_dimensions(&bytes)
            .with_context(|| format!("read image header of {}", path.display()))?;
        let label = format!("scan-{index}");
        let side = match duplex {
            DuplexMode::Simplex => ScanSide::Front,
            DuplexMode::DuplexFrontFirst => {
                if index % 2 == 0 {
                    ScanSide::Front
                } else {
                    ScanSide::Back
                }
            }
            DuplexMode::DuplexBackFirst => {
                if index % 2 == 0 {
                    ScanSide::Back
                } else {
                    ScanSide::Front
                }
            }
        };
        let sheet = if duplex.is_duplex() {
            index / 2 + 1
        } else {
            index + 1
        };

        sources.push(ScanSource::new(
            sheet,
            side,
            label.clone(),
            width,
            height,
            dpi,
            pdfxi_convert::ImageFormat::Png,
        ));
        loaded.push(LoadedScan {
            label,
            path: path.clone(),
            bytes,
            format,
        });
    }
    let plan = build_scan_plan(&sources, &options)
        .map_err(|error| anyhow::anyhow!("scan assembly: {error}"))?;
    let by_label: BTreeMap<&str, &LoadedScan> = loaded
        .iter()
        .map(|scan| (scan.label.as_str(), scan))
        .collect();
    let mut rows = Vec::with_capacity(plan.page_count());
    let mut documents = Vec::with_capacity(plan.page_count());
    for page in plan.pages() {
        let scan = by_label
            .get(page.label())
            .context("scan plan references an unknown image")?;
        let size = page.page_size();
        let options = CreateOptions::new()
            .with_page_size(PageSize::new(size.width, size.height)?)
            .with_margins(Margins::NONE);
        let pdf = create_from_image(&scan.bytes, scan.format, &options)
            .with_context(|| format!("embed scan image {}", scan.path.display()))?;
        documents.push(
            Document::from_bytes(pdf)
                .with_context(|| format!("parse embedded scan {}", scan.path.display()))?,
        );
        rows.push(ScanRow {
            sheet: page.sheet(),
            side: page.side().label(),
            label: page.label().to_owned(),
            file: scan.path.display().to_string(),
            format: "png",
            width_px: page.width_px(),
            height_px: page.height_px(),
            page_width: size.width,
            page_height: size.height,
        });
    }
    let mut documents = documents.into_iter();
    let primary = documents.next().context("scan produced no pages")?;
    let extras: Vec<Document> = documents.collect();
    let merged = primary
        .apply_page_plan(&PagePlan::merge(extras))
        .map_err(|error| anyhow::anyhow!("merge scan pages: {error}"))?;
    std::fs::write(out, &merged).with_context(|| format!("write {}", out.display()))?;

    let reopened = Document::from_bytes(merged.clone())
        .map_err(|error| anyhow::anyhow!("reopen {}: {error}", out.display()))?;
    let verified_pages = reopened
        .page_count()
        .map_err(|error| anyhow::anyhow!("count pages: {error}"))?;
    if verified_pages != plan.page_count() {
        bail!(
            "wrote {verified_pages} page(s) but the scan plan has {}",
            plan.page_count()
        );
    }
    for (index, row) in rows.iter().enumerate() {
        let info = reopened
            .page(index)
            .map_err(|error| anyhow::anyhow!("read output page {}: {error}", index + 1))?;
        let width = info.media_box.width();
        let height = info.media_box.height();
        if (width - row.page_width).abs() > 0.01 || (height - row.page_height).abs() > 0.01 {
            bail!(
                "page {} is {width}x{height} pt but the scan plan says {}x{}",
                index + 1,
                row.page_width,
                row.page_height
            );
        }
    }

    let report = ScanPdfReport {
        out: out.display().to_string(),
        dpi,
        duplex: plan.report().duplex().label(),
        pages: plan.page_count(),
        front: plan.report().front_count(),
        back: plan.report().back_count(),
        summary: plan.summary().to_owned(),
        capture: plan.capture_plan().summary().to_owned(),
        rows,
        verified_pages,
        bytes: merged.len(),
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }
    println!("out: {}", report.out);
    println!("dpi: {}  duplex: {}", report.dpi, report.duplex);
    println!(
        "pages: {}  front: {}  back: {}",
        report.pages, report.front, report.back
    );
    for row in &report.rows {
        println!(
            "{}  sheet {} {}  {}  {}x{} px  {:.2}x{:.2} pt",
            row.file,
            row.sheet,
            row.side,
            row.format,
            row.width_px,
            row.height_px,
            row.page_width,
            row.page_height
        );
    }
    println!("summary: {}", report.summary);
    println!("capture: {}", report.capture);
    println!("verified_pages: {}", report.verified_pages);
    println!("bytes: {}", report.bytes);
    Ok(())
}
