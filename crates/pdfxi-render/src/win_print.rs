//! Windows GDI print backend: native print dialog, rasters, and the device DC.
//!
//! This module is the one place in the workspace that talks to a printer. It
//! renders each page through a [`Renderer`] at the printer's own resolution,
//! converts the RGBA raster to a top-down 32-bit DIB, and sends it with
//! `StretchDIBits` after `StartDocW`/`StartPage`.
//!
//! The Win32 entry points are declared directly. That keeps the dependency
//! graph unchanged and confines `unsafe` to this module; the crate keeps
//! `deny(unsafe_code)` everywhere else and this module overrides it.
//!
//! Non-Windows builds compile the same public API and return
//! [`PrintBackendError::Unsupported`], so callers need no platform branches.

#![allow(unsafe_code)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use crate::{CancellationToken, Orientation, PageIndex, PageScaleMode, Renderer};

/// Errors returned by the print backend.
#[derive(Debug, thiserror::Error)]
pub enum PrintBackendError {
    /// This build or platform has no print backend.
    #[error("printing is not supported on this platform")]
    Unsupported,
    /// The document permissions deny printing.
    #[error("this document's permissions do not allow printing")]
    PermissionDenied,
    /// The printer driver could not create a device context.
    #[error("no printer named \"{0}\" is available")]
    NoDevice(String),
    /// The user cancelled the native print dialog.
    #[error("the print dialog was cancelled")]
    Cancelled,
    /// The native dialog failed with a Common Dialog error code.
    #[error("the print dialog failed (Common Dialog error {0})")]
    Dialog(u32),
    /// The job could not be started on the device context.
    #[error("the print job could not start (Win32 error {0})")]
    StartDoc(u32),
    /// The driver reported no usable printable area.
    #[error("the printer reported no usable printable area")]
    NoPrintableArea,
    /// The document has no pages.
    #[error("the document has no pages to print")]
    EmptyDocument,
    /// A page failed while being sent to the driver.
    #[error("a page could not be printed (Win32 error {0})")]
    Page(u32),
    /// The renderer did not report a page size.
    #[error("the page size is not available: {0}")]
    NoPageSize(String),
    /// The page selection resolved to no pages.
    #[error("no pages were selected for printing")]
    EmptySelection,
    /// The computed placement was not usable.
    #[error("invalid print placement: {0}")]
    Placement(String),
    /// Rendering a page failed.
    #[error("document rendering failed: {0}")]
    Render(#[from] crate::RenderError),
}

/// Physical geometry reported by a printer device context.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PrinterMetrics {
    /// Horizontal resolution in dots per inch (`LOGPIXELSX`).
    pub dpi_x: u32,
    /// Vertical resolution in dots per inch (`LOGPIXELSY`).
    pub dpi_y: u32,
    /// Printable width in device pixels (`HORZRES`).
    pub printable_width_px: i32,
    /// Printable height in device pixels (`VERTRES`).
    pub printable_height_px: i32,
}

impl PrinterMetrics {
    /// Validate that the driver reported a usable area and resolution.
    ///
    /// # Errors
    ///
    /// Returns [`PrintBackendError::NoPrintableArea`] for zero DPI or a
    /// non-positive printable area.
    pub fn validate(&self) -> Result<(), PrintBackendError> {
        if self.dpi_x == 0 || self.dpi_y == 0 {
            return Err(PrintBackendError::NoPrintableArea);
        }
        if self.printable_width_px <= 0 || self.printable_height_px <= 0 {
            return Err(PrintBackendError::NoPrintableArea);
        }
        Ok(())
    }
}

/// Where one source page's raster lands on the printable area.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PagePlacement {
    /// Renderer scale (1.0 is 72 DPI, matching [`Renderer::render_page`]).
    pub render_scale: f32,
    /// Destination left edge in device pixels.
    pub dest_x: i32,
    /// Destination top edge in device pixels.
    pub dest_y: i32,
    /// Destination width in device pixels.
    pub dest_width: i32,
    /// Destination height in device pixels.
    pub dest_height: i32,
    /// The raster must be rotated 90 degrees clockwise before drawing.
    pub rotate_clockwise: bool,
}

