//! OCR engine contract, language routing, sidecar wire format, and confidence
//! policy for `pdfxi-text`.
//!
//! This module is **pure data and policy**. It defines the boundary types an
//! external OCR backend (a future out-of-process Tesseract sidecar) must speak,
//! but it never spawns a process, opens a file, or touches the network. An
//! [`OcrEngine`] implementation supplied by a caller performs recognition; the
//! types here validate its inputs and outputs and make them deterministic.
//!
//! # Design notes
//!
//! * [`OcrImage`] carries owned pixels in one of two layouts (8-bit grayscale
//!   or 8-bit RGBA) together with a DPI. Construction validates dimensions,
//!   pixel count, and the documented DPI range so a hostile caller cannot
//!   trick a backend into reading out of bounds.
//! * [`LanguageRouter`] maps a requested language list onto an engine's
//!   supported set. `auto` is resolved with a small, deterministic script
//!   heuristic over already-extracted text ([`detect_script`]). Unsupported
//!   requests either error ([`FallbackPolicy::Strict`]) or resolve to the
//!   engine's first supported language with a recorded
//!   [`FallbackReason`](enum@FallbackReason).
//! * [`SidecarRequest`] and [`SidecarResponse`] are plain-data models with a
//!   hand-written, dependency-free JSON codec ([`SidecarRequest::to_json`] /
//!   [`SidecarRequest::from_json`]). Pixel bytes travel as lowercase hexadecimal
//!   so the encoding needs no base64 crate. Every decoder path revalidates the
//!   documented limits.
//! * [`ConfidencePolicy`] filters low-confidence spans and produces a
//!   [`SuspectReport`] of spans worth a human review, including "unusual"
//!   characters such as replacement glyphs, zero-width marks, and control
//!   characters ([`is_unusual_character`]).

use std::collections::BTreeMap;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Minimum accepted image resolution in dots per inch.
///
/// Values below this are rejected with [`OcrError::InvalidDpi`].
pub const MIN_IMAGE_DPI: f32 = 1.0;
/// Maximum accepted image resolution in dots per inch.
///
/// Beyond this the sidecar would spend unreasonable time and memory, so such
/// values are rejected with [`OcrError::InvalidDpi`].
pub const MAX_IMAGE_DPI: f32 = 2400.0;
/// Largest accepted value for either image dimension, in pixels.
pub const MAX_IMAGE_DIMENSION: u32 = 20_000;
/// Largest accepted total pixel count for an image.
///
/// This bounds the work a decoder may perform on untrusted sidecar input.
pub const MAX_IMAGE_PIXELS: usize = 100_000_000;
/// Wire-format version emitted and accepted by the sidecar JSON codec.
pub const SUPPORTED_SIDECAR_VERSION: u32 = 1;
/// Default minimum span confidence (on a `0..=100` scale) kept by
/// [`filter_by_confidence`].
pub const DEFAULT_CONFIDENCE_THRESHOLD: f32 = 60.0;
/// Default confidence below which a span is included in a [`SuspectReport`].
pub const DEFAULT_SUSPECT_THRESHOLD: f32 = 80.0;

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Errors produced by the OCR contract, routing, sidecar codec, or policy.
#[derive(Debug, thiserror::Error, PartialEq)]
pub enum OcrError {
    /// The image metadata or pixel buffer is internally inconsistent.
    #[error("invalid OCR image: {0}")]
    InvalidImage(String),
    /// The image DPI lies outside [`MIN_IMAGE_DPI`]`..=`[`MAX_IMAGE_DPI`].
    #[error("image DPI {0} is outside the supported range {1}..={2}")]
    InvalidDpi(f32, f32, f32),
    /// A language tag is empty, malformed, or not ASCII.
    #[error("invalid language tag: {0}")]
    InvalidLanguageTag(String),
    /// A language was requested in strict mode but the engine lacks support.
    #[error("unsupported language: {0}")]
    UnsupportedLanguage(String),
    /// The engine advertises no supported languages, so no fallback exists.
    #[error("OCR engine has no supported languages")]
    NoSupportedLanguages,
    /// A sidecar payload fails structural or semantic validation.
    #[error("invalid sidecar payload: {0}")]
    InvalidSidecar(String),
    /// The sidecar JSON is malformed or has unexpected fields/types.
    #[error("sidecar JSON error: {0}")]
    SidecarJson(String),
    /// A [`ConfidencePolicy`] has out-of-range thresholds.
    #[error("invalid confidence policy: {0}")]
    InvalidConfidencePolicy(String),
    /// A backend-specific failure not covered by a typed variant.
    #[error("OCR backend error: {0}")]
    Backend(String),
    /// The operation was cancelled through a shared cancellation token.
    #[error("OCR operation was cancelled")]
    Cancelled,
}

// ---------------------------------------------------------------------------
// Image model
// ---------------------------------------------------------------------------

/// Pixel layout of an [`OcrImage`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OcrPixelFormat {
    /// One 8-bit luminance byte per pixel.
    Grayscale,
    /// Four 8-bit bytes per pixel in red, green, blue, alpha order.
    Rgba,
}

impl OcrPixelFormat {
    /// Return the number of bytes used by one pixel of this format.
    pub const fn bytes_per_pixel(self) -> usize {
        match self {
            Self::Grayscale => 1,
            Self::Rgba => 4,
        }
    }

    /// Return the sidecar token for this format (`"gray"` or `"rgba"`).
    pub const fn sidecar_token(self) -> &'static str {
        match self {
            Self::Grayscale => "gray",
            Self::Rgba => "rgba",
        }
    }

    /// Parse a sidecar format token, returning `None` for unknown tokens.
    pub fn from_sidecar_token(token: &str) -> Option<Self> {
        match token {
            "gray" => Some(Self::Grayscale),
            "rgba" => Some(Self::Rgba),
            _ => None,
        }
    }
}

/// Compute the expected byte length of an image, enforcing all size limits.
fn expected_byte_len(width: u32, height: u32, format: OcrPixelFormat) -> Result<usize, OcrError> {
    if width == 0 || height == 0 {
        return Err(OcrError::InvalidImage(
            "width and height must be positive".to_string(),
        ));
    }
    if width > MAX_IMAGE_DIMENSION || height > MAX_IMAGE_DIMENSION {
        return Err(OcrError::InvalidImage(format!(
            "image dimension exceeds {MAX_IMAGE_DIMENSION}"
        )));
    }
    let pixels = (width as usize)
        .checked_mul(height as usize)
        .ok_or_else(|| OcrError::InvalidImage("pixel count overflow".to_string()))?;
    if pixels > MAX_IMAGE_PIXELS {
        return Err(OcrError::InvalidImage(format!(
            "pixel count {pixels} exceeds {MAX_IMAGE_PIXELS}"
        )));
    }
    pixels
        .checked_mul(format.bytes_per_pixel())
        .ok_or_else(|| OcrError::InvalidImage("byte length overflow".to_string()))
}

/// An owned raster image handed to an [`OcrEngine`].
///
/// The pixel buffer length must exactly equal
/// `width * height * format.bytes_per_pixel()`; this is checked at construction
/// so backends can index without bounds checks of their own.
#[derive(Clone, Debug, PartialEq)]
pub struct OcrImage {
    width: u32,
    height: u32,
    dpi: f32,
    format: OcrPixelFormat,
    pixels: Vec<u8>,
}

impl OcrImage {
    /// Create and validate an image.
    ///
    /// Returns [`OcrError::InvalidDpi`] when `dpi` is not finite or falls
    /// outside the documented range, and [`OcrError::InvalidImage`] for bad
    /// dimensions or a pixel buffer whose length does not match the layout.
    pub fn new(
        width: u32,
        height: u32,
        dpi: f32,
        format: OcrPixelFormat,
        pixels: Vec<u8>,
    ) -> Result<Self, OcrError> {
        if !dpi.is_finite() || !(MIN_IMAGE_DPI..=MAX_IMAGE_DPI).contains(&dpi) {
            return Err(OcrError::InvalidDpi(dpi, MIN_IMAGE_DPI, MAX_IMAGE_DPI));
        }
        let expected = expected_byte_len(width, height, format)?;
        if pixels.len() != expected {
            return Err(OcrError::InvalidImage(format!(
                "expected {expected} pixel bytes, got {}",
                pixels.len()
            )));
        }
        Ok(Self {
            width,
            height,
            dpi,
            format,
            pixels,
        })
    }

    /// Return the image width in pixels.
    pub const fn width(&self) -> u32 {
        self.width
    }

    /// Return the image height in pixels.
    pub const fn height(&self) -> u32 {
        self.height
    }

    /// Return the image resolution in dots per inch.
    pub const fn dpi(&self) -> f32 {
        self.dpi
    }

    /// Return the pixel layout.
    pub const fn format(&self) -> OcrPixelFormat {
        self.format
    }

    /// Return the raw pixel bytes.
    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }
}

// ---------------------------------------------------------------------------
// Output model
// ---------------------------------------------------------------------------

/// An axis-aligned bounding box in image pixel coordinates.
///
/// The origin is the top-left corner; `x` grows right and `y` grows downward.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct OcrBox {
    /// Left edge in pixels.
    pub x: f32,
    /// Top edge in pixels.
    pub y: f32,
    /// Box width in pixels.
    pub width: f32,
    /// Box height in pixels.
    pub height: f32,
}

impl OcrBox {
    /// Create a bounding box from its top-left corner and extent.
    pub const fn new(x: f32, y: f32, width: f32, height: f32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }
}

