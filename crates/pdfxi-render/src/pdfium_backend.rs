//! PDFium-backed implementation of the [`Renderer`] contract.
//!
//! This module is only compiled when the `pdfium` Cargo feature is enabled so
//! that default builds of `pdfxi-render` remain free of native dependencies.
//!
//! # Thread safety
//!
//! PDFium is *not* thread-safe: all of its global state and document handles
//! must be used by one thread at a time. Even though `pdfium-render`'s default
//! `thread_safe` feature serializes individual FPDF calls behind a global
//! marshall, a higher-level lock is still required so that a multi-step
//! operation (load document → get page → render → copy pixels) cannot interleave
//! with another caller. This module therefore keeps one process-wide PDFium
//! instance behind a [`std::sync::Mutex`] and holds that mutex for the whole render. The
//! design is a deliberately bounded "pool" of one worker; it can be widened to
//! a real per-worker pool later without changing the [`Renderer`] contract.
//!
//! # Text extraction
//!
//! With this feature enabled, [`PdfiumRenderer`] also implements
//! `pdfxi_text::TextExtractor`. It takes the same lock for the whole load →
//! page → text → character-copy sequence, visits characters in the order
//! PDFium returns them, and merges them into text spans with the native-free
//! helpers in the private `text_extract` module. Malformed glyph metrics are
//! normalized rather than panicking, and every failure is a structured
//! [`pdfxi_text::TextError`].
//!
//! # Library discovery
//!
//! The native library is located once, on first use, from the following sources
//! in priority order:
//!
//! 1. the `PDFIUM_PATH` environment variable (a full path to the DLL/SO);
//! 2. `%LOCALAPPDATA%\pdfxi\pdfium\bin\pdfium.dll` (the standard install);
//! 3. `pdfium.dll` in the current directory;
//! 4. the platform's system library search path.
//!
//! # Errors
//!
//! Every failure is reported as a [`RenderError`] variant. In particular,
//! malformed documents, missing libraries, and invalid page indices never
//! panic; they produce a structured error that callers can map to a message.

use std::{
    path::{Path, PathBuf},
    sync::{Mutex, MutexGuard, OnceLock},
};

use pdfium_render::prelude::{PdfRenderConfig, Pdfium, PdfiumError};

use pdfxi_text::{
    CancellationToken as TextCancellationToken, Progress as TextProgress, TextError, TextExtractor,
    TextPage,
};

use crate::text_extract::{RawChar, chars_to_text_page};
use crate::{CancellationToken, PageIndex, Progress, RenderError, RenderedPage, Renderer};

/// The one PDFium instance shared by every renderer in this process.
static PDFIUM: OnceLock<Mutex<Pdfium>> = OnceLock::new();

/// Guards the one-time initialization of [`PDFIUM`].
static INIT_LOCK: Mutex<()> = Mutex::new(());

/// Build the ordered list of candidate locations for the PDFium library.
fn library_candidates() -> Vec<PathBuf> {
    let mut candidates = Vec::new();

    if let Some(path) = std::env::var_os("PDFIUM_PATH")
        && !path.is_empty()
    {
        candidates.push(PathBuf::from(path));
    }

    if let Some(local_app_data) = std::env::var_os("LOCALAPPDATA") {
        candidates.push(
            Path::new(&local_app_data)
                .join("pdfxi")
                .join("pdfium")
                .join("bin")
                .join("pdfium.dll"),
        );
    }

    candidates.push(PathBuf::from(Pdfium::pdfium_platform_library_name()));
    candidates
}

/// Bind the PDFium shared library and create the process-wide instance.
///
/// This is called at most once, while `INIT_LOCK` is held.
fn bind_pdfium() -> Result<Mutex<Pdfium>, RenderError> {
    let mut attempts = Vec::new();

    for candidate in library_candidates() {
        match Pdfium::bind_to_library(&candidate) {
            Ok(bindings) => return Ok(Mutex::new(configure_pdfium(Pdfium::new(bindings)))),
            Err(PdfiumError::PdfiumLibraryBindingsAlreadyInitialized) => {
                // Another component already installed bindings in
                // pdfium-render's global `OnceCell`. `Pdfium::default()` reuses
                // those bindings in exactly this branch; it cannot reach any of
                // its panic paths because the cell is known to be initialized.
                return Ok(Mutex::new(configure_pdfium(Pdfium::default())));
            }
            Err(error) => attempts.push(format!("{}: {error}", candidate.display())),
        }
    }

    match Pdfium::bind_to_system_library() {
        Ok(bindings) => Ok(Mutex::new(configure_pdfium(Pdfium::new(bindings)))),
        Err(PdfiumError::PdfiumLibraryBindingsAlreadyInitialized) => {
            Ok(Mutex::new(configure_pdfium(Pdfium::default())))
        }
        Err(error) => {
            attempts.push(format!("system library: {error}"));
            Err(RenderError::Backend(format!(
                "could not load the PDFium library (tried: {})",
                attempts.join("; ")
            )))
        }
    }
}

