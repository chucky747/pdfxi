//! Trapping presets and transparency-flattening policy (feature PRINT-006).
//!
//! This module models the *inputs* to a print-production prepress stage without
//! performing any rasterization itself. It produces a small, deterministic
//! [`TrapPlan`] that a native backend can consume. Two related concerns live
//! here:
//!
//! * **Trapping**: a [`TrappingPreset`] carries the default trap width, the
//!   width and colour used when trapping against black, the stroke join style,
//!   and a sliding-trap policy. These are the knobs exposed by an Acrobat-style
//!   *Trapping Setup* dialog, expressed in PDF points.
//! * **Transparency flattening**: a [`FlattenPolicy`] carries the target
//!   resolution, the vector-versus-raster balance, and how overprint is to be
//!   handled while flattening transparent groups.
//!
//! # Backend limits (honest delegation)
//!
//! This crate never changes page pixels. It validates, normalizes and *plans*.
//! Every operation that actually paints traps or flattens transparency is the
//! responsibility of a native prepress backend. The plan exposes
//! [`TrapPlan::needs_backend_prepress`] and [`TrapPlan::warnings`] so a caller
//! can see exactly which requested features need native support:
//!
//! * a custom CMYK trap colour needs a backend CMYK transform;
//! * sliding traps need geometric analysis of adjacent ink edges;
//! * simulated overprint during flattening needs native compositing; and
//! * raster-biased flattening needs a rasterizer at the requested resolution.
//!
//! Numeric inputs are bounded and every value is checked for finiteness, so the
//! same inputs always yield the same plan. There is no filesystem, wall-clock,
//! randomness, unsafe code or new dependency in this module.

/// Maximum default trap width, in PDF points.
pub const MAX_TRAP_WIDTH_PT: f32 = 12.0;
/// Maximum black trap width, in PDF points.
pub const MAX_BLACK_WIDTH_PT: f32 = 48.0;
/// Maximum sliding distance, in PDF points.
pub const MAX_SLIDE_PT: f32 = 24.0;
/// Minimum supported flattening resolution, in DPI.
pub const MIN_FLATTEN_DPI: u32 = 72;
/// Maximum supported flattening resolution, in DPI.
pub const MAX_FLATTEN_DPI: u32 = 2400;
/// Maximum nesting depth when flattening transparency groups.
pub const MAX_FLATTEN_DEPTH: u8 = 32;
/// Maximum length, in bytes, of a trapping preset name.
pub const MAX_PRESET_NAME_BYTES: usize = 128;

/// The default trap width, in PDF points.
pub const DEFAULT_TRAP_WIDTH_PT: f32 = 0.25;
/// The default black trap width, in PDF points.
pub const DEFAULT_BLACK_WIDTH_PT: f32 = 1.0;
/// The default flattening resolution, in DPI.
pub const DEFAULT_FLATTEN_DPI: u32 = 300;
/// The default maximum flattening depth.
pub const DEFAULT_FLATTEN_DEPTH: u8 = 8;

