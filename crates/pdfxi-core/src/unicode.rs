//! Unicode text model for CJK and right-to-left scripts.
//!
//! This module is a dependency-free, deterministic foundation for handling
//! mixed-script text. It provides script classification ([`Script`]), strong
//! direction detection ([`Direction`]), bidirectional run detection
//! ([`bidi_runs`]), base-direction resolution ([`resolve_base_direction`]) and
//! a deterministic [`ShapingPlan`] and a [`FallbackPlan`] that assigns an
//! ordered font-fallback chain to each script run from caller-supplied
//! candidates. It performs **no glyph shaping and no font I/O**: a plan is an
//! ordered list of same-script runs with byte ranges, scripts, directions and
//! candidate font chains so a renderer or extractor can apply a real shaper and
//! font selection later. A glyph-shaper backend (for example HarfBuzz or
//! DirectWrite) remains a host/render boundary.
//!
//! # Grapheme safety and scope
//!
//! Rust `char` values are Unicode scalar values. Valid UTF-8 cannot encode a
//! surrogate code point, so scalar-level validation is inherently surrogate
//! safe. Full extended grapheme-cluster segmentation would need a large table
//! (and an external crate), which this crate deliberately avoids. Instead the
//! caller may supply grapheme-cluster boundaries as byte offsets via
//! [`validate_grapheme_boundaries`]; this function guarantees each boundary is
//! a UTF-8 character boundary inside the text (a necessary condition for a
//! grapheme boundary) and that boundaries strictly increase. The text model
//! then treats each scalar as an atomic unit: classification and run grouping
//! never split a multi-byte character. Combining marks, ZWJ sequences and
//! locale-specific collation remain the caller's responsibility when providing
//! boundaries.
//!
//! # Direction resolution
//!
//! [`resolve_base_direction`] implements a documented *first-strong* heuristic:
//! the first character with an inherent RTL script (Hebrew or Arabic) yields
//! [`Direction::Rtl`], the first with an inherently LTR script (Latin or CJK)
//! yields [`Direction::Ltr`], and text with no strong character defaults to
//! [`Direction::Ltr`]. This is a deterministic subset of the Unicode
//! Bidirectional Algorithm (UAX #9), not a full embedding-level resolution;
//! it is sufficient to derive a stable, reproducible visual ordering plan for
//! CJK/RTL runs without adding a dependency.

use thiserror::Error;

/// A coarse Unicode script family used for text classification.
///
/// Every scalar maps to exactly one family. `Other` covers scripts and symbols
/// outside the documented ranges (for example Greek, Cyrillic, digits,
/// punctuation and emoji).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Script {
    /// Latin script, including ASCII, Latin-1 letters and Latin Extended.
    Latin,
    /// CJK ideographs, kana, bopomofo, radicals and Hangul syllables.
    Cjk,
    /// Hebrew script (including Hebrew presentation forms).
    Hebrew,
    /// Arabic script (including presentation forms and extended ranges).
    Arabic,
    /// Any script or symbol not covered by the other families.
    Other,
}

impl Script {
    /// Return a stable, lowercase name for this script family.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Latin => "latin",
            Self::Cjk => "cjk",
            Self::Hebrew => "hebrew",
            Self::Arabic => "arabic",
            Self::Other => "other",
        }
    }
}

/// The resolved writing direction of text or of one run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Left-to-right (Latin, CJK and neutral text).
    Ltr,
    /// Right-to-left (Hebrew and Arabic).
    Rtl,
}

impl Direction {
    /// Return the inherent direction of a script family, when it has one.
    ///
    /// Latin and CJK are [`Direction::Ltr`], Hebrew and Arabic are
    /// [`Direction::Rtl`], and [`Script::Other`] is neutral (`None`).
    pub const fn for_script(script: Script) -> Option<Self> {
        match script {
            Script::Latin | Script::Cjk => Some(Self::Ltr),
            Script::Hebrew | Script::Arabic => Some(Self::Rtl),
            Script::Other => None,
        }
    }

    /// Return `true` for a right-to-left direction.
    pub const fn is_rtl(self) -> bool {
        matches!(self, Self::Rtl)
    }

    /// Return the stable lowercase name `ltr` or `rtl`.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ltr => "ltr",
            Self::Rtl => "rtl",
        }
    }
}

/// Maximum accepted byte length of a caller-supplied font-family name.
pub const MAX_FONT_FAMILY_BYTES: usize = 128;
/// Maximum accepted number of font candidates in one fallback plan.
pub const MAX_FONT_CANDIDATES: usize = 64;

/// A caller-supplied font candidate with the scripts it can render.
///
/// The fallback planner never opens a font file or talks to a font engine. It
/// only uses this declaration to choose an ordered chain for each script run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FontCandidate {
    family: String,
    scripts: Vec<Script>,
}

