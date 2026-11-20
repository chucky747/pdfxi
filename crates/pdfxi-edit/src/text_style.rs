//! Text style model: font, size, scaling, spacing and color (EDIT-002).
//!
//! This is a pure, dependency-free **model**. It describes the style of text
//! runs and resolves a deterministic paragraph style; it does not introspect
//! fonts, emit PDF content streams, or mutate source bytes. A host writer
//! serializes the resolved [`ParagraphStylePlan`] into its own operators.
//!
//! Validation enforces a positive finite font size, a positive finite
//! horizontal scaling, bounded character/word spacing, a valid RGB fill color,
//! and a non-empty font family. Failures are typed [`StyleError`] variants.
//!
//! Paragraph resolution follows a documented **first-run-wins** rule: for each
//! style attribute, the first run whose [`StyleOverride`] sets that attribute
//! determines the paragraph's effective value; attributes no run sets fall
//! back to the supplied default. Resolution depends only on run order, never
//! on hash iteration, so it is deterministic.

use std::fmt;

/// An RGB fill color; each component is in the closed range `0.0..=1.0`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FillColor {
    /// Red component in `0.0..=1.0`.
    pub r: f64,
    /// Green component in `0.0..=1.0`.
    pub g: f64,
    /// Blue component in `0.0..=1.0`.
    pub b: f64,
}

impl FillColor {
    /// Opaque black (`0, 0, 0`).
    pub const BLACK: Self = Self {
        r: 0.0,
        g: 0.0,
        b: 0.0,
    };
    /// Opaque white (`1, 1, 1`).
    pub const WHITE: Self = Self {
        r: 1.0,
        g: 1.0,
        b: 1.0,
    };
}

/// Identifies a spacing attribute in a validation error.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpacingField {
    /// Character spacing (`Tc`).
    Character,
    /// Word spacing (`Tw`).
    Word,
}

/// Identifies a color component in a validation error.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColorComponent {
    /// Red channel.
    Red,
    /// Green channel.
    Green,
    /// Blue channel.
    Blue,
}

/// Typed errors produced while validating or resolving text styles.
#[derive(Clone, Debug, PartialEq)]
pub enum StyleError {
    /// The font family was empty or only whitespace.
    EmptyFontFamily,
    /// The font family exceeded the configured byte limit.
    FontFamilyTooLong {
        /// Observed byte length.
        len: usize,
        /// Configured limit.
        limit: usize,
    },
    /// The font size was non-finite or not positive.
    InvalidFontSize(f64),
    /// The font size exceeded the configured limit.
    FontSizeTooLarge {
        /// Observed size.
        size: f64,
        /// Configured limit.
        limit: f64,
    },
    /// Horizontal scaling was non-finite or not positive.
    InvalidHorizontalScaling(f64),
    /// A spacing value was non-finite or outside the allowed magnitude.
    SpacingOutOfRange {
        /// Which spacing attribute was invalid.
        field: SpacingField,
        /// The rejected value.
        value: f64,
        /// Maximum allowed absolute value.
        limit: f64,
    },
    /// A color component was non-finite or outside `0.0..=1.0`.
    InvalidColor {
        /// Which component was invalid.
        component: ColorComponent,
        /// The rejected value.
        value: f64,
    },
}

impl fmt::Display for StyleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyFontFamily => write!(f, "font family is empty"),
            Self::FontFamilyTooLong { len, limit } => {
                write!(f, "font family is {len} bytes, above the {limit} limit")
            }
            Self::InvalidFontSize(size) => write!(f, "invalid font size: {size}"),
            Self::FontSizeTooLarge { size, limit } => {
                write!(f, "font size {size} exceeds the {limit} limit")
            }
            Self::InvalidHorizontalScaling(value) => {
                write!(f, "invalid horizontal scaling: {value}")
            }
            Self::SpacingOutOfRange {
                field,
                value,
                limit,
            } => write!(f, "{field:?} spacing {value} is outside +/-{limit}"),
            Self::InvalidColor { component, value } => {
                write!(f, "color component {component:?} is invalid: {value}")
            }
        }
    }
}

impl std::error::Error for StyleError {}

/// Bounds applied while validating styles.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StyleLimits {
    /// Maximum byte length of a font family name.
    pub max_family_bytes: usize,
    /// Maximum allowed font size.
    pub max_font_size: f64,
    /// Maximum allowed absolute character/word spacing.
    pub max_spacing_magnitude: f64,
}

impl Default for StyleLimits {
    fn default() -> Self {
        Self {
            max_family_bytes: 128,
            max_font_size: 10_000.0,
            max_spacing_magnitude: 1_000.0,
        }
    }
}