/// Compute where a page lands on a printer.
///
/// The page is rendered at `render_scale` (points to pixels) and centered in
/// the printable area. [`PageScaleMode::Fit`] chooses the smaller of the
/// horizontal and vertical ratios; actual size and custom percentages keep
/// physical size and may crop, matching the driver's own behaviour. Landscape
/// rotates a portrait page so the paper's long edge is horizontal.
///
/// # Errors
///
/// Returns [`PrintBackendError::NoPrintableArea`] for unusable metrics and
/// [`PrintBackendError::Placement`] for non-finite or non-positive geometry.
pub fn compute_placement(
    page_width_pt: f32,
    page_height_pt: f32,
    metrics: PrinterMetrics,
    mode: PageScaleMode,
    orientation: Orientation,
) -> Result<PagePlacement, PrintBackendError> {
    metrics.validate()?;
    if !page_width_pt.is_finite()
        || !page_height_pt.is_finite()
        || page_width_pt <= 0.0
        || page_height_pt <= 0.0
    {
        return Err(PrintBackendError::Placement(
            "page dimensions must be finite and positive".to_owned(),
        ));
    }

    let rotate_clockwise = orientation == Orientation::Landscape && page_width_pt < page_height_pt;
    let (width_pt, height_pt) = if rotate_clockwise {
        (page_height_pt, page_width_pt)
    } else {
        (page_width_pt, page_height_pt)
    };

    let base_width = f64::from(width_pt) / 72.0 * f64::from(metrics.dpi_x);
    let base_height = f64::from(height_pt) / 72.0 * f64::from(metrics.dpi_y);
    if base_width <= 0.0 || base_height <= 0.0 {
        return Err(PrintBackendError::Placement(
            "page does not occupy a positive pixel area".to_owned(),
        ));
    }

    let factor = match mode {
        PageScaleMode::Actual => 1.0,
        PageScaleMode::CustomPercent(percent) => {
            if !percent.is_finite() || percent <= 0.0 {
                return Err(PrintBackendError::Placement(format!(
                    "custom percent {percent} is not usable"
                )));
            }
            f64::from(percent) / 100.0
        }
        PageScaleMode::Fit => (f64::from(metrics.printable_width_px) / base_width)
            .min(f64::from(metrics.printable_height_px) / base_height),
    };
    if !factor.is_finite() || factor <= 0.0 {
        return Err(PrintBackendError::Placement(
            "print scale is not finite and positive".to_owned(),
        ));
    }

    let mut dest_width = (base_width * factor).round().max(1.0) as i32;
    let mut dest_height = (base_height * factor).round().max(1.0) as i32;
    if mode == PageScaleMode::Fit {
        dest_width = dest_width.min(metrics.printable_width_px);
        dest_height = dest_height.min(metrics.printable_height_px);
    }
    let dest_x = (metrics.printable_width_px - dest_width) / 2;
    let dest_y = (metrics.printable_height_px - dest_height) / 2;

    let render_scale = dest_width as f32 / width_pt;
    if !render_scale.is_finite() || render_scale <= 0.0 {
        return Err(PrintBackendError::Placement(
            "render scale is not finite and positive".to_owned(),
        ));
    }
    Ok(PagePlacement {
        render_scale,
        dest_x,
        dest_y,
        dest_width,
        dest_height,
        rotate_clockwise,
    })
}

/// The page choice returned by the native dialog.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DialogPageChoice {
    /// The "All" radio button was selected.
    pub all: bool,
    /// The "Current page" radio button was selected.
    pub current: bool,
    /// The "Pages" radio button was selected.
    pub page_numbers: bool,
    /// First page in the range, 1-based.
    pub from: u16,
    /// Last page in the range, 1-based.
    pub to: u16,
}

