//! Page-image export: PNG, baseline JPEG, and uncompressed multi-page TIFF.
//!
//! This module turns any [`Renderer`] into encoded page images with explicit
//! DPI and page-fit controls. It is **dependency-free**: the three encoders are
//! written in pure Rust so default builds stay as light as the rest of
//! `pdfxi-render`. The raster pipeline (page selection, DPI-to-scale
//! conversion, page fitting, and cooperative cancellation) is reused from the
//! [`crate::print`] module, so export and print share one validated path.
//!
//! # Formats and colour
//!
//! * [`ImageFormat::Png`] emits a non-interlaced, 8-bit PNG with colour type 0
//!   (grayscale), 2 (RGB), or 6 (RGBA). Pixel rows use filter type 0 and a
//!   stored-deflate zlib stream, so the bytes are fully deterministic. A `pHYs`
//!   chunk records the DPI as pixels-per-metre.
//! * [`ImageFormat::Jpeg`] emits a baseline sequential JFIF file using the
//!   standard Annex K Huffman tables and the standard Annex K quantization
//!   tables scaled by [`ExportOptions::quality`]. Chroma is not subsampled
//!   (4:4:4). JPEG has no alpha channel, so [`AlphaPolicy::Preserve`] is
//!   rejected for this format.
//! * [`ImageFormat::Tiff`] emits little-endian TIFF with no compression. Grayscale
//!   (1 sample), RGB (3 samples), and RGBA (4 samples with an `ExtraSamples`
//!   tag) are supported. [`export_pages`] batches all selected pages into a
//!   **single multi-page TIFF** (a chain of IFDs); each other format yields one
//!   file per page. `XResolution`/`YResolution` rationals record the DPI.
//!
//! # Transparency
//!
//! [`AlphaPolicy::Preserve`] keeps the alpha channel and is only valid for RGBA
//! output in a format that can store it (PNG or TIFF).
//! [`AlphaPolicy::Flatten`] composites RGB and grayscale output onto a solid
//! background; its default is white, the conventional page background.
//!
//! # Determinism
//!
//! PNG and TIFF output is byte-for-byte reproducible for a fixed raster and
//! options. JPEG output is deterministic too (integer quantization and fixed
//! tables; no adaptive arithmetic). All length arithmetic is checked and every
//! error is a typed [`ExportError`]; there is no `unwrap`, `panic`, or `unsafe`.

use crate::{
    CancellationToken, PageIndex, PrintError, PrintJob, PrintSettings, PrintSpooler, Progress,
    RenderError,
};

use crate::print::{Margins, PageScaleMode, PageSelection, Paper, PrintPage};

/// Number of PDF points in one inch; a renderer scale of `1.0` is 72 DPI.
const POINTS_PER_INCH: f64 = 72.0;
/// Inches-to-metres conversion used for PNG's `pHYs` chunk.
const METRES_PER_INCH: f64 = 0.0254;

/// Largest accepted export DPI. Keeps pixel dimensions and JPEG density tags
/// within the ranges representable by the encoders.
pub const MAX_EXPORT_DPI: u32 = 12_000;
/// Default export resolution in dots per inch.
pub const DEFAULT_EXPORT_DPI: u32 = 300;

/// A raster file format produced by page-image export.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ImageFormat {
    /// 8-bit Portable Network Graphics (grayscale, RGB, or RGBA).
    Png,
    /// Baseline sequential JFIF (grayscale or YCbCr 4:4:4).
    Jpeg,
    /// Little-endian, uncompressed, optionally multi-page TIFF.
    Tiff,
}

impl ImageFormat {
    /// Every supported format, in a stable order.
    pub const ALL: &'static [Self] = &[Self::Png, Self::Jpeg, Self::Tiff];

    /// Return every supported format in a stable order.
    #[must_use]
    pub fn all() -> &'static [Self] {
        Self::ALL
    }

    /// Classify a file-name extension, ignoring an optional leading dot and
    /// ASCII case. `jpg` and `jpeg` both select [`Self::Jpeg`].
    #[must_use]
    pub fn from_extension(extension: &str) -> Option<Self> {
        let extension = extension.strip_prefix('.').unwrap_or(extension);
        match extension.to_ascii_lowercase().as_str() {
            "png" => Some(Self::Png),
            "jpg" | "jpeg" => Some(Self::Jpeg),
            "tif" | "tiff" => Some(Self::Tiff),
            _ => None,
        }
    }

    /// Return the canonical lower-case file extension for this format.
    #[must_use]
    pub const fn extension(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Jpeg => "jpg",
            Self::Tiff => "tiff",
        }
    }

    /// Return a short human-readable label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        self.extension()
    }
}

impl std::fmt::Display for ImageFormat {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.label())
    }
}

/// Number of samples per pixel written for a requested colour mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ColorMode {
    /// A single luminance sample per pixel.
    Grayscale,
    /// Three red/green/blue samples per pixel.
    Rgb,
    /// Four red/green/blue/alpha samples per pixel.
    Rgba,
}

impl ColorMode {
    /// Return the number of output samples per pixel.
    #[must_use]
    pub const fn channels(self) -> u8 {
        match self {
            Self::Grayscale => 1,
            Self::Rgb => 3,
            Self::Rgba => 4,
        }
    }

    /// Return a short human-readable label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Grayscale => "grayscale",
            Self::Rgb => "rgb",
            Self::Rgba => "rgba",
        }
    }
}

/// How input alpha is handled when producing the output raster.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AlphaPolicy {
    /// Keep the alpha channel. Requires [`ColorMode::Rgba`] and a format that
    /// can store alpha (PNG or TIFF).
    Preserve,
    /// Composite semi-transparent pixels onto a solid RGB background.
    Flatten {
        /// The RGB background colour, with components in `0..=255`.
        background: [u8; 3],
    },
}

impl Default for AlphaPolicy {
    fn default() -> Self {
        Self::Flatten {
            background: [255, 255, 255],
        }
    }
}

/// How a page is scaled onto its export raster, in addition to the target DPI.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum FitMode {
    /// Render at exactly the target DPI with no size adjustment.
    #[default]
    Actual,
    /// Multiply the DPI-derived scale by this factor.
    Scale(f32),
    /// Scale each page to fit inside a box of at most `width` x `height`
    /// pixels while preserving its aspect ratio.
    FitWithin {
        /// Maximum output width in pixels; must be positive.
        width: u32,
        /// Maximum output height in pixels; must be positive.
        height: u32,
    },
}

/// All settings that control one export operation.
///
/// Build a value directly or use [`ExportOptions::default`], then pass it to
/// [`export_pages`] (which calls [`Self::validate`]) or to
/// [`encode_image`] for a single already-rasterized page.
#[derive(Clone, Debug, PartialEq)]
pub struct ExportOptions {
    /// Output file format.
    pub format: ImageFormat,
    /// Number of colour samples per output pixel.
    pub color: ColorMode,
    /// How input alpha is handled.
    pub alpha: AlphaPolicy,
    /// Which document pages to export. Uses the 1-based range syntax shared
    /// with printing (`""`/`All`, `"1-3,5"`, `"7-"`, and so on).
    pub selection: PageSelection,
    /// Target resolution in dots per inch. Must be in `1..=MAX_EXPORT_DPI`.
    pub dpi: u32,
    /// Additional scaling applied on top of the DPI-derived scale.
    pub fit: FitMode,
    /// JPEG quality in `1..=100`; ignored for other formats.
    pub quality: u8,
}

impl Default for ExportOptions {
    fn default() -> Self {
        Self {
            format: ImageFormat::Png,
            color: ColorMode::Rgba,
            alpha: AlphaPolicy::Preserve,
            selection: PageSelection::All,
            dpi: DEFAULT_EXPORT_DPI,
            fit: FitMode::Actual,
            quality: 85,
        }
    }
}

impl ExportOptions {
    /// Validate fields that do not require a document's page count.
    ///
    /// Page-number bounds are checked later by [`export_pages`] when the
    /// selection is resolved against the renderer.
    ///
    /// # Errors
    ///
    /// Returns the matching [`ExportError`] variant for the first invalid
    /// field: DPI, JPEG quality, fit mode, or format/alpha compatibility.
    pub fn validate(&self) -> Result<(), ExportError> {
        if self.dpi == 0 || self.dpi > MAX_EXPORT_DPI {
            return Err(ExportError::InvalidDpi(self.dpi));
        }
        if self.format == ImageFormat::Jpeg && (self.quality == 0 || self.quality > 100) {
            return Err(ExportError::InvalidQuality(self.quality));
        }
        match self.fit {
            FitMode::Scale(factor) => {
                if !factor.is_finite() || factor <= 0.0 {
                    return Err(ExportError::InvalidScale(factor));
                }
            }
            FitMode::FitWithin { width, height } => {
                if width == 0 || height == 0 {
                    return Err(ExportError::InvalidFit);
                }
            }
            FitMode::Actual => {}
        }
        if self.alpha == AlphaPolicy::Preserve
            && !(self.color == ColorMode::Rgba
                && matches!(self.format, ImageFormat::Png | ImageFormat::Tiff))
        {
            return Err(ExportError::AlphaNotSupported(
                self.format.label(),
                self.color.label(),
            ));
        }
        Ok(())
    }

    /// Translate the fit mode into the print pipeline's scale mode and paper.
    fn to_print_settings(&self) -> Result<PrintSettings, ExportError> {
        let (scale, paper) = match self.fit {
            FitMode::Actual => (PageScaleMode::Actual, Paper::default()),
            FitMode::Scale(factor) => (
                PageScaleMode::CustomPercent(factor * 100.0),
                Paper::default(),
            ),
            FitMode::FitWithin { width, height } => {
                // Print fits into a paper area measured in points. A box of
                // `width` pixels at `dpi` corresponds to width*72/dpi points.
                let width_pt = f64::from(width) * POINTS_PER_INCH / f64::from(self.dpi);
                let height_pt = f64::from(height) * POINTS_PER_INCH / f64::from(self.dpi);
                if !width_pt.is_finite()
                    || width_pt <= 0.0
                    || !height_pt.is_finite()
                    || height_pt <= 0.0
                    || width_pt > f64::from(f32::MAX)
                    || height_pt > f64::from(f32::MAX)
                {
                    return Err(ExportError::InvalidFit);
                }
                (
                    PageScaleMode::Fit,
                    Paper::new("export-fit", width_pt as f32, height_pt as f32),
                )
            }
        };
        Ok(PrintSettings {
            selection: self.selection.clone(),
            dpi: self.dpi,
            scale,
            paper,
            margins: Margins::NONE,
            ..PrintSettings::default()
        })
    }
}

