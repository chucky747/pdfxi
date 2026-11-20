//! Paragraph text-edit model with deterministic line-break reflow (EDIT-001).
//!
//! This module is a **model only**. It takes paragraph text that a caller has
//! already extracted, applies text replacements, and reflows the result into
//! lines against a target width using glyph advances supplied by the caller.
//! It performs **no font introspection** and **never reads or rewrites PDF
//! content streams**: a host writer serializes the resulting [`EditPlan`] (its
//! ordered [`EditStep`]s and [`ReflowedLine`]s) into whichever content-stream
//! representation it owns. No source bytes are mutated here.
//!
//! All output is deterministic: replacements are sorted by source span,
//! reflow is a greedy left-to-right word wrap, and equal inputs always produce
//! equal plans. Hostile or oversized inputs are rejected with typed
//! [`EditError`] variants rather than panicking.

use std::fmt;

/// A half-open range of Unicode **scalar** indices in a source paragraph.
///
/// `start` is inclusive and `end` is exclusive. Both are character (scalar)
/// offsets, not byte offsets, so they remain stable across UTF-8 input.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TextSpan {
    /// Inclusive start scalar index.
    pub start: usize,
    /// Exclusive end scalar index.
    pub end: usize,
}

/// One replacement applied to a [`ParagraphEdit`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Replacement {
    /// Source span removed by this replacement (may be empty for insertion).
    pub span: TextSpan,
    /// Text inserted in place of the span (may be empty for deletion).
    pub text: String,
}

/// A paragraph text-edit model: source text plus the replacements to apply.
///
/// Construct one of these from already-extracted paragraph text, then call
/// [`ParagraphEdit::build_plan`] to obtain a deterministic [`EditPlan`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParagraphEdit {
    /// Source paragraph text (caller-supplied; never mutated).
    pub source: String,
    /// Replacements to apply; order here does not affect the resulting plan.
    pub replacements: Vec<Replacement>,
}

/// Caller-supplied glyph advance function.
///
/// The implementation decides what a "width" means; this module only sums
/// advances and compares them to [`ReflowOptions::target_width`]. Returned
/// advances must be finite and non-negative or reflow returns a typed error.
pub trait GlyphMetrics {
    /// Advance width of one Unicode scalar, in the same units as the target width.
    fn advance(&self, c: char) -> f64;
}

/// A trivial [`GlyphMetrics`] that gives every scalar the same advance.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FixedMetrics {
    /// Advance assigned to every scalar. Must be finite and non-negative.
    pub advance: f64,
}

impl GlyphMetrics for FixedMetrics {
    fn advance(&self, _c: char) -> f64 {
        self.advance
    }
}

/// Adapts a closure `Fn(char) -> f64` into a [`GlyphMetrics`] implementation.
pub struct FnMetrics<F> {
    f: F,
}

impl<F> FnMetrics<F> {
    /// Wraps a glyph-advance closure.
    pub fn new(f: F) -> Self {
        Self { f }
    }
}

impl<F: Fn(char) -> f64> GlyphMetrics for FnMetrics<F> {
    fn advance(&self, c: char) -> f64 {
        (self.f)(c)
    }
}

/// Configuration for deterministic line-break reflow.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ReflowOptions {
    /// Target line width in the same units as glyph advances.
    pub target_width: f64,
    /// Keep the source paragraph's leading whitespace on the first line.
    pub preserve_leading_whitespace: bool,
    /// Keep the source paragraph's trailing whitespace on the last line.
    pub preserve_trailing_whitespace: bool,
}

impl Default for ReflowOptions {
    fn default() -> Self {
        Self {
            target_width: 500.0,
            preserve_leading_whitespace: true,
            preserve_trailing_whitespace: true,
        }
    }
}

/// Bounds applied to a single [`ParagraphEdit::build_plan`] call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EditLimits {
    /// Maximum number of scalars in the source paragraph.
    pub max_source_chars: usize,
    /// Maximum number of scalars in any single replacement text.
    pub max_replacement_chars: usize,
    /// Maximum number of scalars in the assembled result text.
    pub max_result_chars: usize,
    /// Maximum number of replacements.
    pub max_edits: usize,
    /// Maximum number of reflowed lines.
    pub max_lines: usize,
}