/// One recognized text run.
#[derive(Clone, Debug, PartialEq)]
pub struct OcrSpan {
    /// The recognized UTF-8 text.
    pub text: String,
    /// Bounding box of the run in image pixel coordinates.
    pub bbox: OcrBox,
    /// Recognition confidence on a `0..=100` scale.
    pub confidence: f32,
    /// Language tag this run was recognized with.
    pub language: String,
}

impl OcrSpan {
    /// Create a recognized span.
    pub fn new(
        text: impl Into<String>,
        bbox: OcrBox,
        confidence: f32,
        language: impl Into<String>,
    ) -> Self {
        Self {
            text: text.into(),
            bbox,
            confidence,
            language: language.into(),
        }
    }
}

/// All recognized text for one page.
///
/// Spans are sorted into a deterministic reading order (top-to-bottom, then
/// left-to-right) at construction so two engines returning the same spans in
/// different orders produce equal [`OcrPage`] values.
#[derive(Clone, Debug, PartialEq)]
pub struct OcrPage {
    page: usize,
    spans: Vec<OcrSpan>,
    detected_language: String,
}

impl OcrPage {
    /// Create a page, sorting its spans into deterministic reading order.
    pub fn new(page: usize, mut spans: Vec<OcrSpan>, detected_language: impl Into<String>) -> Self {
        spans.sort_by(|a, b| {
            a.bbox
                .y
                .total_cmp(&b.bbox.y)
                .then_with(|| a.bbox.x.total_cmp(&b.bbox.x))
                .then_with(|| a.text.cmp(&b.text))
                .then_with(|| a.language.cmp(&b.language))
                .then_with(|| a.confidence.total_cmp(&b.confidence))
        });
        Self {
            page,
            spans,
            detected_language: detected_language.into(),
        }
    }

    /// Return the zero-based page number this recognition belongs to.
    pub const fn page(&self) -> usize {
        self.page
    }

    /// Return the recognized spans in deterministic reading order.
    pub fn spans(&self) -> &[OcrSpan] {
        &self.spans
    }

    /// Return the page-level detected language tag.
    pub fn detected_language(&self) -> &str {
        &self.detected_language
    }

    /// Return the number of recognized spans.
    pub fn span_count(&self) -> usize {
        self.spans.len()
    }

    /// Return `true` when the page has no recognized spans.
    pub fn is_empty(&self) -> bool {
        self.spans.is_empty()
    }

    /// Concatenate every span's text in reading order, with no separators.
    pub fn text(&self) -> String {
        let mut out = String::new();
        for span in &self.spans {
            out.push_str(&span.text);
        }
        out
    }
}

/// Options passed to [`OcrEngine::recognize`].
#[derive(Clone, Debug, PartialEq)]
pub struct OcrOptions {
    /// Requested language tags; an empty vector lets the engine decide.
    pub languages: Vec<String>,
    /// Minimum confidence for a span to be kept, on a `0..=100` scale.
    pub confidence_threshold: f32,
    /// Whether the backend should attempt page-orientation detection.
    pub detect_orientation: bool,
}

impl Default for OcrOptions {
    fn default() -> Self {
        Self {
            languages: Vec::new(),
            confidence_threshold: DEFAULT_CONFIDENCE_THRESHOLD,
            detect_orientation: false,
        }
    }
}

impl OcrOptions {
    /// Create options with the given language list and default thresholds.
    pub fn new(languages: Vec<String>) -> Self {
        Self {
            languages,
            ..Self::default()
        }
    }

    /// Return a copy with `languages` replaced.
    pub fn with_languages(mut self, languages: Vec<String>) -> Self {
        self.languages = languages;
        self
    }

    /// Return a copy with `confidence_threshold` replaced.
    pub fn with_confidence_threshold(mut self, confidence_threshold: f32) -> Self {
        self.confidence_threshold = confidence_threshold;
        self
    }

    /// Validate language tags and the confidence threshold.
    pub fn validate(&self) -> Result<(), OcrError> {
        for tag in &self.languages {
            validate_language_tag(tag)?;
        }
        if !self.confidence_threshold.is_finite()
            || !(0.0..=100.0).contains(&self.confidence_threshold)
        {
            return Err(OcrError::InvalidConfidencePolicy(format!(
                "confidence threshold {} is outside 0..=100",
                self.confidence_threshold
            )));
        }
        Ok(())
    }
}

/// A backend capable of recognizing text in an [`OcrImage`].
///
/// Implementations must be deterministic for a given input and must return
/// named errors instead of panicking. The trait is object-safe, so callers can
/// hold a `&dyn OcrEngine`. It deliberately takes no cancellation token or
/// progress callback: the contract layer stays pure, and a process-managing
/// wrapper (outside this crate) owns cancellation for the sidecar job.
pub trait OcrEngine {
    /// Return the human-readable name of this engine.
    fn name(&self) -> &str;

    /// Return the language tags this engine supports, in a stable order.
    fn languages(&self) -> &[String];

    /// Recognize `image` using `options`, producing one [`OcrPage`].
    ///
    /// The returned page's [`OcrPage::page`] is caller-defined by the backend;
    /// the sidecar bridge sets it explicitly via
    /// [`SidecarResponse::to_ocr_page`].
    fn recognize(&self, image: OcrImage, options: &OcrOptions) -> Result<OcrPage, OcrError>;
}

// ---------------------------------------------------------------------------
// Language tags and script detection
// ---------------------------------------------------------------------------

/// Validate a language tag.
///
/// A tag is either the special value `auto`, or one to five ASCII
/// alphanumeric subtags of one to eight characters each, separated by single
/// hyphens (for example `eng`, `en-US`, `zh-Hans`). Tags longer than 40 bytes
/// are rejected. The special `auto` tag is accepted here and resolved by a
/// [`LanguageRouter`].
pub fn validate_language_tag(tag: &str) -> Result<(), OcrError> {
    if tag == "auto" {
        return Ok(());
    }
    if tag.is_empty() || tag.len() > 40 || !tag.is_ascii() {
        return Err(OcrError::InvalidLanguageTag(tag.to_string()));
    }
    for subtag in tag.split('-') {
        if subtag.is_empty()
            || subtag.len() > 8
            || !subtag.bytes().all(|b| b.is_ascii_alphanumeric())
        {
            return Err(OcrError::InvalidLanguageTag(tag.to_string()));
        }
    }
    Ok(())
}

/// A writing system recognized by the deterministic script heuristic.
///
/// Only the scripts listed here are counted; alphabetic characters from other
/// writing systems are ignored by [`detect_script`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Script {
    /// Latin script (ASCII and Latin extended).
    Latin,
    /// Cyrillic script.
    Cyrillic,
    /// Greek script.
    Greek,
    /// Arabic script.
    Arabic,
    /// Hebrew script.
    Hebrew,
    /// Han ideographs (Chinese).
    Han,
    /// Japanese hiragana.
    Hiragana,
    /// Japanese katakana.
    Katakana,
    /// Korean hangul.
    Hangul,
    /// Devanagari script (Hindi and related languages).
    Devanagari,
    /// Thai script.
    Thai,
}

impl Script {
    /// Return the representative Tesseract-style language tag for this script.
    ///
    /// The mapping is fixed and documented so routing is reproducible: Latin
    /// to `eng`, Cyrillic to `rus`, Greek to `ell`, Arabic to `ara`, Hebrew to
    /// `heb`, Han to `chi_sim`, hiragana/katakana to `jpn`, hangul to `kor`,
    /// Devanagari to `hin`, and Thai to `tha`.
    pub const fn language_hint(self) -> &'static str {
        match self {
            Self::Latin => "eng",
            Self::Cyrillic => "rus",
            Self::Greek => "ell",
            Self::Arabic => "ara",
            Self::Hebrew => "heb",
            Self::Han => "chi_sim",
            Self::Hiragana | Self::Katakana => "jpn",
            Self::Hangul => "kor",
            Self::Devanagari => "hin",
            Self::Thai => "tha",
        }
    }
}

/// Classify one alphabetic character into a recognized [`Script`].
fn script_of(c: char) -> Option<Script> {
    let u = c as u32;
    if u <= 0x024F || (0x1E00..=0x1EFF).contains(&u) {
        return Some(Script::Latin);
    }
    if (0x0370..=0x03FF).contains(&u) || (0x1F00..=0x1FFF).contains(&u) {
        return Some(Script::Greek);
    }
    if (0x0400..=0x052F).contains(&u) {
        return Some(Script::Cyrillic);
    }
    if (0x0590..=0x05FF).contains(&u) {
        return Some(Script::Hebrew);
    }
    if (0x0600..=0x06FF).contains(&u) || (0x0750..=0x077F).contains(&u) {
        return Some(Script::Arabic);
    }
    if (0x0900..=0x097F).contains(&u) {
        return Some(Script::Devanagari);
    }
    if (0x0E00..=0x0E7F).contains(&u) {
        return Some(Script::Thai);
    }
    if (0x3040..=0x309F).contains(&u) {
        return Some(Script::Hiragana);
    }
    if (0x30A0..=0x30FF).contains(&u) || (0x31F0..=0x31FF).contains(&u) {
        return Some(Script::Katakana);
    }
    if (0xAC00..=0xD7AF).contains(&u) || (0x1100..=0x11FF).contains(&u) {
        return Some(Script::Hangul);
    }
    if (0x3400..=0x4DBF).contains(&u) || (0x4E00..=0x9FFF).contains(&u) {
        return Some(Script::Han);
    }
    None
}