impl FontCandidate {
    /// Create a font candidate from a family name and the scripts it supports.
    ///
    /// Duplicate scripts are removed in first-mention order so the declaration
    /// is deterministic.
    ///
    /// # Errors
    ///
    /// Returns [`TextError::EmptyFontFamily`] for an empty name,
    /// [`TextError::InvalidFontFamily`] for an oversized or control-containing
    /// name, and [`TextError::EmptyCandidates`] when no script is listed.
    pub fn new(family: &str, scripts: Vec<Script>) -> Result<Self, TextError> {
        if family.is_empty() {
            return Err(TextError::EmptyFontFamily);
        }
        if family.len() > MAX_FONT_FAMILY_BYTES {
            return Err(TextError::InvalidFontFamily(family.to_owned()));
        }
        if family.chars().any(|c| c.is_control()) {
            return Err(TextError::InvalidFontFamily(family.to_owned()));
        }
        if scripts.is_empty() {
            return Err(TextError::EmptyCandidates);
        }
        let mut unique = Vec::with_capacity(scripts.len());
        for script in scripts {
            if !unique.contains(&script) {
                unique.push(script);
            }
        }
        Ok(Self {
            family: family.to_owned(),
            scripts: unique,
        })
    }

    /// Return the declared font-family name.
    pub fn family(&self) -> &str {
        &self.family
    }

    /// Return the declared scripts in first-mention order.
    pub fn scripts(&self) -> &[Script] {
        &self.scripts
    }

    /// Return whether this candidate declares support for `script`.
    pub fn supports(&self, script: Script) -> bool {
        self.scripts.contains(&script)
    }
}

/// One script run assigned an ordered fallback font chain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FallbackStep {
    start: usize,
    end: usize,
    script: Script,
    direction: Direction,
    chain: Vec<String>,
}

impl FallbackStep {
    /// Byte offset of the first character of the run.
    pub const fn start(&self) -> usize {
        self.start
    }

    /// Byte offset just past the last character of the run.
    pub const fn end(&self) -> usize {
        self.end
    }

    /// The script family shared by the run.
    pub const fn script(&self) -> Script {
        self.script
    }

    /// The direction assigned to the run.
    pub const fn direction(&self) -> Direction {
        self.direction
    }

    /// The ordered fallback font-family names, most-preferred first.
    pub fn font_chain(&self) -> &[String] {
        &self.chain
    }

    /// The first font family in the chain (the one a host should try first).
    pub fn primary_font(&self) -> &str {
        self.chain.first().map_or("", |family| family.as_str())
    }
}

/// A deterministic shaping/fallback plan built from bidi runs and candidates.
///
/// A plan is pure data: it records, for every input run, the ordered list of
/// caller-supplied font families that declare support for its script. A real
/// glyph shaper (for example HarfBuzz or DirectWrite) and font selection remain
/// a host/render boundary; this crate performs no rasterization or font I/O.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FallbackPlan {
    steps: Vec<FallbackStep>,
    summary: String,
}

impl FallbackPlan {
    /// Return the ordered fallback steps, one per input run.
    pub fn steps(&self) -> &[FallbackStep] {
        &self.steps
    }

    /// Return a byte-for-byte deterministic summary of the plan.
    ///
    /// The form is `runs=<n>; chain=<script>:<font>><font>|...`; scripts and
    /// fonts appear in run and candidate order respectively.
    pub fn summary(&self) -> &str {
        &self.summary
    }
}

impl std::fmt::Display for FallbackPlan {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.summary)
    }
}

/// Build a deterministic [`FallbackPlan`] from bidi runs and font candidates.
///
/// Runs are consumed in the order supplied (logical order from [`bidi_runs`], or
/// visual order from [`ShapingPlan::runs`]). For each run, every candidate that
/// declares support for the run's script contributes its family name to the
/// chain in candidate order. The first chain entry is the preferred font.
///
/// # Errors
///
/// Returns [`TextError::EmptyInput`] for no runs, [`TextError::EmptyCandidates`]
/// or [`TextError::TooManyCandidates`] for an invalid candidate set, and
/// [`TextError::NoFontForScript`] when no candidate supports a run's script.
pub fn build_fallback_plan(
    runs: &[BidiRun],
    candidates: &[FontCandidate],
) -> Result<FallbackPlan, TextError> {
    if runs.is_empty() {
        return Err(TextError::EmptyInput);
    }
    if candidates.is_empty() {
        return Err(TextError::EmptyCandidates);
    }
    if candidates.len() > MAX_FONT_CANDIDATES {
        return Err(TextError::TooManyCandidates(
            candidates.len(),
            MAX_FONT_CANDIDATES,
        ));
    }
    let mut steps = Vec::with_capacity(runs.len());
    for run in runs {
        let mut chain = Vec::new();
        for candidate in candidates {
            if candidate.supports(run.script()) {
                chain.push(candidate.family().to_owned());
            }
        }
        if chain.is_empty() {
            return Err(TextError::NoFontForScript(run.script()));
        }
        steps.push(FallbackStep {
            start: run.start(),
            end: run.end(),
            script: run.script(),
            direction: run.direction(),
            chain,
        });
    }
    let parts: Vec<String> = steps
        .iter()
        .map(|step| format!("{}:{}", step.script.as_str(), step.chain.join(">")))
        .collect();
    let summary = format!("runs={}; chain={}", steps.len(), parts.join("|"));
    Ok(FallbackPlan { steps, summary })
}

/// Convenience entry point that derives logical-order bidi runs from `text`.
///
/// This is equivalent to calling [`bidi_runs`] and passing the result to
/// [`build_fallback_plan`]. The text is never shaped or rasterized here.
///
/// # Errors
///
/// Returns [`TextError::EmptyInput`] for empty text and any error from
/// [`build_fallback_plan`].
pub fn fallback_plan_from_text(
    text: &str,
    candidates: &[FontCandidate],
) -> Result<FallbackPlan, TextError> {
    if text.is_empty() {
        return Err(TextError::EmptyInput);
    }
    build_fallback_plan(&bidi_runs(text), candidates)
}