/// Resolve the native dialog's choice to zero-based pages.
///
/// Ranges are clamped to the document and reversed bounds are swapped, so a
/// stale dialog cannot produce an invalid page. "Current page" is clamped too.
///
/// # Errors
///
/// Returns [`PrintBackendError::EmptySelection`] when the choice resolves to no
/// pages and [`PrintBackendError::EmptyDocument`] for a zero-page document.
pub fn resolve_dialog_pages(
    choice: DialogPageChoice,
    page_count: usize,
    current_page: usize,
) -> Result<Vec<usize>, PrintBackendError> {
    if page_count == 0 {
        return Err(PrintBackendError::EmptyDocument);
    }
    if choice.current {
        return Ok(vec![current_page.min(page_count - 1)]);
    }
    if choice.page_numbers {
        let from = usize::from(choice.from.max(1)).min(page_count);
        let to = usize::from(choice.to.max(1)).min(page_count);
        let (start, end) = if from <= to { (from, to) } else { (to, from) };
        return Ok((start - 1..end).collect());
    }
    // `all` is the default when the dialog returns no page flag.
    Ok((0..page_count).collect())
}

/// Result of a print job.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrintOutcome {
    /// Number of page images sent to the driver, copies included.
    pub pages_printed: usize,
    /// Printer name, or a description for the native dialog path.
    pub printer: String,
    /// Output file for the print-to-file path.
    pub output: Option<PathBuf>,
}

/// Print using the native Windows print dialog.
///
/// The dialog supplies printer, page range, copies and paper. `seed_from`,
/// `seed_to` and `seed_copies` set its initial state; `scale` and `orientation`
/// are applied while rasterizing because the dialog does not expose them.
///
/// # Errors
///
/// Returns [`PrintBackendError::Cancelled`] when the user cancels, plus the
/// device, dialog and driver errors above.
#[cfg(windows)]
#[allow(clippy::too_many_arguments)]
pub fn print_with_dialog(
    renderer: &dyn Renderer,
    page_count: usize,
    current_page: usize,
    seed_from: u16,
    seed_to: u16,
    seed_copies: u16,
    scale: PageScaleMode,
    orientation: Orientation,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(usize, usize),
) -> Result<PrintOutcome, PrintBackendError> {
    if page_count == 0 {
        return Err(PrintBackendError::EmptyDocument);
    }
    let mut dialog: win::PrintDlgW = unsafe { std::mem::zeroed() };
    dialog.lStructSize = std::mem::size_of::<win::PrintDlgW>() as u32;
    dialog.Flags = win::PD_ALL_PAGES
        | win::PD_PAGE_NUMBERS
        | win::PD_CURRENT_PAGE
        | win::PD_NO_SELECTION
        | win::PD_RETURN_DC;
    dialog.nMinPage = 1;
    dialog.nMaxPage = u16::try_from(page_count).unwrap_or(u16::MAX);
    dialog.nFromPage = seed_from.clamp(1, dialog.nMaxPage);
    dialog.nToPage = seed_to.clamp(1, dialog.nMaxPage);
    dialog.nCopies = seed_copies.max(1);

    let accepted = unsafe { win::PrintDlgW(&mut dialog) };
    if accepted == 0 {
        let code = unsafe { win::CommDlgExtendedError() };
        free_dialog_handles(&dialog);
        return if code == 0 {
            Err(PrintBackendError::Cancelled)
        } else {
            Err(PrintBackendError::Dialog(code))
        };
    }

    let hdc = dialog.hDC;
    if hdc.is_null() {
        free_dialog_handles(&dialog);
        return Err(PrintBackendError::NoDevice(
            "the print dialog returned no device context".to_owned(),
        ));
    }
    let metrics = unsafe { device_metrics(hdc) };
    if let Err(error) = metrics.validate() {
        free_dialog_handles(&dialog);
        unsafe { win::DeleteDC(hdc) };
        return Err(error);
    }

    let choice = DialogPageChoice {
        all: dialog.Flags & (win::PD_PAGE_NUMBERS | win::PD_CURRENT_PAGE) == 0,
        current: dialog.Flags & win::PD_CURRENT_PAGE != 0,
        page_numbers: dialog.Flags & win::PD_PAGE_NUMBERS != 0,
        from: dialog.nFromPage,
        to: dialog.nToPage,
    };
    let pages = resolve_dialog_pages(choice, page_count, current_page)?;
    let copies = dialog.nCopies.max(1);

    let doc_name = wide("PDFXI document");
    let info = win::DocInfoW {
        cbSize: std::mem::size_of::<win::DocInfoW>() as i32,
        lpszDocName: doc_name.as_ptr(),
        lpszOutput: std::ptr::null(),
        lpszDatatype: std::ptr::null(),
        fwType: 0,
    };
    let started = unsafe { win::StartDocW(hdc, &info) };
    if started <= 0 {
        let code = unsafe { win::GetLastError() };
        free_dialog_handles(&dialog);
        unsafe { win::DeleteDC(hdc) };
        return Err(PrintBackendError::StartDoc(code));
    }

    let result = print_pages(
        hdc,
        metrics,
        renderer,
        &pages,
        copies,
        scale,
        orientation,
        cancel,
        progress,
    );
    match &result {
        Ok(_) => unsafe {
            win::EndDoc(hdc);
        },
        Err(_) => unsafe {
            win::AbortDoc(hdc);
        },
    };
    free_dialog_handles(&dialog);
    unsafe { win::DeleteDC(hdc) };
    let pages_printed = result?;
    Ok(PrintOutcome {
        pages_printed,
        printer: "system print dialog".to_owned(),
        output: None,
    })
}

