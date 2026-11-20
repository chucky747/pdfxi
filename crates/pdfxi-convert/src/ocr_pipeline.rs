//! OCR pipeline closure for searchable PDFs (CREATE-008).
//!
//! ADR 0008 requires a local, caller-supplied equivalent, so this module never
//! spawns Tesseract or any other process. A caller implements [`OcrEngine`]
//! with text, per-word boxes, and confidence values; this module validates the
//! inputs, invokes the engine in a deterministic page order, thresholds the
//! results, and plans the invisible searchable-text layer whose positions come
//! from the returned word boxes. No filesystem, network, or device access
//! happens here.
//!
//! # Relationship to the existing OCR-sidecar contract
//!
//! `pdfxi-text` already publishes the pure OCR boundary types (`OcrImage`,
//! `OcrBox`, `OcrSpan`, `OcrPage`, `OcrOptions`, `ConfidencePolicy`, and the
//! `SidecarRequest`/`SidecarResponse` JSON codec). This crate deliberately
//! does **not** add a dependency on `pdfxi-text`: `docs/ARCHITECTURE.md` keeps
//! feature crates dependency-light, and the convert lane must not change the
//! dependency graph. The types below therefore mirror that contract's shape so
//! a caller can bridge a `pdfxi_text::ocr::OcrEngine` result with a thin,
//! local adapter; the geometry and confidence scales are identical
//! (`0..=100`, pixel boxes with a top-left origin, DPI in `1..=2400`).
//! [`crate::ConversionOptions::ocr_language`] can be forwarded into
//! [`OcrOptions::languages`] by that adapter.
//!
//! Every stage is pure and deterministic: input pages are sorted by their
//! explicit page number before engine invocation, word results are sorted into
//! reading order, boxes are clamped to the source raster, and both the report
//! and the resulting [SearchableTextPlan] use fixed field order.

use crate::image::{MAX_IMAGE_DIMENSION, MAX_IMAGE_PIXELS};
use std::collections::BTreeSet;

/// Minimum accepted image resolution in dots per inch.
pub const OCR_MIN_IMAGE_DPI: f64 = 1.0;
/// Maximum accepted image resolution in dots per inch.
pub const OCR_MAX_IMAGE_DPI: f64 = 2400.0;
/// Default minimum confidence (on a `0..=100` scale) for a word to be kept.
pub const DEFAULT_CONFIDENCE_THRESHOLD: f32 = 60.0;
/// Default confidence (on a `0..=100` scale) below which a kept word is flagged
/// for human review.
pub const DEFAULT_SUSPECT_THRESHOLD: f32 = 80.0;
/// Maximum number of pages accepted in one OCR job.
pub const OCR_MAX_PAGES: usize = 10_000;
/// Maximum number of recognized words accepted on a single page.
pub const OCR_MAX_WORDS_PER_PAGE: usize = 100_000;

/// Pixel layout of an [`OcrImage`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PixelFormat {
    /// One 8-bit luminance byte per pixel.
    Grayscale,
    /// Four 8-bit bytes per pixel in red, green, blue, alpha order.
    Rgba,
}

impl PixelFormat {
    /// Return the number of bytes used by one pixel of this format.
    #[must_use]
    pub const fn bytes_per_pixel(self) -> usize {
        match self {
            Self::Grayscale => 1,
            Self::Rgba => 4,
        }
    }
}

/// A validated image handed to an [`OcrEngine`].
///
/// The caller owns the recognition backend; this type only guarantees that the
/// pixel buffer length is exactly `width_px * height_px * bytes_per_pixel` and
/// that the geometry and DPI are within documented bounds.
#[derive(Debug, Clone, PartialEq)]
pub struct OcrImage {
    page: usize,
    width_px: u32,
    height_px: u32,
    dpi: f64,
    format: PixelFormat,
    pixels: Vec<u8>,
}

impl OcrImage {
    /// Create and validate an OCR input image.
    ///
    /// # Errors
    ///
    /// Returns [`OcrPipelineError::InvalidDpi`] for a non-finite or out-of-range
    /// resolution and [`OcrPipelineError::InvalidImage`] for bad dimensions or
    /// a pixel buffer whose length does not match the declared layout.
    pub fn new(
        page: usize,
        width_px: u32,
        height_px: u32,
        dpi: f64,
        format: PixelFormat,
        pixels: Vec<u8>,
    ) -> Result<Self, OcrPipelineError> {
        if !dpi.is_finite() || !(OCR_MIN_IMAGE_DPI..=OCR_MAX_IMAGE_DPI).contains(&dpi) {
            return Err(OcrPipelineError::InvalidDpi(
                dpi,
                OCR_MIN_IMAGE_DPI,
                OCR_MAX_IMAGE_DPI,
            ));
        }
        if width_px == 0
            || height_px == 0
            || width_px > MAX_IMAGE_DIMENSION
            || height_px > MAX_IMAGE_DIMENSION
        {
            return Err(OcrPipelineError::InvalidImage(format!(
                "image geometry {width_px}x{height_px} is outside the supported bounds"
            )));
        }
        let width = usize::try_from(width_px)
            .map_err(|_| OcrPipelineError::InvalidImage("width does not fit usize".to_owned()))?;
        let height = usize::try_from(height_px)
            .map_err(|_| OcrPipelineError::InvalidImage("height does not fit usize".to_owned()))?;
        let pixels_count = width.checked_mul(height).ok_or_else(|| {
            OcrPipelineError::InvalidImage("pixel count overflows usize".to_owned())
        })?;
        if pixels_count > MAX_IMAGE_PIXELS {
            return Err(OcrPipelineError::InvalidImage(format!(
                "pixel count {pixels_count} exceeds {MAX_IMAGE_PIXELS}"
            )));
        }
        let expected = pixels_count
            .checked_mul(format.bytes_per_pixel())
            .ok_or_else(|| {
                OcrPipelineError::InvalidImage("pixel buffer length overflows usize".to_owned())
            })?;
        if pixels.len() != expected {
            return Err(OcrPipelineError::InvalidImage(format!(
                "expected {expected} pixel bytes, got {}",
                pixels.len()
            )));
        }
        Ok(Self {
            page,
            width_px,
            height_px,
            dpi,
            format,
            pixels,
        })
    }