/// Detect the dominant writing system of `text` by counting alphabetic
/// characters per script.
///
/// Whitespace, digits, punctuation, and characters from unrecognized scripts
/// are ignored. The script with the most letters wins; ties are broken by the
/// declaration order of [`Script`] (Latin first), which makes the result fully
/// deterministic. Returns `None` when `text` contains no recognized letters.
pub fn detect_script(text: &str) -> Option<Script> {
    const ORDER: [Script; 11] = [
        Script::Latin,
        Script::Cyrillic,
        Script::Greek,
        Script::Arabic,
        Script::Hebrew,
        Script::Han,
        Script::Hiragana,
        Script::Katakana,
        Script::Hangul,
        Script::Devanagari,
        Script::Thai,
    ];
    let mut counts = [0usize; ORDER.len()];
    for c in text.chars() {
        if !c.is_alphabetic() {
            continue;
        }
        if let Some(script) = script_of(c)
            && let Some(index) = ORDER.iter().position(|candidate| *candidate == script)
        {
            counts[index] += 1;
        }
    }
    let mut best: Option<(usize, usize)> = None;
    for (index, count) in counts.into_iter().enumerate() {
        if count > 0 && best.is_none_or(|(_, best_count)| count > best_count) {
            best = Some((index, count));
        }
    }
    best.map(|(index, _)| ORDER[index])
}

/// Detect a representative language tag from `text` using [`detect_script`].
///
/// Returns `None` when no recognized script is present. The tag is a *hint*
/// (see [`Script::language_hint`]); a [`LanguageRouter`] still checks it against
/// the engine's supported set.
pub fn detect_language(text: &str) -> Option<String> {
    detect_script(text).map(|script| script.language_hint().to_string())
}

// ---------------------------------------------------------------------------
// Language routing
// ---------------------------------------------------------------------------

/// Why a requested language resolved to a particular tag.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FallbackReason {
    /// The requested tag was directly supported by the engine.
    Exact,
    /// `auto` resolved to a supported language via script detection.
    AutoDetected,
    /// `auto` found no recognizable script, so the engine default was used.
    AutoUndetected,
    /// `auto` detected a script whose language the engine does not support.
    AutoUnsupported,
    /// The requested tag was unsupported and lenient fallback applied.
    UnsupportedRequested,
}

/// How a [`LanguageRouter`] treats an unsupported requested language.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FallbackPolicy {
    /// Return [`OcrError::UnsupportedLanguage`] on the first unsupported tag.
    Strict,
    /// Resolve unsupported tags to the engine's first supported language and
    /// record a [`FallbackReason::UnsupportedRequested`].
    Lenient,
}

/// One requested language and the tag it resolved to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LanguageRoute {
    /// The requested tag, normalized to lowercase (except the literal `auto`).
    pub requested: String,
    /// The tag that will actually be passed to the engine.
    pub resolved: String,
    /// The documented reason for this resolution.
    pub reason: FallbackReason,
}

/// Maps requested languages onto an engine's supported set.
///
/// The engine's first supported language is the deterministic default used by
/// every fallback. A router with an empty supported set is valid to construct
/// but every `route` call returns [`OcrError::NoSupportedLanguages`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LanguageRouter {
    supported: Vec<String>,
}

impl LanguageRouter {
    /// Create a router from an engine's advertised language list.
    ///
    /// Tags are validated and lowercased; duplicates are removed while keeping
    /// first-seen order so the default stays deterministic.
    pub fn new(supported: impl IntoIterator<Item = impl Into<String>>) -> Result<Self, OcrError> {
        let mut tags: Vec<String> = Vec::new();
        for item in supported {
            let tag = item.into();
            validate_language_tag(&tag)?;
            let normalized = if tag == "auto" {
                tag
            } else {
                tag.to_ascii_lowercase()
            };
            if !tags.contains(&normalized) {
                tags.push(normalized);
            }
        }
        Ok(Self { supported: tags })
    }

    /// Build a router directly from an engine's `languages()` slice.
    pub fn from_engine(engine: &dyn OcrEngine) -> Result<Self, OcrError> {
        Self::new(engine.languages().iter().cloned())
    }

    /// Return the supported tags in stable order.
    pub fn supported(&self) -> &[String] {
        &self.supported
    }

    /// Return the deterministic default language, if one exists.
    pub fn default_language(&self) -> Option<&str> {
        self.supported.first().map(String::as_str)
    }

    /// Return `true` when `tag` is directly supported (case-insensitive).
    pub fn supports(&self, tag: &str) -> bool {
        self.supported
            .iter()
            .any(|candidate| candidate == &tag.to_ascii_lowercase())
    }

    /// Route each requested tag, returning one [`LanguageRoute`] per request in
    /// the same order.
    ///
    /// `auto` is resolved against `extracted_text` with [`detect_language`].
    /// Malformed tags always error. Unsupported tags either error ([`FallbackPolicy::Strict`])
    /// or fall back to [`Self::default_language`] with a recorded reason
    /// ([`FallbackPolicy::Lenient`]).
    pub fn route(
        &self,
        requested: &[String],
        extracted_text: &str,
        policy: FallbackPolicy,
    ) -> Result<Vec<LanguageRoute>, OcrError> {
        let default = self
            .default_language()
            .ok_or(OcrError::NoSupportedLanguages)?
            .to_string();
        let mut routes = Vec::with_capacity(requested.len());
        for tag in requested {
            validate_language_tag(tag)?;
            if tag == "auto" {
                let detected = detect_language(extracted_text);
                match detected {
                    Some(hint) if self.supports(&hint) => routes.push(LanguageRoute {
                        requested: tag.clone(),
                        resolved: hint,
                        reason: FallbackReason::AutoDetected,
                    }),
                    Some(_) => routes.push(LanguageRoute {
                        requested: tag.clone(),
                        resolved: default.clone(),
                        reason: FallbackReason::AutoUnsupported,
                    }),
                    None => routes.push(LanguageRoute {
                        requested: tag.clone(),
                        resolved: default.clone(),
                        reason: FallbackReason::AutoUndetected,
                    }),
                }
                continue;
            }
            let normalized = tag.to_ascii_lowercase();
            if self.supports(&normalized) {
                routes.push(LanguageRoute {
                    requested: normalized.clone(),
                    resolved: normalized,
                    reason: FallbackReason::Exact,
                });
            } else if policy == FallbackPolicy::Strict {
                return Err(OcrError::UnsupportedLanguage(tag.clone()));
            } else {
                routes.push(LanguageRoute {
                    requested: normalized,
                    resolved: default.clone(),
                    reason: FallbackReason::UnsupportedRequested,
                });
            }
        }
        Ok(routes)
    }

    /// Extract the unique resolved tags from a set of routes, in first-seen
    /// order, ready to hand to an engine.
    pub fn resolved_languages(routes: &[LanguageRoute]) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for route in routes {
            if !out.contains(&route.resolved) {
                out.push(route.resolved.clone());
            }
        }
        out
    }
}

// ---------------------------------------------------------------------------
// Confidence policy and suspect report
// ---------------------------------------------------------------------------

/// Threshold-based filtering and suspect-character policy.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ConfidencePolicy {
    /// Spans below this confidence are removed by [`filter_by_confidence`].
    pub min_confidence: f32,
    /// Spans below this confidence appear in a [`SuspectReport`].
    pub suspect_below: f32,
    /// Whether unusual characters cause a span to appear in a suspect report.
    pub flag_unusual_characters: bool,
}

impl Default for ConfidencePolicy {
    fn default() -> Self {
        Self {
            min_confidence: DEFAULT_CONFIDENCE_THRESHOLD,
            suspect_below: DEFAULT_SUSPECT_THRESHOLD,
            flag_unusual_characters: true,
        }
    }
}

impl ConfidencePolicy {
    /// Create a policy with explicit thresholds.
    pub const fn new(min_confidence: f32, suspect_below: f32) -> Self {
        Self {
            min_confidence,
            suspect_below,
            flag_unusual_characters: true,
        }
    }

    /// Validate that both thresholds lie in `0..=100`.
    pub fn validate(&self) -> Result<(), OcrError> {
        for (name, value) in [
            ("min_confidence", self.min_confidence),
            ("suspect_below", self.suspect_below),
        ] {
            if !value.is_finite() || !(0.0..=100.0).contains(&value) {
                return Err(OcrError::InvalidConfidencePolicy(format!(
                    "{name} {value} is outside 0..=100"
                )));
            }
        }
        Ok(())
    }
}

/// Return `true` when `c` is a character worth flagging for human review.
///
/// Flagged characters are the replacement glyph `U+FFFD`, control characters
/// other than tab/newline/carriage return, combining marks (`U+0300..U+036F`),
/// zero-width and bidirectional marks (`U+200B..U+200F`, `U+202A..U+202E`,
/// `U+2060..U+206F`, `U+FEFF`), and private-use code points
/// (`U+E000..U+F8FF`). Normal whitespace and printable text are not flagged.
pub fn is_unusual_character(c: char) -> bool {
    if c == '\u{FFFD}' {
        return true;
    }
    if c.is_control() {
        return !matches!(c, '\t' | '\n' | '\r');
    }
    matches!(
        c as u32,
        0x0300..=0x036F
            | 0x200B..=0x200F
            | 0x202A..=0x202E
            | 0x2060..=0x206F
            | 0xFEFF
            | 0xE000..=0xF8FF
    )
}

