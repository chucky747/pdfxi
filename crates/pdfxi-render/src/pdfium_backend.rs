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
//! The native library is loaded once, on first use, from locations the running
//! user cannot influence:
//!
//! - **Release builds** load only `pdfium.dll` from the directory of the
//!   running executable. That directory comes from [`std::env::current_exe`]
//!   and must be absolute; when it cannot be determined, loading fails with
//!   [`LibraryError::ExecutableDirectory`]. The working directory, `PATH`, and
//!   the system library search are deliberately never consulted, so a planted
//!   DLL in an unrelated directory cannot be loaded.
//! - **Debug builds and tests** (`cfg(debug_assertions)`) try the executable
//!   directory first, then `PDFIUM_PATH` (a full path to the DLL/SO), then the
//!   per-user developer cache `%LOCALAPPDATA%\pdfxi\pdfium\bin`. The working
//!   directory and the system search path are still never used.
//!
//! # Input limit
//!
//! [`PdfiumRenderer::from_file`] stats the file before reading it and rejects
//! documents larger than [`MAX_INPUT_BYTES`] (2 GiB) with the typed
//! [`InputTooLarge`] error, so a hostile or accidental huge file cannot cause
//! an unbounded whole-file allocation.
//!
//! # Typed errors for new failure modes
//!
//! [`RenderError`] has no variant for library discovery or input size, and
//! this module cannot add one without editing `crates/pdfxi-render/src/lib.rs`.
//! The typed [`LibraryError`] and [`InputTooLarge`] values are therefore
//! converted to `RenderError::Backend` through their `Display` text, which is
//! the exact diagnostic a caller sees.
//!
//! # Errors
//!
//! Every failure is reported as a [`RenderError`] variant. In particular,
//! malformed documents, missing libraries, and invalid page indices never
//! panic; they produce a structured error that callers can map to a message.

use std::{
    ffi::OsStr,
    path::{Path, PathBuf},
    sync::{Mutex, MutexGuard, OnceLock},
};

use pdfium_render::prelude::{
    PdfDocumentMetadataTagType, PdfRenderConfig, PdfSecurityHandlerRevision, Pdfium, PdfiumError,
    PdfiumInternalError,
};

use pdfxi_text::{
    CancellationToken as TextCancellationToken, Progress as TextProgress, TextError, TextExtractor,
    TextPage,
};

use crate::text_extract::{RawChar, chars_to_text_page};

/// A short, user-facing description of a PDFium failure.
///
/// `pdfium-render` implements `Display` for [`PdfiumError`] as a pretty-printed
/// debug dump, which leaked text such as `PdfiumLibraryInternalError(\n
/// Unknown,\n)` into the status line and error banner. Map the library's typed
/// outcome to one sentence instead.
fn describe_pdfium_error(error: &PdfiumError) -> &'static str {
    match error {
        PdfiumError::PdfiumLibraryInternalError(inner) => match inner {
            PdfiumInternalError::FileError => "the file could not be read",
            PdfiumInternalError::FormatError => "the file is not a valid PDF",
            PdfiumInternalError::PasswordError => "the document needs a password",
            PdfiumInternalError::SecurityError => {
                "the document security settings are not supported"
            }
            PdfiumInternalError::PageError => "the page could not be read",
            PdfiumInternalError::Unknown => "the PDF engine could not read this document",
        },
        PdfiumError::PageIndexOutOfBounds => "the page number is outside this document",
        PdfiumError::UnrecognizedPath => "the file path could not be used",
        _ => "the PDF engine could not complete the operation",
    }
}
use crate::PageDimensions;
use crate::{
    CancellationToken, MAX_RASTER_PIXELS, PageIndex, Progress, RenderError, RenderedPage, Renderer,
    clamp_render_scale,
};

/// The one PDFium instance shared by every renderer in this process.
static PDFIUM: OnceLock<Mutex<Pdfium>> = OnceLock::new();

/// Guards the one-time initialization of [`PDFIUM`].
static INIT_LOCK: Mutex<()> = Mutex::new(());

/// Largest input document [`PdfiumRenderer::from_file`] will read, in bytes.
///
/// 2 GiB keeps a whole-file read bounded while accepting every ordinary PDF.
/// Larger documents need the incremental, streamed backend that is not part of
/// this release.
pub const MAX_INPUT_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// A document rejected before it is read into memory.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("input file is {size} bytes; the maximum accepted size is {limit} bytes")]
pub struct InputTooLarge {
    /// Reported size of the rejected file.
    pub size: u64,
    /// The limit that `size` exceeded.
    pub limit: u64,
}