/// A complete, validated style for one text run.
#[derive(Clone, Debug, PartialEq)]
pub struct RunStyle {
    /// Font family name (non-empty, at most [`StyleLimits::max_family_bytes`]).
    pub font_family: String,
    /// Font size in points; finite, positive, and bounded.
    pub font_size: f64,
    /// Horizontal scaling percentage; `100.0` is normal. Finite and positive.
    pub horizontal_scaling: f64,
    /// Character spacing (`Tc`) in text space units; bounded.
    pub character_spacing: f64,
    /// Word spacing (`Tw`) in text space units; bounded.
    pub word_spacing: f64,
    /// Fill color applied to the glyphs.
    pub fill_color: FillColor,
}

impl Default for RunStyle {
    fn default() -> Self {
        Self {
            font_family: "Helvetica".to_string(),
            font_size: 12.0,
            horizontal_scaling: 100.0,
            character_spacing: 0.0,
            word_spacing: 0.0,
            fill_color: FillColor::BLACK,
        }
    }
}

/// A partial style override for a run; `None` means "inherit".
#[derive(Clone, Debug, PartialEq, Default)]
pub struct StyleOverride {
    /// Overrides the font family.
    pub font_family: Option<String>,
    /// Overrides the font size.
    pub font_size: Option<f64>,
    /// Overrides horizontal scaling.
    pub horizontal_scaling: Option<f64>,
    /// Overrides character spacing.
    pub character_spacing: Option<f64>,
    /// Overrides word spacing.
    pub word_spacing: Option<f64>,
    /// Overrides the fill color.
    pub fill_color: Option<FillColor>,
}

/// A text run paired with its partial style override.
#[derive(Clone, Debug, PartialEq)]
pub struct Run {
    /// Run text.
    pub text: String,
    /// Style attributes this run overrides.
    pub style_override: StyleOverride,
}

/// A run with its fully resolved, validated effective style.
#[derive(Clone, Debug, PartialEq)]
pub struct StyledRun {
    /// Run text (unchanged).
    pub text: String,
    /// Effective style for this run.
    pub style: RunStyle,
}

/// Deterministic style plan for a whole paragraph.
#[derive(Clone, Debug, PartialEq)]
pub struct ParagraphStylePlan {
    /// Effective paragraph style resolved with the first-run-wins rule.
    pub paragraph_style: RunStyle,
    /// Per-run effective styles in the same order as the input runs.
    pub runs: Vec<StyledRun>,
}

/// Validates a complete style against `limits`.
pub fn validate_style(style: &RunStyle, limits: &StyleLimits) -> Result<(), StyleError> {
    let family = style.font_family.trim();
    if family.is_empty() {
        return Err(StyleError::EmptyFontFamily);
    }
    if style.font_family.len() > limits.max_family_bytes {
        return Err(StyleError::FontFamilyTooLong {
            len: style.font_family.len(),
            limit: limits.max_family_bytes,
        });
    }
    if !style.font_size.is_finite() || style.font_size <= 0.0 {
        return Err(StyleError::InvalidFontSize(style.font_size));
    }
    if style.font_size > limits.max_font_size {
        return Err(StyleError::FontSizeTooLarge {
            size: style.font_size,
            limit: limits.max_font_size,
        });
    }
    if !style.horizontal_scaling.is_finite() || style.horizontal_scaling <= 0.0 {
        return Err(StyleError::InvalidHorizontalScaling(
            style.horizontal_scaling,
        ));
    }
    check_spacing(
        SpacingField::Character,
        style.character_spacing,
        limits.max_spacing_magnitude,
    )?;
    check_spacing(
        SpacingField::Word,
        style.word_spacing,
        limits.max_spacing_magnitude,
    )?;
    check_color(style.fill_color)?;
    Ok(())
}

fn check_spacing(field: SpacingField, value: f64, limit: f64) -> Result<(), StyleError> {
    if !value.is_finite() || value.abs() > limit {
        return Err(StyleError::SpacingOutOfRange {
            field,
            value,
            limit,
        });
    }
    Ok(())
}

fn check_color(color: FillColor) -> Result<(), StyleError> {
    for (component, value) in [
        (ColorComponent::Red, color.r),
        (ColorComponent::Green, color.g),
        (ColorComponent::Blue, color.b),
    ] {
        if !value.is_finite() || !(0.0..=1.0).contains(&value) {
            return Err(StyleError::InvalidColor { component, value });
        }
    }
    Ok(())
}