impl Default for EditLimits {
    fn default() -> Self {
        Self {
            max_source_chars: 1_000_000,
            max_replacement_chars: 1_000_000,
            max_result_chars: 2_000_000,
            max_edits: 10_000,
            max_lines: 1_000_000,
        }
    }
}

/// Typed errors produced while planning or applying a paragraph edit.
#[derive(Clone, Debug, PartialEq)]
pub enum EditError {
    /// The source paragraph was empty.
    EmptyInput,
    /// The source paragraph exceeded the configured scalar limit.
    SourceTooLong {
        /// Observed scalar count.
        len: usize,
        /// Configured limit.
        limit: usize,
    },
    /// A replacement text exceeded the configured scalar limit.
    ReplacementTooLong {
        /// Observed scalar count.
        len: usize,
        /// Configured limit.
        limit: usize,
    },
    /// The assembled result exceeded the configured scalar limit.
    ResultTooLong {
        /// Observed scalar count.
        len: usize,
        /// Configured limit.
        limit: usize,
    },
    /// More replacements than the configured limit were supplied.
    TooManyEdits {
        /// Observed replacement count.
        count: usize,
        /// Configured limit.
        limit: usize,
    },
    /// A replacement span was out of range or inverted.
    InvalidSpan {
        /// Inclusive start scalar index.
        start: usize,
        /// Exclusive end scalar index.
        end: usize,
        /// Number of scalars in the source paragraph.
        source_len: usize,
    },
    /// Two replacements overlapped in the source.
    OverlappingEdits {
        /// Index of the earlier replacement in input order.
        first: usize,
        /// Index of the later replacement in input order.
        second: usize,
    },
    /// The target width was not finite (NaN or infinity).
    NonFiniteWidth,
    /// The target width was zero or negative.
    NonPositiveWidth,
    /// A caller-supplied glyph advance was non-finite or negative.
    InvalidMetric {
        /// Scalar position whose advance was invalid.
        index: usize,
        /// The invalid advance value.
        value: f64,
    },
    /// Summed glyph advances overflowed to a non-finite value.
    MetricOverflow,
    /// Reflow produced more lines than the configured limit.
    TooManyLines {
        /// Observed line count.
        count: usize,
        /// Configured limit.
        limit: usize,
    },
    /// `EditPlan::apply` was called with text that is neither the plan's source nor its result.
    SourceMismatch,
}

impl fmt::Display for EditError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyInput => write!(f, "source paragraph is empty"),
            Self::SourceTooLong { len, limit } => {
                write!(f, "source has {len} scalars, above the {limit} limit")
            }
            Self::ReplacementTooLong { len, limit } => {
                write!(f, "replacement has {len} scalars, above the {limit} limit")
            }
            Self::ResultTooLong { len, limit } => {
                write!(f, "result has {len} scalars, above the {limit} limit")
            }
            Self::TooManyEdits { count, limit } => {
                write!(f, "{count} edits exceed the {limit} edit limit")
            }
            Self::InvalidSpan {
                start,
                end,
                source_len,
            } => write!(
                f,
                "invalid span {start}..{end} for source of {source_len} scalars"
            ),
            Self::OverlappingEdits { first, second } => {
                write!(f, "replacements {first} and {second} overlap")
            }
            Self::NonFiniteWidth => write!(f, "target width is not finite"),
            Self::NonPositiveWidth => write!(f, "target width must be positive"),
            Self::InvalidMetric { index, value } => {
                write!(f, "glyph advance at scalar {index} is invalid: {value}")
            }
            Self::MetricOverflow => write!(f, "glyph advance sum overflowed"),
            Self::TooManyLines { count, limit } => {
                write!(
                    f,
                    "reflow produced {count} lines, above the {limit} line limit"
                )
            }
            Self::SourceMismatch => write!(f, "input is neither the plan source nor its result"),
        }
    }
}

impl std::error::Error for EditError {}

/// One ordered step of an [`EditPlan`], ready for a host writer to serialize.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EditStep {
    /// Zero-based ordinal of this step in the plan (ascending, contiguous).
    pub order: usize,
    /// Source span removed by this step.
    pub span: TextSpan,
    /// Text inserted by this step (empty for a pure deletion).
    pub inserted: String,
}

/// One reflowed output line with its measured advance width.
#[derive(Clone, Debug, PartialEq)]
pub struct ReflowedLine {
    /// Line text, including any preserved leading/trailing whitespace.
    pub text: String,
    /// Sum of caller-supplied glyph advances for `text`.
    pub width: f64,
}