/// Typed errors produced by Unicode text validation.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum TextError {
    /// The text or shaping plan was empty.
    #[error("text input is empty")]
    EmptyInput,
    /// A caller-supplied grapheme boundary lies past the end of the text.
    #[error("grapheme boundary {offset} is outside the text length {len}")]
    BoundaryOutOfRange {
        /// The rejected byte offset.
        offset: usize,
        /// The length of the text in bytes.
        len: usize,
    },
    /// A caller-supplied grapheme boundary splits a multi-byte character.
    #[error("grapheme boundary {offset} is not a UTF-8 character boundary")]
    BoundaryNotOnChar {
        /// The rejected byte offset.
        offset: usize,
    },
    /// Caller-supplied grapheme boundaries were not strictly increasing.
    #[error("grapheme boundaries must strictly increase ({previous} >= {offset})")]
    BoundariesNotIncreasing {
        /// The preceding boundary.
        previous: usize,
        /// The boundary that failed to increase.
        offset: usize,
    },
    /// The supplied bytes were not valid UTF-8 (this also rejects surrogates).
    #[error("input is not valid UTF-8: {0}")]
    InvalidUtf8(#[from] std::str::Utf8Error),
    /// A font-family name was empty.
    #[error("font family name is empty")]
    EmptyFontFamily,
    /// A font-family name was oversized or contained a control character.
    #[error("invalid font family name: {0}")]
    InvalidFontFamily(String),
    /// No font candidate declared support for a required script.
    #[error("no font candidate supports script {0:?}")]
    NoFontForScript(Script),
    /// The font-candidate set was empty where at least one is required.
    #[error("font candidate set is empty")]
    EmptyCandidates,
    /// Too many font candidates were supplied to a fallback plan.
    #[error("font candidate count {0} exceeds the limit {1}")]
    TooManyCandidates(usize, usize),
}

/// Validate that `bytes` is well-formed UTF-8.
///
/// Well-formed UTF-8 excludes lone surrogates and overlong encodings. The
/// returned `&str` can therefore be processed safely at the scalar level.
///
/// # Errors
///
/// Returns [`TextError::InvalidUtf8`] when `bytes` is not valid UTF-8.
pub fn validate_utf8(bytes: &[u8]) -> Result<&str, TextError> {
    std::str::from_utf8(bytes).map_err(TextError::from)
}

/// Validate caller-supplied grapheme-cluster boundaries for `text`.
///
/// Boundaries are byte offsets into `text` and must be strictly increasing,
/// lie in `0..=text.len()`, and fall on a UTF-8 character boundary. The terminal
/// boundary equal to `text.len()` is allowed. Because this crate does not ship
/// a grapheme table, callers are responsible for choosing true cluster
/// boundaries; this function enforces the char-boundary safety that any valid
/// grapheme segmentation must satisfy.
///
/// # Errors
///
/// Returns a typed [`TextError`] for an out-of-range, non-increasing or
/// character-splitting boundary. An empty boundary slice is valid.
pub fn validate_grapheme_boundaries(text: &str, boundaries: &[usize]) -> Result<(), TextError> {
    let len = text.len();
    let mut previous: Option<usize> = None;
    for &offset in boundaries {
        if offset > len {
            return Err(TextError::BoundaryOutOfRange { offset, len });
        }
        if !text.is_char_boundary(offset) {
            return Err(TextError::BoundaryNotOnChar { offset });
        }
        if let Some(before) = previous
            && before >= offset
        {
            return Err(TextError::BoundariesNotIncreasing {
                previous: before,
                offset,
            });
        }
        previous = Some(offset);
    }
    Ok(())
}

/// Classify one Unicode scalar into a [`Script`] family.
///
/// Ranges follow the public Unicode code-point charts. Hebrew covers
/// `U+0590..=U+05FF` and `U+FB1D..=U+FB4F`; Arabic covers
/// `U+0600..=U+06FF`, `U+0750..=U+077F`, `U+08A0..=U+08FF` and
/// `U+FB50..=U+FDFF`/`U+FE70..=U+FEFF`; CJK covers kana, bopomofo, radicals,
/// the unified ideograph planes and Hangul. Latin covers ASCII and the Latin
/// Extended blocks. Everything else is [`Script::Other`].
pub fn classify_char(c: char) -> Script {
    let value = u32::from(c);
    if (0x0590..=0x05FF).contains(&value) || (0xFB1D..=0xFB4F).contains(&value) {
        Script::Hebrew
    } else if (0x0600..=0x06FF).contains(&value)
        || (0x0750..=0x077F).contains(&value)
        || (0x08A0..=0x08FF).contains(&value)
        || (0xFB50..=0xFDFF).contains(&value)
        || (0xFE70..=0xFEFF).contains(&value)
    {
        Script::Arabic
    } else if (0x3040..=0x30FF).contains(&value) // kana
        || (0x2E80..=0x2FFF).contains(&value) // radicals
        || (0x3100..=0x312F).contains(&value) // bopomofo
        || (0x3130..=0x318F).contains(&value) // hangul compatibility jamo
        || (0x3190..=0x319F).contains(&value) // CJK symbols and punctuation
        || (0x3400..=0x4DBF).contains(&value) // ideographs extension A
        || (0x4E00..=0x9FFF).contains(&value) // unified ideographs
        || (0xA960..=0xA97F).contains(&value) // hangul jamo extended A
        || (0xAC00..=0xD7AF).contains(&value) // hangul syllables
        || (0xD7B0..=0xD7FF).contains(&value) // hangul jamo extended B
        || (0xF900..=0xFAFF).contains(&value) // compatibility ideographs
        || (0x20000..=0x2FA1F).contains(&value) // ideographs extensions B..F
        || (0x30000..=0x323AF).contains(&value)
    // ideographs extensions G..H
    {
        Script::Cjk
    } else if c.is_ascii_alphabetic()
        || (0x00C0..=0x024F).contains(&value) // Latin-1 supplement + Extended A/B
        || (0x0250..=0x02AF).contains(&value) // IPA extensions (Latin-shaped)
        || (0x1E00..=0x1EFF).contains(&value)
    // Latin Extended Additional
    {
        Script::Latin
    } else {
        Script::Other
    }
}