/// Structured errors produced while building trapping presets or flatten
/// policies.
#[derive(Debug, thiserror::Error, PartialEq)]
pub enum TrapError {
    /// A trapping preset name was empty or whitespace only.
    #[error("trapping preset name must not be empty")]
    EmptyPresetName,
    /// A trapping preset name exceeded the byte-length cap.
    #[error("trapping preset name length {len} exceeds the {limit}-byte limit")]
    PresetNameTooLong {
        /// Actual byte length of the name.
        len: usize,
        /// Enforced byte-length limit.
        limit: usize,
    },
    /// A trap width was not finite, not positive, or above the cap.
    #[error("trap width {value} must be finite, in (0.0..={limit}] points")]
    InvalidTrapWidth {
        /// The rejected value.
        value: f32,
        /// Enforced point limit.
        limit: f32,
    },
    /// A black trap width was not finite, not positive, or above the cap.
    #[error("black trap width {value} must be finite, in (0.0..={limit}] points")]
    InvalidBlackWidth {
        /// The rejected value.
        value: f32,
        /// Enforced point limit.
        limit: f32,
    },
    /// A custom trap-colour component was not finite or was outside `0.0..=1.0`.
    #[error("trap colour component '{field}' must be finite and in 0.0..=1.0, got {value}")]
    InvalidTrapComponent {
        /// Name of the offending component.
        field: &'static str,
        /// The rejected value.
        value: f32,
    },
    /// A sliding-trap distance was not finite or was outside `0.0..=MAX_SLIDE_PT`.
    #[error("sliding distance {value} must be finite and in 0.0..={limit} points")]
    InvalidSlideDistance {
        /// The rejected value.
        value: f32,
        /// Enforced point limit.
        limit: f32,
    },
    /// A flattening resolution was outside the supported DPI range.
    #[error("flatten resolution {value} must be in {min}..={max} DPI")]
    InvalidResolution {
        /// The rejected value.
        value: u32,
        /// Minimum supported DPI.
        min: u32,
        /// Maximum supported DPI.
        max: u32,
    },
    /// A flattening depth was zero or above the cap.
    #[error("flatten depth {value} must be in 1..={limit}")]
    InvalidFlattenDepth {
        /// The rejected value.
        value: u8,
        /// Enforced depth limit.
        limit: u8,
    },
}

/// The geometry used to join trap strokes at corners.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TrapJoinStyle {
    /// A sharp, extended miter join.
    #[default]
    Miter,
    /// A rounded join.
    Round,
    /// A cropped, bevel join.
    Bevel,
}

impl TrapJoinStyle {
    /// Parse a join style from a case-insensitive token.
    ///
    /// Accepts `miter`/`mitre`, `round`, and `bevel`.
    ///
    /// # Errors
    ///
    /// Returns [`TrapError::InvalidTrapComponent`] with field `"join_style"`
    /// for an unrecognised token. (The value is stored in the `value` field as
    /// a sentinel of `f32::NAN`; callers should treat an unrecognised token as
    /// a parsing error.)
    pub fn parse(value: &str) -> Result<Self, TrapError> {
        match value.trim().to_ascii_lowercase().as_str() {
            "miter" | "mitre" => Ok(Self::Miter),
            "round" => Ok(Self::Round),
            "bevel" => Ok(Self::Bevel),
            other => Err(TrapError::InvalidTrapComponent {
                field: "join_style",
                value: other.len() as f32,
            }),
        }
    }

    /// Return the canonical token for this join style.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Miter => "miter",
            Self::Round => "round",
            Self::Bevel => "bevel",
        }
    }

    /// Return a stable two-bit encoding of this join style.
    pub const fn bits(self) -> u8 {
        match self {
            Self::Miter => 0,
            Self::Round => 1,
            Self::Bevel => 2,
        }
    }
}

impl std::fmt::Display for TrapJoinStyle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

/// The colour used to build trap strokes where a colour traps over black.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum TrapColor {
    /// Let the backend derive the trap colour from the neighbouring inks.
    #[default]
    Auto,
    /// Knock out the underlying ink instead of laying down a trap.
    Knockout,
    /// Use an explicit device-CMYK colour; each component is in `0.0..=1.0`.
    CustomCmyk {
        /// Cyan component in `0.0..=1.0`.
        c: f32,
        /// Magenta component in `0.0..=1.0`.
        m: f32,
        /// Yellow component in `0.0..=1.0`.
        y: f32,
        /// Black (key) component in `0.0..=1.0`.
        k: f32,
    },
}

impl TrapColor {
    /// Validate that every component of this colour is finite and in `0.0..=1.0`.
    ///
    /// # Errors
    ///
    /// Returns [`TrapError::InvalidTrapComponent`] for the first bad component.
    pub fn validate(self) -> Result<(), TrapError> {
        if let Self::CustomCmyk { c, m, y, k } = self {
            for (field, value) in [("c", c), ("m", m), ("y", y), ("k", k)] {
                if !value.is_finite() || !(0.0..=1.0).contains(&value) {
                    return Err(TrapError::InvalidTrapComponent { field, value });
                }
            }
        }
        Ok(())
    }

    /// Return `true` when this colour requires a native CMYK transform.
    pub const fn needs_backend_cmyk(self) -> bool {
        matches!(self, Self::CustomCmyk { .. })
    }