/// Print to a named printer, writing to `output` when the driver supports it.
///
/// This is the non-interactive path used by tests and by print-to-file. The
/// Microsoft Print to PDF driver writes the PDF to `output` after `EndDoc`.
///
/// # Errors
///
/// Returns the device, driver, rendering and I/O errors above.
#[cfg(windows)]
#[allow(clippy::too_many_arguments)]
pub fn print_to_file(
    renderer: &dyn Renderer,
    printer: &str,
    output: &Path,
    pages: &[usize],
    copies: u16,
    scale: PageScaleMode,
    orientation: Orientation,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(usize, usize),
) -> Result<PrintOutcome, PrintBackendError> {
    if pages.is_empty() {
        return Err(PrintBackendError::EmptySelection);
    }
    let driver = wide("WINSPOOL");
    let device = wide(printer);
    let hdc = unsafe {
        win::CreateDCW(
            driver.as_ptr(),
            device.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
        )
    };
    if hdc.is_null() {
        return Err(PrintBackendError::NoDevice(printer.to_owned()));
    }
    let metrics = unsafe { device_metrics(hdc) };
    if let Err(error) = metrics.validate() {
        unsafe { win::DeleteDC(hdc) };
        return Err(error);
    }

    let doc_name = wide("PDFXI document");
    let output_wide = wide(&output.to_string_lossy());
    let info = win::DocInfoW {
        cbSize: std::mem::size_of::<win::DocInfoW>() as i32,
        lpszDocName: doc_name.as_ptr(),
        lpszOutput: output_wide.as_ptr(),
        lpszDatatype: std::ptr::null(),
        fwType: 0,
    };
    let started = unsafe { win::StartDocW(hdc, &info) };
    if started <= 0 {
        let code = unsafe { win::GetLastError() };
        unsafe { win::DeleteDC(hdc) };
        return Err(PrintBackendError::StartDoc(code));
    }

    let result = print_pages(
        hdc,
        metrics,
        renderer,
        pages,
        copies.max(1),
        scale,
        orientation,
        cancel,
        progress,
    );
    match &result {
        Ok(_) => unsafe {
            win::EndDoc(hdc);
        },
        Err(_) => unsafe {
            win::AbortDoc(hdc);
        },
    };
    unsafe { win::DeleteDC(hdc) };
    let pages_printed = result?;
    Ok(PrintOutcome {
        pages_printed,
        printer: printer.to_owned(),
        output: Some(output.to_path_buf()),
    })
}