/// Return the script with the most classified characters in `text`.
///
/// [`Script::Other`] characters count, but a non-`Other` script wins any tie.
/// Among equal non-`Other` counts the fixed priority Latin, CJK, Hebrew,
/// Arabic decides. Empty text yields [`Script::Other`].
pub fn dominant_script(text: &str) -> Script {
    let priority = [
        Script::Latin,
        Script::Cjk,
        Script::Hebrew,
        Script::Arabic,
        Script::Other,
    ];
    let mut best = Script::Other;
    let mut best_count = 0usize;
    for script in priority {
        let count = text.chars().filter(|&c| classify_char(c) == script).count();
        if count > best_count {
            best = script;
            best_count = count;
        }
    }
    best
}

/// Return the inherent direction of a scalar when it is a strong character.
///
/// Hebrew and Arabic scalars are [`Direction::Rtl`]; Latin and CJK scalars are
/// [`Direction::Ltr`]; neutral or unclassified scalars return `None`.
pub fn is_strong_direction(c: char) -> Option<Direction> {
    Direction::for_script(classify_char(c))
}

/// Resolve the paragraph base direction using the documented first-strong rule.
///
/// The first strong scalar decides; text with no strong scalar defaults to
/// [`Direction::Ltr`]. Empty text therefore resolves to LTR as well.
pub fn resolve_base_direction(text: &str) -> Direction {
    for c in text.chars() {
        if let Some(direction) = is_strong_direction(c) {
            return direction;
        }
    }
    Direction::Ltr
}

/// One maximal run of consecutive scalars with the same script and direction.
///
/// `start` and `end` are byte offsets into the source text (`end` exclusive),
/// so a run can be sliced without splitting a multi-byte scalar. `text` holds
/// the run's characters for convenient inspection in a shaping plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BidiRun {
    start: usize,
    end: usize,
    script: Script,
    direction: Direction,
    text: String,
}

impl BidiRun {
    /// Byte offset of the first character of this run.
    pub const fn start(&self) -> usize {
        self.start
    }

    /// Byte offset just past the last character of this run.
    pub const fn end(&self) -> usize {
        self.end
    }

    /// Script family shared by every character in the run.
    pub const fn script(&self) -> Script {
        self.script
    }

    /// Direction assigned to the run.
    pub const fn direction(&self) -> Direction {
        self.direction
    }

    /// The run's characters, in logical (input) order.
    pub fn text(&self) -> &str {
        &self.text
    }
}

/// Detect maximal same-script, same-direction runs in logical order.
///
/// Neutral characters ([`Script::Other`], which includes digits and spaces)
/// form their own runs and are assigned [`Direction::Ltr`] for stable output.
/// Byte ranges always start and end on character boundaries as a consequence
/// of iterating with [`str::char_indices`].
pub fn bidi_runs(text: &str) -> Vec<BidiRun> {
    let mut runs: Vec<BidiRun> = Vec::new();
    for (offset, c) in text.char_indices() {
        let script = classify_char(c);
        let direction = Direction::for_script(script).unwrap_or(Direction::Ltr);
        let end = offset + c.len_utf8();
        if let Some(last) = runs.last_mut()
            && last.script == script
            && last.direction == direction
        {
            last.end = end;
            last.text.push(c);
        } else {
            runs.push(BidiRun {
                start: offset,
                end,
                script,
                direction,
                text: c.to_string(),
            });
        }
    }
    runs
}

/// A deterministic, shaping-free plan describing how mixed text should be
/// ordered before a renderer selects glyphs and fonts.
///
/// `runs` are emitted in visual order: logical order for an LTR base direction
/// and run-reversed order for an RTL base direction. This is a paragraph-level
/// reversal of whole runs (not a full UAX #9 resolution of embedding levels);
/// it is stable and sufficient to expose mixed-direction ordering without any
/// shaping dependency.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShapingPlan {
    base_direction: Direction,
    runs: Vec<BidiRun>,
}

impl ShapingPlan {
    /// The resolved paragraph base direction.
    pub const fn base_direction(&self) -> Direction {
        self.base_direction
    }

    /// The runs in visual order.
    pub fn runs(&self) -> &[BidiRun] {
        &self.runs
    }
}