/// Return a copy of `page` retaining only spans at or above the policy's
/// [`ConfidencePolicy::min_confidence`].
///
/// Span order and page metadata are preserved because [`OcrPage::new`] applies
/// the same deterministic ordering.
pub fn filter_by_confidence(
    page: &OcrPage,
    policy: &ConfidencePolicy,
) -> Result<OcrPage, OcrError> {
    policy.validate()?;
    let spans = page
        .spans()
        .iter()
        .filter(|span| span.confidence >= policy.min_confidence)
        .cloned()
        .collect();
    Ok(OcrPage::new(page.page(), spans, page.detected_language()))
}

/// One span that a reviewer should inspect.
#[derive(Clone, Debug, PartialEq)]
pub struct SuspectSpan {
    /// Index of the span within the source [`OcrPage`].
    pub span_index: usize,
    /// The recognized text of the suspect span.
    pub text: String,
    /// The span's confidence.
    pub confidence: f32,
    /// Distinct unusual characters found in the span, in first-seen order.
    pub unusual_characters: Vec<char>,
    /// Whether the span falls below the policy's filter threshold.
    pub below_filter_threshold: bool,
}

/// A deterministic list of spans flagged for review.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SuspectReport {
    spans: Vec<SuspectSpan>,
}

impl SuspectReport {
    /// Return the flagged spans ordered by ascending source span index.
    pub fn spans(&self) -> &[SuspectSpan] {
        &self.spans
    }

    /// Return the number of flagged spans.
    pub fn len(&self) -> usize {
        self.spans.len()
    }

    /// Return `true` when no spans were flagged.
    pub fn is_empty(&self) -> bool {
        self.spans.is_empty()
    }
}

/// Build a [`SuspectReport`] from `page` and `policy`.
///
/// A span is flagged when its confidence is below
/// [`ConfidencePolicy::suspect_below`] or, when
/// [`ConfidencePolicy::flag_unusual_characters`] is set, when it contains any
/// character for which [`is_unusual_character`] is true. Spans already removed
/// by [`filter_by_confidence`] are still reported, with
/// [`SuspectSpan::below_filter_threshold`] set.
pub fn suspect_report(
    page: &OcrPage,
    policy: &ConfidencePolicy,
) -> Result<SuspectReport, OcrError> {
    policy.validate()?;
    let mut spans = Vec::new();
    for (index, span) in page.spans().iter().enumerate() {
        let mut unusual = Vec::new();
        if policy.flag_unusual_characters {
            for c in span.text.chars() {
                if is_unusual_character(c) && !unusual.contains(&c) {
                    unusual.push(c);
                }
            }
        }
        let low_confidence = span.confidence < policy.suspect_below;
        if low_confidence || !unusual.is_empty() {
            spans.push(SuspectSpan {
                span_index: index,
                text: span.text.clone(),
                confidence: span.confidence,
                unusual_characters: unusual,
                below_filter_threshold: span.confidence < policy.min_confidence,
            });
        }
    }
    Ok(SuspectReport { spans })
}

// ---------------------------------------------------------------------------
// Hex helpers
// ---------------------------------------------------------------------------

/// Encode bytes as lowercase hexadecimal.
fn hex_encode(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(DIGITS[(byte >> 4) as usize] as char);
        out.push(DIGITS[(byte & 0x0F) as usize] as char);
    }
    out
}

/// Decode a lowercase-or-uppercase hexadecimal string into bytes.
fn hex_decode(text: &str) -> Result<Vec<u8>, OcrError> {
    if !text.len().is_multiple_of(2) {
        return Err(OcrError::InvalidSidecar(
            "hex pixel string has odd length".to_string(),
        ));
    }
    fn value(b: u8) -> Option<u8> {
        match b {
            b'0'..=b'9' => Some(b - b'0'),
            b'a'..=b'f' => Some(b - b'a' + 10),
            b'A'..=b'F' => Some(b - b'A' + 10),
            _ => None,
        }
    }
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len() / 2);
    for pair in bytes.as_chunks::<2>().0 {
        let high = value(pair[0]).ok_or_else(|| {
            OcrError::InvalidSidecar("hex pixel string contains a non-hex digit".to_string())
        })?;
        let low = value(pair[1]).ok_or_else(|| {
            OcrError::InvalidSidecar("hex pixel string contains a non-hex digit".to_string())
        })?;
        out.push((high << 4) | low);
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Minimal JSON codec (hand written, no dependencies)
// ---------------------------------------------------------------------------

mod json {
    use std::collections::BTreeMap;

    /// A parsed JSON value. Objects use a `BTreeMap` so serialization order is
    /// deterministic and duplicate keys can be detected on parse.
    #[derive(Clone, Debug, PartialEq)]
    pub(super) enum Value {
        Null,
        Bool(bool),
        Number(f64),
        String(String),
        Array(Vec<Value>),
        Object(BTreeMap<String, Value>),
    }

    /// A small recursive-descent parser for the JSON subset emitted by this
    /// module. It rejects trailing data, duplicate keys, bad escapes, invalid
    /// numbers, and unescaped control characters.
    pub(super) struct Parser {
        chars: Vec<char>,
        pos: usize,
    }

    impl Parser {
        pub(super) fn new(input: &str) -> Self {
            Self {
                chars: input.chars().collect(),
                pos: 0,
            }
        }

        fn peek(&self) -> Option<char> {
            self.chars.get(self.pos).copied()
        }

        fn bump(&mut self) -> Option<char> {
            let c = self.peek()?;
            self.pos += 1;
            Some(c)
        }

        fn skip_ws(&mut self) {
            while matches!(self.peek(), Some(' ' | '\t' | '\n' | '\r')) {
                self.pos += 1;
            }
        }

        fn expect(&mut self, expected: char) -> Result<(), String> {
            if self.bump() == Some(expected) {
                Ok(())
            } else {
                Err(format!("expected '{expected}'"))
            }
        }

        pub(super) fn parse(mut self) -> Result<Value, String> {
            self.skip_ws();
            let value = self.parse_value()?;
            self.skip_ws();
            if self.peek().is_some() {
                return Err("trailing characters after JSON value".to_string());
            }
            Ok(value)
        }

        fn parse_value(&mut self) -> Result<Value, String> {
            self.skip_ws();
            match self.peek() {
                Some('{') => self.parse_object(),
                Some('[') => self.parse_array(),
                Some('"') => self.parse_string().map(Value::String),
                Some('t') => self.parse_literal("true", Value::Bool(true)),
                Some('f') => self.parse_literal("false", Value::Bool(false)),
                Some('n') => self.parse_literal("null", Value::Null),
                Some('-' | '0'..='9') => self.parse_number(),
                other => Err(format!("unexpected character {other:?} in JSON")),
            }
        }

        fn parse_literal(&mut self, word: &str, value: Value) -> Result<Value, String> {
            for expected in word.chars() {
                if self.bump() != Some(expected) {
                    return Err(format!("invalid literal, expected '{word}'"));
                }
            }
            Ok(value)
        }

        fn parse_number(&mut self) -> Result<Value, String> {
            let start = self.pos;
            if self.peek() == Some('-') {
                self.pos += 1;
            }
            match self.peek() {
                Some('0') => self.pos += 1,
                Some('1'..='9') => {
                    while matches!(self.peek(), Some('0'..='9')) {
                        self.pos += 1;
                    }
                }
                other => return Err(format!("invalid number, found {other:?}")),
            }
            if self.peek() == Some('.') {
                self.pos += 1;
                if !matches!(self.peek(), Some('0'..='9')) {
                    return Err("number has no digits after decimal point".to_string());
                }
                while matches!(self.peek(), Some('0'..='9')) {
                    self.pos += 1;
                }
            }
            if matches!(self.peek(), Some('e' | 'E')) {
                self.pos += 1;
                if matches!(self.peek(), Some('+' | '-')) {
                    self.pos += 1;
                }
                if !matches!(self.peek(), Some('0'..='9')) {
                    return Err("number has no digits in exponent".to_string());
                }
                while matches!(self.peek(), Some('0'..='9')) {
                    self.pos += 1;
                }
            }
            let text: String = self.chars[start..self.pos].iter().collect();
            let number = text
                .parse::<f64>()
                .map_err(|_| format!("invalid JSON number '{text}'"))?;
            if !number.is_finite() {
                return Err(format!("number '{text}' is not finite"));
            }
            Ok(Value::Number(number))
        }

        /// Parse exactly four hexadecimal digits following a `\u` escape.
        fn parse_hex4(&mut self) -> Result<u32, String> {
            let mut code = 0u32;
            for _ in 0..4 {
                let hex = self.bump().ok_or("short unicode escape")?;
                let digit = hex.to_digit(16).ok_or("invalid unicode escape")?;
                code = (code << 4) | digit;
            }
            Ok(code)
        }

        fn parse_string(&mut self) -> Result<String, String> {
            self.expect('"')?;
            let mut out = String::new();
            loop {
                let c = self.bump().ok_or("unterminated JSON string")?;
                match c {
                    '"' => return Ok(out),
                    '\\' => {
                        let escape = self.bump().ok_or("unterminated escape sequence")?;
                        match escape {
                            '"' => out.push('"'),
                            '\\' => out.push('\\'),
                            '/' => out.push('/'),
                            'b' => out.push('\u{0008}'),
                            'f' => out.push('\u{000C}'),
                            'n' => out.push('\n'),
                            'r' => out.push('\r'),
                            't' => out.push('\t'),
                            'u' => {
                                let first = self.parse_hex4()?;
                                if (0xD800..=0xDBFF).contains(&first) {
                                    // A high surrogate must be immediately followed by a
                                    // `\uXXXX` low surrogate; combine the pair into one
                                    // Unicode scalar value as JSON requires.
                                    if self.bump() != Some('\\') || self.bump() != Some('u') {
                                        return Err(
                                            "high surrogate must be followed by a \\u escape"
                                                .to_string(),
                                        );
                                    }
                                    let second = self.parse_hex4()?;
                                    if !(0xDC00..=0xDFFF).contains(&second) {
                                        return Err("invalid low surrogate in \\u pair".to_string());
                                    }
                                    let scalar =
                                        0x1_0000 + ((first - 0xD800) << 10) + (second - 0xDC00);
                                    let ch = char::from_u32(scalar)
                                        .ok_or("surrogate pair is not a scalar value")?;
                                    out.push(ch);
                                } else if (0xDC00..=0xDFFF).contains(&first) {
                                    return Err("lone low surrogate \\u escape".to_string());
                                } else {
                                    let ch = char::from_u32(first)
                                        .ok_or("unicode escape is not a scalar value")?;
                                    out.push(ch);
                                }
                            }
                            other => return Err(format!("invalid escape '\\{other}'")),
                        }
                    }
                    c if c < '\u{20}' => {
                        return Err("unescaped control character in string".to_string());
                    }
                    c => out.push(c),
                }
            }
        }

        fn parse_array(&mut self) -> Result<Value, String> {
            self.expect('[')?;
            let mut values = Vec::new();
            self.skip_ws();
            if self.peek() == Some(']') {
                self.pos += 1;
                return Ok(Value::Array(values));
            }
            loop {
                values.push(self.parse_value()?);
                self.skip_ws();
                match self.bump() {
                    Some(',') => continue,
                    Some(']') => break,
                    other => return Err(format!("expected ',' or ']' but found {other:?}")),
                }
            }
            Ok(Value::Array(values))
        }

        fn parse_object(&mut self) -> Result<Value, String> {
            self.expect('{')?;
            let mut map = BTreeMap::new();
            self.skip_ws();
            if self.peek() == Some('}') {
                self.pos += 1;
                return Ok(Value::Object(map));
            }
            loop {
                self.skip_ws();
                if self.peek() != Some('"') {
                    return Err("object key must be a string".to_string());
                }
                let key = self.parse_string()?;
                self.skip_ws();
                self.expect(':')?;
                let value = self.parse_value()?;
                if map.insert(key.clone(), value).is_some() {
                    return Err(format!("duplicate object key '{key}'"));
                }
                self.skip_ws();
                match self.bump() {
                    Some(',') => continue,
                    Some('}') => break,
                    other => return Err(format!("expected ',' or '}}' but found {other:?}")),
                }
            }
            Ok(Value::Object(map))
        }
    }

    /// Escape a string for inclusion in JSON, including surrogate-free
    /// `\uXXXX` encoding for non-printable code points.
    pub(super) fn escape(input: &str) -> String {
        let mut out = String::with_capacity(input.len() + 2);
        out.push('"');
        for c in input.chars() {
            match c {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                '\u{0008}' => out.push_str("\\b"),
                '\u{000C}' => out.push_str("\\f"),
                c if c < '\u{20}' => {
                    out.push_str(&format!("\\u{:04x}", c as u32));
                }
                c => out.push(c),
            }
        }
        out.push('"');
        out
    }
}

/// Parse JSON into the internal value tree used by the sidecar decoders.
fn parse_json(input: &str) -> Result<json::Value, OcrError> {
    json::Parser::new(input)
        .parse()
        .map_err(OcrError::SidecarJson)
}

/// Extract a field from a JSON object, rejecting unknown keys via `allowed`.
fn fields(object: &BTreeMap<String, json::Value>, allowed: &[&str]) -> Result<(), OcrError> {
    for key in object.keys() {
        if !allowed.contains(&key.as_str()) {
            return Err(OcrError::SidecarJson(format!("unknown field '{key}'")));
        }
    }
    Ok(())
}

fn get<'a>(
    object: &'a BTreeMap<String, json::Value>,
    key: &str,
) -> Result<&'a json::Value, OcrError> {
    object
        .get(key)
        .ok_or_else(|| OcrError::SidecarJson(format!("missing field '{key}'")))
}