/// Print stubs for platforms without a GDI backend.
///
/// # Errors
///
/// Always returns [`PrintBackendError::Unsupported`].
#[cfg(not(windows))]
#[allow(clippy::too_many_arguments)]
pub fn print_with_dialog(
    _renderer: &dyn Renderer,
    _page_count: usize,
    _current_page: usize,
    _seed_from: u16,
    _seed_to: u16,
    _seed_copies: u16,
    _scale: PageScaleMode,
    _orientation: Orientation,
    _cancel: &AtomicBool,
    _progress: &mut dyn FnMut(usize, usize),
) -> Result<PrintOutcome, PrintBackendError> {
    Err(PrintBackendError::Unsupported)
}

/// Print stubs for platforms without a GDI backend.
///
/// # Errors
///
/// Always returns [`PrintBackendError::Unsupported`].
#[cfg(not(windows))]
#[allow(clippy::too_many_arguments)]
pub fn print_to_file(
    _renderer: &dyn Renderer,
    _printer: &str,
    _output: &Path,
    _pages: &[usize],
    _copies: u16,
    _scale: PageScaleMode,
    _orientation: Orientation,
    _cancel: &AtomicBool,
    _progress: &mut dyn FnMut(usize, usize),
) -> Result<PrintOutcome, PrintBackendError> {
    Err(PrintBackendError::Unsupported)
}

/// Send every page copy to the started GDI document.
#[cfg(windows)]
#[allow(clippy::too_many_arguments)]
fn print_pages(
    hdc: win::Hdc,
    metrics: PrinterMetrics,
    renderer: &dyn Renderer,
    pages: &[usize],
    copies: u16,
    scale: PageScaleMode,
    orientation: Orientation,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(usize, usize),
) -> Result<usize, PrintBackendError> {
    const HALFTONE: i32 = 4;
    const SRCCOPY: u32 = 0x00CC_0020;
    unsafe {
        win::SetStretchBltMode(hdc, HALFTONE);
    }
    let total = pages.len().saturating_mul(usize::from(copies));
    let mut done = 0usize;
    for _ in 0..copies {
        for &page in pages {
            if cancel.load(Ordering::Relaxed) {
                return Err(PrintBackendError::Cancelled);
            }
            let index = PageIndex::new(page);
            let cancellation = CancellationToken::new();
            // Measure at 72 DPI, where one pixel is one point. The trait keeps
            // page geometry optional, and measuring through a render works for
            // every backend without a separate geometry API.
            let measured = renderer.render_page(index, 1.0, &cancellation, &mut |_| {})?;
            if measured.width() == 0 || measured.height() == 0 {
                return Err(PrintBackendError::NoPageSize(
                    "the page measured zero pixels".to_owned(),
                ));
            }
            let placement = compute_placement(
                measured.width() as f32,
                measured.height() as f32,
                metrics,
                scale,
                orientation,
            )?;
            let rendered = if (placement.render_scale - 1.0).abs() <= f32::EPSILON {
                measured
            } else {
                renderer.render_page(index, placement.render_scale, &cancellation, &mut |_| {})?
            };
            if rendered.width() == 0 || rendered.height() == 0 {
                return Err(PrintBackendError::NoPageSize("empty raster".to_owned()));
            }
            let (width, height, pixels) = if placement.rotate_clockwise {
                rotate_clockwise(&rendered)
            } else {
                to_bgra(
                    rendered.pixels().to_vec(),
                    rendered.width(),
                    rendered.height(),
                )
            };
            if width == 0 || height == 0 {
                return Err(PrintBackendError::NoPageSize("empty DIB".to_owned()));
            }
            let started = unsafe { win::StartPage(hdc) };
            if started <= 0 {
                return Err(PrintBackendError::Page(unsafe { win::GetLastError() }));
            }
            let header = win::BitmapInfoHeader {
                biSize: std::mem::size_of::<win::BitmapInfoHeader>() as u32,
                biWidth: width,
                biHeight: -height,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: 0,
                biSizeImage: (width as u32)
                    .saturating_mul(height as u32)
                    .saturating_mul(4),
                biXPelsPerMeter: 0,
                biYPelsPerMeter: 0,
                biClrUsed: 0,
                biClrImportant: 0,
            };
            let copied = unsafe {
                win::StretchDIBits(
                    hdc,
                    placement.dest_x,
                    placement.dest_y,
                    placement.dest_width,
                    placement.dest_height,
                    0,
                    0,
                    width,
                    height,
                    pixels.as_ptr().cast(),
                    &header,
                    0,
                    SRCCOPY,
                )
            };
            let ended = unsafe { win::EndPage(hdc) };
            if copied <= 0 || ended <= 0 {
                return Err(PrintBackendError::Page(unsafe { win::GetLastError() }));
            }
            done += 1;
            progress(done, total);
        }
    }
    Ok(done)
}