/// Build a deterministic [`ShapingPlan`] for `text`.
///
/// # Errors
///
/// Returns [`TextError::EmptyInput`] when `text` is empty.
pub fn shaping_plan(text: &str) -> Result<ShapingPlan, TextError> {
    if text.is_empty() {
        return Err(TextError::EmptyInput);
    }
    let base_direction = resolve_base_direction(text);
    let mut runs = bidi_runs(text);
    if base_direction == Direction::Rtl {
        runs.reverse();
    }
    Ok(ShapingPlan {
        base_direction,
        runs,
    })
}

/// Build a [`ShapingPlan`] after validating caller-supplied grapheme boundaries.
///
/// This is the grapheme-aware entry point: boundaries are checked with
/// [`validate_grapheme_boundaries`] before the plan is produced, so a caller
/// that segmented the text into clusters cannot accidentally feed a plan built
/// from a split scalar.
///
/// # Errors
///
/// Returns [`TextError::BoundaryOutOfRange`], [`TextError::BoundaryNotOnChar`],
/// [`TextError::BoundariesNotIncreasing`] or [`TextError::EmptyInput`] as
/// appropriate.
pub fn shaping_plan_with_boundaries(
    text: &str,
    boundaries: &[usize],
) -> Result<ShapingPlan, TextError> {
    validate_grapheme_boundaries(text, boundaries)?;
    shaping_plan(text)
}

/// Build a deterministic [`ShapingPlan`] with a caller-specified base direction.
///
/// This overrides the first-strong heuristic of [`shaping_plan`], which is
/// useful when a caller knows a paragraph's direction from surrounding context
/// (for example a document-level `/ReadingDirection`) while its first strong
/// character is ambiguous. As in [`shaping_plan`], an RTL base reverses the
/// whole run list at paragraph level; each run keeps its own direction.
///
/// # Errors
///
/// Returns [`TextError::EmptyInput`] when `text` is empty.
#[must_use = "the shaping plan describes the requested ordering"]
pub fn shaping_plan_with_base_direction(
    text: &str,
    base: Direction,
) -> Result<ShapingPlan, TextError> {
    if text.is_empty() {
        return Err(TextError::EmptyInput);
    }
    let mut runs = bidi_runs(text);
    if base.is_rtl() {
        runs.reverse();
    }
    Ok(ShapingPlan {
        base_direction: base,
        runs,
    })
}

/// A deterministic aggregate analysis of one text string.
///
/// Combines script classification, direction resolution, bidi-run detection and
/// caller-validated grapheme boundaries into one report with a stable
/// [`Self::summary`]. Runs are kept in *logical* (input) order so the report is
/// independent of base direction; use [`shaping_plan`] (or
/// [`shaping_plan_with_base_direction`]) for visual ordering. Byte ranges of
/// each run lie on UTF-8 character boundaries, matching
/// [`validate_grapheme_boundaries`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextAnalysis {
    text_len: usize,
    dominant: Script,
    base_direction: Direction,
    boundaries: Vec<usize>,
    runs: Vec<BidiRun>,
}

impl TextAnalysis {
    /// Length of the analysed text in bytes.
    #[must_use]
    pub const fn text_len(&self) -> usize {
        self.text_len
    }

    /// The dominant script family of the text.
    #[must_use]
    pub const fn dominant_script(&self) -> Script {
        self.dominant
    }

    /// The resolved paragraph base direction (first-strong heuristic).
    #[must_use]
    pub const fn base_direction(&self) -> Direction {
        self.base_direction
    }

    /// The validated caller-supplied grapheme boundaries, in order.
    #[must_use]
    pub fn boundaries(&self) -> &[usize] {
        &self.boundaries
    }

    /// Detected bidi runs in logical (input) order.
    #[must_use]
    pub fn runs(&self) -> &[BidiRun] {
        &self.runs
    }

    /// A stable, human-readable summary of the analysis.
    ///
    /// The form is `bytes=<n> dominant=<script> base=<dir> runs=<n>
    /// boundaries=<n>`. All fields are deterministic for a given input.
    #[must_use]
    pub fn summary(&self) -> String {
        format!(
            "bytes={} dominant={} base={} runs={} boundaries={}",
            self.text_len,
            self.dominant.as_str(),
            self.base_direction.as_str(),
            self.runs.len(),
            self.boundaries.len()
        )
    }
}

impl std::fmt::Display for TextAnalysis {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.summary())
    }
}

/// Analyze `text` into a deterministic [`TextAnalysis`] report.
///
/// Caller-supplied grapheme `boundaries` are checked with
/// [`validate_grapheme_boundaries`] before the report is built, so the returned
/// analysis never assumes a boundary that splits a multi-byte character.
///
/// # Errors
///
/// Returns [`TextError::EmptyInput`] for empty text and any boundary error from
/// [`validate_grapheme_boundaries`].
#[must_use = "the text analysis report carries the classification result"]
pub fn analyze_text(text: &str, boundaries: &[usize]) -> Result<TextAnalysis, TextError> {
    if text.is_empty() {
        return Err(TextError::EmptyInput);
    }
    validate_grapheme_boundaries(text, boundaries)?;
    Ok(TextAnalysis {
        text_len: text.len(),
        dominant: dominant_script(text),
        base_direction: resolve_base_direction(text),
        boundaries: boundaries.to_vec(),
        runs: bidi_runs(text),
    })
}