/// Apply optional configuration that improves rendering fidelity.
fn configure_pdfium(mut pdfium: Pdfium) -> Pdfium {
    // A platform font provider lets text-only pages render their glyphs. If the
    // platform has none, PDFium falls back to its own font substitution, so a
    // failure here is deliberately non-fatal.
    let _ = pdfium.use_platform_default_font_provider();
    pdfium
}

/// Return the shared PDFium mutex, binding the native library on first use.
fn pdfium_mutex() -> Result<&'static Mutex<Pdfium>, RenderError> {
    if let Some(instance) = PDFIUM.get() {
        return Ok(instance);
    }

    let _init = INIT_LOCK
        .lock()
        .map_err(|_| RenderError::Backend("PDFium initialization lock is poisoned".to_owned()))?;

    if let Some(instance) = PDFIUM.get() {
        return Ok(instance);
    }

    // A concurrent initialization is impossible while `INIT_LOCK` is held, but
    // tolerate it anyway rather than panicking on a duplicate `OnceLock::set`.
    let _ = PDFIUM.set(bind_pdfium()?);

    PDFIUM.get().ok_or_else(|| {
        RenderError::Backend("PDFium instance is unavailable after initialization".to_owned())
    })
}

/// Acquire exclusive access to PDFium for one complete operation.
fn lock_pdfium() -> Result<MutexGuard<'static, Pdfium>, RenderError> {
    pdfium_mutex()?
        .lock()
        .map_err(|_| RenderError::Backend("PDFium render lock is poisoned".to_owned()))
}

/// A [`Renderer`] that rasterizes documents with the native PDFium library.
///
/// Construct one with [`PdfiumRenderer::from_file`] or
/// [`PdfiumRenderer::from_bytes`]. The source bytes are retained and reused for
/// every render, so the renderer does not depend on the original file staying
/// on disk.
pub struct PdfiumRenderer {
    bytes: Vec<u8>,
    source: Option<PathBuf>,
}

impl std::fmt::Debug for PdfiumRenderer {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Deliberately omit document bytes: logs must never contain content.
        formatter
            .debug_struct("PdfiumRenderer")
            .field("byte_len", &self.bytes.len())
            .field("source", &self.source)
            .finish()
    }
}

impl PdfiumRenderer {
    /// Open a PDF from `path`, validating that PDFium can parse it.
    ///
    /// Returns [`RenderError::Backend`] if the file cannot be read or the
    /// document is not a valid PDF.
    pub fn from_file(path: impl AsRef<Path>) -> Result<Self, RenderError> {
        let path = path.as_ref();
        let bytes = std::fs::read(path).map_err(|error| {
            RenderError::Backend(format!("cannot read {}: {error}", path.display()))
        })?;
        let mut renderer = Self::from_bytes(bytes)?;
        renderer.source = Some(path.to_path_buf());
        Ok(renderer)
    }

    /// Open a PDF from an in-memory byte buffer, validating it with PDFium.
    ///
    /// Returns [`RenderError::Backend`] if the native library is unavailable or
    /// the bytes do not describe a valid PDF document.
    pub fn from_bytes(bytes: Vec<u8>) -> Result<Self, RenderError> {
        let pdfium = lock_pdfium()?;
        let document = pdfium
            .load_pdf_from_byte_slice(&bytes, None)
            .map_err(|error| RenderError::Backend(format!("cannot open document: {error}")))?;
        // Touch the page tree so a structurally broken PDF fails at open time.
        let _page_count = document.pages().len();
        // `document` borrows `bytes`, so release it before taking ownership.
        drop(document);
        Ok(Self {
            bytes,
            source: None,
        })
    }

    /// Return the source path, if this renderer was opened from a file.
    pub fn source(&self) -> Option<&Path> {
        self.source.as_deref()
    }
}