    /// Return a stable two-bit encoding of this colour choice.
    pub const fn bits(self) -> u8 {
        match self {
            Self::Auto => 0,
            Self::Knockout => 1,
            Self::CustomCmyk { .. } => 2,
        }
    }
}

/// The sliding-trap policy used when an ink edge crosses a colour boundary.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum TrapSliding {
    /// Sliding traps are disabled.
    #[default]
    Disabled,
    /// Let the backend choose the slide distance automatically.
    Automatic,
    /// Cap the slide distance at `max_slide_pt` points.
    Limited {
        /// Maximum slide distance, in points, in `0.0..=MAX_SLIDE_PT`.
        max_slide_pt: f32,
    },
}

impl TrapSliding {
    /// Validate the optional slide distance.
    ///
    /// # Errors
    ///
    /// Returns [`TrapError::InvalidSlideDistance`] when a [`Self::Limited`]
    /// distance is not finite or is outside `0.0..=MAX_SLIDE_PT`.
    pub fn validate(self) -> Result<(), TrapError> {
        if let Self::Limited { max_slide_pt } = self
            && (!max_slide_pt.is_finite() || !(0.0..=MAX_SLIDE_PT).contains(&max_slide_pt))
        {
            return Err(TrapError::InvalidSlideDistance {
                value: max_slide_pt,
                limit: MAX_SLIDE_PT,
            });
        }
        Ok(())
    }

    /// Return `true` when the backend must perform geometric sliding analysis.
    pub const fn needs_backend_geometry(self) -> bool {
        !matches!(self, Self::Disabled)
    }

    /// Return a stable two-bit encoding of this sliding choice.
    pub const fn bits(self) -> u8 {
        match self {
            Self::Disabled => 0,
            Self::Automatic => 1,
            Self::Limited { .. } => 2,
        }
    }
}

/// A validated, named trapping preset (feature PRINT-006).
#[derive(Clone, Debug, PartialEq)]
pub struct TrappingPreset {
    name: String,
    trap_width_pt: f32,
    black_width_pt: f32,
    black_color: TrapColor,
    join_style: TrapJoinStyle,
    sliding: TrapSliding,
}

impl TrappingPreset {
    /// Create and validate a trapping preset.
    ///
    /// # Errors
    ///
    /// Returns a typed [`TrapError`] for an empty/oversized name, a non-finite
    /// or out-of-range trap/black width, an invalid custom colour, or an
    /// invalid sliding distance.
    pub fn new(
        name: &str,
        trap_width_pt: f32,
        black_width_pt: f32,
        black_color: TrapColor,
        join_style: TrapJoinStyle,
        sliding: TrapSliding,
    ) -> Result<Self, TrapError> {
        let trimmed = name.trim();
        if trimmed.is_empty() {
            return Err(TrapError::EmptyPresetName);
        }
        let len = name.len();
        if len > MAX_PRESET_NAME_BYTES {
            return Err(TrapError::PresetNameTooLong {
                len,
                limit: MAX_PRESET_NAME_BYTES,
            });
        }
        if !trap_width_pt.is_finite()
            || !(0.0..=MAX_TRAP_WIDTH_PT).contains(&trap_width_pt)
            || trap_width_pt == 0.0
        {
            return Err(TrapError::InvalidTrapWidth {
                value: trap_width_pt,
                limit: MAX_TRAP_WIDTH_PT,
            });
        }
        if !black_width_pt.is_finite()
            || !(0.0..=MAX_BLACK_WIDTH_PT).contains(&black_width_pt)
            || black_width_pt == 0.0
        {
            return Err(TrapError::InvalidBlackWidth {
                value: black_width_pt,
                limit: MAX_BLACK_WIDTH_PT,
            });
        }
        black_color.validate()?;
        sliding.validate()?;
        Ok(Self {
            name: name.to_owned(),
            trap_width_pt,
            black_width_pt,
            black_color,
            join_style,
            sliding,
        })
    }