/// Structured errors produced while exporting page images.
#[derive(Debug, thiserror::Error, PartialEq)]
pub enum ExportError {
    /// The requested DPI was zero or larger than [`MAX_EXPORT_DPI`].
    #[error("DPI must be between 1 and 12000, got {0}")]
    InvalidDpi(u32),
    /// JPEG quality was outside `1..=100`.
    #[error("JPEG quality must be between 1 and 100, got {0}")]
    InvalidQuality(u8),
    /// A custom scale factor was not finite and positive.
    #[error("export scale must be finite and positive, got {0}")]
    InvalidScale(f32),
    /// A fit box had a zero dimension or could not be represented.
    #[error("fit dimensions must be positive and representable")]
    InvalidFit,
    /// The selection resolved to no pages.
    #[error("the export selection resolved to no pages")]
    EmptySelection,
    /// An input raster did not match its declared dimensions.
    #[error("raster buffer for a {0}x{1} image does not have width*height*4 bytes")]
    InvalidRaster(u32, u32),
    /// Alpha preservation was requested for an incompatible format/colour.
    #[error("alpha cannot be preserved with format '{0}' and colour '{1}'")]
    AlphaNotSupported(&'static str, &'static str),
    /// The operation was cancelled through its token.
    #[error("image export was cancelled")]
    Cancelled,
    /// The renderer failed.
    #[error("rendering failed: {0}")]
    Render(#[from] RenderError),
    /// The shared print plan rejected the settings.
    #[error("invalid export plan: {0}")]
    Plan(String),
    /// An encoder rejected its input.
    #[error("image encoding failed: {0}")]
    Encode(String),
}

impl From<PrintError> for ExportError {
    fn from(error: PrintError) -> Self {
        match error {
            PrintError::Cancelled => Self::Cancelled,
            PrintError::Render(render_error) => Self::Render(render_error),
            other => Self::Plan(other.to_string()),
        }
    }
}

/// One encoded image produced by [`export_pages`].
///
/// For PNG and JPEG, `page_count` is `1`: each value is one page. For TIFF,
/// a single value carries the whole multi-page file; `page` is the first
/// selected page and `page_count` is the number of IFDs it contains.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExportedImage {
    /// First source page contained in this file.
    pub page: PageIndex,
    /// Number of pages contained in this file (greater than one only for TIFF).
    pub page_count: u32,
    /// Pixel width of the first page.
    pub width: u32,
    /// Pixel height of the first page.
    pub height: u32,
    /// Format of `bytes`.
    pub format: ImageFormat,
    /// DPI recorded in the file's metadata.
    pub dpi: u32,
    /// Complete encoded file bytes.
    pub bytes: Vec<u8>,
}

/// A colour-reduced raster ready for encoding.
struct ReducedRaster {
    page: PageIndex,
    width: u32,
    height: u32,
    data: Vec<u8>,
    channels: u8,
}

/// Reduce a tightly packed RGBA raster to the requested colour mode and alpha
/// policy. Returns the sample buffer and its channel count.
fn reduce_pixels(
    width: u32,
    height: u32,
    rgba: &[u8],
    options: &ExportOptions,
) -> Result<(Vec<u8>, u8), ExportError> {
    let expected = usize::try_from(width)
        .ok()
        .and_then(|w| usize::try_from(height).ok().and_then(|h| w.checked_mul(h)))
        .and_then(|area| area.checked_mul(4))
        .ok_or(ExportError::InvalidRaster(width, height))?;
    if rgba.len() != expected {
        return Err(ExportError::InvalidRaster(width, height));
    }

    let channels = options.color.channels();
    let mut out = Vec::with_capacity(
        usize::try_from(width)
            .ok()
            .and_then(|w| usize::try_from(height).ok().and_then(|h| w.checked_mul(h)))
            .and_then(|area| area.checked_mul(usize::from(channels)))
            .unwrap_or(0),
    );
    for &[r, g, b, a] in rgba.as_chunks::<4>().0 {
        let (rr, gg, bb) = match options.alpha {
            AlphaPolicy::Preserve => (r, g, b),
            AlphaPolicy::Flatten { background } => {
                let alpha = u32::from(a);
                let blend = |channel: u8, base: u8| -> u8 {
                    let value = u32::from(channel) * alpha + u32::from(base) * (255 - alpha);
                    ((value + 127) / 255) as u8
                };
                (
                    blend(r, background[0]),
                    blend(g, background[1]),
                    blend(b, background[2]),
                )
            }
        };
        match options.color {
            ColorMode::Rgba => {
                out.push(rr);
                out.push(gg);
                out.push(bb);
                out.push(if options.alpha == AlphaPolicy::Preserve {
                    a
                } else {
                    255
                });
            }
            ColorMode::Rgb => {
                out.push(rr);
                out.push(gg);
                out.push(bb);
            }
            ColorMode::Grayscale => {
                // ITU-R BT.601 luma, rounded to the nearest byte.
                let luma = 0.299 * f64::from(rr) + 0.587 * f64::from(gg) + 0.114 * f64::from(bb);
                out.push(luma.round().clamp(0.0, 255.0) as u8);
            }
        }
    }
    Ok((out, channels))
}

/// Encode a single already-rasterized page according to `options`.
///
/// `rgba` must contain exactly `width * height * 4` bytes in row-major order
/// from the top-left. For TIFF this produces a single-page file; use
/// [`export_pages`] for a multi-page TIFF.
///
/// # Errors
///
/// Returns [`ExportError::InvalidRaster`] for a mismatched buffer, and
/// propagates validation and encoder errors from the selected format.
pub fn encode_image(
    width: u32,
    height: u32,
    rgba: &[u8],
    options: &ExportOptions,
) -> Result<Vec<u8>, ExportError> {
    options.validate()?;
    let (data, channels) = reduce_pixels(width, height, rgba, options)?;
    match options.format {
        ImageFormat::Png => encode_png(width, height, &data, channels, options.dpi),
        ImageFormat::Jpeg => {
            encode_jpeg(width, height, &data, channels, options.dpi, options.quality)
        }
        ImageFormat::Tiff => encode_tiff(
            std::slice::from_ref(&ReducedRaster {
                page: PageIndex::new(0),
                width,
                height,
                data,
                channels,
            }),
            options.dpi,
        ),
    }
}

/// Render and encode the pages selected by `options`.
///
/// The raster work reuses the print spooler, so DPI, page fitting, page-range
/// resolution, cancellation, and progress all behave as documented in
/// [`crate::print`]. Cancellation is additionally checked between page encodes.
/// Progress reports come from the spooler and run from `0` to the page count.
///
/// # Errors
///
/// Returns validation, selection, renderer, cancellation, or encoding errors as
/// typed [`ExportError`] variants.
pub fn export_pages(
    renderer: &dyn crate::Renderer,
    options: &ExportOptions,
    cancellation: &CancellationToken,
    progress: &mut dyn FnMut(Progress),
) -> Result<Vec<ExportedImage>, ExportError> {
    options.validate()?;
    let page_count = renderer.page_count()?;
    let settings = options.to_print_settings()?;
    let job = PrintJob::new(&settings, page_count)?;
    let spool = PrintSpooler.spool(renderer, &job, cancellation, progress)?;

    let mut rasters = Vec::with_capacity(spool.pages.len());
    for page in spool.pages {
        cancellation.check().map_err(|_| ExportError::Cancelled)?;
        let PrintPage {
            page: index,
            width_px,
            height_px,
            rgba,
            ..
        } = page;
        let (data, channels) = reduce_pixels(width_px, height_px, &rgba, options)?;
        rasters.push(ReducedRaster {
            page: index,
            width: width_px,
            height: height_px,
            data,
            channels,
        });
    }
    if rasters.is_empty() {
        return Err(ExportError::EmptySelection);
    }

    let first = &rasters[0];
    let mut results = Vec::new();
    match options.format {
        ImageFormat::Tiff => {
            cancellation.check().map_err(|_| ExportError::Cancelled)?;
            let bytes = encode_tiff(&rasters, options.dpi)?;
            let page_count = u32::try_from(rasters.len())
                .map_err(|_| ExportError::Encode("too many pages for a single TIFF".to_owned()))?;
            results.push(ExportedImage {
                page: first.page,
                page_count,
                width: first.width,
                height: first.height,
                format: options.format,
                dpi: options.dpi,
                bytes,
            });
        }
        ImageFormat::Png | ImageFormat::Jpeg => {
            // This arm is reached only for PNG and JPEG; TIFF is batched above.
            for raster in &rasters {
                cancellation.check().map_err(|_| ExportError::Cancelled)?;
                let bytes = if options.format == ImageFormat::Png {
                    encode_png(
                        raster.width,
                        raster.height,
                        &raster.data,
                        raster.channels,
                        options.dpi,
                    )?
                } else {
                    encode_jpeg(
                        raster.width,
                        raster.height,
                        &raster.data,
                        raster.channels,
                        options.dpi,
                        options.quality,
                    )?
                };
                results.push(ExportedImage {
                    page: raster.page,
                    page_count: 1,
                    width: raster.width,
                    height: raster.height,
                    format: options.format,
                    dpi: options.dpi,
                    bytes,
                });
            }
        }
    }
    Ok(results)
}

// ---------------------------------------------------------------------------
// PNG
// ---------------------------------------------------------------------------