/// Rotate an RGBA raster 90 degrees clockwise and convert it to BGRA.
#[cfg(windows)]
fn rotate_clockwise(rendered: &crate::RenderedPage) -> (i32, i32, Vec<u8>) {
    let width = rendered.width();
    let height = rendered.height();
    let source = rendered.pixels();
    let mut output = vec![0_u8; source.len()];
    for y in 0..height {
        for x in 0..width {
            let source_index = ((y * width + x) * 4) as usize;
            // Clockwise: destination x is the source's inverted row, y is x.
            let dest_x = height - 1 - y;
            let dest_y = x;
            let dest_index = ((dest_y * height + dest_x) * 4) as usize;
            output[dest_index] = source[source_index + 2];
            output[dest_index + 1] = source[source_index + 1];
            output[dest_index + 2] = source[source_index];
            output[dest_index + 3] = 255;
        }
    }
    (height as i32, width as i32, output)
}

/// Convert an RGBA raster to a 32-bit BGRA DIB buffer.
#[cfg(windows)]
fn to_bgra(mut pixels: Vec<u8>, width: u32, height: u32) -> (i32, i32, Vec<u8>) {
    let (chunks, _) = pixels.as_chunks_mut::<4>();
    for chunk in chunks {
        chunk.swap(0, 2);
        chunk[3] = 255;
    }
    (width as i32, height as i32, pixels)
}

/// Read the printer's DPI and printable area.
#[cfg(windows)]
unsafe fn device_metrics(hdc: win::Hdc) -> PrinterMetrics {
    // SAFETY: `hdc` was returned by PrintDlg or CreateDC and is owned by the
    // caller for the duration of the read.
    unsafe {
        PrinterMetrics {
            dpi_x: u32::try_from(win::GetDeviceCaps(hdc, win::LOGPIXELSX)).unwrap_or(0),
            dpi_y: u32::try_from(win::GetDeviceCaps(hdc, win::LOGPIXELSY)).unwrap_or(0),
            printable_width_px: win::GetDeviceCaps(hdc, win::HORZRES),
            printable_height_px: win::GetDeviceCaps(hdc, win::VERTRES),
        }
    }
}

/// Release the DEVMODE and DEVNAMES blocks the dialog allocates.
#[cfg(windows)]
fn free_dialog_handles(dialog: &win::PrintDlgW) {
    unsafe {
        if !dialog.hDevMode.is_null() {
            win::GlobalFree(dialog.hDevMode);
        }
        if !dialog.hDevNames.is_null() {
            win::GlobalFree(dialog.hDevNames);
        }
    }
}

/// Encode a Rust string as a NUL-terminated UTF-16 buffer.
#[cfg(windows)]
fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Minimal Win32 declarations for the print path.
#[cfg(windows)]
#[allow(non_snake_case)]
mod win {
    use std::ffi::c_void;

    /// Opaque Win32 handle/pointer.
    pub type Handle = *mut c_void;
    /// Device context handle.
    pub type Hdc = *mut c_void;

    pub const LOGPIXELSX: i32 = 88;
    pub const LOGPIXELSY: i32 = 90;
    pub const HORZRES: i32 = 8;
    pub const VERTRES: i32 = 10;