    /// Create a conservative default preset with the given name.
    ///
    /// # Errors
    ///
    /// Propagates [`TrapError`] from [`Self::new`].
    pub fn default_named(name: &str) -> Result<Self, TrapError> {
        Self::new(
            name,
            DEFAULT_TRAP_WIDTH_PT,
            DEFAULT_BLACK_WIDTH_PT,
            TrapColor::default(),
            TrapJoinStyle::default(),
            TrapSliding::default(),
        )
    }

    /// Return the preset name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Return the default trap width in points.
    pub const fn trap_width_pt(&self) -> f32 {
        self.trap_width_pt
    }

    /// Return the black trap width in points.
    pub const fn black_width_pt(&self) -> f32 {
        self.black_width_pt
    }

    /// Return the colour used when trapping over black.
    pub const fn black_color(&self) -> TrapColor {
        self.black_color
    }

    /// Return the trap join style.
    pub const fn join_style(&self) -> TrapJoinStyle {
        self.join_style
    }

    /// Return the sliding-trap policy.
    pub const fn sliding(&self) -> TrapSliding {
        self.sliding
    }
}

/// How transparency flattening should balance vector and raster output.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum VectorRasterBalance {
    /// Preserve as much vector geometry as possible.
    PreserveVectors,
    /// Balance vector preservation against rasterization.
    #[default]
    Balanced,
    /// Prefer rasterizing complex transparency into a single image.
    PreferRaster,
}

impl VectorRasterBalance {
    /// Parse a balance from a case-insensitive token.
    ///
    /// Accepts `vector`/`vectors`/`preserve`, `balanced`/`balance`, and
    /// `raster`/`rasterize`.
    ///
    /// # Errors
    ///
    /// Returns [`TrapError::InvalidTrapComponent`] with field
    /// `"vector_raster_balance"` for an unrecognised token.
    pub fn parse(value: &str) -> Result<Self, TrapError> {
        match value.trim().to_ascii_lowercase().as_str() {
            "vector" | "vectors" | "preserve" | "preserve-vectors" => Ok(Self::PreserveVectors),
            "balanced" | "balance" => Ok(Self::Balanced),
            "raster" | "rasterize" | "prefer-raster" => Ok(Self::PreferRaster),
            other => Err(TrapError::InvalidTrapComponent {
                field: "vector_raster_balance",
                value: other.len() as f32,
            }),
        }
    }

    /// Return the canonical token for this balance.
    pub const fn name(self) -> &'static str {
        match self {
            Self::PreserveVectors => "preserve-vectors",
            Self::Balanced => "balanced",
            Self::PreferRaster => "prefer-raster",
        }
    }

    /// Return `true` when a native rasterizer at the target DPI is required.
    pub const fn needs_rasterizer(self) -> bool {
        matches!(self, Self::PreferRaster)
    }

    /// Return a stable two-bit encoding of this balance.
    pub const fn bits(self) -> u8 {
        match self {
            Self::PreserveVectors => 0,
            Self::Balanced => 1,
            Self::PreferRaster => 2,
        }
    }
}

impl std::fmt::Display for VectorRasterBalance {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

/// How overprint is handled while flattening transparency.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FlattenOverprint {
    /// Preserve the existing overprint settings without simulating them.
    #[default]
    Preserve,
    /// Ask the backend to simulate overprint during flattening.
    Simulate,
    /// Convert overprinted marks to knockout.
    Knockout,
}

impl FlattenOverprint {
    /// Parse an overprint policy from a case-insensitive token.
    ///
    /// Accepts `preserve`/`keep`, `simulate`/`preview`, and `knockout`.
    ///
    /// # Errors
    ///
    /// Returns [`TrapError::InvalidTrapComponent`] with field
    /// `"flatten_overprint"` for an unrecognised token.
    pub fn parse(value: &str) -> Result<Self, TrapError> {
        match value.trim().to_ascii_lowercase().as_str() {
            "preserve" | "keep" => Ok(Self::Preserve),
            "simulate" | "preview" => Ok(Self::Simulate),
            "knockout" => Ok(Self::Knockout),
            other => Err(TrapError::InvalidTrapComponent {
                field: "flatten_overprint",
                value: other.len() as f32,
            }),
        }
    }