/// The fixed eight-byte PNG signature.
const PNG_SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
/// Largest raw byte count a single deflate stored block can carry.
const MAX_STORED_BLOCK: usize = u16::MAX as usize;
/// Initial value of the CRC-32 register used by PNG chunk checksums.
const CRC_INIT: u32 = 0xFFFF_FFFF;
/// Adler-32 modulus (the largest prime below 2^16).
const ADLER_MODULUS: u32 = 65_521;

/// Continue an IEEE CRC-32 register over `data`.
fn crc32_continue(mut crc: u32, data: &[u8]) -> u32 {
    for &byte in data {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            let mask = 0u32.wrapping_sub(crc & 1);
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    crc
}

/// Return the standard CRC-32 checksum of `data`.
#[cfg(test)]
fn crc32(data: &[u8]) -> u32 {
    crc32_continue(CRC_INIT, data) ^ CRC_INIT
}

/// Return the standard Adler-32 checksum of `data`.
fn adler32(data: &[u8]) -> u32 {
    let mut a = 1u32;
    let mut b = 0u32;
    for &byte in data {
        a = (a + u32::from(byte)) % ADLER_MODULUS;
        b = (b + a) % ADLER_MODULUS;
    }
    (b << 16) | a
}

/// Append one PNG chunk (big-endian length, type, payload, CRC-32).
fn push_png_chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) -> Result<(), ExportError> {
    let length = u32::try_from(data.len())
        .map_err(|_| ExportError::Encode("PNG chunk exceeds 2^32 bytes".to_owned()))?;
    out.extend_from_slice(&length.to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let crc = crc32_continue(crc32_continue(CRC_INIT, kind), data) ^ CRC_INIT;
    out.extend_from_slice(&crc.to_be_bytes());
    Ok(())
}

/// Wrap `raw` in a zlib stream made of uncompressed deflate stored blocks.
fn zlib_stored(raw: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(raw.len() + (raw.len() / MAX_STORED_BLOCK + 1) * 5 + 6);
    out.extend_from_slice(&[0x78, 0x01]);
    if raw.is_empty() {
        out.extend_from_slice(&[0x01, 0x00, 0x00, 0xFF, 0xFF]);
    } else {
        let mut remaining = raw.len();
        for chunk in raw.chunks(MAX_STORED_BLOCK) {
            remaining -= chunk.len();
            let len = chunk.len() as u16;
            out.push(u8::from(remaining == 0));
            out.extend_from_slice(&len.to_le_bytes());
            out.extend_from_slice(&(!len).to_le_bytes());
            out.extend_from_slice(chunk);
        }
    }
    out.extend_from_slice(&adler32(raw).to_be_bytes());
    out
}

/// Encode reduced samples as a deterministic 8-bit PNG file.
fn encode_png(
    width: u32,
    height: u32,
    data: &[u8],
    channels: u8,
    dpi: u32,
) -> Result<Vec<u8>, ExportError> {
    let color_type = match channels {
        1 => 0u8,
        3 => 2u8,
        4 => 6u8,
        other => {
            return Err(ExportError::Encode(format!(
                "unsupported PNG channels {other}"
            )));
        }
    };
    let width_usize = usize::try_from(width)
        .map_err(|_| ExportError::Encode("PNG width does not fit in usize".to_owned()))?;
    let height_usize = usize::try_from(height)
        .map_err(|_| ExportError::Encode("PNG height does not fit in usize".to_owned()))?;
    let row_len = width_usize
        .checked_mul(usize::from(channels))
        .ok_or_else(|| ExportError::Encode("PNG row length overflow".to_owned()))?;
    if data.len() != row_len.saturating_mul(height_usize) {
        return Err(ExportError::Encode(
            "PNG sample buffer length mismatch".to_owned(),
        ));
    }

    let mut raw = Vec::with_capacity(height_usize.saturating_mul(row_len.saturating_add(1)));
    for row in data.chunks(row_len) {
        raw.push(0); // PNG filter type: None
        raw.extend_from_slice(row);
    }
    if width == 0 {
        raw.resize(height_usize, 0);
    }

    let mut header = Vec::with_capacity(13);
    header.extend_from_slice(&width.to_be_bytes());
    header.extend_from_slice(&height.to_be_bytes());
    header.extend_from_slice(&[8, color_type, 0, 0, 0]);

    let pixels_per_metre = (f64::from(dpi) / METRES_PER_INCH).round() as u32;
    let mut phys = Vec::with_capacity(9);
    phys.extend_from_slice(&pixels_per_metre.to_be_bytes());
    phys.extend_from_slice(&pixels_per_metre.to_be_bytes());
    phys.push(1); // unit specifier: metre

    let mut out = Vec::with_capacity(PNG_SIGNATURE.len() + 13 + 21 + raw.len() + 24);
    out.extend_from_slice(&PNG_SIGNATURE);
    push_png_chunk(&mut out, b"IHDR", &header)?;
    push_png_chunk(&mut out, b"pHYs", &phys)?;
    push_png_chunk(&mut out, b"IDAT", &zlib_stored(&raw))?;
    push_png_chunk(&mut out, b"IEND", &[])?;
    Ok(out)
}

// ---------------------------------------------------------------------------
// TIFF (uncompressed, multi-page)
// ---------------------------------------------------------------------------

/// Layout information for one TIFF page, computed before bytes are written.
struct TiffLayout {
    data_offset: u32,
    data_len: u32,
    ifd_offset: u32,
    bits_offset: Option<u32>,
    xres_offset: u32,
    yres_offset: u32,
}

/// Encode reduced rasters into one little-endian, uncompressed, multi-page TIFF.
fn encode_tiff(rasters: &[ReducedRaster], dpi: u32) -> Result<Vec<u8>, ExportError> {
    if rasters.is_empty() {
        return Err(ExportError::Encode(
            "TIFF needs at least one page".to_owned(),
        ));
    }

    let mut offset = 8u32;
    let mut layouts = Vec::with_capacity(rasters.len());
    for raster in rasters {
        if raster.width == 0 || raster.height == 0 {
            return Err(ExportError::Encode(
                "TIFF page has zero dimensions".to_owned(),
            ));
        }
        let row_len =
            usize::from(raster.channels)
                .checked_mul(usize::try_from(raster.width).map_err(|_| {
                    ExportError::Encode("TIFF width does not fit in usize".to_owned())
                })?)
                .ok_or_else(|| ExportError::Encode("TIFF row length overflow".to_owned()))?;
        let expected =
            row_len
                .checked_mul(usize::try_from(raster.height).map_err(|_| {
                    ExportError::Encode("TIFF height does not fit in usize".to_owned())
                })?)
                .ok_or_else(|| ExportError::Encode("TIFF data length overflow".to_owned()))?;
        if raster.data.len() != expected {
            return Err(ExportError::Encode(
                "TIFF sample buffer length mismatch".to_owned(),
            ));
        }

        let data_offset = offset;
        let data_len = u32::try_from(raster.data.len())
            .map_err(|_| ExportError::Encode("TIFF strip too large".to_owned()))?;
        offset = data_offset
            .checked_add(data_len)
            .ok_or_else(|| ExportError::Encode("TIFF offset overflow".to_owned()))?;

        let extra_entry = u32::from(raster.channels == 4);
        let entry_count = 12u32 + extra_entry;
        let bits_len = if raster.channels > 2 {
            u32::from(raster.channels) * 2
        } else {
            0
        };
        let ifd_offset = offset;
        let values_offset = ifd_offset
            .checked_add(2)
            .and_then(|v| v.checked_add(entry_count.checked_mul(12)?))
            .and_then(|v| v.checked_add(4))
            .ok_or_else(|| ExportError::Encode("TIFF IFD offset overflow".to_owned()))?;
        let bits_offset = if bits_len > 0 {
            Some(values_offset)
        } else {
            None
        };
        let xres_offset = values_offset
            .checked_add(bits_len)
            .ok_or_else(|| ExportError::Encode("TIFF XResolution offset overflow".to_owned()))?;
        let yres_offset = xres_offset
            .checked_add(8)
            .ok_or_else(|| ExportError::Encode("TIFF YResolution offset overflow".to_owned()))?;
        let ifd_size = yres_offset
            .checked_add(8)
            .and_then(|v| v.checked_sub(ifd_offset))
            .ok_or_else(|| ExportError::Encode("TIFF IFD size overflow".to_owned()))?;
        offset = ifd_offset
            .checked_add(ifd_size)
            .ok_or_else(|| ExportError::Encode("TIFF offset overflow".to_owned()))?;

        layouts.push(TiffLayout {
            data_offset,
            data_len,
            ifd_offset,
            bits_offset,
            xres_offset,
            yres_offset,
        });
    }

    let mut out = Vec::with_capacity(usize::try_from(offset).unwrap_or(usize::MAX));
    out.extend_from_slice(b"II"); // little-endian
    out.extend_from_slice(&42u16.to_le_bytes());
    // Pixel data precedes each IFD, so the first IFD lives after page 0's strip.
    let first_ifd_offset = layouts
        .first()
        .map(|layout| layout.ifd_offset)
        .ok_or_else(|| ExportError::Encode("TIFF has no pages".to_owned()))?;
    out.extend_from_slice(&first_ifd_offset.to_le_bytes());

    for (index, raster) in rasters.iter().enumerate() {
        let layout = &layouts[index];
        let data_offset = usize::try_from(layout.data_offset)
            .map_err(|_| ExportError::Encode("TIFF data offset too large".to_owned()))?;
        let ifd_offset = usize::try_from(layout.ifd_offset)
            .map_err(|_| ExportError::Encode("TIFF IFD offset too large".to_owned()))?;
        if out.len() < data_offset {
            out.resize(data_offset, 0);
        }
        out.extend_from_slice(&raster.data);
        if out.len() < ifd_offset {
            out.resize(ifd_offset, 0);
        }

        let next_ifd = if index + 1 < layouts.len() {
            layouts[index + 1].ifd_offset
        } else {
            0
        };
        write_tiff_ifd(&mut out, raster, layout, next_ifd, dpi)?;
    }
    Ok(out)
}

/// Append one image-file-directory (IFD) for a TIFF page.
fn write_tiff_ifd(
    out: &mut Vec<u8>,
    raster: &ReducedRaster,
    layout: &TiffLayout,
    next_ifd: u32,
    dpi: u32,
) -> Result<(), ExportError> {
    // (tag, field type, count, 4-byte value/inline data). Types: 3 = SHORT,
    // 4 = LONG, 5 = RATIONAL. Entries are emitted in ascending tag order.
    let mut entries: Vec<(u16, u16, u32, [u8; 4])> = Vec::new();
    entries.push((256, 4, 1, raster.width.to_le_bytes())); // ImageWidth
    entries.push((257, 4, 1, raster.height.to_le_bytes())); // ImageLength
    if raster.channels > 2 {
        let bits = layout
            .bits_offset
            .ok_or_else(|| ExportError::Encode("missing TIFF BitsPerSample offset".to_owned()))?;
        entries.push((258, 3, u32::from(raster.channels), bits.to_le_bytes()));
    } else {
        entries.push((258, 3, 1, 8u32.to_le_bytes()));
    }
    entries.push((259, 3, 1, 1u32.to_le_bytes())); // Compression = none
    let photometric = if raster.channels == 1 { 1u32 } else { 2u32 };
    entries.push((262, 3, 1, photometric.to_le_bytes()));
    entries.push((273, 4, 1, layout.data_offset.to_le_bytes())); // StripOffsets
    entries.push((277, 3, 1, u32::from(raster.channels).to_le_bytes()));
    entries.push((278, 4, 1, raster.height.to_le_bytes())); // RowsPerStrip
    entries.push((279, 4, 1, layout.data_len.to_le_bytes())); // StripByteCounts
    entries.push((282, 5, 1, layout.xres_offset.to_le_bytes())); // XResolution
    entries.push((283, 5, 1, layout.yres_offset.to_le_bytes())); // YResolution
    entries.push((296, 3, 1, 2u32.to_le_bytes())); // ResolutionUnit = inch
    if raster.channels == 4 {
        entries.push((338, 3, 1, 2u32.to_le_bytes())); // ExtraSamples = unassociated alpha
    }

    let entry_count = u16::try_from(entries.len())
        .map_err(|_| ExportError::Encode("too many TIFF IFD entries".to_owned()))?;
    out.extend_from_slice(&entry_count.to_le_bytes());
    for (tag, kind, count, value) in entries {
        out.extend_from_slice(&tag.to_le_bytes());
        out.extend_from_slice(&kind.to_le_bytes());
        out.extend_from_slice(&count.to_le_bytes());
        out.extend_from_slice(&value);
    }
    out.extend_from_slice(&next_ifd.to_le_bytes());

    if let Some(_bits) = layout.bits_offset
        && raster.channels > 2
    {
        for _ in 0..raster.channels {
            out.extend_from_slice(&8u16.to_le_bytes());
        }
    }
    out.extend_from_slice(&dpi.to_le_bytes()); // XResolution numerator
    out.extend_from_slice(&1u32.to_le_bytes()); // XResolution denominator
    out.extend_from_slice(&dpi.to_le_bytes()); // YResolution numerator
    out.extend_from_slice(&1u32.to_le_bytes()); // YResolution denominator
    Ok(())
}

// ---------------------------------------------------------------------------
// Baseline JPEG
// ---------------------------------------------------------------------------

/// Zig-zag order mapping a scan position (0..64) to a natural coefficient
/// index (`row * 8 + column`). Position 0 is the DC coefficient.
#[rustfmt::skip]
const ZIGZAG: [usize; 64] = [
     0,  1,  8, 16,  9,  2,  3, 10,
    17, 24, 32, 25, 18, 11,  4,  5,
    12, 19, 26, 33, 40, 48, 41, 34,
    27, 20, 13,  6,  7, 14, 21, 28,
    35, 42, 49, 56, 57, 50, 43, 36,
    29, 22, 15, 23, 30, 37, 44, 51,
    58, 59, 52, 45, 38, 31, 39, 46,
    53, 60, 61, 54, 47, 55, 62, 63,
];

/// Standard ISO/IEC 10918-1 Annex K luminance quantization table, in natural
/// (row-major) order. The chrominance table follows below. Both are scaled by
/// the requested quality before use; see [`scaled_quant_table`].
#[rustfmt::skip]
const QUANT_LUMA_STD: [u16; 64] = [
    16, 11, 10, 16, 24, 40, 51, 61,
    12, 12, 14, 19, 26, 58, 60, 55,
    14, 13, 16, 24, 40, 57, 69, 56,
    14, 17, 22, 29, 51, 87, 80, 62,
    18, 22, 37, 56, 68, 109, 103, 77,
    24, 35, 55, 64, 81, 104, 113, 92,
    49, 64, 78, 87, 103, 121, 120, 101,
    72, 92, 95, 98, 112, 100, 103, 99,
];

/// Standard ISO/IEC 10918-1 Annex K chrominance quantization table.
#[rustfmt::skip]
const QUANT_CHROMA_STD: [u16; 64] = [
    17, 18, 24, 47, 99, 99, 99, 99,
    18, 21, 26, 66, 99, 99, 99, 99,
    24, 26, 56, 99, 99, 99, 99, 99,
    47, 66, 99, 99, 99, 99, 99, 99,
    99, 99, 99, 99, 99, 99, 99, 99,
    99, 99, 99, 99, 99, 99, 99, 99,
    99, 99, 99, 99, 99, 99, 99, 99,
    99, 99, 99, 99, 99, 99, 99, 99,
];

// Standard Annex K Huffman table specifications. `bits[n]` is the number of
// codes of length n+1; `values` is the concatenated list of symbols.
#[rustfmt::skip]
const DC_LUMA_BITS: [u8; 16] = [0, 1, 5, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0, 0, 0];
#[rustfmt::skip]
const DC_LUMA_VALUES: [u8; 12] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11];
#[rustfmt::skip]
const DC_CHROMA_BITS: [u8; 16] = [0, 3, 1, 1, 1, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0];
#[rustfmt::skip]
const DC_CHROMA_VALUES: [u8; 12] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11];
#[rustfmt::skip]
const AC_LUMA_BITS: [u8; 16] = [0, 2, 1, 3, 3, 2, 4, 3, 5, 5, 4, 4, 0, 0, 1, 0x7d];
#[rustfmt::skip]
const AC_LUMA_VALUES: [u8; 162] = [
    0x01,0x02,0x03,0x00,0x04,0x11,0x05,0x12,0x21,0x31,0x41,0x06,0x13,0x51,0x61,0x07,
    0x22,0x71,0x14,0x32,0x81,0x91,0xa1,0x08,0x23,0x42,0xb1,0xc1,0x15,0x52,0xd1,0xf0,
    0x24,0x33,0x62,0x72,0x82,0x09,0x0a,0x16,0x17,0x18,0x19,0x1a,0x25,0x26,0x27,0x28,
    0x29,0x2a,0x34,0x35,0x36,0x37,0x38,0x39,0x3a,0x43,0x44,0x45,0x46,0x47,0x48,0x49,
    0x4a,0x53,0x54,0x55,0x56,0x57,0x58,0x59,0x5a,0x63,0x64,0x65,0x66,0x67,0x68,0x69,
    0x6a,0x73,0x74,0x75,0x76,0x77,0x78,0x79,0x7a,0x83,0x84,0x85,0x86,0x87,0x88,0x89,
    0x8a,0x92,0x93,0x94,0x95,0x96,0x97,0x98,0x99,0x9a,0xa2,0xa3,0xa4,0xa5,0xa6,0xa7,
    0xa8,0xa9,0xaa,0xb2,0xb3,0xb4,0xb5,0xb6,0xb7,0xb8,0xb9,0xba,0xc2,0xc3,0xc4,0xc5,
    0xc6,0xc7,0xc8,0xc9,0xca,0xd2,0xd3,0xd4,0xd5,0xd6,0xd7,0xd8,0xd9,0xda,0xe1,0xe2,
    0xe3,0xe4,0xe5,0xe6,0xe7,0xe8,0xe9,0xea,0xf1,0xf2,0xf3,0xf4,0xf5,0xf6,0xf7,0xf8,
    0xf9,0xfa,
];
#[rustfmt::skip]
const AC_CHROMA_BITS: [u8; 16] = [0, 2, 1, 2, 4, 4, 3, 4, 7, 5, 4, 4, 0, 1, 2, 0x77];
#[rustfmt::skip]
const AC_CHROMA_VALUES: [u8; 162] = [
    0x00,0x01,0x02,0x03,0x11,0x04,0x05,0x21,0x31,0x06,0x12,0x41,0x51,0x07,0x61,0x71,
    0x13,0x22,0x32,0x81,0x08,0x14,0x42,0x91,0xa1,0xb1,0xc1,0x09,0x23,0x33,0x52,0xf0,
    0x15,0x62,0x72,0xd1,0x0a,0x16,0x24,0x34,0xe1,0x25,0xf1,0x17,0x18,0x19,0x1a,0x26,
    0x27,0x28,0x29,0x2a,0x35,0x36,0x37,0x38,0x39,0x3a,0x43,0x44,0x45,0x46,0x47,0x48,
    0x49,0x4a,0x53,0x54,0x55,0x56,0x57,0x58,0x59,0x5a,0x63,0x64,0x65,0x66,0x67,0x68,
    0x69,0x6a,0x73,0x74,0x75,0x76,0x77,0x78,0x79,0x7a,0x82,0x83,0x84,0x85,0x86,0x87,
    0x88,0x89,0x8a,0x92,0x93,0x94,0x95,0x96,0x97,0x98,0x99,0x9a,0xa2,0xa3,0xa4,0xa5,
    0xa6,0xa7,0xa8,0xa9,0xaa,0xb2,0xb3,0xb4,0xb5,0xb6,0xb7,0xb8,0xb9,0xba,0xc2,0xc3,
    0xc4,0xc5,0xc6,0xc7,0xc8,0xc9,0xca,0xd2,0xd3,0xd4,0xd5,0xd6,0xd7,0xd8,0xd9,0xda,
    0xe2,0xe3,0xe4,0xe5,0xe6,0xe7,0xe8,0xe9,0xea,0xf2,0xf3,0xf4,0xf5,0xf6,0xf7,0xf8,
    0xf9,0xfa,
];