    /// Return the zero-based page number this image belongs to.
    #[must_use]
    pub const fn page(&self) -> usize {
        self.page
    }

    /// Return the raster width in pixels.
    #[must_use]
    pub const fn width_px(&self) -> u32 {
        self.width_px
    }

    /// Return the raster height in pixels.
    #[must_use]
    pub const fn height_px(&self) -> u32 {
        self.height_px
    }

    /// Return the resolution in dots per inch.
    #[must_use]
    pub const fn dpi(&self) -> f64 {
        self.dpi
    }

    /// Return the pixel layout.
    #[must_use]
    pub const fn format(&self) -> PixelFormat {
        self.format
    }

    /// Return the raw pixel bytes.
    #[must_use]
    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }
}

/// An axis-aligned word box in image pixel coordinates.
///
/// The origin is the top-left corner; `x` grows right and `y` grows downward.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OcrBox {
    /// Left edge in pixels.
    pub x: f64,
    /// Top edge in pixels.
    pub y: f64,
    /// Box width in pixels.
    pub width: f64,
    /// Box height in pixels.
    pub height: f64,
}

impl OcrBox {
    /// Create a box from its top-left corner and extent.
    #[must_use]
    pub const fn new(x: f64, y: f64, width: f64, height: f64) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    /// Return `true` when all coordinates are finite and the extent is
    /// non-negative.
    #[must_use]
    pub const fn is_valid(self) -> bool {
        self.x.is_finite()
            && self.y.is_finite()
            && self.width.is_finite()
            && self.height.is_finite()
            && self.width >= 0.0
            && self.height >= 0.0
    }

    /// Clamp this box into the source raster, preserving its extent where
    /// possible and never producing a negative coordinate or extent.
    #[must_use]
    pub fn clamped(self, width_px: u32, height_px: u32) -> Self {
        let max_width = f64::from(width_px);
        let max_height = f64::from(height_px);
        let x = self.x.clamp(0.0, max_width);
        let y = self.y.clamp(0.0, max_height);
        let width = self.width.clamp(0.0, max_width - x);
        let height = self.height.clamp(0.0, max_height - y);
        Self {
            x,
            y,
            width,
            height,
        }
    }
}

/// One recognized word with its box and confidence.
#[derive(Debug, Clone, PartialEq)]
pub struct OcrWord {
    /// The recognized UTF-8 word text.
    pub text: String,
    /// Bounding box in image pixel coordinates.
    pub bbox: OcrBox,
    /// Recognition confidence on a `0..=100` scale.
    pub confidence: f32,
}

impl OcrWord {
    /// Create a recognized word.
    #[must_use]
    pub fn new(text: impl Into<String>, bbox: OcrBox, confidence: f32) -> Self {
        Self {
            text: text.into(),
            bbox,
            confidence,
        }
    }
}

/// Join non-empty text fragments with exactly one space, with no leading or
/// trailing whitespace. This is the single deterministic text-assembly rule
/// used by both the raw page result and the searchable-text layer.
fn join_non_empty<'a>(texts: impl IntoIterator<Item = &'a str>) -> String {
    let mut out = String::new();
    for text in texts {
        if text.is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(text);
    }
    out
}

/// All recognized words for one page.
///
/// Words are sorted into a deterministic reading order (top-to-bottom, then
/// left-to-right, then by text and confidence) at construction, so two engines
/// returning the same words in different orders produce equal values.
#[derive(Debug, Clone, PartialEq)]
pub struct OcrPageText {
    page: usize,
    words: Vec<OcrWord>,
    detected_language: String,
}