    /// Return the canonical token for this policy.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Preserve => "preserve",
            Self::Simulate => "simulate",
            Self::Knockout => "knockout",
        }
    }

    /// Return `true` when native overprint compositing is required.
    pub const fn needs_backend_compositing(self) -> bool {
        matches!(self, Self::Simulate)
    }

    /// Return a stable two-bit encoding of this policy.
    pub const fn bits(self) -> u8 {
        match self {
            Self::Preserve => 0,
            Self::Simulate => 1,
            Self::Knockout => 2,
        }
    }
}

impl std::fmt::Display for FlattenOverprint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

/// A validated transparency-flattening policy (feature PRINT-006).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FlattenPolicy {
    resolution_dpi: u32,
    balance: VectorRasterBalance,
    overprint: FlattenOverprint,
    max_depth: u8,
}

impl FlattenPolicy {
    /// Create and validate a flattening policy.
    ///
    /// `resolution_dpi` must be in [`MIN_FLATTEN_DPI`..=[`MAX_FLATTEN_DPI`]].
    /// The default maximum depth ([`DEFAULT_FLATTEN_DEPTH`]) is used; use
    /// [`Self::with_max_depth`] to override it.
    ///
    /// # Errors
    ///
    /// Returns [`TrapError::InvalidResolution`] for an out-of-range resolution.
    pub fn new(
        resolution_dpi: u32,
        balance: VectorRasterBalance,
        overprint: FlattenOverprint,
    ) -> Result<Self, TrapError> {
        Self::with_max_depth(resolution_dpi, balance, overprint, DEFAULT_FLATTEN_DEPTH)
    }

    /// Create and validate a flattening policy with an explicit group depth.
    ///
    /// # Errors
    ///
    /// Returns [`TrapError::InvalidResolution`] or
    /// [`TrapError::InvalidFlattenDepth`] for out-of-range inputs.
    pub fn with_max_depth(
        resolution_dpi: u32,
        balance: VectorRasterBalance,
        overprint: FlattenOverprint,
        max_depth: u8,
    ) -> Result<Self, TrapError> {
        if !(MIN_FLATTEN_DPI..=MAX_FLATTEN_DPI).contains(&resolution_dpi) {
            return Err(TrapError::InvalidResolution {
                value: resolution_dpi,
                min: MIN_FLATTEN_DPI,
                max: MAX_FLATTEN_DPI,
            });
        }
        if max_depth == 0 || max_depth > MAX_FLATTEN_DEPTH {
            return Err(TrapError::InvalidFlattenDepth {
                value: max_depth,
                limit: MAX_FLATTEN_DEPTH,
            });
        }
        Ok(Self {
            resolution_dpi,
            balance,
            overprint,
            max_depth,
        })
    }

    /// Return the target flattening resolution in DPI.
    pub const fn resolution_dpi(&self) -> u32 {
        self.resolution_dpi
    }

    /// Return the vector/raster balance.
    pub const fn balance(&self) -> VectorRasterBalance {
        self.balance
    }

    /// Return the overprint policy.
    pub const fn overprint(&self) -> FlattenOverprint {
        self.overprint
    }

    /// Return the maximum transparency-group flattening depth.
    pub const fn max_depth(&self) -> u8 {
        self.max_depth
    }
}

impl Default for FlattenPolicy {
    fn default() -> Self {
        Self {
            resolution_dpi: DEFAULT_FLATTEN_DPI,
            balance: VectorRasterBalance::default(),
            overprint: FlattenOverprint::default(),
            max_depth: DEFAULT_FLATTEN_DEPTH,
        }
    }
}