    pub const PD_ALL_PAGES: u32 = 0x0000_0000;
    pub const PD_PAGE_NUMBERS: u32 = 0x0000_0002;
    pub const PD_NO_SELECTION: u32 = 0x0000_0004;
    pub const PD_RETURN_DC: u32 = 0x0000_0100;
    pub const PD_CURRENT_PAGE: u32 = 0x0040_0000;

    #[repr(C)]
    pub struct PrintDlgW {
        pub lStructSize: u32,
        pub hwndOwner: Handle,
        pub hDevMode: Handle,
        pub hDevNames: Handle,
        pub hDC: Hdc,
        pub Flags: u32,
        pub nFromPage: u16,
        pub nToPage: u16,
        pub nMinPage: u16,
        pub nMaxPage: u16,
        pub nCopies: u16,
        pub hInstance: Handle,
        pub lCustData: isize,
        pub lpfnPrintHook: Handle,
        pub lpfnSetupHook: Handle,
        pub lpPrintTemplateName: *const u16,
        pub lpSetupTemplateName: *const u16,
        pub hPrintTemplate: Handle,
        pub hSetupTemplate: Handle,
    }

    #[repr(C)]
    pub struct DocInfoW {
        pub cbSize: i32,
        pub lpszDocName: *const u16,
        pub lpszOutput: *const u16,
        pub lpszDatatype: *const u16,
        pub fwType: u32,
    }

    #[repr(C)]
    pub struct BitmapInfoHeader {
        pub biSize: u32,
        pub biWidth: i32,
        pub biHeight: i32,
        pub biPlanes: u16,
        pub biBitCount: u16,
        pub biCompression: u32,
        pub biSizeImage: u32,
        pub biXPelsPerMeter: i32,
        pub biYPelsPerMeter: i32,
        pub biClrUsed: u32,
        pub biClrImportant: u32,
    }

    #[link(name = "comdlg32")]
    unsafe extern "system" {
        pub fn PrintDlgW(dialog: *mut PrintDlgW) -> i32;
        pub fn CommDlgExtendedError() -> u32;
    }