impl OcrPageText {
    /// Create a page result, sorting its words into deterministic reading order.
    #[must_use]
    pub fn new(page: usize, mut words: Vec<OcrWord>, detected_language: impl Into<String>) -> Self {
        words.sort_by(|left, right| {
            left.bbox
                .y
                .total_cmp(&right.bbox.y)
                .then_with(|| left.bbox.x.total_cmp(&right.bbox.x))
                .then_with(|| left.text.cmp(&right.text))
                .then_with(|| left.confidence.total_cmp(&right.confidence))
        });
        Self {
            page,
            words,
            detected_language: detected_language.into(),
        }
    }

    /// Return the zero-based page number.
    #[must_use]
    pub const fn page(&self) -> usize {
        self.page
    }

    /// Return the words in deterministic reading order.
    #[must_use]
    pub fn words(&self) -> &[OcrWord] {
        &self.words
    }

    /// Return the number of recognized words.
    #[must_use]
    pub fn word_count(&self) -> usize {
        self.words.len()
    }

    /// Return `true` when no words were recognized.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.words.is_empty()
    }

    /// Return the page-level detected language tag.
    #[must_use]
    pub fn detected_language(&self) -> &str {
        &self.detected_language
    }

    /// Assemble the recognized words into deterministic page text.
    #[must_use]
    pub fn text(&self) -> String {
        join_non_empty(self.words.iter().map(|word| word.text.as_str()))
    }
}

/// Options passed to [`OcrEngine::recognize`].
#[derive(Debug, Clone, PartialEq)]
pub struct OcrOptions {
    /// Requested language tags; an empty vector lets the engine decide.
    pub languages: Vec<String>,
    /// Minimum confidence for a word to be kept, on a `0..=100` scale.
    pub confidence_threshold: f32,
}

impl Default for OcrOptions {
    fn default() -> Self {
        Self {
            languages: Vec::new(),
            confidence_threshold: DEFAULT_CONFIDENCE_THRESHOLD,
        }
    }
}

impl OcrOptions {
    /// Build options with the given language list and default threshold.
    #[must_use]
    pub fn new(languages: Vec<String>) -> Self {
        Self {
            languages,
            ..Self::default()
        }
    }

    /// Validate language tags and the confidence threshold.
    ///
    /// # Errors
    ///
    /// Returns [`OcrPipelineError::InvalidOptions`] for a blank language tag or
    /// a non-finite/out-of-range confidence threshold.
    pub fn validate(&self) -> Result<(), OcrPipelineError> {
        for language in &self.languages {
            if language.trim().is_empty() {
                return Err(OcrPipelineError::InvalidOptions(
                    "language tags must not be blank".to_owned(),
                ));
            }
        }
        if !self.confidence_threshold.is_finite()
            || !(0.0..=100.0).contains(&self.confidence_threshold)
        {
            return Err(OcrPipelineError::InvalidOptions(format!(
                "confidence threshold {} is outside 0..=100",
                self.confidence_threshold
            )));
        }
        Ok(())
    }
}

/// A caller-supplied backend capable of recognizing text in an [`OcrImage`].
///
/// Implementations must be deterministic for a given input and must return
/// named errors rather than panicking. The trait is object-safe so a caller can
/// hold a `&dyn OcrEngine`. It deliberately takes no process, file, or network
/// handle: managing an external sidecar is the caller's responsibility.
pub trait OcrEngine {
    /// Return the human-readable name of this engine.
    fn name(&self) -> &str;

    /// Recognize `image` using `options`, producing one [`OcrPageText`].
    ///
    /// The returned page number must equal [`OcrImage::page`]; the pipeline
    /// rejects a mismatch with [`OcrPipelineError::PageMismatch`].
    fn recognize(
        &self,
        image: &OcrImage,
        options: &OcrOptions,
    ) -> Result<OcrPageText, OcrPipelineError>;
}

/// A deterministic confidence threshold policy.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ConfidencePolicy {
    keep_threshold: f32,
    suspect_threshold: f32,
}

impl ConfidencePolicy {
    /// Build a policy from a keep threshold and a suspect threshold.
    ///
    /// Both values must be finite and within `0..=100`.
    ///
    /// # Errors
    ///
    /// Returns [`OcrPipelineError::InvalidOptions`] when either threshold is
    /// outside the documented range.
    pub fn new(keep_threshold: f32, suspect_threshold: f32) -> Result<Self, OcrPipelineError> {
        for threshold in [keep_threshold, suspect_threshold] {
            if !threshold.is_finite() || !(0.0..=100.0).contains(&threshold) {
                return Err(OcrPipelineError::InvalidOptions(format!(
                    "confidence threshold {threshold} is outside 0..=100"
                )));
            }
        }
        Ok(Self {
            keep_threshold,
            suspect_threshold,
        })
    }

    /// Return the minimum confidence for a word to be kept.
    #[must_use]
    pub const fn keep_threshold(&self) -> f32 {
        self.keep_threshold
    }

    /// Return the confidence below which a kept word is flagged as suspect.
    #[must_use]
    pub const fn suspect_threshold(&self) -> f32 {
        self.suspect_threshold
    }
}

impl Default for ConfidencePolicy {
    fn default() -> Self {
        Self {
            keep_threshold: DEFAULT_CONFIDENCE_THRESHOLD,
            suspect_threshold: DEFAULT_SUSPECT_THRESHOLD,
        }
    }
}