/// A deterministic prepress description a native backend can consume.
///
/// Build one with [`TrapPlan::build`]. The plan contains only resolved, validated
/// values; no I/O and no rasterization happen here.
#[derive(Clone, Debug, PartialEq)]
pub struct TrapPlan {
    /// Name of the trapping preset that produced this plan.
    pub preset_name: String,
    /// Resolved default trap width, in points.
    pub trap_width_pt: f32,
    /// Resolved black trap width, in points.
    pub black_width_pt: f32,
    /// Resolved black trap colour choice.
    pub black_color: TrapColor,
    /// Resolved trap join style.
    pub join_style: TrapJoinStyle,
    /// Resolved sliding-trap policy.
    pub sliding: TrapSliding,
    /// Resolved flattening resolution, in DPI.
    pub flatten_resolution_dpi: u32,
    /// Resolved vector/raster balance.
    pub vector_raster_balance: VectorRasterBalance,
    /// Resolved flattening overprint policy.
    pub flatten_overprint: FlattenOverprint,
    /// Maximum transparency-group flattening depth.
    pub flatten_max_depth: u8,
    /// Deterministic bit encoding of the plan's categorical choices.
    pub policy_bits: u32,
    /// Whether a native prepress backend is required for the requested features.
    pub needs_backend_prepress: bool,
    /// Human-readable notes describing which features need native support.
    pub warnings: Vec<String>,
}