impl From<InputTooLarge> for RenderError {
    fn from(error: InputTooLarge) -> Self {
        // `RenderError` has no size variant; see the module documentation.
        RenderError::Backend(error.to_string())
    }
}

/// A PDFium library location that could not be used.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum LibraryError {
    /// The running executable's directory could not be resolved.
    #[error("cannot determine the directory of the running executable: {0}")]
    ExecutableDirectory(String),
    /// No candidate location held a loadable PDFium library.
    #[error("could not load the PDFium library (tried: {tried})")]
    NotFound {
        /// Every attempted location together with its loader error.
        tried: String,
    },
}

impl From<LibraryError> for RenderError {
    fn from(error: LibraryError) -> Self {
        // `RenderError` has no library variant; see the module documentation.
        RenderError::Backend(error.to_string())
    }
}

/// Reject a file whose stat length exceeds [`MAX_INPUT_BYTES`].
///
/// Pure so the cap decision can be tested without a large file on disk.
pub fn check_input_size(size: u64) -> Result<(), InputTooLarge> {
    if size > MAX_INPUT_BYTES {
        return Err(InputTooLarge {
            size,
            limit: MAX_INPUT_BYTES,
        });
    }
    Ok(())
}

/// Ordered PDFium library candidates for one build mode.
///
/// Pure so the search policy is testable without loading a native library:
/// `exe_dir` is the running executable's directory and the only source in a
/// release build; `debug_build` appends `env_override` (`PDFIUM_PATH`) and
/// `dev_cache_dir` (the per-user developer cache). No candidate is ever a bare
/// file name, so the working directory and the system search path can never be
/// used.
fn library_candidates(
    library_name: &OsStr,
    exe_dir: Option<&Path>,
    debug_build: bool,
    env_override: Option<&Path>,
    dev_cache_dir: Option<&Path>,
) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(directory) = exe_dir {
        candidates.push(directory.join(library_name));
    }
    if debug_build {
        if let Some(path) = env_override {
            candidates.push(path.to_path_buf());
        }
        if let Some(directory) = dev_cache_dir {
            candidates.push(directory.join(library_name));
        }
    }
    candidates
}

/// Directory of the running executable, which must be absolute.
fn executable_directory() -> Result<PathBuf, LibraryError> {
    let executable = std::env::current_exe()
        .map_err(|error| LibraryError::ExecutableDirectory(error.to_string()))?;
    if !executable.is_absolute() {
        return Err(LibraryError::ExecutableDirectory(format!(
            "{} is not an absolute path",
            executable.display()
        )));
    }
    executable.parent().map(Path::to_path_buf).ok_or_else(|| {
        LibraryError::ExecutableDirectory(format!(
            "{} has no parent directory",
            executable.display()
        ))
    })
}

/// Candidate library locations for this process and build mode.
fn candidate_libraries() -> Result<Vec<PathBuf>, LibraryError> {
    let exe_dir = executable_directory()?;
    let debug_build = cfg!(debug_assertions);
    let env_override = if debug_build {
        std::env::var_os("PDFIUM_PATH")
            .filter(|path| !path.is_empty())
            .map(PathBuf::from)
    } else {
        None
    };
    let dev_cache_dir = if debug_build {
        std::env::var_os("LOCALAPPDATA")
            .filter(|base| !base.is_empty())
            .map(|base| Path::new(&base).join("pdfxi").join("pdfium").join("bin"))
    } else {
        None
    };
    let library_name = Pdfium::pdfium_platform_library_name();
    Ok(library_candidates(
        &library_name,
        Some(exe_dir.as_path()),
        debug_build,
        env_override.as_deref(),
        dev_cache_dir.as_deref(),
    ))
}