/// The position of one placed word in PDF points.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TextPosition {
    /// Left edge in PDF points.
    pub x_pt: f64,
    /// Bottom edge in PDF points (PDF coordinates grow upward).
    pub y_pt: f64,
    /// Width in PDF points.
    pub width_pt: f64,
    /// Height in PDF points.
    pub height_pt: f64,
}

/// One kept word together with its invisible text-layer placement.
#[derive(Debug, Clone, PartialEq)]
pub struct PlacedWord {
    text: String,
    position: TextPosition,
    confidence: f32,
    suspect: bool,
}

impl PlacedWord {
    /// Create a placed word.
    #[must_use]
    pub const fn new(text: String, position: TextPosition, confidence: f32, suspect: bool) -> Self {
        Self {
            text,
            position,
            confidence,
            suspect,
        }
    }

    /// Return the word text.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Return the placement in PDF points.
    #[must_use]
    pub const fn position(&self) -> TextPosition {
        self.position
    }

    /// Return the recognition confidence.
    #[must_use]
    pub const fn confidence(&self) -> f32 {
        self.confidence
    }

    /// Return `true` when the word was flagged for human review.
    #[must_use]
    pub const fn is_suspect(&self) -> bool {
        self.suspect
    }
}

/// The searchable-text layer planned for one page.
#[derive(Debug, Clone, PartialEq)]
pub struct SearchableTextLayer {
    page: usize,
    placed: Vec<PlacedWord>,
    text: String,
    source_word_count: usize,
}

impl SearchableTextLayer {
    /// Create a page layer.
    #[must_use]
    pub fn new(
        page: usize,
        placed: Vec<PlacedWord>,
        text: String,
        source_word_count: usize,
    ) -> Self {
        Self {
            page,
            placed,
            text,
            source_word_count,
        }
    }

    /// Return the zero-based page number.
    #[must_use]
    pub const fn page(&self) -> usize {
        self.page
    }

    /// Return the kept, placed words in reading order.
    #[must_use]
    pub fn placed_words(&self) -> &[PlacedWord] {
        &self.placed
    }

    /// Return the assembled page text.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Return the number of words recognized before confidence filtering.
    #[must_use]
    pub const fn source_word_count(&self) -> usize {
        self.source_word_count
    }

    /// Return `true` when the layer has no kept words.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.placed.is_empty()
    }
}

/// A deterministic report of one OCR pipeline run.
#[derive(Debug, Clone, PartialEq)]
pub struct OcrReport {
    engine: String,
    page_count: usize,
    kept_words: usize,
    rejected_words: usize,
    suspect_words: usize,
    empty_pages: Vec<usize>,
    summary: String,
}

impl OcrReport {
    /// Return the engine name reported by [`OcrEngine::name`].
    #[must_use]
    pub fn engine(&self) -> &str {
        &self.engine
    }

    /// Return the number of planned pages.
    #[must_use]
    pub const fn page_count(&self) -> usize {
        self.page_count
    }

    /// Return the number of words kept in the searchable layer.
    #[must_use]
    pub const fn kept_words(&self) -> usize {
        self.kept_words
    }

    /// Return the number of words dropped by the confidence threshold.
    #[must_use]
    pub const fn rejected_words(&self) -> usize {
        self.rejected_words
    }

    /// Return the number of kept words flagged as suspect.
    #[must_use]
    pub const fn suspect_words(&self) -> usize {
        self.suspect_words
    }

    /// Return the sorted page numbers with no kept words.
    #[must_use]
    pub fn empty_pages(&self) -> &[usize] {
        &self.empty_pages
    }

    /// Return a stable, human-readable summary.
    #[must_use]
    pub fn summary(&self) -> &str {
        &self.summary
    }
}

/// The deterministic output of an OCR pipeline run.
#[derive(Debug, Clone, PartialEq)]
pub struct SearchableTextPlan {
    layers: Vec<SearchableTextLayer>,
    report: OcrReport,
}

impl SearchableTextPlan {
    /// Return the per-page searchable-text layers in ascending page order.
    #[must_use]
    pub fn layers(&self) -> &[SearchableTextLayer] {
        &self.layers
    }

    /// Return the deterministic run report.
    #[must_use]
    pub fn report(&self) -> &OcrReport {
        &self.report
    }

    /// Return the stable report summary.
    #[must_use]
    pub fn summary(&self) -> &str {
        self.report.summary()
    }
}