impl TrapPlan {
    /// Build a deterministic plan from a trapping preset and flatten policy.
    ///
    /// The function is pure: it never mutates its inputs and returns a
    /// byte-identical plan for identical inputs. Backend-only features are
    /// reported through [`Self::needs_backend_prepress`] and [`Self::warnings`]
    /// rather than being silently approximated.
    pub fn build(preset: &TrappingPreset, policy: &FlattenPolicy) -> Result<Self, TrapError> {
        // Re-validate defensively so a plan can never be built from a value
        // that drifted outside its documented bounds.
        preset.black_color.validate()?;
        preset.sliding.validate()?;

        let mut warnings = Vec::new();
        let mut needs_backend_prepress = false;

        if preset.black_color.needs_backend_cmyk() {
            warnings.push("custom CMYK trap colour requires a backend colour transform".to_owned());
            needs_backend_prepress = true;
        }
        if preset.sliding.needs_backend_geometry() {
            warnings.push(
                "sliding traps require backend geometry analysis of adjacent inks".to_owned(),
            );
            needs_backend_prepress = true;
        }
        if policy.overprint.needs_backend_compositing() {
            warnings.push(
                "simulated overprint during flattening requires backend compositing".to_owned(),
            );
            needs_backend_prepress = true;
        }
        if policy.balance.needs_rasterizer() {
            warnings.push(format!(
                "raster-biased flattening requires a rasterizer at {} DPI",
                policy.resolution_dpi
            ));
            needs_backend_prepress = true;
        }
        if preset.black_color == TrapColor::Knockout {
            warnings.push(
                "knockout trap colour delegates knockout generation to the backend".to_owned(),
            );
            needs_backend_prepress = true;
        }

        // Stable encoding: 2 bits per categorical choice, low to high.
        let policy_bits = u32::from(preset.join_style.bits())
            | (u32::from(preset.black_color.bits()) << 2)
            | (u32::from(preset.sliding.bits()) << 4)
            | (u32::from(policy.balance.bits()) << 6)
            | (u32::from(policy.overprint.bits()) << 8);

        Ok(Self {
            preset_name: preset.name.clone(),
            trap_width_pt: preset.trap_width_pt,
            black_width_pt: preset.black_width_pt,
            black_color: preset.black_color,
            join_style: preset.join_style,
            sliding: preset.sliding,
            flatten_resolution_dpi: policy.resolution_dpi,
            vector_raster_balance: policy.balance,
            flatten_overprint: policy.overprint,
            flatten_max_depth: policy.max_depth,
            policy_bits,
            needs_backend_prepress,
            warnings,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_preset_and_policy_are_valid() {
        let preset = TrappingPreset::default_named("Default").expect("default preset");
        assert_eq!(preset.name(), "Default");
        assert_eq!(preset.trap_width_pt(), DEFAULT_TRAP_WIDTH_PT);
        assert_eq!(preset.black_width_pt(), DEFAULT_BLACK_WIDTH_PT);
        assert_eq!(preset.black_color(), TrapColor::Auto);
        assert_eq!(preset.join_style(), TrapJoinStyle::Miter);
        assert_eq!(preset.sliding(), TrapSliding::Disabled);

        let policy = FlattenPolicy::default();
        assert_eq!(policy.resolution_dpi(), DEFAULT_FLATTEN_DPI);
        assert_eq!(policy.balance(), VectorRasterBalance::Balanced);
        assert_eq!(policy.overprint(), FlattenOverprint::Preserve);
        assert_eq!(policy.max_depth(), DEFAULT_FLATTEN_DEPTH);
    }

    #[test]
    fn preset_validation_rejects_bad_widths_and_names() {
        assert_eq!(
            TrappingPreset::default_named("   "),
            Err(TrapError::EmptyPresetName)
        );
        assert_eq!(
            TrappingPreset::default_named(&"x".repeat(MAX_PRESET_NAME_BYTES + 1)),
            Err(TrapError::PresetNameTooLong {
                len: MAX_PRESET_NAME_BYTES + 1,
                limit: MAX_PRESET_NAME_BYTES,
            })
        );
        assert_eq!(
            TrappingPreset::new(
                "p",
                0.0,
                1.0,
                TrapColor::Auto,
                TrapJoinStyle::Miter,
                TrapSliding::Disabled
            ),
            Err(TrapError::InvalidTrapWidth {
                value: 0.0,
                limit: MAX_TRAP_WIDTH_PT
            })
        );
        assert!(matches!(
            TrappingPreset::new(
                "p",
                f32::NAN,
                1.0,
                TrapColor::Auto,
                TrapJoinStyle::Miter,
                TrapSliding::Disabled
            ),
            Err(TrapError::InvalidTrapWidth { .. })
        ));
        assert!(matches!(
            TrappingPreset::new(
                "p",
                0.25,
                MAX_BLACK_WIDTH_PT + 1.0,
                TrapColor::Auto,
                TrapJoinStyle::Miter,
                TrapSliding::Disabled
            ),
            Err(TrapError::InvalidBlackWidth { .. })
        ));
    }

    #[test]
    fn categorical_tokens_parse_and_display() {
        assert_eq!(TrapJoinStyle::parse(" MITRE "), Ok(TrapJoinStyle::Miter));
        assert_eq!(TrapJoinStyle::parse("round"), Ok(TrapJoinStyle::Round));
        assert_eq!(TrapJoinStyle::parse("bevel"), Ok(TrapJoinStyle::Bevel));
        assert!(matches!(
            TrapJoinStyle::parse("nope"),
            Err(TrapError::InvalidTrapComponent { .. })
        ));
        assert_eq!(
            VectorRasterBalance::parse("preserve"),
            Ok(VectorRasterBalance::PreserveVectors)
        );
        assert_eq!(
            VectorRasterBalance::parse("rasterize"),
            Ok(VectorRasterBalance::PreferRaster)
        );
        assert_eq!(
            FlattenOverprint::parse("simulate"),
            Ok(FlattenOverprint::Simulate)
        );
        assert_eq!(FlattenOverprint::Simulate.to_string(), "simulate");
        assert_eq!(VectorRasterBalance::Balanced.to_string(), "balanced");
    }

    #[test]
    fn trap_colour_and_sliding_validation() {
        assert_eq!(TrapColor::Auto.validate(), Ok(()));
        assert_eq!(
            TrapColor::CustomCmyk {
                c: 0.1,
                m: 0.2,
                y: 0.3,
                k: 0.4
            }
            .validate(),
            Ok(())
        );
        assert!(matches!(
            TrapColor::CustomCmyk {
                c: 1.5,
                m: 0.2,
                y: 0.3,
                k: 0.4
            }
            .validate(),
            Err(TrapError::InvalidTrapComponent { field: "c", .. })
        ));
        assert!(
            TrapColor::CustomCmyk {
                c: f32::NAN,
                m: 0.0,
                y: 0.0,
                k: 0.0
            }
            .needs_backend_cmyk()
        );
        assert_eq!(
            TrapSliding::Limited { max_slide_pt: 2.0 }.validate(),
            Ok(())
        );
        assert!(matches!(
            TrapSliding::Limited {
                max_slide_pt: MAX_SLIDE_PT + 1.0
            }
            .validate(),
            Err(TrapError::InvalidSlideDistance { .. })
        ));
        assert!(TrapSliding::Automatic.needs_backend_geometry());
        assert!(!TrapSliding::Disabled.needs_backend_geometry());
    }

    #[test]
    fn flatten_policy_validation() {
        assert!(
            FlattenPolicy::new(
                MIN_FLATTEN_DPI - 1,
                VectorRasterBalance::Balanced,
                FlattenOverprint::Preserve
            )
            .is_err()
        );
        assert!(
            FlattenPolicy::new(
                MAX_FLATTEN_DPI + 1,
                VectorRasterBalance::Balanced,
                FlattenOverprint::Preserve
            )
            .is_err()
        );
        assert_eq!(
            FlattenPolicy::with_max_depth(
                300,
                VectorRasterBalance::Balanced,
                FlattenOverprint::Preserve,
                0
            ),
            Err(TrapError::InvalidFlattenDepth {
                value: 0,
                limit: MAX_FLATTEN_DEPTH
            })
        );
        let policy = FlattenPolicy::with_max_depth(
            600,
            VectorRasterBalance::PreserveVectors,
            FlattenOverprint::Simulate,
            12,
        )
        .expect("valid policy");
        assert_eq!(policy.resolution_dpi(), 600);
        assert_eq!(policy.max_depth(), 12);
    }

    #[test]
    fn plan_is_deterministic_and_encodes_policy() {
        let preset = TrappingPreset::new(
            "Production",
            0.3,
            1.2,
            TrapColor::CustomCmyk {
                c: 0.0,
                m: 0.0,
                y: 0.0,
                k: 1.0,
            },
            TrapJoinStyle::Round,
            TrapSliding::Limited { max_slide_pt: 2.0 },
        )
        .expect("preset");
        let policy = FlattenPolicy::new(
            600,
            VectorRasterBalance::PreferRaster,
            FlattenOverprint::Simulate,
        )
        .expect("policy");
        let a = TrapPlan::build(&preset, &policy).expect("plan a");
        let b = TrapPlan::build(&preset, &policy).expect("plan b");
        assert_eq!(a, b);

        let expected_bits = u32::from(TrapJoinStyle::Round.bits())
            | (u32::from(
                TrapColor::CustomCmyk {
                    c: 0.0,
                    m: 0.0,
                    y: 0.0,
                    k: 1.0,
                }
                .bits(),
            ) << 2)
            | (u32::from(TrapSliding::Limited { max_slide_pt: 2.0 }.bits()) << 4)
            | (u32::from(VectorRasterBalance::PreferRaster.bits()) << 6)
            | (u32::from(FlattenOverprint::Simulate.bits()) << 8);
        assert_eq!(a.policy_bits, expected_bits);
        assert_eq!(a.preset_name, "Production");
        assert_eq!(a.flatten_resolution_dpi, 600);
        assert_eq!(a.flatten_max_depth, DEFAULT_FLATTEN_DEPTH);
    }

    #[test]
    fn plan_reports_backend_limits_honestly() {
        let preset = TrappingPreset::default_named("Safe").expect("preset");
        let policy = FlattenPolicy::default();
        let plan = TrapPlan::build(&preset, &policy).expect("plan");
        assert!(
            !plan.needs_backend_prepress,
            "pure defaults need no native prepress feature"
        );
        assert!(plan.warnings.is_empty());

        let raster_preset = TrappingPreset::new(
            "Raster",
            0.25,
            1.0,
            TrapColor::CustomCmyk {
                c: 0.0,
                m: 0.0,
                y: 0.0,
                k: 1.0,
            },
            TrapJoinStyle::Miter,
            TrapSliding::Automatic,
        )
        .expect("raster preset");
        let raster_policy = FlattenPolicy::new(
            1200,
            VectorRasterBalance::PreferRaster,
            FlattenOverprint::Simulate,
        )
        .expect("raster policy");
        let plan = TrapPlan::build(&raster_preset, &raster_policy).expect("plan");
        assert!(plan.needs_backend_prepress);
        assert!(plan.warnings.iter().any(|w| w.contains("CMYK")));
        assert!(plan.warnings.iter().any(|w| w.contains("sliding")));
        assert!(plan.warnings.iter().any(|w| w.contains("overprint")));
        assert!(plan.warnings.iter().any(|w| w.contains("rasterizer")));
    }
}