/// A deterministic, ordered, idempotent edit plan a host writer can serialize.
///
/// All fields are plain data so a host can serialize them without this crate
/// knowing anything about PDF content streams. [`EditPlan::apply`] is
/// idempotent: applying the plan to its own result is a no-op.
#[derive(Clone, Debug, PartialEq)]
pub struct EditPlan {
    /// Source paragraph the plan was built from.
    pub source: String,
    /// Ordered, non-overlapping edit steps.
    pub steps: Vec<EditStep>,
    /// Text obtained by applying all steps to `source` (no inserted line breaks).
    pub result: String,
    /// Deterministic reflow of `result` into lines of at most the target width.
    pub lines: Vec<ReflowedLine>,
}

impl EditPlan {
    /// Applies this plan to `input`.
    ///
    /// Deterministic and idempotent:
    /// - if `input` already equals [`EditPlan::result`], the plan is a no-op and
    ///   returns `result` unchanged;
    /// - if `input` equals [`EditPlan::source`], the ordered steps are applied
    ///   and `result` is returned;
    /// - any other input returns [`EditError::SourceMismatch`].
    pub fn apply(&self, input: &str) -> Result<String, EditError> {
        if input == self.result {
            return Ok(self.result.clone());
        }
        if input != self.source {
            return Err(EditError::SourceMismatch);
        }
        let offsets = char_byte_offsets(&self.source);
        Ok(apply_steps(&self.source, &self.steps, &offsets))
    }

    /// Returns the reflowed lines joined by `'\n'` in deterministic order.
    ///
    /// This is the host-writable text-with-breaks; this crate does not turn it
    /// into a PDF content stream.
    pub fn lines_text(&self) -> String {
        let mut out = String::new();
        for (i, line) in self.lines.iter().enumerate() {
            if i > 0 {
                out.push('\n');
            }
            out.push_str(&line.text);
        }
        out
    }
}

impl ParagraphEdit {
    /// Validates this edit and builds a deterministic [`EditPlan`].
    ///
    /// Replacements are sorted by `(start, end, input index)`, checked for
    /// overlap, applied to the source, and the result is reflowed into lines.
    /// No PDF bytes are read or written.
    pub fn build_plan(
        &self,
        options: &ReflowOptions,
        metrics: &dyn GlyphMetrics,
        limits: &EditLimits,
    ) -> Result<EditPlan, EditError> {
        if !options.target_width.is_finite() {
            return Err(EditError::NonFiniteWidth);
        }
        if options.target_width <= 0.0 {
            return Err(EditError::NonPositiveWidth);
        }

        let source_len = self.source.chars().count();
        if source_len == 0 {
            return Err(EditError::EmptyInput);
        }
        if source_len > limits.max_source_chars {
            return Err(EditError::SourceTooLong {
                len: source_len,
                limit: limits.max_source_chars,
            });
        }
        if self.replacements.len() > limits.max_edits {
            return Err(EditError::TooManyEdits {
                count: self.replacements.len(),
                limit: limits.max_edits,
            });
        }
        for replacement in &self.replacements {
            let len = replacement.text.chars().count();
            if len > limits.max_replacement_chars {
                return Err(EditError::ReplacementTooLong {
                    len,
                    limit: limits.max_replacement_chars,
                });
            }
            if replacement.span.start > replacement.span.end || replacement.span.end > source_len {
                return Err(EditError::InvalidSpan {
                    start: replacement.span.start,
                    end: replacement.span.end,
                    source_len,
                });
            }
        }

        let mut ordered: Vec<(usize, &Replacement)> =
            self.replacements.iter().enumerate().collect();
        ordered.sort_by_key(|(index, r)| (r.span.start, r.span.end, *index));

        let mut prev_end = 0usize;
        let mut prev_index = 0usize;
        for (i, (index, r)) in ordered.iter().enumerate() {
            if i > 0 && r.span.start < prev_end {
                return Err(EditError::OverlappingEdits {
                    first: prev_index,
                    second: *index,
                });
            }
            prev_end = r.span.end;
            prev_index = *index;
        }

        let steps: Vec<EditStep> = ordered
            .iter()
            .enumerate()
            .map(|(order, (_, r))| EditStep {
                order,
                span: r.span,
                inserted: r.text.clone(),
            })
            .collect();

        let offsets = char_byte_offsets(&self.source);
        let result = apply_steps(&self.source, &steps, &offsets);
        let result_len = result.chars().count();
        if result_len > limits.max_result_chars {
            return Err(EditError::ResultTooLong {
                len: result_len,
                limit: limits.max_result_chars,
            });
        }

        let lines = reflow_paragraph(&result, options, metrics, limits)?;
        Ok(EditPlan {
            source: self.source.clone(),
            steps,
            result,
            lines,
        })
    }
}