/// Applies a partial override onto a base style and validates the result.
///
/// Every attribute set in `override_` replaces the base value; unset
/// attributes are inherited. The merged style is validated before being
/// returned, so per-run application cannot produce an invalid [`RunStyle`].
pub fn apply_style(
    base: &RunStyle,
    override_: &StyleOverride,
    limits: &StyleLimits,
) -> Result<RunStyle, StyleError> {
    let merged = RunStyle {
        font_family: override_
            .font_family
            .clone()
            .unwrap_or_else(|| base.font_family.clone()),
        font_size: override_.font_size.unwrap_or(base.font_size),
        horizontal_scaling: override_
            .horizontal_scaling
            .unwrap_or(base.horizontal_scaling),
        character_spacing: override_
            .character_spacing
            .unwrap_or(base.character_spacing),
        word_spacing: override_.word_spacing.unwrap_or(base.word_spacing),
        fill_color: override_.fill_color.unwrap_or(base.fill_color),
    };
    validate_style(&merged, limits)?;
    Ok(merged)
}

/// Resolves the paragraph style using the documented first-run-wins rule.
///
/// For each attribute, the first override in `overrides` that sets it wins;
/// attributes no override sets come from `default`. The resolved style is
/// validated before being returned.
pub fn resolve_paragraph_style(
    overrides: &[StyleOverride],
    default: &RunStyle,
    limits: &StyleLimits,
) -> Result<RunStyle, StyleError> {
    let mut family: Option<&String> = None;
    let mut font_size = None;
    let mut horizontal_scaling = None;
    let mut character_spacing = None;
    let mut word_spacing = None;
    let mut fill_color = None;
    for o in overrides {
        if family.is_none() {
            family = o.font_family.as_ref();
        }
        if font_size.is_none() {
            font_size = o.font_size;
        }
        if horizontal_scaling.is_none() {
            horizontal_scaling = o.horizontal_scaling;
        }
        if character_spacing.is_none() {
            character_spacing = o.character_spacing;
        }
        if word_spacing.is_none() {
            word_spacing = o.word_spacing;
        }
        if fill_color.is_none() {
            fill_color = o.fill_color;
        }
    }
    let resolved = RunStyle {
        font_family: family
            .cloned()
            .unwrap_or_else(|| default.font_family.clone()),
        font_size: font_size.unwrap_or(default.font_size),
        horizontal_scaling: horizontal_scaling.unwrap_or(default.horizontal_scaling),
        character_spacing: character_spacing.unwrap_or(default.character_spacing),
        word_spacing: word_spacing.unwrap_or(default.word_spacing),
        fill_color: fill_color.unwrap_or(default.fill_color),
    };
    validate_style(&resolved, limits)?;
    Ok(resolved)
}