/// A typed failure in the OCR pipeline.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum OcrPipelineError {
    /// No input images were supplied.
    #[error("OCR job has no pages")]
    EmptyInput,
    /// More pages were supplied than the configured budget.
    #[error("OCR job has {count} pages (limit {limit})")]
    TooManyPages {
        /// Number of supplied pages.
        count: usize,
        /// Enforced page budget.
        limit: usize,
    },
    /// A page number appeared more than once.
    #[error("duplicate OCR page {page}")]
    DuplicatePage {
        /// The repeated page number.
        page: usize,
    },
    /// The image geometry or pixel buffer is invalid.
    #[error("invalid OCR image: {0}")]
    InvalidImage(String),
    /// The image DPI lies outside the documented range.
    #[error("OCR image DPI {0} is outside {1}..={2}")]
    InvalidDpi(f64, f64, f64),
    /// The supplied options or confidence policy are invalid.
    #[error("invalid OCR options: {0}")]
    InvalidOptions(String),
    /// A recognized word box was malformed.
    #[error("invalid word box on page {page}: {reason}")]
    InvalidBox {
        /// Page of the offending word.
        page: usize,
        /// Stable explanation.
        reason: &'static str,
    },
    /// A recognized word confidence was not a finite number.
    #[error("invalid confidence on page {page}: {confidence}")]
    InvalidConfidence {
        /// Page of the offending word.
        page: usize,
        /// The rejected confidence value.
        confidence: f32,
    },
    /// The engine returned a different page number than requested.
    #[error("OCR page mismatch: expected {expected}, got {actual}")]
    PageMismatch {
        /// Page number that was submitted.
        expected: usize,
        /// Page number the engine returned.
        actual: usize,
    },
    /// The engine returned too many words for one page.
    #[error("page {page} has {count} words (limit {limit})")]
    TooManyWords {
        /// Offending page number.
        page: usize,
        /// Number of words returned.
        count: usize,
        /// Enforced per-page budget.
        limit: usize,
    },
    /// The caller-supplied engine reported a backend failure.
    #[error("OCR engine {engine} failed on page {page}: {message}")]
    Engine {
        /// Page that failed.
        page: usize,
        /// Engine name from [`OcrEngine::name`].
        engine: String,
        /// Backend-supplied message.
        message: String,
    },
}

/// Map a clamped pixel box into PDF points using the image resolution.
///
/// The pixel origin is top-left and the PDF origin is bottom-left, so `y_pt` is
/// derived from the box bottom edge measured upward from the page bottom.
fn map_box(bbox: OcrBox, image: &OcrImage) -> TextPosition {
    let scale = 72.0 / image.dpi();
    let page_height_pt = f64::from(image.height_px()) * scale;
    TextPosition {
        x_pt: bbox.x * scale,
        y_pt: page_height_pt - (bbox.y + bbox.height) * scale,
        width_pt: bbox.width * scale,
        height_pt: bbox.height * scale,
    }
}