/// Validate raw bytes as UTF-8, then analyze them with [`analyze_text`].
///
/// This is the byte-oriented entry point: it exercises the same UTF-8
/// validation as [`validate_utf8`] (which also excludes encoded surrogates)
/// before producing the report.
///
/// # Errors
///
/// Returns [`TextError::InvalidUtf8`] for malformed bytes, then any error from
/// [`analyze_text`].
#[must_use = "the text analysis report carries the classification result"]
pub fn analyze_utf8(bytes: &[u8], boundaries: &[usize]) -> Result<TextAnalysis, TextError> {
    analyze_text(validate_utf8(bytes)?, boundaries)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cjk_detection() {
        // Ideographs, hiragana, katakana, Hangul syllables and an ideograph
        // from a supplementary plane all classify as CJK.
        for c in ['漢', '字', 'あ', 'ア', '한', '\u{20000}'] {
            assert_eq!(classify_char(c), Script::Cjk, "char {c:?}");
        }
        assert_eq!(dominant_script("你好世界"), Script::Cjk);
    }

    #[test]
    fn arabic_and_hebrew_bidi_runs() {
        let text = "abשלוםcdمرحبا";
        let runs = bidi_runs(text);
        let described: Vec<(Script, Direction, &str)> = runs
            .iter()
            .map(|run| (run.script(), run.direction(), run.text()))
            .collect();
        assert_eq!(
            described,
            vec![
                (Script::Latin, Direction::Ltr, "ab"),
                (Script::Hebrew, Direction::Rtl, "שלום"),
                (Script::Latin, Direction::Ltr, "cd"),
                (Script::Arabic, Direction::Rtl, "مرحبا"),
            ]
        );
        // Byte ranges are contiguous and cover the whole input exactly once.
        assert_eq!(runs.first().map(BidiRun::start), Some(0));
        assert_eq!(runs.last().map(BidiRun::end), Some(text.len()));
        for pair in runs.windows(2) {
            assert_eq!(pair[0].end(), pair[1].start());
        }
    }

    #[test]
    fn mixed_direction_ordering_plan() {
        // A Latin first-strong character gives an LTR plan in logical order.
        let ltr = shaping_plan("Hello שלום").expect("plan");
        assert_eq!(ltr.base_direction(), Direction::Ltr);
        let ltr_scripts: Vec<Script> = ltr.runs().iter().map(BidiRun::script).collect();
        assert_eq!(
            ltr_scripts,
            vec![Script::Latin, Script::Other, Script::Hebrew]
        );

        // A Hebrew first-strong character gives an RTL plan whose whole runs
        // are reversed (deterministic paragraph-level visual order): the Latin
        // run that came last in logical order is emitted first.
        let rtl = shaping_plan("שלום hello").expect("plan");
        assert_eq!(rtl.base_direction(), Direction::Rtl);
        let rtl_scripts: Vec<Script> = rtl.runs().iter().map(BidiRun::script).collect();
        assert_eq!(
            rtl_scripts,
            vec![Script::Latin, Script::Other, Script::Hebrew]
        );
        // Each run keeps its own direction even when the paragraph is RTL.
        assert_eq!(rtl.runs()[0].direction(), Direction::Ltr);
        assert_eq!(rtl.runs()[2].direction(), Direction::Rtl);
    }

    #[test]
    fn invalid_and_empty_input() {
        assert_eq!(shaping_plan(""), Err(TextError::EmptyInput));
        assert_eq!(resolve_base_direction(""), Direction::Ltr);
        assert_eq!(bidi_runs(""), Vec::new());
        assert_eq!(dominant_script(""), Script::Other);
        // Encoded UTF-16 surrogate U+D800 is invalid UTF-8.
        assert!(matches!(
            validate_utf8(&[0xED, 0xA0, 0x80]),
            Err(TextError::InvalidUtf8(_))
        ));
        assert_eq!(validate_utf8(b"ok"), Ok("ok"));
    }

    #[test]
    fn surrogate_and_char_boundary_safety() {
        let text = "a漢😀b";
        let bad = 2; // inside the 3-byte '漢' starting at byte 1
        assert_eq!(
            validate_grapheme_boundaries(text, &[bad]),
            Err(TextError::BoundaryNotOnChar { offset: bad })
        );
        let beyond = text.len() + 1;
        assert_eq!(
            validate_grapheme_boundaries(text, &[beyond]),
            Err(TextError::BoundaryOutOfRange {
                offset: beyond,
                len: text.len()
            })
        );
        assert_eq!(
            validate_grapheme_boundaries(text, &[4, 4]),
            Err(TextError::BoundariesNotIncreasing {
                previous: 4,
                offset: 4
            })
        );
        // Valid cluster boundaries: start, after each scalar, and end.
        let valid = [0, 1, 4, 8, text.len()];
        assert_eq!(validate_grapheme_boundaries(text, &valid), Ok(()));
        let plan = shaping_plan_with_boundaries(text, &valid).expect("plan");
        // Every run range lies on a character boundary.
        for run in plan.runs() {
            assert!(text.is_char_boundary(run.start()));
            assert!(text.is_char_boundary(run.end()));
        }
    }

    #[test]
    fn determinism() {
        let first = shaping_plan("abc שלום 漢字 مرحبا").expect("plan");
        let second = shaping_plan("abc שלום 漢字 مرحبا").expect("plan");
        assert_eq!(first, second);
        assert_eq!(bidi_runs("abc שלום"), bidi_runs("abc שלום"));
    }

    #[test]
    fn script_classification() {
        assert_eq!(classify_char('A'), Script::Latin);
        assert_eq!(classify_char('é'), Script::Latin);
        assert_eq!(classify_char('א'), Script::Hebrew);
        assert_eq!(classify_char('ب'), Script::Arabic);
        assert_eq!(classify_char('中'), Script::Cjk);
        assert_eq!(classify_char('Ω'), Script::Other);
        assert_eq!(classify_char('7'), Script::Other);
        assert_eq!(classify_char('😀'), Script::Other);
        assert_eq!(dominant_script(" רק עברית"), Script::Hebrew);
    }

    #[test]
    fn direction_resolution() {
        assert_eq!(is_strong_direction('a'), Some(Direction::Ltr));
        assert_eq!(is_strong_direction('漢'), Some(Direction::Ltr));
        assert_eq!(is_strong_direction('ש'), Some(Direction::Rtl));
        assert_eq!(is_strong_direction('م'), Some(Direction::Rtl));
        assert_eq!(is_strong_direction(' '), None);
        assert_eq!(resolve_base_direction("123 !!!"), Direction::Ltr);
        assert_eq!(resolve_base_direction("123 שלום"), Direction::Rtl);
        assert_eq!(resolve_base_direction("مرحبا world"), Direction::Rtl);
        assert_eq!(Direction::for_script(Script::Other), None);
        assert!(Direction::Rtl.is_rtl());
        assert!(!Direction::Ltr.is_rtl());
        assert_eq!(Script::Cjk.as_str(), "cjk");
        assert_eq!(Direction::Rtl.as_str(), "rtl");
    }

    // ---- T-0172 explicit direction and aggregate text analysis ----

    #[test]
    fn explicit_base_direction_overrides_first_strong() {
        let forced_rtl = shaping_plan_with_base_direction("hello world", Direction::Rtl)
            .unwrap_or_else(|error| panic!("plan: {error}"));
        assert_eq!(forced_rtl.base_direction(), Direction::Rtl);
        let logical: Vec<String> = bidi_runs("hello world")
            .iter()
            .map(|run| run.text().to_owned())
            .collect();
        let mut expected = logical.clone();
        expected.reverse();
        let visual: Vec<String> = forced_rtl
            .runs()
            .iter()
            .map(|run| run.text().to_owned())
            .collect();
        assert_eq!(visual, expected);

        let forced_ltr = shaping_plan_with_base_direction("שלום world", Direction::Ltr)
            .unwrap_or_else(|error| panic!("plan: {error}"));
        assert_eq!(forced_ltr.base_direction(), Direction::Ltr);
        let natural: Vec<String> = bidi_runs("שלום world")
            .iter()
            .map(|run| run.text().to_owned())
            .collect();
        let visual_ltr: Vec<String> = forced_ltr
            .runs()
            .iter()
            .map(|run| run.text().to_owned())
            .collect();
        assert_eq!(visual_ltr, natural);
        assert_eq!(
            shaping_plan_with_base_direction("", Direction::Rtl),
            Err(TextError::EmptyInput)
        );
    }

    #[test]
    fn text_analysis_report_is_deterministic_and_has_stable_summary() {
        let text = "hello שלום 漢字";
        let boundaries = [0, 6, 15, text.len()];
        let a = analyze_text(text, &boundaries).unwrap_or_else(|e| panic!("analysis: {e}"));
        let b = analyze_text(text, &boundaries).unwrap_or_else(|e| panic!("analysis: {e}"));
        assert_eq!(a, b);
        assert_eq!(a.summary(), b.summary());
        assert_eq!(a.summary(), a.to_string());
        assert_eq!(a.base_direction(), Direction::Ltr);
        assert_eq!(a.dominant_script(), Script::Latin);
        assert_eq!(a.boundaries(), &boundaries);
        assert_eq!(a.text_len(), text.len());
        assert!(a.summary().starts_with("bytes="));
        assert!(a.summary().contains("dominant=latin"));
        assert!(a.summary().contains("base=ltr"));
    }

    #[test]
    fn text_analysis_validates_grapheme_boundaries() {
        let text = "a漢😀b";
        let bad = 2; // inside the 3-byte '漢' starting at byte 1
        assert_eq!(
            analyze_text(text, &[bad]),
            Err(TextError::BoundaryNotOnChar { offset: bad })
        );
        assert_eq!(
            analyze_text(text, &[text.len() + 1]),
            Err(TextError::BoundaryOutOfRange {
                offset: text.len() + 1,
                len: text.len()
            })
        );
        assert_eq!(
            analyze_text(text, &[4, 4]),
            Err(TextError::BoundariesNotIncreasing {
                previous: 4,
                offset: 4
            })
        );
    }

    #[test]
    fn text_analysis_empty_and_invalid_utf8_are_typed_errors() {
        assert_eq!(analyze_text("", &[]), Err(TextError::EmptyInput));
        assert_eq!(analyze_utf8(b"", &[]), Err(TextError::EmptyInput));
        assert!(matches!(
            analyze_utf8(&[0xED, 0xA0, 0x80], &[]),
            Err(TextError::InvalidUtf8(_))
        ));
        let analysis = analyze_utf8(b"hello", &[0, 5]).unwrap_or_else(|e| panic!("analysis: {e}"));
        assert_eq!(analysis.base_direction(), Direction::Ltr);
        assert_eq!(analysis.dominant_script(), Script::Latin);
    }

    #[test]
    fn text_analysis_byte_ranges_cover_text_on_char_boundaries() {
        let text = "abc שלום 漢字 مرحبا";
        let analysis = analyze_text(text, &[]).unwrap_or_else(|e| panic!("analysis: {e}"));
        let runs = analysis.runs();
        assert_eq!(runs.first().map(BidiRun::start), Some(0));
        assert_eq!(runs.last().map(BidiRun::end), Some(text.len()));
        for pair in runs.windows(2) {
            assert_eq!(pair[0].end(), pair[1].start());
        }
        for run in runs {
            assert!(text.is_char_boundary(run.start()));
            assert!(text.is_char_boundary(run.end()));
        }
    }

    // ---- T-0181 PLAT-007 deterministic shaping/fallback plan ----

    #[test]
    fn font_candidate_validation_normalizes_scripts() {
        let candidate = FontCandidate::new("Noto", vec![Script::Cjk, Script::Latin, Script::Cjk])
            .expect("candidate");
        assert_eq!(candidate.family(), "Noto");
        assert_eq!(candidate.scripts(), &[Script::Cjk, Script::Latin]);
        assert!(candidate.supports(Script::Cjk));
        assert!(!candidate.supports(Script::Arabic));
        assert!(matches!(
            FontCandidate::new("", vec![Script::Latin]),
            Err(TextError::EmptyFontFamily)
        ));
        assert!(matches!(
            FontCandidate::new("bad\0name", vec![Script::Latin]),
            Err(TextError::InvalidFontFamily(_))
        ));
        assert!(matches!(
            FontCandidate::new("Empty", vec![]),
            Err(TextError::EmptyCandidates)
        ));
    }

    #[test]
    fn fallback_plan_selects_ordered_font_chain_per_run() {
        let text = "hi שלום 漢字";
        let runs = bidi_runs(text);
        let candidates = [
            FontCandidate::new(
                "Fallback",
                vec![Script::Latin, Script::Hebrew, Script::Cjk, Script::Other],
            )
            .expect("fallback"),
            FontCandidate::new("CJK", vec![Script::Cjk]).expect("cjk"),
            FontCandidate::new("Hebrew", vec![Script::Hebrew]).expect("hebrew"),
        ];
        let plan = build_fallback_plan(&runs, &candidates).expect("plan");
        assert_eq!(
            plan,
            build_fallback_plan(&runs, &candidates).expect("plan again")
        );
        assert_eq!(plan.steps().len(), runs.len());
        assert_eq!(plan.summary(), plan.to_string());
        assert!(plan.summary().starts_with("runs="));
        assert!(plan.summary().contains("chain="));
        // The Latin run picks the first (fallback) candidate; the CJK run picks
        // the fallback first and the dedicated CJK font second.
        let cjk_step = plan
            .steps()
            .iter()
            .find(|step| step.script() == Script::Cjk)
            .expect("cjk step");
        assert_eq!(
            cjk_step.font_chain(),
            &["Fallback".to_owned(), "CJK".to_owned()]
        );
        assert_eq!(cjk_step.primary_font(), "Fallback");
        assert_eq!(cjk_step.direction(), Direction::Ltr);
        let hebrew_step = plan
            .steps()
            .iter()
            .find(|step| step.script() == Script::Hebrew)
            .expect("hebrew step");
        assert_eq!(hebrew_step.direction(), Direction::Rtl);
        // Byte ranges still lie on character boundaries.
        for step in plan.steps() {
            assert!(text.is_char_boundary(step.start()));
            assert!(text.is_char_boundary(step.end()));
        }
    }

    #[test]
    fn fallback_plan_from_text_matches_explicit_runs() {
        let text = "hello world";
        let candidates =
            [FontCandidate::new("Base", vec![Script::Latin, Script::Other]).expect("candidate")];
        let via_text = fallback_plan_from_text(text, &candidates).expect("plan from text");
        let via_runs = build_fallback_plan(&bidi_runs(text), &candidates).expect("plan from runs");
        assert_eq!(via_text, via_runs);
        assert_eq!(
            via_text.summary(),
            "runs=3; chain=latin:Base|other:Base|latin:Base"
        );
    }

    #[test]
    fn fallback_plan_reports_typed_errors() {
        let runs = bidi_runs("hello");
        let base =
            [FontCandidate::new("Base", vec![Script::Latin, Script::Other]).expect("candidate")];
        assert_eq!(build_fallback_plan(&[], &base), Err(TextError::EmptyInput));
        assert_eq!(
            build_fallback_plan(&runs, &[]),
            Err(TextError::EmptyCandidates)
        );
        assert_eq!(
            fallback_plan_from_text("", &base),
            Err(TextError::EmptyInput)
        );
        assert_eq!(
            build_fallback_plan(&bidi_runs("漢字"), &base),
            Err(TextError::NoFontForScript(Script::Cjk))
        );
        let too_many: Vec<FontCandidate> = (0..MAX_FONT_CANDIDATES.saturating_add(1))
            .map(|index| {
                FontCandidate::new(&format!("Font{index}"), vec![Script::Latin, Script::Other])
                    .expect("candidate")
            })
            .collect();
        assert_eq!(
            build_fallback_plan(&runs, &too_many),
            Err(TextError::TooManyCandidates(
                MAX_FONT_CANDIDATES.saturating_add(1),
                MAX_FONT_CANDIDATES
            ))
        );
    }
}