/// Byte offset of each scalar boundary in `s`, plus the final byte length.
fn char_byte_offsets(s: &str) -> Vec<usize> {
    let mut offsets: Vec<usize> = s.char_indices().map(|(byte, _)| byte).collect();
    offsets.push(s.len());
    offsets
}

/// Applies already-validated, sorted, non-overlapping steps to `source`.
fn apply_steps(source: &str, steps: &[EditStep], offsets: &[usize]) -> String {
    let mut out = String::new();
    let mut cursor = 0usize;
    for step in steps {
        let start_byte = offsets[step.span.start];
        let end_byte = offsets[step.span.end];
        out.push_str(&source[cursor..start_byte]);
        out.push_str(&step.inserted);
        cursor = end_byte;
    }
    out.push_str(&source[cursor..]);
    out
}

/// Sums the caller-supplied advances of every scalar in `text`.
fn measure(text: &str, metrics: &dyn GlyphMetrics) -> Result<f64, EditError> {
    let mut total = 0.0f64;
    for (index, c) in text.chars().enumerate() {
        let advance = metrics.advance(c);
        if !advance.is_finite() || advance < 0.0 {
            return Err(EditError::InvalidMetric {
                index,
                value: advance,
            });
        }
        total += advance;
    }
    if !total.is_finite() {
        return Err(EditError::MetricOverflow);
    }
    Ok(total)
}

/// Small tolerance so a word that exactly fills the target width is not wrapped.
const WIDTH_EPSILON: f64 = 1e-9;