fn as_u32(value: &json::Value, key: &str) -> Result<u32, OcrError> {
    match value {
        json::Value::Number(n) if n.fract() == 0.0 && *n >= 0.0 && *n <= u32::MAX as f64 => {
            Ok(*n as u32)
        }
        _ => Err(OcrError::SidecarJson(format!(
            "field '{key}' must be a non-negative integer within u32"
        ))),
    }
}

fn as_f32(value: &json::Value, key: &str) -> Result<f32, OcrError> {
    match value {
        json::Value::Number(n)
            if n.is_finite() && *n >= f32::MIN as f64 && *n <= f32::MAX as f64 =>
        {
            Ok(*n as f32)
        }
        _ => Err(OcrError::SidecarJson(format!(
            "field '{key}' must be a finite f32"
        ))),
    }
}

fn as_str<'a>(value: &'a json::Value, key: &str) -> Result<&'a str, OcrError> {
    match value {
        json::Value::String(s) => Ok(s),
        _ => Err(OcrError::SidecarJson(format!(
            "field '{key}' must be a string"
        ))),
    }
}

fn as_array<'a>(value: &'a json::Value, key: &str) -> Result<&'a [json::Value], OcrError> {
    match value {
        json::Value::Array(items) => Ok(items),
        _ => Err(OcrError::SidecarJson(format!(
            "field '{key}' must be an array"
        ))),
    }
}

fn as_object<'a>(
    value: &'a json::Value,
    key: &str,
) -> Result<&'a BTreeMap<String, json::Value>, OcrError> {
    match value {
        json::Value::Object(map) => Ok(map),
        _ => Err(OcrError::SidecarJson(format!(
            "field '{key}' must be an object"
        ))),
    }
}

fn format_f32(value: f32) -> String {
    // All structs validate finiteness before serialization.
    format!("{value}")
}

// ---------------------------------------------------------------------------
// Sidecar contract
// ---------------------------------------------------------------------------

/// One recognized span in a [`SidecarResponse`], as plain JSON data.
#[derive(Clone, Debug, PartialEq)]
pub struct SidecarSpan {
    /// Recognized text.
    pub text: String,
    /// Left edge of the bounding box.
    pub x: f32,
    /// Top edge of the bounding box.
    pub y: f32,
    /// Bounding-box width.
    pub width: f32,
    /// Bounding-box height.
    pub height: f32,
    /// Recognition confidence on a `0..=100` scale.
    pub confidence: f32,
    /// Language tag used for this span.
    pub language: String,
}

/// A request sent to a future out-of-process OCR sidecar.
///
/// Pixel bytes are represented as a lowercase hexadecimal string (`pixels_hex`)
/// so the wire format needs no binary framing or base64 dependency. The codec
/// is documented as JSON with these fields:
///
/// ```text
/// {
///   "version": 1,
///   "width": 40,
///   "height": 10,
///   "dpi": 300.0,
///   "format": "gray",       // "gray" or "rgba"
///   "pixels_hex": "00ff...",// width*height*bytes_per_pixel*2 hex digits
///   "languages": ["eng"],
///   "confidence_threshold": 60.0
/// }
/// ```
#[derive(Clone, Debug, PartialEq)]
pub struct SidecarRequest {
    /// Wire-format version; must equal [`SUPPORTED_SIDECAR_VERSION`].
    pub version: u32,
    /// Image width in pixels.
    pub width: u32,
    /// Image height in pixels.
    pub height: u32,
    /// Image resolution in DPI.
    pub dpi: f32,
    /// Sidecar pixel format token (`"gray"` or `"rgba"`).
    pub format: String,
    /// Hex-encoded pixel bytes.
    pub pixels_hex: String,
    /// Requested language tags.
    pub languages: Vec<String>,
    /// Minimum confidence the backend should report, on a `0..=100` scale.
    pub confidence_threshold: f32,
}

impl SidecarRequest {
    /// Build a request from an [`OcrImage`] and routing options.
    pub fn from_image(
        image: &OcrImage,
        languages: Vec<String>,
        confidence_threshold: f32,
    ) -> Result<Self, OcrError> {
        let request = Self {
            version: SUPPORTED_SIDECAR_VERSION,
            width: image.width(),
            height: image.height(),
            dpi: image.dpi(),
            format: image.format().sidecar_token().to_string(),
            pixels_hex: hex_encode(image.pixels()),
            languages,
            confidence_threshold,
        };
        request.validate()?;
        Ok(request)
    }

    /// Validate version, sizes, DPI, format token, hex pixel length, language
    /// tags, and confidence threshold.
    pub fn validate(&self) -> Result<(), OcrError> {
        if self.version != SUPPORTED_SIDECAR_VERSION {
            return Err(OcrError::InvalidSidecar(format!(
                "unsupported sidecar version {}",
                self.version
            )));
        }
        let format = OcrPixelFormat::from_sidecar_token(&self.format).ok_or_else(|| {
            OcrError::InvalidSidecar(format!("unknown pixel format '{}'", self.format))
        })?;
        let expected_bytes = expected_byte_len(self.width, self.height, format)?;
        if self.pixels_hex.len() != expected_bytes * 2 {
            return Err(OcrError::InvalidSidecar(format!(
                "expected {} hex characters, got {}",
                expected_bytes * 2,
                self.pixels_hex.len()
            )));
        }
        let decoded = hex_decode(&self.pixels_hex)?;
        if decoded.len() != expected_bytes {
            return Err(OcrError::InvalidSidecar(
                "decoded pixel length mismatch".to_string(),
            ));
        }
        if !self.dpi.is_finite() || !(MIN_IMAGE_DPI..=MAX_IMAGE_DPI).contains(&self.dpi) {
            return Err(OcrError::InvalidDpi(self.dpi, MIN_IMAGE_DPI, MAX_IMAGE_DPI));
        }
        for tag in &self.languages {
            validate_language_tag(tag)?;
        }
        if !self.confidence_threshold.is_finite()
            || !(0.0..=100.0).contains(&self.confidence_threshold)
        {
            return Err(OcrError::InvalidConfidencePolicy(format!(
                "confidence threshold {} is outside 0..=100",
                self.confidence_threshold
            )));
        }
        Ok(())
    }