/// A canonical Huffman lookup table: one `(code, length)` per possible symbol.
struct HuffmanTable {
    codes: [Option<(u16, u8)>; 256],
}

impl HuffmanTable {
    /// Build the canonical codes from a length-count vector and symbol list.
    fn new(bits: &[u8; 16], values: &[u8]) -> Self {
        let mut codes = [None; 256];
        let mut code = 0u16;
        let mut index = 0usize;
        for length in 1u8..=16 {
            let count = usize::from(bits[usize::from(length - 1)]);
            for _ in 0..count {
                if let Some(&symbol) = values.get(index) {
                    let symbol_index = usize::from(symbol);
                    if symbol_index < codes.len() {
                        codes[symbol_index] = Some((code, length));
                    }
                }
                code = code.wrapping_add(1);
                index += 1;
            }
            code <<= 1;
        }
        Self { codes }
    }

    /// Return the canonical `(code, bit-length)` for `symbol`.
    fn code(&self, symbol: u8) -> Result<(u16, u8), ExportError> {
        self.codes[usize::from(symbol)]
            .ok_or_else(|| ExportError::Encode("symbol missing from JPEG Huffman table".to_owned()))
    }
}

/// Scale a standard quantization table by the requested JPEG quality.
///
/// Uses the IJG rule: for quality < 50 the scale is `5000/quality`, otherwise
/// `200 - quality*2`; each entry is `(base*scale + 50)/100` clamped to `1..=255`.
fn scaled_quant_table(base: &[u16; 64], quality: u8) -> [u8; 64] {
    let scale = i32::from(if quality < 50 {
        5000 / u16::from(quality)
    } else {
        200 - 2 * u16::from(quality)
    });
    let mut scaled = [0u8; 64];
    for (index, &value) in base.iter().enumerate() {
        let raw = (i32::from(value) * scale + 50) / 100;
        scaled[index] = raw.clamp(1, 255) as u8;
    }
    scaled
}