    #[link(name = "gdi32")]
    unsafe extern "system" {
        pub fn CreateDCW(
            driver: *const u16,
            device: *const u16,
            port: *const u16,
            dev_mode: *const c_void,
        ) -> Hdc;
        pub fn DeleteDC(hdc: Hdc) -> i32;
        pub fn StartDocW(hdc: Hdc, info: *const DocInfoW) -> i32;
        pub fn StartPage(hdc: Hdc) -> i32;
        pub fn EndPage(hdc: Hdc) -> i32;
        pub fn EndDoc(hdc: Hdc) -> i32;
        pub fn AbortDoc(hdc: Hdc) -> i32;
        pub fn GetDeviceCaps(hdc: Hdc, index: i32) -> i32;
        pub fn SetStretchBltMode(hdc: Hdc, mode: i32) -> i32;
        pub fn StretchDIBits(
            hdc: Hdc,
            dest_x: i32,
            dest_y: i32,
            dest_width: i32,
            dest_height: i32,
            source_x: i32,
            source_y: i32,
            source_width: i32,
            source_height: i32,
            bits: *const c_void,
            header: *const BitmapInfoHeader,
            usage: u32,
            rop: u32,
        ) -> i32;
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        pub fn GetLastError() -> u32;
        pub fn GlobalFree(handle: Handle) -> Handle;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metrics(dpi: u32, width: i32, height: i32) -> PrinterMetrics {
        PrinterMetrics {
            dpi_x: dpi,
            dpi_y: dpi,
            printable_width_px: width,
            printable_height_px: height,
        }
    }

    #[test]
    fn actual_size_uses_printer_dpi_and_centers_the_page() {
        let placement = compute_placement(
            612.0,
            792.0,
            metrics(300, 2400, 3300),
            PageScaleMode::Actual,
            Orientation::Portrait,
        )
        .expect("placement");
        assert_eq!(placement.dest_width, 2550);
        assert_eq!(placement.dest_height, 3300);
        assert_eq!(placement.dest_x, -75);
        assert_eq!(placement.dest_y, 0);
        assert!(!placement.rotate_clockwise);
    }

    #[test]
    fn fit_chooses_the_tighter_ratio_and_stays_inside_the_area() {
        let placement = compute_placement(
            612.0,
            792.0,
            metrics(300, 2400, 3300),
            PageScaleMode::Fit,
            Orientation::Portrait,
        )
        .expect("placement");
        assert_eq!(placement.dest_width, 2400);
        assert!((placement.dest_height - 3106).abs() <= 1);
        assert_eq!(placement.dest_x, 0);
        assert!((placement.dest_y - 97).abs() <= 1);
    }

    #[test]
    fn landscape_rotates_a_portrait_page() {
        let placement = compute_placement(
            612.0,
            792.0,
            metrics(300, 3300, 2400),
            PageScaleMode::Fit,
            Orientation::Landscape,
        )
        .expect("placement");
        assert!(placement.rotate_clockwise);
        assert!(placement.dest_width > placement.dest_height);
        assert!(placement.dest_width <= 3300);
        assert!(placement.dest_height <= 2400);
    }

    #[test]
    fn custom_percent_scales_physical_size() {
        let placement = compute_placement(
            612.0,
            792.0,
            metrics(300, 4000, 4000),
            PageScaleMode::CustomPercent(50.0),
            Orientation::Portrait,
        )
        .expect("placement");
        assert_eq!(placement.dest_width, 1275);
        assert_eq!(placement.dest_height, 1650);
    }

    #[test]
    fn unusable_metrics_and_pages_are_typed_errors() {
        assert!(matches!(
            compute_placement(
                612.0,
                792.0,
                metrics(0, 2400, 3300),
                PageScaleMode::Actual,
                Orientation::Portrait
            ),
            Err(PrintBackendError::NoPrintableArea)
        ));
        assert!(matches!(
            compute_placement(
                0.0,
                792.0,
                metrics(300, 2400, 3300),
                PageScaleMode::Actual,
                Orientation::Portrait
            ),
            Err(PrintBackendError::Placement(_))
        ));
        assert!(matches!(
            compute_placement(
                612.0,
                792.0,
                metrics(300, 2400, 3300),
                PageScaleMode::CustomPercent(0.0),
                Orientation::Portrait
            ),
            Err(PrintBackendError::Placement(_))
        ));
    }

    #[test]
    fn dialog_ranges_clamp_and_swap() {
        let all = DialogPageChoice {
            all: true,
            ..DialogPageChoice::default()
        };
        assert_eq!(resolve_dialog_pages(all, 3, 0).expect("all"), vec![0, 1, 2]);

        let narrowed = DialogPageChoice {
            page_numbers: true,
            from: 9,
            to: 2,
            ..DialogPageChoice::default()
        };
        assert_eq!(
            resolve_dialog_pages(narrowed, 3, 0).expect("range"),
            vec![1, 2]
        );

        let current = DialogPageChoice {
            current: true,
            ..DialogPageChoice::default()
        };
        assert_eq!(
            resolve_dialog_pages(current, 3, 99).expect("current"),
            vec![2]
        );
        assert!(matches!(
            resolve_dialog_pages(all, 0, 0),
            Err(PrintBackendError::EmptyDocument)
        ));
    }

    #[test]
    fn rotating_a_raster_moves_the_top_left_pixel_to_the_top_right() {
        #[cfg(windows)]
        {
            let rendered = crate::RenderedPage::new(
                PageIndex::new(0),
                2,
                1,
                vec![255, 0, 0, 255, 0, 255, 0, 255],
            );
            let (width, height, pixels) = rotate_clockwise(&rendered);
            assert_eq!((width, height), (1, 2));
            assert_eq!(&pixels[0..4], &[0, 0, 255, 255]);
            assert_eq!(&pixels[4..8], &[0, 255, 0, 255]);
        }
    }
}