impl Renderer for PdfiumRenderer {
    fn render_page(
        &self,
        page: PageIndex,
        scale: f32,
        cancellation: &CancellationToken,
        progress: &mut dyn FnMut(Progress),
    ) -> Result<RenderedPage, RenderError> {
        cancellation.check()?;
        if !scale.is_finite() || scale <= 0.0 {
            return Err(RenderError::InvalidScale(scale));
        }

        progress(Progress::new(0, 4));
        cancellation.check()?;

        // One lock covers the entire multi-step operation; PDFium calls from
        // other renderers cannot interleave with this render.
        let pdfium = lock_pdfium()?;
        let document = pdfium
            .load_pdf_from_byte_slice(&self.bytes, None)
            .map_err(|error| RenderError::Backend(format!("cannot open document: {error}")))?;

        progress(Progress::new(1, 4));
        cancellation.check()?;

        let pages = document.pages();
        let page_count = pages.len();
        if page.get() >= page_count as usize {
            return Err(RenderError::PageOutOfRange(page.get()));
        }

        let pdf_page = pages
            .get(page.get() as i32)
            .map_err(|error| RenderError::Backend(format!("cannot load page {page}: {error}")))?;

        progress(Progress::new(2, 4));
        cancellation.check()?;

        // At scale 1.0 a PDF point maps to one pixel, i.e. 72 DPI.
        let config = PdfRenderConfig::new().scale_page_by_factor(scale);
        let bitmap = pdf_page
            .render_with_config(&config)
            .map_err(|error| RenderError::Backend(format!("cannot render page {page}: {error}")))?;

        let width = u32::try_from(bitmap.width()).map_err(|_| {
            RenderError::Backend("render produced an invalid raster width".to_owned())
        })?;
        let height = u32::try_from(bitmap.height()).map_err(|_| {
            RenderError::Backend("render produced an invalid raster height".to_owned())
        })?;
        if width == 0 || height == 0 {
            return Err(RenderError::Backend(
                "render produced a zero-sized raster".to_owned(),
            ));
        }

        let pixels = bitmap.as_rgba_bytes();
        let expected = (width as usize)
            .checked_mul(height as usize)
            .and_then(|area| area.checked_mul(4))
            .ok_or_else(|| RenderError::Backend("raster size overflow".to_owned()))?;
        if pixels.len() != expected {
            return Err(RenderError::Backend(format!(
                "raster has {} bytes, expected {expected}",
                pixels.len()
            )));
        }

        progress(Progress::new(3, 4));
        cancellation.check()?;
        progress(Progress::new(4, 4));

        Ok(RenderedPage::new(page, width, height, pixels))
    }

    fn page_count(&self) -> Result<usize, RenderError> {
        let pdfium = lock_pdfium()?;
        let document = pdfium
            .load_pdf_from_byte_slice(&self.bytes, None)
            .map_err(|error| RenderError::Backend(format!("cannot open document: {error}")))?;
        Ok(document.pages().len() as usize)
    }
}

impl TextExtractor for PdfiumRenderer {
    /// Recover the text of `page` through PDFium.
    ///
    /// Characters are returned in PDFium's own text-page order (the order its
    /// text operators produce for ordinary PDFs); this implementation does not
    /// sort or regroup them geometrically. Consecutive characters that share a
    /// baseline, font size, and direction become one [`pdfxi_text::TextSpan`].
    /// Cancellation is checked before any native work and between characters.
    fn extract_page(
        &self,
        page: usize,
        cancellation: &TextCancellationToken,
        progress: &mut dyn FnMut(TextProgress),
    ) -> Result<TextPage, TextError> {
        // Check before touching PDFium so a cancelled job does no native work.
        cancellation.check()?;
        progress(TextProgress::new(0, 3));
        cancellation.check()?;

        // The same single, process-wide lock as `render_page` covers the whole
        // operation; no other caller can interleave with this character walk.
        let pdfium = lock_pdfium().map_err(|error| TextError::Backend(error.to_string()))?;
        let document = pdfium
            .load_pdf_from_byte_slice(&self.bytes, None)
            .map_err(|error| TextError::Backend(format!("cannot open document: {error}")))?;

        progress(TextProgress::new(1, 3));
        cancellation.check()?;

        let pages = document.pages();
        if page >= pages.len() as usize {
            return Err(TextError::PageOutOfRange(page));
        }
        let pdf_page = pages.get(page as i32).map_err(|error| {
            TextError::Backend(format!("cannot load page {page} for text: {error}"))
        })?;
        let text = pdf_page.text().map_err(|error| {
            TextError::Backend(format!("cannot read text on page {page}: {error}"))
        })?;

        progress(TextProgress::new(2, 3));
        cancellation.check()?;

        let mut chars = Vec::with_capacity(text.len().max(0) as usize);
        for character in text.chars().iter() {
            // A cancelled extraction stops promptly even mid-page.
            cancellation.check()?;

            // Characters without a Unicode mapping (for example some generated
            // spacing marks) carry no searchable text and are skipped.
            let Some(unicode) = character.unicode_string() else {
                continue;
            };

            // Glyph metrics can fail independently on malformed pages. Falling
            // back to zero lets the normalizer produce a finite, usable span
            // instead of panicking or aborting the whole page.
            let (left, right, bottom, top) = match character.tight_bounds() {
                Ok(bounds) => (
                    bounds.left().value,
                    bounds.right().value,
                    bounds.bottom().value,
                    bounds.top().value,
                ),
                Err(_) => (0.0, 0.0, 0.0, 0.0),
            };
            let baseline_y = character.origin_y().map(|y| y.value).unwrap_or(bottom);
            let font_size = character.scaled_font_size().value;
            let direction_degrees = character.angle_degrees().unwrap_or(0.0);

            chars.push(RawChar::new(
                unicode,
                left,
                right,
                bottom,
                top,
                baseline_y,
                font_size,
                direction_degrees,
            ));
        }

        progress(TextProgress::new(3, 3));
        Ok(chars_to_text_page(page, &chars))
    }

    fn page_count(&self) -> Result<usize, TextError> {
        // Reuse the renderer's validated path so both traits agree on the count.
        Renderer::page_count(self).map_err(|error| TextError::Backend(error.to_string()))
    }
}