/// Number of bits needed to represent the magnitude of `value`.
fn magnitude_category(value: i32) -> u8 {
    let magnitude = value.unsigned_abs();
    if magnitude == 0 {
        0
    } else {
        32 - magnitude.leading_zeros() as u8
    }
}

/// A JPEG entropy bit writer with MSB-first packing and 0xFF byte stuffing.
struct BitWriter {
    bytes: Vec<u8>,
    accumulator: u32,
    bits: u8,
}

impl BitWriter {
    fn new() -> Self {
        Self {
            bytes: Vec::new(),
            accumulator: 0,
            bits: 0,
        }
    }

    /// Write the low `length` bits of `value`, most significant bit first.
    fn write_bits(&mut self, value: u32, length: u8) {
        if length == 0 {
            return;
        }
        let mask = (1u32 << length) - 1;
        self.accumulator = (self.accumulator << length) | (value & mask);
        self.bits += length;
        while self.bits >= 8 {
            self.bits -= 8;
            let byte = ((self.accumulator >> self.bits) & 0xFF) as u8;
            self.bytes.push(byte);
            if byte == 0xFF {
                self.bytes.push(0x00); // JPEG byte stuffing
            }
            self.accumulator &= (1u32 << self.bits) - 1;
        }
    }

    /// Emit a Huffman symbol followed by `additional` payload bits.
    fn write_symbol(
        &mut self,
        table: &HuffmanTable,
        symbol: u8,
        additional: u32,
        bits: u8,
    ) -> Result<(), ExportError> {
        let (code, length) = table.code(symbol)?;
        self.write_bits(u32::from(code), length);
        if bits > 0 {
            self.write_bits(additional, bits);
        }
        Ok(())
    }

    /// Pad the final byte with ones and return the entropy bytes.
    fn finish(mut self) -> Vec<u8> {
        if self.bits > 0 {
            let pad = 8 - self.bits;
            self.write_bits((1u32 << pad) - 1, pad);
        }
        self.bytes
    }
}

/// Append a JPEG marker segment: 0xFF, marker id, big-endian length, payload.
fn push_jpeg_segment(out: &mut Vec<u8>, marker: u8, payload: &[u8]) -> Result<(), ExportError> {
    let length = u16::try_from(
        payload
            .len()
            .checked_add(2)
            .ok_or_else(|| ExportError::Encode("JPEG segment length overflow".to_owned()))?,
    )
    .map_err(|_| ExportError::Encode("JPEG segment exceeds 65535 bytes".to_owned()))?;
    out.push(0xFF);
    out.push(marker);
    out.extend_from_slice(&length.to_be_bytes());
    out.extend_from_slice(payload);
    Ok(())
}

/// Precomputed 8x8 cosine basis: `cos((2*x+1)*u*pi/16)`.
fn dct_cosines() -> [[f64; 8]; 8] {
    let mut table = [[0.0; 8]; 8];
    for (u, row) in table.iter_mut().enumerate() {
        for (x, slot) in row.iter_mut().enumerate() {
            *slot = ((2 * x + 1) as f64 * u as f64 * std::f64::consts::PI / 16.0).cos();
        }
    }
    table
}

/// Forward 8x8 type-II DCT of a sample block, returned in natural row-major
/// order. Samples are level-shifted by 128 first.
fn forward_dct(block: &[f64; 64], cosines: &[[f64; 8]; 8]) -> [f64; 64] {
    let mut coefficients = [0.0f64; 64];
    let inv_sqrt2 = 1.0 / 2.0f64.sqrt();
    for v in 0..8 {
        for u in 0..8 {
            let mut sum = 0.0f64;
            for y in 0..8 {
                for x in 0..8 {
                    sum += (block[y * 8 + x] - 128.0) * cosines[u][x] * cosines[v][y];
                }
            }
            let cu = if u == 0 { inv_sqrt2 } else { 1.0 };
            let cv = if v == 0 { inv_sqrt2 } else { 1.0 };
            coefficients[v * 8 + u] = 0.25 * cu * cv * sum;
        }
    }
    coefficients
}