    /// Serialize this request to deterministic JSON.
    pub fn to_json(&self) -> Result<String, OcrError> {
        self.validate()?;
        let mut out = String::from("{");
        out.push_str(&format!("\"version\":{},", self.version));
        out.push_str(&format!("\"width\":{},", self.width));
        out.push_str(&format!("\"height\":{},", self.height));
        out.push_str(&format!("\"dpi\":{},", format_f32(self.dpi)));
        out.push_str("\"format\":");
        out.push_str(&json::escape(&self.format));
        out.push_str(",\"pixels_hex\":");
        out.push_str(&json::escape(&self.pixels_hex));
        out.push_str(",\"languages\":[");
        for (index, tag) in self.languages.iter().enumerate() {
            if index > 0 {
                out.push(',');
            }
            out.push_str(&json::escape(tag));
        }
        out.push_str("],\"confidence_threshold\":");
        out.push_str(&format_f32(self.confidence_threshold));
        out.push('}');
        Ok(out)
    }

    /// Parse and validate a request from JSON.
    pub fn from_json(input: &str) -> Result<Self, OcrError> {
        let value = parse_json(input)?;
        let object = as_object(&value, "request")?;
        fields(
            object,
            &[
                "version",
                "width",
                "height",
                "dpi",
                "format",
                "pixels_hex",
                "languages",
                "confidence_threshold",
            ],
        )?;
        let languages = as_array(get(object, "languages")?, "languages")?
            .iter()
            .map(|item| as_str(item, "languages").map(str::to_string))
            .collect::<Result<Vec<_>, _>>()?;
        let request = Self {
            version: as_u32(get(object, "version")?, "version")?,
            width: as_u32(get(object, "width")?, "width")?,
            height: as_u32(get(object, "height")?, "height")?,
            dpi: as_f32(get(object, "dpi")?, "dpi")?,
            format: as_str(get(object, "format")?, "format")?.to_string(),
            pixels_hex: as_str(get(object, "pixels_hex")?, "pixels_hex")?.to_string(),
            languages,
            confidence_threshold: as_f32(
                get(object, "confidence_threshold")?,
                "confidence_threshold",
            )?,
        };
        request.validate()?;
        Ok(request)
    }
}

/// A response returned by a future out-of-process OCR sidecar.
///
/// JSON shape:
///
/// ```text
/// {
///   "version": 1,
///   "detected_language": "eng",
///   "spans": [
///     {"text":"Hi","x":0.0,"y":0.0,"width":8.0,"height":10.0,
///      "confidence":92.5,"language":"eng"}
///   ]
/// }
/// ```
#[derive(Clone, Debug, PartialEq)]
pub struct SidecarResponse {
    /// Wire-format version; must equal [`SUPPORTED_SIDECAR_VERSION`].
    pub version: u32,
    /// Page-level detected language tag.
    pub detected_language: String,
    /// Recognized spans.
    pub spans: Vec<SidecarSpan>,
}

impl SidecarResponse {
    /// Create a response, validating all language tags and confidences.
    pub fn new(
        detected_language: impl Into<String>,
        spans: Vec<SidecarSpan>,
    ) -> Result<Self, OcrError> {
        let response = Self {
            version: SUPPORTED_SIDECAR_VERSION,
            detected_language: detected_language.into(),
            spans,
        };
        response.validate()?;
        Ok(response)
    }

    /// Validate version, language tags, and finite values/bounding boxes.
    pub fn validate(&self) -> Result<(), OcrError> {
        if self.version != SUPPORTED_SIDECAR_VERSION {
            return Err(OcrError::InvalidSidecar(format!(
                "unsupported sidecar version {}",
                self.version
            )));
        }
        validate_language_tag(&self.detected_language)?;
        for span in &self.spans {
            validate_language_tag(&span.language)?;
            for (name, value) in [
                ("x", span.x),
                ("y", span.y),
                ("width", span.width),
                ("height", span.height),
                ("confidence", span.confidence),
            ] {
                if !value.is_finite() {
                    return Err(OcrError::InvalidSidecar(format!(
                        "span {name} is not finite"
                    )));
                }
            }
            if span.width < 0.0 || span.height < 0.0 {
                return Err(OcrError::InvalidSidecar(
                    "span bounding box has negative extent".to_string(),
                ));
            }
            if !(0.0..=100.0).contains(&span.confidence) {
                return Err(OcrError::InvalidSidecar(format!(
                    "span confidence {} is outside 0..=100",
                    span.confidence
                )));
            }
        }
        Ok(())
    }

    /// Convert this response into an [`OcrPage`] for `page`, applying the
    /// deterministic span ordering of [`OcrPage::new`].
    pub fn to_ocr_page(&self, page: usize) -> Result<OcrPage, OcrError> {
        self.validate()?;
        let spans = self
            .spans
            .iter()
            .map(|span| {
                OcrSpan::new(
                    span.text.clone(),
                    OcrBox::new(span.x, span.y, span.width, span.height),
                    span.confidence,
                    span.language.clone(),
                )
            })
            .collect();
        Ok(OcrPage::new(page, spans, self.detected_language.clone()))
    }

    /// Serialize this response to deterministic JSON.
    pub fn to_json(&self) -> Result<String, OcrError> {
        self.validate()?;
        let mut out = String::from("{");
        out.push_str(&format!("\"version\":{},", self.version));
        out.push_str("\"detected_language\":");
        out.push_str(&json::escape(&self.detected_language));
        out.push_str(",\"spans\":[");
        for (index, span) in self.spans.iter().enumerate() {
            if index > 0 {
                out.push(',');
            }
            out.push_str("{\"text\":");
            out.push_str(&json::escape(&span.text));
            out.push_str(&format!(
                ",\"x\":{},\"y\":{},\"width\":{},\"height\":{},\"confidence\":{},\"language\":",
                format_f32(span.x),
                format_f32(span.y),
                format_f32(span.width),
                format_f32(span.height),
                format_f32(span.confidence),
            ));
            out.push_str(&json::escape(&span.language));
            out.push('}');
        }
        out.push_str("]}");
        Ok(out)
    }

    /// Parse and validate a response from JSON.
    pub fn from_json(input: &str) -> Result<Self, OcrError> {
        let value = parse_json(input)?;
        let object = as_object(&value, "response")?;
        fields(object, &["version", "detected_language", "spans"])?;
        let span_values = as_array(get(object, "spans")?, "spans")?;
        let mut spans = Vec::with_capacity(span_values.len());
        for item in span_values {
            let span = as_object(item, "span")?;
            fields(
                span,
                &[
                    "text",
                    "x",
                    "y",
                    "width",
                    "height",
                    "confidence",
                    "language",
                ],
            )?;
            spans.push(SidecarSpan {
                text: as_str(get(span, "text")?, "text")?.to_string(),
                x: as_f32(get(span, "x")?, "x")?,
                y: as_f32(get(span, "y")?, "y")?,
                width: as_f32(get(span, "width")?, "width")?,
                height: as_f32(get(span, "height")?, "height")?,
                confidence: as_f32(get(span, "confidence")?, "confidence")?,
                language: as_str(get(span, "language")?, "language")?.to_string(),
            });
        }
        let response = Self {
            version: as_u32(get(object, "version")?, "version")?,
            detected_language: as_str(get(object, "detected_language")?, "detected_language")?
                .to_string(),
            spans,
        };
        response.validate()?;
        Ok(response)
    }
}