/// Bind the PDFium shared library and create the process-wide instance.
///
/// This is called at most once, while `INIT_LOCK` is held.
fn bind_pdfium() -> Result<Mutex<Pdfium>, RenderError> {
    let candidates = candidate_libraries()?;
    if candidates.is_empty() {
        return Err(LibraryError::NotFound {
            tried: "no candidate locations".to_owned(),
        }
        .into());
    }

    let mut attempts = Vec::new();
    for candidate in candidates {
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

    Err(LibraryError::NotFound {
        tried: attempts.join("; "),
    }
    .into())
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

/// Metadata read from a loaded PDF document by the native backend.
///
/// Produced by [`PdfiumRenderer::document_meta`] as the native counterpart of
/// the app layer's window-free open record. Page count, title, and encryption
/// state are validated before this value is returned.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DocumentMeta {
    /// Number of pages in the document.
    pub page_count: usize,
    /// Title from the document information dictionary, when present and non-empty.
    pub title: Option<String>,
    /// Security handler revision reported by PDFium; `-1` means unprotected.
    pub security_revision: i32,
}

impl DocumentMeta {
    /// Return `true` when the document reports an active security handler.
    pub fn is_encrypted(&self) -> bool {
        self.security_revision >= 0
    }
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
    /// The file is stat'ed before it is read: a length above
    /// [`MAX_INPUT_BYTES`] is rejected through [`check_input_size`] with an
    /// [`InputTooLarge`] diagnostic instead of an unbounded allocation.
    ///
    /// Returns [`RenderError::Backend`] if the file cannot be read, is too
    /// large, or is not a valid PDF.
    pub fn from_file(path: impl AsRef<Path>) -> Result<Self, RenderError> {
        let path = path.as_ref();
        let metadata = std::fs::metadata(path).map_err(|error| {
            RenderError::Backend(format!("cannot read {}: {error}", path.display()))
        })?;
        check_input_size(metadata.len()).map_err(RenderError::from)?;
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
            .map_err(|error| {
                RenderError::Backend(format!(
                    "cannot open document: {}",
                    describe_pdfium_error(&error)
                ))
            })?;
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
    /// Return a page's size in points without rasterizing the page.
    ///
    /// The page box is read directly from PDFium under the same process-wide
    /// lock used for rendering.
    ///
    /// # Errors
    /// [`RenderError::PageOutOfRange`] when the page does not exist and
    /// [`RenderError::Backend`] when PDFium cannot load the page.
    pub fn page_size(&self, page: PageIndex) -> Result<PageDimensions, RenderError> {
        let pdfium = lock_pdfium()?;
        let document = pdfium
            .load_pdf_from_byte_slice(&self.bytes, None)
            .map_err(|error| {
                RenderError::Backend(format!(
                    "cannot open document: {}",
                    describe_pdfium_error(&error)
                ))
            })?;
        let pages = document.pages();
        if page.get() >= pages.len() as usize {
            return Err(RenderError::PageOutOfRange(page.get()));
        }
        let pdf_page = pages.get(page.get() as i32).map_err(|error| {
            RenderError::Backend(format!(
                "cannot load page {page}: {}",
                describe_pdfium_error(&error)
            ))
        })?;
        Ok(PageDimensions {
            width_pt: pdf_page.width().value,
            height_pt: pdf_page.height().value,
        })
    }

    /// Read page count, information-dictionary title, and encryption state from
    /// the loaded document.
    ///
    /// The native library is bound on first use and one process-wide lock covers
    /// the whole load to metadata copy sequence, matching [`Self::render_page`].
    /// Every failure is a structured [`RenderError`]; malformed metadata is
    /// normalized rather than panicking.
    pub fn document_meta(&self) -> Result<DocumentMeta, RenderError> {
        let pdfium = lock_pdfium()?;
        let document = pdfium
            .load_pdf_from_byte_slice(&self.bytes, None)
            .map_err(|error| {
                RenderError::Backend(format!(
                    "cannot open document: {}",
                    describe_pdfium_error(&error)
                ))
            })?;
        let page_count = document.pages().len() as usize;
        let title = document
            .metadata()
            .get(PdfDocumentMetadataTagType::Title)
            .map(|tag| tag.value().to_owned())
            .filter(|value| !value.is_empty());
        let security_revision = document
            .permissions()
            .security_handler_revision()
            .map(|revision| match revision {
                PdfSecurityHandlerRevision::Unprotected => -1,
                PdfSecurityHandlerRevision::Revision2 => 2,
                PdfSecurityHandlerRevision::Revision3 => 3,
                PdfSecurityHandlerRevision::Revision4 => 4,
            })
            .unwrap_or(-1);
        Ok(DocumentMeta {
            page_count,
            title,
            security_revision,
        })
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
            .map_err(|error| {
                RenderError::Backend(format!(
                    "cannot open document: {}",
                    describe_pdfium_error(&error)
                ))
            })?;

        progress(Progress::new(1, 4));
        cancellation.check()?;

        let pages = document.pages();
        let page_count = pages.len();
        if page.get() >= page_count as usize {
            return Err(RenderError::PageOutOfRange(page.get()));
        }

        let pdf_page = pages.get(page.get() as i32).map_err(|error| {
            RenderError::Backend(format!(
                "cannot load page {page}: {}",
                describe_pdfium_error(&error)
            ))
        })?;

        progress(Progress::new(2, 4));
        cancellation.check()?;

        // At scale 1.0 a PDF point maps to one pixel, i.e. 72 DPI. A very large
        // page at a high zoom is clamped so one raster cannot allocate an
        // unbounded buffer (MAX_RASTER_PIXELS = 16 Mpx = 64 MiB RGBA).
        let scale = clamp_render_scale(
            pdf_page.width().value,
            pdf_page.height().value,
            scale,
            MAX_RASTER_PIXELS,
        );
        let config = PdfRenderConfig::new().scale_page_by_factor(scale);
        let bitmap = pdf_page.render_with_config(&config).map_err(|error| {
            RenderError::Backend(format!(
                "cannot render page {page}: {}",
                describe_pdfium_error(&error)
            ))
        })?;

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
            .map_err(|error| {
                RenderError::Backend(format!(
                    "cannot open document: {}",
                    describe_pdfium_error(&error)
                ))
            })?;
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
            .map_err(|error| {
                TextError::Backend(format!(
                    "cannot open document: {}",
                    describe_pdfium_error(&error)
                ))
            })?;

        progress(TextProgress::new(1, 3));
        cancellation.check()?;

        let pages = document.pages();
        if page >= pages.len() as usize {
            return Err(TextError::PageOutOfRange(page));
        }
        let pdf_page = pages.get(page as i32).map_err(|error| {
            TextError::Backend(format!(
                "cannot load page {} for text: {}",
                page + 1,
                describe_pdfium_error(&error)
            ))
        })?;
        let text = pdf_page.text().map_err(|error| {
            TextError::Backend(format!(
                "cannot read text on page {}: {}",
                page + 1,
                describe_pdfium_error(&error)
            ))
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

#[cfg(test)]
mod tests {
    use super::{MAX_INPUT_BYTES, check_input_size, library_candidates};
    use std::ffi::OsStr;
    use std::path::Path;

    /// The size cap must accept the limit itself and reject one byte more.
    #[test]
    fn input_size_cap_decision() {
        assert_eq!(check_input_size(0), Ok(()));
        assert_eq!(check_input_size(1024), Ok(()));
        assert_eq!(check_input_size(MAX_INPUT_BYTES), Ok(()));

        let rejected = check_input_size(MAX_INPUT_BYTES + 1).expect_err("one byte over the cap");
        assert_eq!(rejected.size, MAX_INPUT_BYTES + 1);
        assert_eq!(rejected.limit, MAX_INPUT_BYTES);
        assert!(check_input_size(u64::MAX).is_err());
    }

    /// Release builds must only ever try the executable directory, while debug
    /// builds add the developer override and cache but never the working
    /// directory or the system search path.
    #[test]
    fn library_search_path_order() {
        let name = OsStr::new("pdfium.dll");
        let exe_dir = Path::new("C:/Program Files/PDFXI");
        let override_path = Path::new("D:/dev/pdfium/pdfium.dll");
        let cache_dir = Path::new("C:/Users/example/AppData/Local/pdfxi/pdfium/bin");

        assert_eq!(
            library_candidates(
                name,
                Some(exe_dir),
                false,
                Some(override_path),
                Some(cache_dir)
            ),
            vec![exe_dir.join(name)]
        );
        // Without an executable directory a release build has no candidates at
        // all: there is no fallback to the environment or the system path.
        assert!(
            library_candidates(name, None, false, Some(override_path), Some(cache_dir)).is_empty()
        );

        assert_eq!(
            library_candidates(
                name,
                Some(exe_dir),
                true,
                Some(override_path),
                Some(cache_dir)
            ),
            vec![
                exe_dir.join(name),
                override_path.to_path_buf(),
                cache_dir.join(name)
            ]
        );
        assert_eq!(
            library_candidates(name, None, true, None, Some(cache_dir)),
            vec![cache_dir.join(name)]
        );
        for candidate in library_candidates(
            name,
            Some(exe_dir),
            true,
            Some(override_path),
            Some(cache_dir),
        ) {
            assert!(
                candidate.is_absolute(),
                "{} must be absolute",
                candidate.display()
            );
        }
    }
}