/// Reflows `text` into deterministic lines using greedy word wrapping.
///
/// Rules (documented contract):
/// - Leading whitespace (run of whitespace scalars at the very start) is
///   preserved on the first line only when `preserve_leading_whitespace` is set.
/// - Trailing whitespace (run at the very end) is preserved on the last line
///   only when `preserve_trailing_whitespace` is set.
/// - All interior whitespace runs (including `'\n'` and `'\t'`) collapse to a
///   single ASCII space between words.
/// - Words are placed greedily; a single word wider than the target is emitted
///   alone on its line rather than broken mid-word.
pub fn reflow_paragraph(
    text: &str,
    options: &ReflowOptions,
    metrics: &dyn GlyphMetrics,
    limits: &EditLimits,
) -> Result<Vec<ReflowedLine>, EditError> {
    if !options.target_width.is_finite() {
        return Err(EditError::NonFiniteWidth);
    }
    if options.target_width <= 0.0 {
        return Err(EditError::NonPositiveWidth);
    }

    // Split off leading whitespace.
    let start_byte = text
        .char_indices()
        .find(|(_, c)| !c.is_whitespace())
        .map(|(byte, _)| byte)
        .unwrap_or(text.len());
    let leading = &text[..start_byte];

    // Split trailing whitespace only from the remaining (non-leading) slice.
    let rest = &text[start_byte..];
    let trailing_len = rest.len() - rest.trim_end().len();
    let end_byte = rest.len() - trailing_len;
    let body = &rest[..end_byte];
    let trailing = &rest[end_byte..];

    let leading = if options.preserve_leading_whitespace {
        leading
    } else {
        ""
    };
    let trailing = if options.preserve_trailing_whitespace {
        trailing
    } else {
        ""
    };

    let words: Vec<&str> = body.split_whitespace().collect();
    let space_width = measure(" ", metrics)?;

    let mut line_texts: Vec<String> = Vec::new();
    if words.is_empty() {
        let only = format!("{leading}{trailing}");
        if !only.is_empty() {
            if 1 > limits.max_lines {
                return Err(EditError::TooManyLines {
                    count: 1,
                    limit: limits.max_lines,
                });
            }
            // Width is recomputed uniformly for every line below.
            line_texts.push(only);
        }
    } else {
        let mut current = String::new();
        let mut current_width = 0.0f64;
        for word in words {
            let word_width = measure(word, metrics)?;
            if current.is_empty() {
                let prefix = if line_texts.is_empty() { leading } else { "" };
                let prefix_width = if line_texts.is_empty() {
                    measure(leading, metrics)?
                } else {
                    0.0
                };
                current.push_str(prefix);
                current.push_str(word);
                current_width = prefix_width + word_width;
            } else if current_width + space_width + word_width
                <= options.target_width + WIDTH_EPSILON
            {
                current.push(' ');
                current.push_str(word);
                current_width += space_width + word_width;
            } else {
                line_texts.push(std::mem::take(&mut current));
                current.push_str(word);
                current_width = word_width;
            }
        }
        if !current.is_empty() || line_texts.is_empty() {
            line_texts.push(current);
        }
        if let Some(last) = line_texts.last_mut() {
            last.push_str(trailing);
        }
    }

    if line_texts.len() > limits.max_lines {
        return Err(EditError::TooManyLines {
            count: line_texts.len(),
            limit: limits.max_lines,
        });
    }

    let mut lines = Vec::with_capacity(line_texts.len());
    for text in line_texts {
        let width = measure(&text, metrics)?;
        lines.push(ReflowedLine { text, width });
    }
    Ok(lines)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edit(source: &str, reps: &[(usize, usize, &str)]) -> ParagraphEdit {
        ParagraphEdit {
            source: source.to_string(),
            replacements: reps
                .iter()
                .map(|(start, end, text)| Replacement {
                    span: TextSpan {
                        start: *start,
                        end: *end,
                    },
                    text: (*text).to_string(),
                })
                .collect(),
        }
    }

    fn unit() -> FixedMetrics {
        FixedMetrics { advance: 1.0 }
    }

    #[test]
    fn reflow_is_deterministic() {
        let e = edit("one two three four", &[(7, 9, "TWO")]);
        let opts = ReflowOptions {
            target_width: 9.0,
            ..ReflowOptions::default()
        };
        let a = e
            .build_plan(&opts, &unit(), &EditLimits::default())
            .unwrap();
        let b = e
            .build_plan(&opts, &unit(), &EditLimits::default())
            .unwrap();
        assert_eq!(a, b);
        assert_eq!(a.lines_text(), b.lines_text());
    }

    #[test]
    fn width_boundaries_exact_and_over() {
        let opts = ReflowOptions {
            target_width: 7.0,
            ..ReflowOptions::default()
        };
        // "aaaa bbb" is exactly 8 with a space; with width 7 it must wrap.
        let exact = edit("aaaa", &[]);
        let plan = exact
            .build_plan(&opts, &unit(), &EditLimits::default())
            .unwrap();
        assert_eq!(plan.lines.len(), 1);
        assert!((plan.lines[0].width - 4.0).abs() < f64::EPSILON);

        let over = edit("aaaa bbb", &[]);
        let plan = over
            .build_plan(&opts, &unit(), &EditLimits::default())
            .unwrap();
        assert_eq!(plan.lines.len(), 2);
        assert_eq!(plan.lines_text(), "aaaa\nbbb");
    }

    #[test]
    fn leading_and_trailing_whitespace_preserved() {
        let e = edit("  alpha beta  ", &[]);
        let opts = ReflowOptions {
            target_width: 100.0,
            preserve_leading_whitespace: true,
            preserve_trailing_whitespace: true,
        };
        let plan = e
            .build_plan(&opts, &unit(), &EditLimits::default())
            .unwrap();
        assert_eq!(plan.lines_text(), "  alpha beta  ");
    }

    #[test]
    fn leading_and_trailing_whitespace_dropped() {
        let e = edit("  alpha beta  ", &[]);
        let opts = ReflowOptions {
            target_width: 100.0,
            preserve_leading_whitespace: false,
            preserve_trailing_whitespace: false,
        };
        let plan = e
            .build_plan(&opts, &unit(), &EditLimits::default())
            .unwrap();
        assert_eq!(plan.lines_text(), "alpha beta");
    }

    #[test]
    fn overlapping_edits_rejected() {
        let e = edit("abcdef", &[(0, 3, "X"), (2, 5, "Y")]);
        let err = e
            .build_plan(&ReflowOptions::default(), &unit(), &EditLimits::default())
            .unwrap_err();
        assert_eq!(
            err,
            EditError::OverlappingEdits {
                first: 0,
                second: 1
            }
        );
    }

    #[test]
    fn invalid_span_rejected() {
        let inverted = edit("abc", &[(2, 1, "X")]);
        assert!(matches!(
            inverted.build_plan(&ReflowOptions::default(), &unit(), &EditLimits::default()),
            Err(EditError::InvalidSpan { .. })
        ));
        let too_long = edit("abc", &[(0, 9, "X")]);
        assert!(matches!(
            too_long.build_plan(&ReflowOptions::default(), &unit(), &EditLimits::default()),
            Err(EditError::InvalidSpan { .. })
        ));
    }

    #[test]
    fn oversized_input_rejected() {
        let limits = EditLimits {
            max_source_chars: 4,
            ..EditLimits::default()
        };
        let e = edit("abcdef", &[]);
        assert!(matches!(
            e.build_plan(&ReflowOptions::default(), &unit(), &limits),
            Err(EditError::SourceTooLong { .. })
        ));

        let limits = EditLimits {
            max_replacement_chars: 2,
            ..EditLimits::default()
        };
        let e = edit("abc", &[(1, 2, "long")]);
        assert!(matches!(
            e.build_plan(&ReflowOptions::default(), &unit(), &limits),
            Err(EditError::ReplacementTooLong { .. })
        ));
    }

    #[test]
    fn empty_source_rejected() {
        let e = edit("", &[]);
        assert_eq!(
            e.build_plan(&ReflowOptions::default(), &unit(), &EditLimits::default())
                .unwrap_err(),
            EditError::EmptyInput
        );
    }

    #[test]
    fn non_finite_and_non_positive_widths_rejected() {
        for width in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let e = edit("abc", &[]);
            let opts = ReflowOptions {
                target_width: width,
                ..ReflowOptions::default()
            };
            assert_eq!(
                e.build_plan(&opts, &unit(), &EditLimits::default())
                    .unwrap_err(),
                EditError::NonFiniteWidth
            );
        }
        let e = edit("abc", &[]);
        let opts = ReflowOptions {
            target_width: 0.0,
            ..ReflowOptions::default()
        };
        assert_eq!(
            e.build_plan(&opts, &unit(), &EditLimits::default())
                .unwrap_err(),
            EditError::NonPositiveWidth
        );
    }

    #[test]
    fn non_finite_metric_rejected() {
        let bad = FnMetrics::new(|c| if c == 'b' { f64::NAN } else { 1.0 });
        let e = edit("abc", &[]);
        let err = e
            .build_plan(&ReflowOptions::default(), &bad, &EditLimits::default())
            .unwrap_err();
        assert!(matches!(err, EditError::InvalidMetric { index: 1, .. }));
    }

    #[test]
    fn plan_apply_is_ordered_idempotent_and_typed() {
        let e = edit("hello brave world", &[(6, 11, "calm"), (12, 17, "planet")]);
        let plan = e
            .build_plan(&ReflowOptions::default(), &unit(), &EditLimits::default())
            .unwrap();
        assert_eq!(plan.steps.len(), 2);
        assert_eq!(plan.steps[0].order, 0);
        assert_eq!(plan.steps[1].order, 1);
        assert_eq!(plan.result, "hello calm planet");

        let once = plan.apply(&plan.source).unwrap();
        assert_eq!(once, plan.result);
        let twice = plan.apply(&once).unwrap();
        assert_eq!(twice, once);
        assert_eq!(
            plan.apply("unrelated text").unwrap_err(),
            EditError::SourceMismatch
        );
    }

    #[test]
    fn edit_plan_round_trip_stability() {
        let e = edit("a b c", &[(4, 5, "C")]);
        let opts = ReflowOptions {
            target_width: 3.0,
            ..ReflowOptions::default()
        };
        let plan = e
            .build_plan(&opts, &unit(), &EditLimits::default())
            .unwrap();
        // Rebuilding from the same input is byte-identical.
        let plan2 = e
            .build_plan(&opts, &unit(), &EditLimits::default())
            .unwrap();
        assert_eq!(plan, plan2);
        // Recorded widths equal a fresh measurement of each line's text.
        for line in &plan.lines {
            assert_eq!(line.width, measure(&line.text, &unit()).unwrap());
        }
        // lines_text reconstructs deterministically from the plan itself.
        let rebuilt: String = plan
            .lines
            .iter()
            .map(|l| l.text.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(plan.lines_text(), rebuilt);
    }
}