/// Parse a sidecar response JSON and return only its top-level `version`.
///
/// This uses the same strict JSON parser as [`SidecarResponse::from_json`], so a
/// version can be reported as a distinct, typed mismatch before the rest of the
/// payload is validated. A missing or wrongly typed `version` is reported as
/// [`OcrError::SidecarJson`], which the process bridge classifies as a malformed
/// response. The full document is still validated by [`SidecarResponse::from_json`]
/// after this check.
pub fn sidecar_response_version(input: &str) -> Result<u32, OcrError> {
    let value = parse_json(input)?;
    let object = as_object(&value, "response")?;
    as_u32(get(object, "version")?, "version")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CancellationToken, Progress};

    /// A deterministic in-memory OCR engine used to exercise the trait.
    struct MockOcrEngine {
        name: String,
        languages: Vec<String>,
    }

    impl MockOcrEngine {
        fn new(languages: &[&str]) -> Self {
            Self {
                name: "mock-tesseract".to_string(),
                languages: languages.iter().map(|tag| (*tag).to_string()).collect(),
            }
        }
    }

    impl OcrEngine for MockOcrEngine {
        fn name(&self) -> &str {
            &self.name
        }

        fn languages(&self) -> &[String] {
            &self.languages
        }

        fn recognize(&self, image: OcrImage, options: &OcrOptions) -> Result<OcrPage, OcrError> {
            options.validate()?;
            let language = options
                .languages
                .first()
                .cloned()
                .unwrap_or_else(|| "eng".to_string());
            let span = OcrSpan::new(
                format!(
                    "{}x{}@{}",
                    image.width(),
                    image.height(),
                    image.dpi() as u32
                ),
                OcrBox::new(0.0, 0.0, 8.0, 10.0),
                95.0,
                language.clone(),
            );
            Ok(OcrPage::new(0, vec![span], language))
        }
    }

    fn tiny_image() -> OcrImage {
        OcrImage::new(
            2,
            2,
            300.0,
            OcrPixelFormat::Grayscale,
            vec![0, 128, 200, 255],
        )
        .expect("valid image")
    }

    #[test]
    fn mock_engine_recognizes_image_and_reports_metadata() {
        let engine = MockOcrEngine::new(&["eng", "fra"]);
        assert_eq!(engine.name(), "mock-tesseract");
        assert_eq!(engine.languages(), &["eng".to_string(), "fra".to_string()]);

        let options = OcrOptions::new(vec!["eng".to_string()]);
        let page = engine
            .recognize(tiny_image(), &options)
            .expect("recognition succeeds");
        assert_eq!(page.page(), 0);
        assert_eq!(page.detected_language(), "eng");
        assert_eq!(page.span_count(), 1);
        assert_eq!(page.text(), "2x2@300");
        assert_eq!(page.spans()[0].bbox, OcrBox::new(0.0, 0.0, 8.0, 10.0));
        assert_eq!(page.spans()[0].confidence, 95.0);
        assert_eq!(page.spans()[0].language, "eng");
    }

    #[test]
    fn ocr_image_validates_dimensions_and_pixel_count() {
        assert!(OcrImage::new(0, 2, 300.0, OcrPixelFormat::Grayscale, vec![]).is_err());
        assert!(OcrImage::new(2, 2, 300.0, OcrPixelFormat::Grayscale, vec![0, 1, 2]).is_err());
        assert!(OcrImage::new(2, 2, 300.0, OcrPixelFormat::Rgba, vec![0; 16]).is_ok());
        assert!(
            OcrImage::new(
                MAX_IMAGE_DIMENSION + 1,
                1,
                300.0,
                OcrPixelFormat::Grayscale,
                vec![]
            )
            .is_err()
        );
        let image = tiny_image();
        assert_eq!((image.width(), image.height()), (2, 2));
        assert_eq!(image.format().bytes_per_pixel(), 1);
        assert_eq!(image.pixels(), &[0, 128, 200, 255]);
    }

    #[test]
    fn ocr_image_rejects_out_of_range_dpi() {
        let pixels = vec![0u8; 4];
        assert_eq!(
            OcrImage::new(
                2,
                2,
                MIN_IMAGE_DPI - 0.5,
                OcrPixelFormat::Grayscale,
                pixels.clone()
            ),
            Err(OcrError::InvalidDpi(
                MIN_IMAGE_DPI - 0.5,
                MIN_IMAGE_DPI,
                MAX_IMAGE_DPI
            ))
        );
        assert!(
            OcrImage::new(
                2,
                2,
                MAX_IMAGE_DPI + 1.0,
                OcrPixelFormat::Grayscale,
                pixels.clone()
            )
            .is_err()
        );
        assert!(OcrImage::new(2, 2, f32::NAN, OcrPixelFormat::Grayscale, pixels).is_err());
    }

    #[test]
    fn validate_language_tags_accepts_expected_forms() {
        for tag in ["eng", "auto", "en-US", "zh-Hans", "pt-BR", "a"] {
            assert_eq!(validate_language_tag(tag), Ok(()), "tag {tag}");
        }
        for tag in [
            "",
            "-eng",
            "eng-",
            "en--US",
            "has space",
            "é",
            "a".repeat(41).as_str(),
        ] {
            assert!(
                matches!(
                    validate_language_tag(tag),
                    Err(OcrError::InvalidLanguageTag(_))
                ),
                "tag {tag}"
            );
        }
    }

    #[test]
    fn detect_script_uses_dominant_letters_with_deterministic_ties() {
        assert_eq!(detect_script("hello world"), Some(Script::Latin));
        assert_eq!(detect_script("Привет мир"), Some(Script::Cyrillic));
        assert_eq!(detect_script("γειά σου"), Some(Script::Greek));
        assert_eq!(detect_script("你好世界"), Some(Script::Han));
        assert_eq!(detect_script("こんにちは"), Some(Script::Hiragana));
        assert_eq!(detect_script("안녕하세요"), Some(Script::Hangul));
        assert_eq!(detect_script("नमस्ते"), Some(Script::Devanagari));
        assert_eq!(detect_script("สวัสดี"), Some(Script::Thai));
        assert_eq!(detect_script("123 !!! ---"), None);
        // Tie between Latin and Cyrillic resolves to the declaration order.
        assert_eq!(detect_script("abc абв"), Some(Script::Latin));
        assert_eq!(detect_language("Привет"), Some("rus".to_string()));
    }

    #[test]
    fn router_routes_exact_fallback_and_auto() {
        let engine = MockOcrEngine::new(&["eng", "fra", "rus"]);
        let router = LanguageRouter::from_engine(&engine).expect("valid router");
        assert_eq!(
            router.supported(),
            &["eng".to_string(), "fra".to_string(), "rus".to_string()]
        );
        assert_eq!(router.default_language(), Some("eng"));
        assert!(router.supports("ENG"));

        let requested = vec!["FRA".to_string(), "deu".to_string(), "auto".to_string()];
        let routes = router
            .route(&requested, "Привет мир", FallbackPolicy::Lenient)
            .expect("routing succeeds");
        assert_eq!(
            routes,
            vec![
                LanguageRoute {
                    requested: "fra".to_string(),
                    resolved: "fra".to_string(),
                    reason: FallbackReason::Exact,
                },
                LanguageRoute {
                    requested: "deu".to_string(),
                    resolved: "eng".to_string(),
                    reason: FallbackReason::UnsupportedRequested,
                },
                LanguageRoute {
                    requested: "auto".to_string(),
                    resolved: "rus".to_string(),
                    reason: FallbackReason::AutoDetected,
                },
            ]
        );
        assert_eq!(
            LanguageRouter::resolved_languages(&routes),
            vec!["fra".to_string(), "eng".to_string(), "rus".to_string()]
        );
    }

    #[test]
    fn router_auto_undetected_and_unsupported_have_documented_reasons() {
        let router = LanguageRouter::new(["eng", "fra"]).expect("valid router");
        let routes = router
            .route(&["auto".to_string()], "123 ---", FallbackPolicy::Lenient)
            .expect("routing succeeds");
        assert_eq!(routes[0].reason, FallbackReason::AutoUndetected);
        assert_eq!(routes[0].resolved, "eng");

        let routes = router
            .route(&["auto".to_string()], "你好世界", FallbackPolicy::Lenient)
            .expect("routing succeeds");
        assert_eq!(routes[0].reason, FallbackReason::AutoUnsupported);
        assert_eq!(routes[0].resolved, "eng");
    }

    #[test]
    fn strict_router_rejects_unsupported_language() {
        let router = LanguageRouter::new(["eng"]).expect("valid router");
        let result = router.route(&["deu".to_string()], "", FallbackPolicy::Strict);
        assert_eq!(
            result,
            Err(OcrError::UnsupportedLanguage("deu".to_string()))
        );

        let empty = LanguageRouter::new(Vec::<String>::new()).expect("empty is constructible");
        assert_eq!(
            empty.route(&[], "", FallbackPolicy::Lenient),
            Err(OcrError::NoSupportedLanguages)
        );
    }

    #[test]
    fn sidecar_request_json_round_trip() {
        let request = SidecarRequest::from_image(
            &tiny_image(),
            vec!["eng".to_string(), "fra".to_string()],
            55.0,
        )
        .expect("valid request");
        let json = request.to_json().expect("serializes");
        assert!(json.contains("\"format\":\"gray\""));
        assert!(json.contains("\"pixels_hex\":\"0080c8ff\""));
        let parsed = SidecarRequest::from_json(&json).expect("parses");
        assert_eq!(parsed, request);
        // Serialization is deterministic and does not depend on object key order.
        assert_eq!(parsed.to_json().expect("reserializes"), json);
    }

    #[test]
    fn sidecar_response_json_round_trip_and_conversion() {
        let response = SidecarResponse::new(
            "eng",
            vec![
                SidecarSpan {
                    text: "Hello".to_string(),
                    x: 1.0,
                    y: 2.0,
                    width: 9.0,
                    height: 11.0,
                    confidence: 92.5,
                    language: "eng".to_string(),
                },
                SidecarSpan {
                    text: " quoting \" and \\ and \n".to_string(),
                    x: 0.0,
                    y: 0.0,
                    width: 4.0,
                    height: 4.0,
                    confidence: 70.0,
                    language: "eng".to_string(),
                },
            ],
        )
        .expect("valid response");
        let json = response.to_json().expect("serializes");
        let parsed = SidecarResponse::from_json(&json).expect("parses");
        assert_eq!(parsed, response);
        let page = parsed.to_ocr_page(3).expect("converts");
        assert_eq!(page.page(), 3);
        assert_eq!(page.detected_language(), "eng");
        assert_eq!(page.span_count(), 2);
    }

    #[test]
    fn sidecar_json_accepts_surrogate_pairs_and_rejects_lone_surrogates() {
        let response = SidecarResponse::new(
            "eng",
            vec![SidecarSpan {
                text: "emoji \u{1F600} end".to_string(),
                x: 0.0,
                y: 0.0,
                width: 4.0,
                height: 4.0,
                confidence: 90.0,
                language: "eng".to_string(),
            }],
        )
        .expect("valid response");

        // The canonical serializer emits the scalar directly as UTF-8; it must
        // round-trip through the strict codec unchanged.
        let canonical = response.to_json().expect("serializes");
        assert_eq!(
            SidecarResponse::from_json(&canonical).expect("round-trips"),
            response
        );

        // Standard JSON escaped surrogate pairs are accepted and decoded.
        let escaped = canonical.replace('\u{1F600}', "\\uD83D\\uDE00");
        assert!(escaped.contains("\\uD83D\\uDE00"));
        assert_eq!(
            SidecarResponse::from_json(&escaped).expect("surrogate pair decodes"),
            response
        );

        // Lowercase hex digits in a pair are accepted too.
        let lowercase = canonical.replace('\u{1F600}', "\\ud83d\\ude00");
        assert_eq!(
            SidecarResponse::from_json(&lowercase).expect("lowercase pair decodes"),
            response
        );

        // Lone surrogates and malformed pairs are rejected.
        for bad in [
            "\\uD83D",               // lone high surrogate
            "\\uDE00",               // lone low surrogate
            "\\uD83D\\u0041",        // high surrogate followed by a non-surrogate
            "\\uD83Dx",              // high surrogate not followed by an escape
            "\\uD83D\\uDE0",         // truncated second escape
            "\\uD800\\uDC00\\uD800", // valid pair plus a trailing lone surrogate
        ] {
            let input = canonical.replace('\u{1F600}', bad);
            assert!(
                SidecarResponse::from_json(&input).is_err(),
                "should reject malformed surrogate sequence: {bad}"
            );
        }
    }

    #[test]
    fn sidecar_json_rejects_malformed_inputs() {
        let request = SidecarRequest::from_image(&tiny_image(), vec!["eng".to_string()], 60.0)
            .expect("valid request");
        let good = request.to_json().expect("serializes");

        let malformed = [
            "",                                                                   // empty
            "{",                                                                  // unterminated
            "{\"version\":1,}",                                                   // trailing comma
            &good[..good.len() - 1],                                              // missing '}'
            &good.replace("\"version\":1", "\"version\":1,\"version\":1"),        // duplicate key
            &good.replace("\"dpi\":300", "\"dpi\":\"high\""),                     // wrong type
            &good.replace("\"eng\"", "\"en g\""), // invalid language tag
            &good.replace("\"pixels_hex\":\"0080c8ff\"", "\"pixels_hex\":\"0\""), // bad pixel len
            &good.replace("\"version\":1", "\"version\":2"), // wrong version
        ];
        for input in malformed {
            assert!(
                SidecarRequest::from_json(input).is_err(),
                "should reject malformed input: {input}"
            );
        }

        let response = SidecarResponse::new(
            "eng",
            vec![SidecarSpan {
                text: "x".to_string(),
                x: 0.0,
                y: 0.0,
                width: 1.0,
                height: 1.0,
                confidence: 90.0,
                language: "eng".to_string(),
            }],
        )
        .expect("valid response");
        let good = response.to_json().expect("serializes");
        assert!(
            SidecarResponse::from_json(&good.replace("\"confidence\":90", "\"confidence\":190"))
                .is_err()
        );
        assert!(SidecarResponse::from_json(&good.replace("eng", "en g")).is_err());
        assert!(SidecarResponse::from_json(&good.replace("\"spans\"", "\"extra\"")).is_err());
    }

    #[test]
    fn sidecar_validates_sizes_dpi_and_language_tags() {
        let mut request = SidecarRequest::from_image(&tiny_image(), vec!["eng".to_string()], 60.0)
            .expect("valid request");
        request.dpi = MAX_IMAGE_DPI + 1.0;
        assert!(matches!(request.validate(), Err(OcrError::InvalidDpi(..))));
        request.dpi = 300.0;
        request.languages = vec!["bad tag".to_string()];
        assert!(matches!(
            request.validate(),
            Err(OcrError::InvalidLanguageTag(_))
        ));
        request.languages = vec!["eng".to_string()];
        request.width = 0;
        assert!(matches!(request.validate(), Err(OcrError::InvalidImage(_))));

        assert!(SidecarResponse::new("eng", vec![]).is_ok());
        assert!(SidecarResponse::new("bad tag", vec![]).is_err());
    }

    #[test]
    fn ocr_page_sorts_spans_deterministically() {
        let a = OcrSpan::new("b", OcrBox::new(5.0, 0.0, 1.0, 1.0), 90.0, "eng");
        let b = OcrSpan::new("a", OcrBox::new(1.0, 0.0, 1.0, 1.0), 90.0, "eng");
        let c = OcrSpan::new("top", OcrBox::new(0.0, -1.0, 1.0, 1.0), 90.0, "eng");
        let page1 = OcrPage::new(0, vec![a.clone(), b.clone(), c.clone()], "eng");
        let page2 = OcrPage::new(0, vec![c, a, b], "eng");
        assert_eq!(page1, page2);
        assert_eq!(
            page1
                .spans()
                .iter()
                .map(|s| s.text.as_str())
                .collect::<Vec<_>>(),
            vec!["top", "a", "b"]
        );
    }

    #[test]
    fn confidence_policy_filters_low_confidence_spans() {
        let page = OcrPage::new(
            0,
            vec![
                OcrSpan::new("keep", OcrBox::new(0.0, 0.0, 4.0, 4.0), 91.0, "eng"),
                OcrSpan::new("drop", OcrBox::new(0.0, 1.0, 4.0, 4.0), 30.0, "eng"),
                OcrSpan::new("edge", OcrBox::new(0.0, 2.0, 4.0, 4.0), 60.0, "eng"),
            ],
            "eng",
        );
        let filtered = filter_by_confidence(&page, &ConfidencePolicy::default()).expect("filters");
        assert_eq!(filtered.span_count(), 2);
        assert_eq!(filtered.text(), "keepedge");
        assert_eq!(filtered.detected_language(), "eng");

        let strict = ConfidencePolicy::new(100.0, 100.0);
        assert!(
            filter_by_confidence(&page, &strict)
                .expect("filters")
                .is_empty()
        );
    }

    #[test]
    fn suspect_report_flags_low_confidence_and_unusual_characters() {
        let page = OcrPage::new(
            0,
            vec![
                OcrSpan::new("clean", OcrBox::new(0.0, 0.0, 5.0, 5.0), 95.0, "eng"),
                OcrSpan::new(
                    "low\u{200B}conf",
                    OcrBox::new(0.0, 1.0, 5.0, 5.0),
                    50.0,
                    "eng",
                ),
                OcrSpan::new(
                    "repl\u{FFFD}ce",
                    OcrBox::new(0.0, 2.0, 5.0, 5.0),
                    95.0,
                    "eng",
                ),
            ],
            "eng",
        );
        let report = suspect_report(&page, &ConfidencePolicy::default()).expect("reports");
        assert_eq!(report.len(), 2);
        assert_eq!(report.spans()[0].span_index, 1);
        assert!(report.spans()[0].below_filter_threshold);
        assert_eq!(report.spans()[0].unusual_characters, vec!['\u{200B}']);
        assert_eq!(report.spans()[1].span_index, 2);
        assert!(!report.spans()[1].below_filter_threshold);
        assert_eq!(report.spans()[1].unusual_characters, vec!['\u{FFFD}']);

        let no_flags = ConfidencePolicy {
            min_confidence: 60.0,
            suspect_below: 80.0,
            flag_unusual_characters: false,
        };
        let report = suspect_report(&page, &no_flags).expect("reports");
        assert_eq!(report.len(), 1, "only the low-confidence span remains");
        assert!(is_unusual_character('\u{0000}'));
        assert!(!is_unusual_character('a'));
        assert!(!is_unusual_character('\n'));
    }

    #[test]
    fn confidence_policy_validates_thresholds() {
        assert!(ConfidencePolicy::default().validate().is_ok());
        assert!(ConfidencePolicy::new(-1.0, 80.0).validate().is_err());
        assert!(ConfidencePolicy::new(60.0, 101.0).validate().is_err());
        assert!(ConfidencePolicy::new(f32::NAN, 80.0).validate().is_err());
        assert!(
            OcrOptions::default()
                .with_confidence_threshold(200.0)
                .validate()
                .is_err()
        );
    }

    #[test]
    fn ocr_operations_leave_cancellation_and_progress_untouched() {
        let token = CancellationToken::new();
        let reports = vec![Progress::new(0, 1)];

        let engine = MockOcrEngine::new(&["eng"]);
        let router = LanguageRouter::from_engine(&engine).expect("valid router");
        let routes = router
            .route(&["auto".to_string()], "hello", FallbackPolicy::Lenient)
            .expect("routing");
        let image = tiny_image();
        let request =
            SidecarRequest::from_image(&image, LanguageRouter::resolved_languages(&routes), 60.0)
                .expect("request");
        let _ = request.to_json().expect("serializes");
        let _ = engine
            .recognize(image, &OcrOptions::default())
            .expect("recognizes");

        assert!(!token.is_cancelled(), "OCR helpers must not cancel a token");
        assert_eq!(reports, vec![Progress::new(0, 1)], "progress untouched");
    }
}