/// Encode reduced grayscale (1 channel) or RGB (3 channels) samples as a
/// baseline sequential JFIF file with 4:4:4 sampling.
fn encode_jpeg(
    width: u32,
    height: u32,
    data: &[u8],
    channels: u8,
    dpi: u32,
    quality: u8,
) -> Result<Vec<u8>, ExportError> {
    if width == 0 || height == 0 {
        return Err(ExportError::Encode(
            "JPEG needs non-zero dimensions".to_owned(),
        ));
    }
    let components: usize = match channels {
        1 => 1,
        3 => 3,
        other => {
            return Err(ExportError::Encode(format!(
                "unsupported JPEG channels {other}"
            )));
        }
    };
    let width_usize = usize::try_from(width)
        .map_err(|_| ExportError::Encode("JPEG width does not fit in usize".to_owned()))?;
    let height_usize = usize::try_from(height)
        .map_err(|_| ExportError::Encode("JPEG height does not fit in usize".to_owned()))?;
    let expected = width_usize
        .checked_mul(height_usize)
        .and_then(|area| area.checked_mul(components))
        .ok_or_else(|| ExportError::Encode("JPEG buffer size overflow".to_owned()))?;
    if data.len() != expected {
        return Err(ExportError::Encode(
            "JPEG sample buffer length mismatch".to_owned(),
        ));
    }

    // Build per-component planes: gray uses luma directly; RGB is converted
    // to YCbCr with the ITU-R BT.601 transform (no chroma subsampling).
    let mut planes: Vec<Vec<f64>> = Vec::with_capacity(components);
    if components == 1 {
        planes.push(data.iter().map(|&sample| f64::from(sample)).collect());
    } else {
        let mut y = vec![0.0f64; expected / 3];
        let mut cb = vec![0.0f64; expected / 3];
        let mut cr = vec![0.0f64; expected / 3];
        for (index, &[r8, g8, b8]) in data.as_chunks::<3>().0.iter().enumerate() {
            let r = f64::from(r8);
            let g = f64::from(g8);
            let b = f64::from(b8);
            y[index] = 0.299 * r + 0.587 * g + 0.114 * b;
            cb[index] = -0.168_736 * r - 0.331_264 * g + 0.5 * b + 128.0;
            cr[index] = 0.5 * r - 0.418_688 * g - 0.081_312 * b + 128.0;
        }
        planes.push(y);
        planes.push(cb);
        planes.push(cr);
    }

    let quant_luma = scaled_quant_table(&QUANT_LUMA_STD, quality);
    let quant_chroma = scaled_quant_table(&QUANT_CHROMA_STD, quality);
    let quant_tables: Vec<&[u8; 64]> = if components == 1 {
        vec![&quant_luma]
    } else {
        vec![&quant_luma, &quant_chroma]
    };
    let dc_luma = HuffmanTable::new(&DC_LUMA_BITS, &DC_LUMA_VALUES);
    let ac_luma = HuffmanTable::new(&AC_LUMA_BITS, &AC_LUMA_VALUES);
    let dc_chroma = HuffmanTable::new(&DC_CHROMA_BITS, &DC_CHROMA_VALUES);
    let ac_chroma = HuffmanTable::new(&AC_CHROMA_BITS, &AC_CHROMA_VALUES);
    let cosines = dct_cosines();

    let mut out = Vec::new();
    out.extend_from_slice(&[0xFF, 0xD8]); // SOI

    // APP0/JFIF: version 1.1, density units = dots per inch.
    let mut app0 = Vec::with_capacity(14);
    app0.extend_from_slice(b"JFIF\0");
    app0.extend_from_slice(&[1, 1, 1]);
    let density = u16::try_from(dpi)
        .map_err(|_| ExportError::Encode("JPEG DPI does not fit in a u16".to_owned()))?;
    app0.extend_from_slice(&density.to_be_bytes());
    app0.extend_from_slice(&density.to_be_bytes());
    app0.extend_from_slice(&[0, 0]);
    push_jpeg_segment(&mut out, 0xE0, &app0)?;

    // DQT: luminance table 0, chrominance table 1, values in zig-zag order.
    let mut dqt = Vec::with_capacity(1 + 64 * quant_tables.len());
    for (id, table) in quant_tables.iter().enumerate() {
        dqt.push(id as u8);
        for &position in &ZIGZAG {
            dqt.push(table[position]);
        }
    }
    push_jpeg_segment(&mut out, 0xDB, &dqt)?;

    // SOF0: 8-bit baseline, dimensions, component ids with 4:4:4 sampling.
    // JPEG stores each dimension as a two-byte value (maximum 65535).
    let jpeg_height = u16::try_from(height)
        .map_err(|_| ExportError::Encode("JPEG height exceeds 65535".to_owned()))?;
    let jpeg_width = u16::try_from(width)
        .map_err(|_| ExportError::Encode("JPEG width exceeds 65535".to_owned()))?;
    let mut sof = Vec::with_capacity(6 + 3 * components);
    sof.push(8);
    sof.extend_from_slice(&jpeg_height.to_be_bytes());
    sof.extend_from_slice(&jpeg_width.to_be_bytes());
    sof.push(components as u8);
    for component in 0..components {
        sof.push((component + 1) as u8);
        sof.push(0x11); // 1x1 sampling
        sof.push(if component == 0 { 0 } else { 1 }); // quant table id
    }
    push_jpeg_segment(&mut out, 0xC0, &sof)?;

    // DHT: DC/AC tables for luminance (id 0) and chrominance (id 1).
    let mut dht = Vec::new();
    for (selector, bits, values) in [
        (0x00u8, &DC_LUMA_BITS[..], &DC_LUMA_VALUES[..]),
        (0x10u8, &AC_LUMA_BITS[..], &AC_LUMA_VALUES[..]),
        (0x01u8, &DC_CHROMA_BITS[..], &DC_CHROMA_VALUES[..]),
        (0x11u8, &AC_CHROMA_BITS[..], &AC_CHROMA_VALUES[..]),
    ] {
        dht.push(selector);
        dht.extend_from_slice(bits);
        dht.extend_from_slice(values);
    }
    push_jpeg_segment(&mut out, 0xC4, &dht)?;

    // SOS: scan all components with their DC/AC table selectors.
    let mut sos = Vec::with_capacity(1 + 2 * components + 3);
    sos.push(components as u8);
    for component in 0..components {
        sos.push((component + 1) as u8);
        sos.push(if component == 0 { 0x00 } else { 0x11 });
    }
    sos.extend_from_slice(&[0, 63, 0]); // Ss, Se, Ah/Al
    push_jpeg_segment(&mut out, 0xDA, &sos)?;

    // Entropy-coded data.
    let mut writer = BitWriter::new();
    let mut dc_predictors = vec![0i32; components];
    let blocks_x = width_usize.div_ceil(8);
    let blocks_y = height_usize.div_ceil(8);
    for by in 0..blocks_y {
        for bx in 0..blocks_x {
            for component in 0..components {
                let plane = &planes[component];
                let mut block = [0.0f64; 64];
                for y in 0..8 {
                    let source_y = (by * 8 + y).min(height_usize - 1);
                    for x in 0..8 {
                        let source_x = (bx * 8 + x).min(width_usize - 1);
                        block[y * 8 + x] = plane[source_y * width_usize + source_x];
                    }
                }
                let coefficients = forward_dct(&block, &cosines);
                let table = quant_tables[if component == 0 { 0 } else { 1 }];
                let mut quantized = [0i32; 64];
                for index in 0..64 {
                    quantized[index] =
                        (coefficients[index] / f64::from(table[index])).round() as i32;
                }

                let (dc_table, ac_table) = if component == 0 {
                    (&dc_luma, &ac_luma)
                } else {
                    (&dc_chroma, &ac_chroma)
                };
                let dc_diff = quantized[0] - dc_predictors[component];
                dc_predictors[component] = quantized[0];
                let category = magnitude_category(dc_diff);
                let (dc_value, dc_bits) = magnitude_bits(dc_diff, category);
                writer.write_symbol(dc_table, category, dc_value, dc_bits)?;

                let mut run = 0u8;
                for position in 1..64 {
                    let coefficient = quantized[ZIGZAG[position]];
                    if coefficient == 0 {
                        run += 1;
                        continue;
                    }
                    while run >= 16 {
                        writer.write_symbol(ac_table, 0xF0, 0, 0)?; // ZRL
                        run -= 16;
                    }
                    let size = magnitude_category(coefficient);
                    let (value, bits) = magnitude_bits(coefficient, size);
                    let symbol = (run << 4) | size;
                    writer.write_symbol(ac_table, symbol, value, bits)?;
                    run = 0;
                }
                if run > 0 {
                    writer.write_symbol(ac_table, 0x00, 0, 0)?; // EOB
                }
            }
        }
    }
    out.extend_from_slice(&writer.finish());
    out.extend_from_slice(&[0xFF, 0xD9]); // EOI
    Ok(out)
}

