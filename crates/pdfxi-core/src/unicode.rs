//! Unicode text model for CJK and right-to-left scripts.
//!
//! This module is a dependency-free, deterministic foundation for handling
//! mixed-script text. It provides script classification ([`Script`]), strong
//! direction detection ([`Direction`]), bidirectional run detection
//! ([`bidi_runs`]), base-direction resolution ([`resolve_base_direction`]) and
//! a deterministic [`ShapingPlan`]. It performs **no glyph shaping**: a plan is
//! an ordered list of same-script runs with byte ranges, scripts and directions
//! so a renderer or extractor can apply font selection and layout later.
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
}