/// Builds a deterministic style plan for a paragraph of runs.
///
/// The paragraph style uses [`resolve_paragraph_style`] (first run wins), and
/// each run's effective style uses [`apply_style`] on `default`. Run order is
/// preserved and repeated calls with equal input produce equal plans.
pub fn style_paragraph(
    runs: &[Run],
    default: &RunStyle,
    limits: &StyleLimits,
) -> Result<ParagraphStylePlan, StyleError> {
    let overrides: Vec<StyleOverride> = runs.iter().map(|r| r.style_override.clone()).collect();
    let paragraph_style = resolve_paragraph_style(&overrides, default, limits)?;
    let mut styled = Vec::with_capacity(runs.len());
    for run in runs {
        styled.push(StyledRun {
            text: run.text.clone(),
            style: apply_style(default, &run.style_override, limits)?,
        });
    }
    Ok(ParagraphStylePlan {
        paragraph_style,
        runs: styled,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn red() -> FillColor {
        FillColor {
            r: 1.0,
            g: 0.0,
            b: 0.0,
        }
    }

    #[test]
    fn style_validation_rejects_bad_inputs() {
        let lim = StyleLimits::default();
        let zero_size = RunStyle {
            font_size: 0.0,
            ..RunStyle::default()
        };
        assert_eq!(
            validate_style(&zero_size, &lim),
            Err(StyleError::InvalidFontSize(0.0))
        );

        let nan_size = RunStyle {
            font_size: f64::NAN,
            ..RunStyle::default()
        };
        assert!(matches!(
            validate_style(&nan_size, &lim),
            Err(StyleError::InvalidFontSize(_))
        ));

        let empty_family = RunStyle {
            font_family: String::new(),
            ..RunStyle::default()
        };
        assert_eq!(
            validate_style(&empty_family, &lim),
            Err(StyleError::EmptyFontFamily)
        );

        let bad_scaling = RunStyle {
            horizontal_scaling: 0.0,
            ..RunStyle::default()
        };
        assert!(matches!(
            validate_style(&bad_scaling, &lim),
            Err(StyleError::InvalidHorizontalScaling(_))
        ));

        let bad_char_spacing = RunStyle {
            character_spacing: 5000.0,
            ..RunStyle::default()
        };
        assert!(matches!(
            validate_style(&bad_char_spacing, &lim),
            Err(StyleError::SpacingOutOfRange {
                field: SpacingField::Character,
                ..
            })
        ));

        let bad_word_spacing = RunStyle {
            word_spacing: -5000.0,
            ..RunStyle::default()
        };
        assert!(matches!(
            validate_style(&bad_word_spacing, &lim),
            Err(StyleError::SpacingOutOfRange {
                field: SpacingField::Word,
                ..
            })
        ));

        let bad_color = RunStyle {
            fill_color: FillColor {
                r: 1.5,
                g: 0.0,
                b: 0.0,
            },
            ..RunStyle::default()
        };
        assert!(matches!(
            validate_style(&bad_color, &lim),
            Err(StyleError::InvalidColor {
                component: ColorComponent::Red,
                ..
            })
        ));
    }

    #[test]
    fn per_run_style_application_merges_and_validates() {
        let base = RunStyle::default();
        let o = StyleOverride {
            font_size: Some(20.0),
            fill_color: Some(red()),
            ..StyleOverride::default()
        };
        let applied = apply_style(&base, &o, &StyleLimits::default()).unwrap();
        assert_eq!(applied.font_size, 20.0);
        assert_eq!(applied.fill_color, red());
        assert_eq!(applied.font_family, base.font_family);
        assert_eq!(applied.character_spacing, base.character_spacing);

        let bad = StyleOverride {
            font_size: Some(-1.0),
            ..StyleOverride::default()
        };
        assert!(matches!(
            apply_style(&base, &bad, &StyleLimits::default()),
            Err(StyleError::InvalidFontSize(_))
        ));
    }

    #[test]
    fn paragraph_style_resolution_first_run_wins_is_deterministic() {
        let default = RunStyle::default();
        let overrides = [
            StyleOverride {
                font_size: Some(20.0),
                ..StyleOverride::default()
            },
            StyleOverride {
                font_size: Some(30.0),
                fill_color: Some(red()),
                ..StyleOverride::default()
            },
        ];
        let resolved =
            resolve_paragraph_style(&overrides, &default, &StyleLimits::default()).unwrap();
        assert_eq!(resolved.font_size, 20.0);
        assert_eq!(resolved.fill_color, red());
        assert_eq!(resolved.font_family, default.font_family);

        let reversed = [overrides[1].clone(), overrides[0].clone()];
        let resolved_rev =
            resolve_paragraph_style(&reversed, &default, &StyleLimits::default()).unwrap();
        assert_eq!(resolved_rev.font_size, 30.0);

        let again = resolve_paragraph_style(&overrides, &default, &StyleLimits::default()).unwrap();
        assert_eq!(resolved, again);
    }

    #[test]
    fn style_round_trip_stability() {
        let default = RunStyle::default();
        let runs = [
            Run {
                text: "hello ".to_string(),
                style_override: StyleOverride {
                    font_size: Some(14.0),
                    ..StyleOverride::default()
                },
            },
            Run {
                text: "world".to_string(),
                style_override: StyleOverride {
                    fill_color: Some(red()),
                    ..StyleOverride::default()
                },
            },
        ];
        let lim = StyleLimits::default();
        let plan = style_paragraph(&runs, &default, &lim).unwrap();
        let plan2 = style_paragraph(&runs, &default, &lim).unwrap();
        assert_eq!(plan, plan2);
        assert_eq!(plan.runs.len(), 2);
        assert_eq!(plan.runs[0].text, "hello ");
        assert_eq!(plan.runs[0].style.font_size, 14.0);
        assert_eq!(plan.runs[1].style.fill_color, red());

        // Applying a fully resolved style as an override is a fixed point.
        let full = StyleOverride {
            font_family: Some(plan.paragraph_style.font_family.clone()),
            font_size: Some(plan.paragraph_style.font_size),
            horizontal_scaling: Some(plan.paragraph_style.horizontal_scaling),
            character_spacing: Some(plan.paragraph_style.character_spacing),
            word_spacing: Some(plan.paragraph_style.word_spacing),
            fill_color: Some(plan.paragraph_style.fill_color),
        };
        let reapplied = apply_style(&plan.paragraph_style, &full, &lim).unwrap();
        assert_eq!(reapplied, plan.paragraph_style);
    }
}