/// Return the value bits and their length for a quantized JPEG coefficient.
///
/// Positive values are written as-is; negative values use the one's-complement
/// convention required by the baseline entropy coder.
fn magnitude_bits(value: i32, category: u8) -> (u32, u8) {
    if category == 0 {
        return (0, 0);
    }
    if value >= 0 {
        (value as u32, category)
    } else {
        (((1i64 << category) - 1 + i64::from(value)) as u32, category)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CancellationToken, PageIndex, Progress, RenderError, Renderer};
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    /// A deterministic, scale-aware renderer used to exercise `export_pages`.
    struct TestRenderer {
        page_count: usize,
        base_width: u32,
        base_height: u32,
        renders: Arc<AtomicUsize>,
    }

    impl TestRenderer {
        fn new(page_count: usize) -> Self {
            Self {
                page_count,
                base_width: 200,
                base_height: 100,
                renders: Arc::new(AtomicUsize::new(0)),
            }
        }

        fn render_count(&self) -> usize {
            self.renders.load(Ordering::SeqCst)
        }
    }

    impl Renderer for TestRenderer {
        fn render_page(
            &self,
            page: PageIndex,
            scale: f32,
            cancellation: &CancellationToken,
            _progress: &mut dyn FnMut(Progress),
        ) -> Result<crate::RenderedPage, RenderError> {
            cancellation.check()?;
            if !scale.is_finite() || scale <= 0.0 {
                return Err(RenderError::InvalidScale(scale));
            }
            if page.get() >= self.page_count {
                return Err(RenderError::PageOutOfRange(page.get()));
            }
            self.renders.fetch_add(1, Ordering::SeqCst);
            let width = (f64::from(self.base_width) * f64::from(scale)).round() as u32;
            let height = (f64::from(self.base_height) * f64::from(scale)).round() as u32;
            let mut pixels = Vec::with_capacity(width as usize * height as usize * 4);
            for y in 0..height {
                for x in 0..width {
                    pixels.push((x % 251) as u8);
                    pixels.push((y % 251) as u8);
                    pixels.push((page.get() % 251) as u8);
                    pixels.push(255);
                }
            }
            Ok(crate::RenderedPage::new(page, width, height, pixels))
        }

        fn page_count(&self) -> Result<usize, RenderError> {
            Ok(self.page_count)
        }
    }

    /// Split a PNG into its `(type, data)` chunks, validating every CRC-32.
    fn png_chunks(bytes: &[u8]) -> Vec<([u8; 4], Vec<u8>)> {
        assert_eq!(&bytes[..8], &PNG_SIGNATURE[..]);
        let mut pos = 8;
        let mut chunks = Vec::new();
        while pos < bytes.len() {
            let length =
                u32::from_be_bytes([bytes[pos], bytes[pos + 1], bytes[pos + 2], bytes[pos + 3]])
                    as usize;
            let mut kind = [0u8; 4];
            kind.copy_from_slice(&bytes[pos + 4..pos + 8]);
            let data = bytes[pos + 8..pos + 8 + length].to_vec();
            let stored_crc = u32::from_be_bytes([
                bytes[pos + 8 + length],
                bytes[pos + 9 + length],
                bytes[pos + 10 + length],
                bytes[pos + 11 + length],
            ]);
            let mut checked = Vec::with_capacity(4 + length);
            checked.extend_from_slice(&kind);
            checked.extend_from_slice(&data);
            assert_eq!(stored_crc, crc32(&checked), "PNG chunk CRC mismatch");
            chunks.push((kind, data));
            pos += 12 + length;
        }
        assert_eq!(pos, bytes.len());
        chunks
    }

    /// Decode the raw scanlines from an IDAT stored-deflate stream.
    fn stored_payload(idat: &[u8]) -> Vec<u8> {
        assert_eq!(&idat[..2], &[0x78u8, 0x01]);
        let mut pos = 2;
        let mut raw = Vec::new();
        loop {
            let header = idat[pos];
            assert_eq!(header & 0x06, 0, "deflate block is not stored");
            let length = u16::from_le_bytes([idat[pos + 1], idat[pos + 2]]);
            let complement = u16::from_le_bytes([idat[pos + 3], idat[pos + 4]]);
            assert_eq!(length ^ 0xFFFF, complement);
            raw.extend_from_slice(&idat[pos + 5..pos + 5 + length as usize]);
            pos += 5 + length as usize;
            if header & 1 == 1 {
                break;
            }
        }
        assert_eq!(
            u32::from_be_bytes([idat[pos], idat[pos + 1], idat[pos + 2], idat[pos + 3]]),
            adler32(&raw)
        );
        assert_eq!(pos + 4, idat.len());
        raw
    }

    /// Parse JPEG marker segments up to (but not including) the entropy data.
    fn jpeg_segments(bytes: &[u8]) -> Vec<(u8, Vec<u8>)> {
        assert_eq!(&bytes[..2], &[0xFF, 0xD8], "JPEG must start with SOI");
        let mut pos = 2;
        let mut segments = Vec::new();
        while pos + 4 <= bytes.len() {
            assert_eq!(bytes[pos], 0xFF, "marker prefix at {pos}");
            let marker = bytes[pos + 1];
            let length = u16::from_be_bytes([bytes[pos + 2], bytes[pos + 3]]) as usize;
            if marker == 0xDA {
                segments.push((marker, bytes[pos + 4..pos + 2 + length].to_vec()));
                break;
            }
            segments.push((marker, bytes[pos + 4..pos + 2 + length].to_vec()));
            pos += 2 + length;
        }
        segments
    }

    fn read_u16_le(bytes: &[u8], offset: usize) -> u16 {
        u16::from_le_bytes([bytes[offset], bytes[offset + 1]])
    }

    fn read_u32_le(bytes: &[u8], offset: usize) -> u32 {
        u32::from_le_bytes([
            bytes[offset],
            bytes[offset + 1],
            bytes[offset + 2],
            bytes[offset + 3],
        ])
    }

    /// One parsed TIFF IFD entry: `(tag, field type, count, inline value)`.
    type IfdEntry = (u16, u16, u32, [u8; 4]);

    /// Parse one TIFF IFD into its entries and the offset of the following IFD
    /// (zero for the last page).
    fn tiff_ifd(bytes: &[u8], offset: usize) -> (Vec<IfdEntry>, u32) {
        let count = read_u16_le(bytes, offset);
        let mut entries = Vec::new();
        for index in 0..count as usize {
            let base = offset + 2 + 12 * index;
            let mut value = [0u8; 4];
            value.copy_from_slice(&bytes[base + 8..base + 12]);
            entries.push((
                read_u16_le(bytes, base),
                read_u16_le(bytes, base + 2),
                read_u32_le(bytes, base + 4),
                value,
            ));
        }
        let next = read_u32_le(bytes, offset + 2 + 12 * count as usize);
        (entries, next)
    }

    fn tiff_entry(entries: &[IfdEntry], tag: u16) -> &IfdEntry {
        entries
            .iter()
            .find(|entry| entry.0 == tag)
            .expect("tag present")
    }

    fn red_green() -> Vec<u8> {
        vec![255, 0, 0, 255, 0, 255, 0, 255]
    }

    fn export_with(renderer: &TestRenderer, options: &ExportOptions) -> Vec<ExportedImage> {
        export_pages(renderer, options, &CancellationToken::new(), &mut |_| {})
            .expect("export succeeds")
    }

    #[test]
    fn dpi_scales_raster_dimensions_and_png_ihdr() {
        for (dpi, expected) in [(72, (200, 100)), (144, (400, 200)), (36, (100, 50))] {
            let renderer = TestRenderer::new(1);
            let options = ExportOptions {
                dpi,
                ..ExportOptions::default()
            };
            let images = export_with(&renderer, &options);
            assert_eq!(images.len(), 1);
            assert_eq!((images[0].width, images[0].height), expected);
            assert_eq!(images[0].dpi, dpi);

            let chunks = png_chunks(&images[0].bytes);
            let ihdr = &chunks[0].1;
            assert_eq!(&ihdr[..4], &expected.0.to_be_bytes());
            assert_eq!(&ihdr[4..8], &expected.1.to_be_bytes());
        }
    }

    #[test]
    fn page_range_expands_to_sorted_zero_based_pages() {
        let renderer = TestRenderer::new(6);
        let options = ExportOptions {
            selection: PageSelection::parse("5,2,3", 6).expect("valid range"),
            ..ExportOptions::default()
        };
        let images = export_with(&renderer, &options);
        let pages: Vec<usize> = images.iter().map(|image| image.page.get()).collect();
        assert_eq!(pages, vec![1, 2, 4]);
        assert_eq!(renderer.render_count(), 3);
    }

    #[test]
    fn png_has_signature_ordered_chunks_and_pixel_round_trip() {
        let pixels = red_green();
        let bytes = encode_image(2, 1, &pixels, &ExportOptions::default()).expect("PNG");
        let chunks = png_chunks(&bytes);
        let kinds: Vec<&[u8; 4]> = chunks.iter().map(|(kind, _)| kind).collect();
        assert_eq!(kinds, vec![b"IHDR", b"pHYs", b"IDAT", b"IEND"]);

        let ihdr = &chunks[0].1;
        assert_eq!(ihdr[8], 8, "bit depth");
        assert_eq!(ihdr[9], 6, "RGBA colour type");
        assert_eq!(&ihdr[10..13], &[0, 0, 0]);

        let raw = stored_payload(&chunks[2].1);
        let mut expected = vec![0u8];
        expected.extend_from_slice(&pixels);
        assert_eq!(
            raw, expected,
            "filter-0 scanline must round-trip the pixels"
        );

        let phys = &chunks[1].1;
        let pixels_per_metre = u32::from_be_bytes([phys[0], phys[1], phys[2], phys[3]]);
        let expected_ppm = (f64::from(DEFAULT_EXPORT_DPI) / METRES_PER_INCH).round() as u32;
        assert_eq!(pixels_per_metre, expected_ppm, "pHYs records the DPI");
        assert_eq!(phys[8], 1, "pHYs unit is metre");
    }

    #[test]
    fn grayscale_rgb_rgba_produce_expected_channels_and_luma() {
        let pixels = red_green();
        for (color, channels, color_type) in [
            (ColorMode::Grayscale, 1u8, 0u8),
            (ColorMode::Rgb, 3u8, 2u8),
            (ColorMode::Rgba, 4u8, 6u8),
        ] {
            let options = ExportOptions {
                color,
                alpha: AlphaPolicy::Flatten {
                    background: [0, 0, 0],
                },
                ..ExportOptions::default()
            };
            let bytes = encode_image(2, 1, &pixels, &options).expect("encode");
            let chunks = png_chunks(&bytes);
            assert_eq!(chunks[0].1[9], color_type, "IHDR colour type for {color:?}");
            let raw = stored_payload(&chunks[2].1);
            // One filter byte per row (height == 1) plus two pixels' samples.
            assert_eq!(raw.len(), 1 + 2 * channels as usize);
        }

        // Grayscale luma of opaque red (0.299*255 ~ 76) and green (0.587*255 ~ 150).
        let options = ExportOptions {
            color: ColorMode::Grayscale,
            alpha: AlphaPolicy::Flatten {
                background: [0, 0, 0],
            },
            ..ExportOptions::default()
        };
        let bytes = encode_image(2, 1, &pixels, &options).expect("gray");
        let raw = stored_payload(&png_chunks(&bytes)[2].1);
        assert_eq!(raw[1], 76);
        assert_eq!(raw[2], 150);
    }

    #[test]
    fn alpha_flatten_composites_onto_the_background() {
        // Fully transparent red over a white background must become white.
        let transparent_red = [255u8, 0, 0, 0];
        let options = ExportOptions {
            color: ColorMode::Rgb,
            alpha: AlphaPolicy::Flatten {
                background: [255, 255, 255],
            },
            ..ExportOptions::default()
        };
        let bytes = encode_image(1, 1, &transparent_red, &options).expect("flatten");
        let raw = stored_payload(&png_chunks(&bytes)[2].1);
        assert_eq!(&raw[1..], &[255, 255, 255]);

        // Half-transparent black over white yields ~127.
        let half_black = [0u8, 0, 0, 128];
        let bytes = encode_image(1, 1, &half_black, &options).expect("flatten");
        let raw = stored_payload(&png_chunks(&bytes)[2].1);
        assert_eq!(&raw[1..], &[127, 127, 127]);
    }

    #[test]
    fn jpeg_header_quality_and_markers_are_valid() {
        let options = ExportOptions {
            format: ImageFormat::Jpeg,
            color: ColorMode::Rgb,
            alpha: AlphaPolicy::Flatten {
                background: [0, 0, 0],
            },
            dpi: 300,
            quality: 90,
            ..ExportOptions::default()
        };
        let bytes = encode_image(3, 2, &[120u8; 24], &options).expect("JPEG");
        assert_eq!(&bytes[bytes.len() - 2..], &[0xFF, 0xD9], "EOI");
        let segments = jpeg_segments(&bytes);
        let markers: Vec<u8> = segments.iter().map(|(marker, _)| *marker).collect();
        assert_eq!(markers, vec![0xE0, 0xDB, 0xC0, 0xC4, 0xDA]);

        // APP0/JFIF density must record the requested DPI.
        let app0 = &segments[0].1;
        assert_eq!(&app0[..5], b"JFIF\0");
        assert_eq!(app0[7], 1, "density units = dots per inch");
        assert_eq!(u16::from_be_bytes([app0[8], app0[9]]), 300);
        assert_eq!(u16::from_be_bytes([app0[10], app0[11]]), 300);

        // SOF0: precision 8, 3x2 dimensions, 3 components.
        let sof = &segments[2].1;
        assert_eq!(sof[0], 8);
        assert_eq!(u16::from_be_bytes([sof[1], sof[2]]), 2);
        assert_eq!(u16::from_be_bytes([sof[3], sof[4]]), 3);
        assert_eq!(sof[5], 3, "three 4:4:4 components");

        // DQT: the first luminance entry shrinks as quality increases.
        let dqt_high = &segments[1].1;
        let low = ExportOptions {
            quality: 10,
            ..options.clone()
        };
        let bytes_low = encode_image(3, 2, &[120u8; 24], &low).expect("low JPEG");
        let dqt_low = &jpeg_segments(&bytes_low)[1].1;
        assert!(u32::from(dqt_low[1]) > u32::from(dqt_high[1]));

        // Grayscale JPEG declares one component.
        let gray = ExportOptions {
            color: ColorMode::Grayscale,
            ..options.clone()
        };
        let gray_bytes = encode_image(2, 2, &[10u8; 16], &gray).expect("gray JPEG");
        assert_eq!(jpeg_segments(&gray_bytes)[2].1[5], 1);
    }

    #[test]
    fn jpeg_quality_is_validated() {
        for quality in [0u8, 101] {
            let options = ExportOptions {
                format: ImageFormat::Jpeg,
                color: ColorMode::Rgb,
                quality,
                ..ExportOptions::default()
            };
            assert_eq!(
                options.validate(),
                Err(ExportError::InvalidQuality(quality))
            );
            assert_eq!(
                encode_image(1, 1, &[0u8; 4], &options),
                Err(ExportError::InvalidQuality(quality))
            );
        }
    }

    #[test]
    fn tiff_multipage_offsets_and_resolution_tags_are_consistent() {
        let first = ReducedRaster {
            page: PageIndex::new(0),
            width: 2,
            height: 1,
            data: vec![255, 0, 0, 255, 0, 255],
            channels: 3,
        };
        let second = ReducedRaster {
            page: PageIndex::new(1),
            width: 1,
            height: 1,
            data: vec![7, 8, 9],
            channels: 3,
        };
        let bytes = encode_tiff(&[first, second], 300).expect("multi-page TIFF");
        assert_eq!(&bytes[..2], b"II");
        assert_eq!(read_u16_le(&bytes, 2), 42);
        let first_ifd = read_u32_le(&bytes, 4) as usize;
        assert!(
            first_ifd > 8 && first_ifd < bytes.len(),
            "first IFD offset must point inside the file, past page 0's strip"
        );

        let (entries0, next0) = tiff_ifd(&bytes, first_ifd);
        assert!(next0 > 0, "first IFD must link to the second");
        let (entries1, next1) = tiff_ifd(&bytes, next0 as usize);
        assert_eq!(next1, 0, "last IFD terminates the chain");

        // Tags are in ascending order in every IFD.
        for entries in [&entries0, &entries1] {
            let tags: Vec<u16> = entries.iter().map(|entry| entry.0).collect();
            assert!(
                tags.windows(2).all(|pair| pair[0] < pair[1]),
                "TIFF tags sorted"
            );
        }

        // Strip offsets/counts and the RGB/luma metadata are present.
        assert_eq!(u32::from_le_bytes(tiff_entry(&entries0, 256).3), 2); // width
        assert_eq!(u32::from_le_bytes(tiff_entry(&entries0, 257).3), 1); // height
        assert_eq!(
            u16::from_le_bytes([
                tiff_entry(&entries0, 277).3[0],
                tiff_entry(&entries0, 277).3[1]
            ]),
            3
        );
        assert_eq!(
            u16::from_le_bytes([
                tiff_entry(&entries0, 259).3[0],
                tiff_entry(&entries0, 259).3[1]
            ]),
            1
        ); // no compression
        assert_eq!(
            u16::from_le_bytes([
                tiff_entry(&entries0, 296).3[0],
                tiff_entry(&entries0, 296).3[1]
            ]),
            2
        ); // inch
        assert!(
            !entries0.iter().any(|entry| entry.0 == 338),
            "RGB has no ExtraSamples tag"
        );

        // XResolution rational at its offset must be 300/1.
        let xres_offset = u32::from_le_bytes(tiff_entry(&entries0, 282).3) as usize;
        assert_eq!(read_u32_le(&bytes, xres_offset), 300);
        assert_eq!(read_u32_le(&bytes, xres_offset + 4), 1);

        // The strip offsets point at the actual pixel bytes.
        let strip0 = u32::from_le_bytes(tiff_entry(&entries0, 273).3) as usize;
        assert_eq!(&bytes[strip0..strip0 + 6], &[255, 0, 0, 255, 0, 255]);
        let strip1 = u32::from_le_bytes(tiff_entry(&entries1, 273).3) as usize;
        assert_eq!(&bytes[strip1..strip1 + 3], &[7, 8, 9]);
    }

    #[test]
    fn tiff_rgba_declares_extra_samples() {
        let raster = ReducedRaster {
            page: PageIndex::new(0),
            width: 1,
            height: 1,
            data: vec![1, 2, 3, 4],
            channels: 4,
        };
        let bytes = encode_tiff(&[raster], 72).expect("RGBA TIFF");
        let first_ifd = read_u32_le(&bytes, 4) as usize;
        let (entries, _) = tiff_ifd(&bytes, first_ifd);
        assert_eq!(tiff_entry(&entries, 338).0, 338, "RGBA needs ExtraSamples");
        assert_eq!(
            u16::from_le_bytes([
                tiff_entry(&entries, 338).3[0],
                tiff_entry(&entries, 338).3[1]
            ]),
            2
        );
        assert_eq!(
            u16::from_le_bytes([
                tiff_entry(&entries, 277).3[0],
                tiff_entry(&entries, 277).3[1]
            ]),
            4
        );
    }

    #[test]
    fn cancellation_stops_export_before_and_between_pages() {
        let renderer = TestRenderer::new(3);
        let cancelled = CancellationToken::new();
        cancelled.cancel();
        let result = export_pages(
            &renderer,
            &ExportOptions::default(),
            &cancelled,
            &mut |_| {},
        );
        assert_eq!(result, Err(ExportError::Cancelled));
        assert_eq!(renderer.render_count(), 0);

        let renderer = TestRenderer::new(3);
        let token = CancellationToken::new();
        let trigger = token.clone();
        let result = export_pages(
            &renderer,
            &ExportOptions::default(),
            &token,
            &mut |progress| {
                if progress.completed == 1 {
                    trigger.cancel();
                }
            },
        );
        assert_eq!(result, Err(ExportError::Cancelled));
        assert_eq!(
            renderer.render_count(),
            1,
            "no page renders after cancellation"
        );
    }

    #[test]
    fn invalid_dpi_fit_and_alpha_policy_are_rejected() {
        for dpi in [0u32, MAX_EXPORT_DPI + 1] {
            let options = ExportOptions {
                dpi,
                ..ExportOptions::default()
            };
            assert_eq!(options.validate(), Err(ExportError::InvalidDpi(dpi)));
        }
        let bad_fit = ExportOptions {
            fit: FitMode::FitWithin {
                width: 0,
                height: 10,
            },
            ..ExportOptions::default()
        };
        assert_eq!(bad_fit.validate(), Err(ExportError::InvalidFit));
        let nan_fit = ExportOptions {
            fit: FitMode::Scale(f32::NAN),
            ..ExportOptions::default()
        };
        assert!(matches!(
            nan_fit.validate(),
            Err(ExportError::InvalidScale(_))
        ));

        // Preserving alpha is impossible for JPEG or non-RGBA colour.
        let jpeg = ExportOptions {
            format: ImageFormat::Jpeg,
            ..ExportOptions::default()
        };
        assert!(matches!(
            jpeg.validate(),
            Err(ExportError::AlphaNotSupported(..))
        ));
        let rgb = ExportOptions {
            color: ColorMode::Rgb,
            ..ExportOptions::default()
        };
        assert!(matches!(
            rgb.validate(),
            Err(ExportError::AlphaNotSupported(..))
        ));
        assert_eq!(ImageFormat::from_extension("pdf"), None);
        assert_eq!(ImageFormat::from_extension(".JPG"), Some(ImageFormat::Jpeg));
    }

    #[test]
    fn mismatched_raster_buffer_is_rejected() {
        let options = ExportOptions::default();
        assert_eq!(
            encode_image(2, 2, &[0u8; 8], &options),
            Err(ExportError::InvalidRaster(2, 2))
        );
    }

    #[test]
    fn png_and_tiff_bytes_are_deterministic() {
        let pixels = vec![42u8; 8 * 6 * 4];
        let options = ExportOptions::default();
        let png_a = encode_image(8, 6, &pixels, &options).expect("png a");
        let png_b = encode_image(8, 6, &pixels, &options).expect("png b");
        assert_eq!(png_a, png_b, "PNG output must be byte-stable");

        let tiff_options = ExportOptions {
            format: ImageFormat::Tiff,
            ..ExportOptions::default()
        };
        let tiff_a = encode_image(8, 6, &pixels, &tiff_options).expect("tiff a");
        let tiff_b = encode_image(8, 6, &pixels, &tiff_options).expect("tiff b");
        assert_eq!(tiff_a, tiff_b, "TIFF output must be byte-stable");

        // Multi-page export is deterministic too, including IFD offsets.
        let renderer = TestRenderer::new(3);
        assert_eq!(
            export_with(&renderer, &tiff_options)[0].bytes,
            export_with(&renderer, &tiff_options)[0].bytes
        );
    }

    #[test]
    fn fit_within_produces_a_bounded_raster() {
        let renderer = TestRenderer::new(1);
        let options = ExportOptions {
            dpi: 72,
            fit: FitMode::FitWithin {
                width: 100,
                height: 80,
            },
            ..ExportOptions::default()
        };
        let images = export_with(&renderer, &options);
        // Base 200x100 at 72 DPI fits inside 100x80 by halving (width-bound)
        // to 100x50. The height constraint (factor 0.8) is the looser one.
        assert_eq!((images[0].width, images[0].height), (100, 50));
    }

    #[test]
    fn export_batches_tiff_pages_and_emits_one_file_per_png_page() {
        let renderer = TestRenderer::new(3);
        let tiff = export_with(
            &renderer,
            &ExportOptions {
                format: ImageFormat::Tiff,
                ..ExportOptions::default()
            },
        );
        assert_eq!(tiff.len(), 1);
        assert_eq!(tiff[0].page_count, 3);
        assert_eq!(tiff[0].page, PageIndex::new(0));

        let png = export_with(&renderer, &ExportOptions::default());
        assert_eq!(png.len(), 3);
        assert!(png.iter().all(|image| image.page_count == 1));
    }
}