/// Run the OCR pipeline over caller-supplied images and engine.
///
/// Images are processed in ascending page order regardless of input order.
/// For each page the engine is invoked once, its words are thresholded by
/// `policy`, and the kept words are converted from pixel boxes into PDF-point
/// positions for an invisible searchable-text layer.
///
/// # Errors
///
/// Returns a typed [`OcrPipelineError`] for invalid options or images, an
/// empty or over-budget job, duplicate/mismatched pages, an oversized word
/// list, a malformed box or confidence, or a backend failure reported by the
/// engine.
pub fn run_ocr_pipeline(
    images: &[OcrImage],
    engine: &dyn OcrEngine,
    options: &OcrOptions,
    policy: &ConfidencePolicy,
) -> Result<SearchableTextPlan, OcrPipelineError> {
    options.validate()?;
    if images.is_empty() {
        return Err(OcrPipelineError::EmptyInput);
    }
    if images.len() > OCR_MAX_PAGES {
        return Err(OcrPipelineError::TooManyPages {
            count: images.len(),
            limit: OCR_MAX_PAGES,
        });
    }

    let mut order: Vec<usize> = (0..images.len()).collect();
    order.sort_by(|left, right| images[*left].page().cmp(&images[*right].page()));

    let mut seen = BTreeSet::new();
    let mut layers = Vec::with_capacity(images.len());
    let mut kept_words = 0usize;
    let mut rejected_words = 0usize;
    let mut suspect_words = 0usize;
    let mut empty_pages = Vec::new();

    for index in order {
        let image = &images[index];
        if !seen.insert(image.page()) {
            return Err(OcrPipelineError::DuplicatePage { page: image.page() });
        }
        let result = engine.recognize(image, options)?;
        if result.page() != image.page() {
            return Err(OcrPipelineError::PageMismatch {
                expected: image.page(),
                actual: result.page(),
            });
        }
        if result.word_count() > OCR_MAX_WORDS_PER_PAGE {
            return Err(OcrPipelineError::TooManyWords {
                page: image.page(),
                count: result.word_count(),
                limit: OCR_MAX_WORDS_PER_PAGE,
            });
        }

        let mut placed = Vec::new();
        for word in result.words() {
            if !word.confidence.is_finite() {
                return Err(OcrPipelineError::InvalidConfidence {
                    page: image.page(),
                    confidence: word.confidence,
                });
            }
            if !word.bbox.is_valid() {
                return Err(OcrPipelineError::InvalidBox {
                    page: image.page(),
                    reason: "box must be finite with non-negative extent",
                });
            }
            if word.confidence < policy.keep_threshold() {
                rejected_words += 1;
                continue;
            }
            let bbox = word.bbox.clamped(image.width_px(), image.height_px());
            let suspect = word.confidence < policy.suspect_threshold();
            if suspect {
                suspect_words += 1;
            }
            placed.push(PlacedWord::new(
                word.text.clone(),
                map_box(bbox, image),
                word.confidence,
                suspect,
            ));
        }

        let text = join_non_empty(placed.iter().map(PlacedWord::text));
        if placed.is_empty() {
            empty_pages.push(image.page());
        }
        kept_words += placed.len();
        layers.push(SearchableTextLayer::new(
            image.page(),
            placed,
            text,
            result.word_count(),
        ));
    }

    let summary = format!(
        "{} pages [{}] kept={} rejected={} suspect={} empty={}",
        layers.len(),
        engine.name(),
        kept_words,
        rejected_words,
        suspect_words,
        empty_pages.len()
    );
    let report = OcrReport {
        engine: engine.name().to_owned(),
        page_count: layers.len(),
        kept_words,
        rejected_words,
        suspect_words,
        empty_pages,
        summary,
    };
    Ok(SearchableTextPlan { layers, report })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::BTreeMap;

    fn image(page: usize) -> OcrImage {
        // 200x100 px at 100 dpi => 144x72 pt. Four grayscale bytes would be
        // wrong here; use RGBA with the exact expected buffer length.
        OcrImage::new(
            page,
            200,
            100,
            100.0,
            PixelFormat::Rgba,
            vec![0u8; 200 * 100 * 4],
        )
        .expect("valid test image")
    }

    fn gray_image(page: usize, width: u32, height: u32) -> OcrImage {
        let len = usize::try_from(width).unwrap_or(0) * usize::try_from(height).unwrap_or(0);
        OcrImage::new(
            page,
            width,
            height,
            100.0,
            PixelFormat::Grayscale,
            vec![0u8; len],
        )
        .expect("valid test image")
    }

    struct MockEngine {
        pages: BTreeMap<usize, Vec<OcrWord>>,
        calls: RefCell<Vec<usize>>,
        fail_page: Option<usize>,
        wrong_page: Option<usize>,
    }

    impl MockEngine {
        fn new(pages: BTreeMap<usize, Vec<OcrWord>>) -> Self {
            Self {
                pages,
                calls: RefCell::new(Vec::new()),
                fail_page: None,
                wrong_page: None,
            }
        }
    }

    impl OcrEngine for MockEngine {
        fn name(&self) -> &str {
            "mock-ocr"
        }

        fn recognize(
            &self,
            image: &OcrImage,
            _options: &OcrOptions,
        ) -> Result<OcrPageText, OcrPipelineError> {
            self.calls.borrow_mut().push(image.page());
            if self.fail_page == Some(image.page()) {
                return Err(OcrPipelineError::Engine {
                    page: image.page(),
                    engine: self.name().to_owned(),
                    message: "backend exploded".to_owned(),
                });
            }
            let reported = self.wrong_page.unwrap_or(image.page());
            let words = self.pages.get(&image.page()).cloned().unwrap_or_default();
            Ok(OcrPageText::new(reported, words, "eng"))
        }
    }

    #[test]
    fn engine_is_invoked_once_per_page() {
        let mut pages = BTreeMap::new();
        pages.insert(
            0,
            vec![OcrWord::new(
                "Hello",
                OcrBox::new(10.0, 20.0, 30.0, 10.0),
                95.0,
            )],
        );
        let engine = MockEngine::new(pages);
        let plan = run_ocr_pipeline(
            &[image(0), image(1)],
            &engine,
            &OcrOptions::default(),
            &ConfidencePolicy::default(),
        )
        .expect("pipeline");
        assert_eq!(engine.calls.borrow().as_slice(), &[0, 1]);
        assert_eq!(plan.report().page_count(), 2);
        assert_eq!(plan.report().engine(), "mock-ocr");
    }

    #[test]
    fn word_boxes_map_to_pdf_points_with_top_left_inversion() {
        let mut pages = BTreeMap::new();
        pages.insert(
            0,
            vec![OcrWord::new(
                "Word",
                OcrBox::new(10.0, 20.0, 30.0, 10.0),
                90.0,
            )],
        );
        let engine = MockEngine::new(pages);
        let plan = run_ocr_pipeline(
            &[image(0)],
            &engine,
            &OcrOptions::default(),
            &ConfidencePolicy::default(),
        )
        .expect("pipeline");
        let position = plan.layers()[0].placed_words()[0].position();
        assert!(
            (position.x_pt - 7.2).abs() < 1e-9,
            "10 px at 100 dpi = 7.2 pt"
        );
        assert!((position.width_pt - 21.6).abs() < 1e-9);
        assert!((position.height_pt - 7.2).abs() < 1e-9);
        // Page height is 72 pt; box bottom is at y=30 px = 21.6 pt from the top.
        assert!((position.y_pt - 50.4).abs() < 1e-9, "72 - 21.6 = 50.4 pt");
    }

    #[test]
    fn out_of_range_boxes_are_clamped_to_the_raster() {
        let mut pages = BTreeMap::new();
        pages.insert(
            0,
            vec![OcrWord::new(
                "Edge",
                OcrBox::new(190.0, 95.0, 50.0, 40.0),
                90.0,
            )],
        );
        let engine = MockEngine::new(pages);
        let plan = run_ocr_pipeline(
            &[image(0)],
            &engine,
            &OcrOptions::default(),
            &ConfidencePolicy::default(),
        )
        .expect("pipeline");
        let position = plan.layers()[0].placed_words()[0].position();
        assert!(
            (position.width_pt - 7.2).abs() < 1e-9,
            "50 px clamped to 10 px"
        );
        assert!(
            (position.height_pt - 3.6).abs() < 1e-9,
            "40 px clamped to 5 px"
        );
        assert!(
            (position.y_pt - 0.0).abs() < 1e-9,
            "clamped to the page bottom"
        );
    }

    #[test]
    fn confidence_threshold_filters_and_flags_suspect_words() {
        let mut pages = BTreeMap::new();
        pages.insert(
            0,
            vec![
                OcrWord::new("high", OcrBox::new(0.0, 0.0, 5.0, 5.0), 95.0),
                OcrWord::new("mid", OcrBox::new(0.0, 10.0, 5.0, 5.0), 70.0),
                OcrWord::new("low", OcrBox::new(0.0, 20.0, 5.0, 5.0), 50.0),
            ],
        );
        let engine = MockEngine::new(pages);
        let policy = ConfidencePolicy::new(60.0, 80.0).expect("policy");
        let plan = run_ocr_pipeline(&[image(0)], &engine, &OcrOptions::default(), &policy)
            .expect("pipeline");
        assert_eq!(plan.report().kept_words(), 2);
        assert_eq!(plan.report().rejected_words(), 1);
        assert_eq!(plan.report().suspect_words(), 1);
        let layer = &plan.layers()[0];
        assert_eq!(layer.text(), "high mid");
        assert!(!layer.placed_words()[0].is_suspect());
        assert!(layer.placed_words()[1].is_suspect());
    }

    #[test]
    fn empty_ocr_result_yields_an_empty_searchable_layer() {
        let engine = MockEngine::new(BTreeMap::new());
        let plan = run_ocr_pipeline(
            &[image(4)],
            &engine,
            &OcrOptions::default(),
            &ConfidencePolicy::default(),
        )
        .expect("pipeline");
        assert!(plan.layers()[0].is_empty());
        assert_eq!(plan.layers()[0].text(), "");
        assert_eq!(plan.report().empty_pages(), &[4]);
        assert_eq!(plan.report().kept_words(), 0);
    }

    #[test]
    fn pages_are_processed_in_deterministic_page_order() {
        let mut pages = BTreeMap::new();
        pages.insert(
            2,
            vec![OcrWord::new("two", OcrBox::new(0.0, 0.0, 1.0, 1.0), 99.0)],
        );
        pages.insert(
            0,
            vec![OcrWord::new("zero", OcrBox::new(0.0, 0.0, 1.0, 1.0), 99.0)],
        );
        pages.insert(
            1,
            vec![OcrWord::new("one", OcrBox::new(0.0, 0.0, 1.0, 1.0), 99.0)],
        );
        let forward = MockEngine::new(pages.clone());
        let plan_a = run_ocr_pipeline(
            &[image(2), image(0), image(1)],
            &forward,
            &OcrOptions::default(),
            &ConfidencePolicy::default(),
        )
        .expect("pipeline a");
        assert_eq!(forward.calls.borrow().as_slice(), &[0, 1, 2]);

        let reverse = MockEngine::new(pages);
        let plan_b = run_ocr_pipeline(
            &[image(1), image(2), image(0)],
            &reverse,
            &OcrOptions::default(),
            &ConfidencePolicy::default(),
        )
        .expect("pipeline b");
        assert_eq!(plan_a, plan_b);
        assert_eq!(
            plan_a
                .layers()
                .iter()
                .map(SearchableTextLayer::page)
                .collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
    }

    #[test]
    fn page_text_assembly_is_stable_under_input_word_order() {
        let first = OcrPageText::new(
            0,
            vec![
                OcrWord::new("world", OcrBox::new(20.0, 0.0, 5.0, 5.0), 80.0),
                OcrWord::new("hello", OcrBox::new(0.0, 0.0, 5.0, 5.0), 90.0),
            ],
            "eng",
        );
        let second = OcrPageText::new(
            0,
            vec![
                OcrWord::new("hello", OcrBox::new(0.0, 0.0, 5.0, 5.0), 90.0),
                OcrWord::new("world", OcrBox::new(20.0, 0.0, 5.0, 5.0), 80.0),
            ],
            "eng",
        );
        assert_eq!(first, second);
        assert_eq!(first.text(), "hello world");
    }

    #[test]
    fn invalid_image_options_and_policy_are_typed_errors() {
        assert_eq!(
            run_ocr_pipeline(
                &[],
                &MockEngine::new(BTreeMap::new()),
                &OcrOptions::default(),
                &ConfidencePolicy::default()
            )
            .expect_err("empty job"),
            OcrPipelineError::EmptyInput
        );

        let bad_dpi = OcrImage::new(0, 2, 2, f64::NAN, PixelFormat::Grayscale, vec![0u8; 4])
            .expect_err("non-finite DPI");
        assert!(matches!(bad_dpi, OcrPipelineError::InvalidDpi(..)));

        let bad_len = OcrImage::new(0, 2, 2, 100.0, PixelFormat::Grayscale, vec![0u8; 3])
            .expect_err("wrong pixel buffer length");
        assert!(matches!(bad_len, OcrPipelineError::InvalidImage(_)));

        let options = OcrOptions::new(vec!["  ".to_owned()]);
        let error = run_ocr_pipeline(
            &[gray_image(0, 2, 2)],
            &MockEngine::new(BTreeMap::new()),
            &options,
            &ConfidencePolicy::default(),
        )
        .expect_err("blank language must be rejected");
        assert!(matches!(error, OcrPipelineError::InvalidOptions(_)));

        assert!(matches!(
            ConfidencePolicy::new(f32::NAN, 80.0),
            Err(OcrPipelineError::InvalidOptions(_))
        ));
    }

    #[test]
    fn engine_failure_page_mismatch_and_duplicate_pages_are_typed() {
        let mut engine = MockEngine::new(BTreeMap::new());
        engine.fail_page = Some(1);
        let error = run_ocr_pipeline(
            &[image(1)],
            &engine,
            &OcrOptions::default(),
            &ConfidencePolicy::default(),
        )
        .expect_err("engine failure");
        assert!(matches!(error, OcrPipelineError::Engine { page: 1, .. }));

        let mut engine = MockEngine::new(BTreeMap::new());
        engine.wrong_page = Some(9);
        let error = run_ocr_pipeline(
            &[image(1)],
            &engine,
            &OcrOptions::default(),
            &ConfidencePolicy::default(),
        )
        .expect_err("page mismatch");
        assert_eq!(
            error,
            OcrPipelineError::PageMismatch {
                expected: 1,
                actual: 9
            }
        );

        let error = run_ocr_pipeline(
            &[image(1), image(1)],
            &MockEngine::new(BTreeMap::new()),
            &OcrOptions::default(),
            &ConfidencePolicy::default(),
        )
        .expect_err("duplicate page");
        assert_eq!(error, OcrPipelineError::DuplicatePage { page: 1 });
    }

    #[test]
    fn malformed_boxes_and_confidences_are_typed_errors() {
        let mut pages = BTreeMap::new();
        pages.insert(
            0,
            vec![OcrWord::new("bad", OcrBox::new(0.0, 0.0, -1.0, 5.0), 90.0)],
        );
        let error = run_ocr_pipeline(
            &[image(0)],
            &MockEngine::new(pages),
            &OcrOptions::default(),
            &ConfidencePolicy::default(),
        )
        .expect_err("negative box extent");
        assert!(matches!(
            error,
            OcrPipelineError::InvalidBox { page: 0, .. }
        ));

        let mut pages = BTreeMap::new();
        pages.insert(
            0,
            vec![OcrWord::new(
                "bad",
                OcrBox::new(0.0, 0.0, 5.0, 5.0),
                f32::NAN,
            )],
        );
        let error = run_ocr_pipeline(
            &[image(0)],
            &MockEngine::new(pages),
            &OcrOptions::default(),
            &ConfidencePolicy::default(),
        )
        .expect_err("non-finite confidence");
        assert!(matches!(
            error,
            OcrPipelineError::InvalidConfidence { page: 0, .. }
        ));
    }

    #[test]
    fn plan_and_report_are_byte_deterministic() {
        let mut pages = BTreeMap::new();
        pages.insert(
            0,
            vec![OcrWord::new(
                "stable",
                OcrBox::new(1.0, 2.0, 3.0, 4.0),
                88.0,
            )],
        );
        let first = run_ocr_pipeline(
            &[image(0)],
            &MockEngine::new(pages.clone()),
            &OcrOptions::default(),
            &ConfidencePolicy::default(),
        )
        .expect("first run");
        let second = run_ocr_pipeline(
            &[image(0)],
            &MockEngine::new(pages),
            &OcrOptions::default(),
            &ConfidencePolicy::default(),
        )
        .expect("second run");
        assert_eq!(first, second);
        assert_eq!(first.summary(), second.summary());
        assert!(first.summary().contains("kept=1 rejected=0 suspect=0"));
    }

    #[test]
    fn image_bounds_constants_are_reused() {
        assert_eq!(MAX_IMAGE_DIMENSION, crate::image::MAX_IMAGE_DIMENSION);
        assert_eq!(MAX_IMAGE_PIXELS, crate::image::MAX_IMAGE_PIXELS);
        let error = OcrImage::new(
            0,
            MAX_IMAGE_DIMENSION + 1,
            1,
            100.0,
            PixelFormat::Grayscale,
            Vec::new(),
        )
        .expect_err("image dimension bound must be reused");
        assert!(matches!(error, OcrPipelineError::InvalidImage(_)));
    }
}
