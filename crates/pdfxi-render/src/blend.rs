//! Blend modes and transparency-group isolation descriptors (feature STD-005).
//!
//! This module extends the raster/color contract in [`crate::color`] with the
//! descriptors needed to describe *how* two colours are composited, not just
//! what bytes a raster holds. It is backend-agnostic and performs no
//! rasterization: it produces a deterministic [`BlendPlan`] a native compositor
//! can consume.
//!
//! The model follows the extended graphics-state blend modes and transparency
//! groups of ISO 32000. [`BlendMode`] covers the standard separable and
//! non-separable modes; [`GroupIsolation`] and [`GroupKnockout`] describe a
//! transparency group's isolation/knockout behaviour. A [`TransparencyGroup`]
//! combines those into a validated descriptor.
//!
//! # Honest backend delegation
//!
//! Actual compositing is delegated to the renderer backend. The plan sets
//! [`BlendPlan::needs_backend_cmm`] for non-separable modes (Hue, Saturation,
//! Color, Luminosity), which a PDF compositor computes in a device-independent
//! colour space rather than per channel. Partial source/backdrop alpha and
//! isolated groups set [`BlendPlan::needs_backend_alpha`]. [`BlendPlan::warnings`]
//! spells out each such requirement so callers never mistake a plan for a
//! rendered result.
//!
//! Inputs are bounded, categorical choices are bit-encoded deterministically,
//! and alpha values are checked for finiteness and range. No filesystem,
//! wall-clock, randomness, unsafe code or new dependency is used.

/// Maximum length, in bytes, of a named transparency-group label.
pub const MAX_GROUP_NAME_BYTES: usize = 128;

/// Structured errors produced while describing blend operations.
#[derive(Debug, thiserror::Error, PartialEq)]
pub enum BlendError {
    /// A blend-mode token was not recognised.
    #[error("unknown blend mode: {0}")]
    UnknownBlendMode(String),
    /// A group-isolation token was not recognised.
    #[error("unknown group isolation: {0}")]
    UnknownIsolation(String),
    /// A group-knockout token was not recognised.
    #[error("unknown group knockout: {0}")]
    UnknownKnockout(String),
    /// A source or backdrop alpha was not finite or was outside `0.0..=1.0`.
    #[error("{field} alpha must be finite and in 0.0..=1.0, got {value}")]
    InvalidAlpha {
        /// Which alpha was invalid (`"source"` or `"backdrop"`).
        field: &'static str,
        /// The rejected value.
        value: f32,
    },
    /// A transparency-group name was empty or whitespace only.
    #[error("transparency group name must not be empty")]
    EmptyGroupName,
    /// A transparency-group name exceeded the byte-length cap.
    #[error("transparency group name length {len} exceeds the {limit}-byte limit")]
    GroupNameTooLong {
        /// Actual byte length.
        len: usize,
        /// Enforced limit.
        limit: usize,
    },
}

/// Normalize a token for tolerant comparison: lowercase and drop `-`, `_` and
/// spaces so `ColorDodge`, `color-dodge` and `color dodge` all match.
fn normalize_token(value: &str) -> String {
    value
        .trim()
        .to_ascii_lowercase()
        .chars()
        .filter(|c| *c != '-' && *c != '_' && *c != ' ')
        .collect()
}

/// Whether a blend mode can be computed independently per colour channel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlendCategory {
    /// Computable independently on each device colour channel.
    Separable,
    /// Requires a device-independent colour space (native CMM).
    NonSeparable,
}

impl BlendCategory {
    /// Return a stable one-bit encoding of this category.
    pub const fn bits(self) -> u8 {
        match self {
            Self::Separable => 0,
            Self::NonSeparable => 1,
        }
    }
}

/// A standard ISO 32000 extended-graphics-state blend mode.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BlendMode {
    /// The default source-over mode.
    #[default]
    Normal,
    /// Multiply the source and backdrop colours.
    Multiply,
    /// Screen the source and backdrop colours.
    Screen,
    /// Multiply or screen depending on the backdrop.
    Overlay,
    /// Use the darker of source and backdrop.
    Darken,
    /// Use the lighter of source and backdrop.
    Lighten,
    /// Brighten the backdrop by the source.
    ColorDodge,
    /// Darken the backdrop by the source.
    ColorBurn,
    /// Multiply or screen depending on the source.
    HardLight,
    /// A softer version of hard light.
    SoftLight,
    /// Absolute difference of source and backdrop.
    Difference,
    /// Difference with reduced contrast.
    Exclusion,
    /// Use the hue of the source with the saturation/luminosity of the backdrop.
    Hue,
    /// Use the saturation of the source with the hue/luminosity of the backdrop.
    Saturation,
    /// Use the hue and saturation of the source with the backdrop's luminosity.
    Color,
    /// Use the luminosity of the source with the backdrop's hue/saturation.
    Luminosity,
    /// The compatibility blend mode (equivalent to Normal with legacy handling).
    Compatible,
}

impl BlendMode {
    /// Parse a blend mode from a case-insensitive token.
    ///
    /// Hyphens, underscores and spaces are ignored, so both `color-dodge` and
    /// `ColorDodge` are accepted. Aliases `dodge`, `burn` and `compatibility`
    /// are also recognised.
    ///
    /// # Errors
    ///
    /// Returns [`BlendError::UnknownBlendMode`] for an unrecognised token.
    pub fn parse(value: &str) -> Result<Self, BlendError> {
        match normalize_token(value).as_str() {
            "normal" => Ok(Self::Normal),
            "multiply" => Ok(Self::Multiply),
            "screen" => Ok(Self::Screen),
            "overlay" => Ok(Self::Overlay),
            "darken" => Ok(Self::Darken),
            "lighten" => Ok(Self::Lighten),
            "colordodge" | "dodge" => Ok(Self::ColorDodge),
            "colorburn" | "burn" => Ok(Self::ColorBurn),
            "hardlight" => Ok(Self::HardLight),
            "softlight" => Ok(Self::SoftLight),
            "difference" => Ok(Self::Difference),
            "exclusion" => Ok(Self::Exclusion),
            "hue" => Ok(Self::Hue),
            "saturation" => Ok(Self::Saturation),
            "color" | "colour" => Ok(Self::Color),
            "luminosity" => Ok(Self::Luminosity),
            "compatible" | "compatibility" => Ok(Self::Compatible),
            other => Err(BlendError::UnknownBlendMode(other.to_owned())),
        }
    }

    /// Return the canonical token for this blend mode.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Multiply => "multiply",
            Self::Screen => "screen",
            Self::Overlay => "overlay",
            Self::Darken => "darken",
            Self::Lighten => "lighten",
            Self::ColorDodge => "color-dodge",
            Self::ColorBurn => "color-burn",
            Self::HardLight => "hard-light",
            Self::SoftLight => "soft-light",
            Self::Difference => "difference",
            Self::Exclusion => "exclusion",
            Self::Hue => "hue",
            Self::Saturation => "saturation",
            Self::Color => "color",
            Self::Luminosity => "luminosity",
            Self::Compatible => "compatible",
        }
    }

    /// Return whether this mode is separable or non-separable.
    pub const fn category(self) -> BlendCategory {
        match self {
            Self::Hue | Self::Saturation | Self::Color | Self::Luminosity => {
                BlendCategory::NonSeparable
            }
            _ => BlendCategory::Separable,
        }
    }

    /// Return `true` when a native colour-management module is required.
    pub const fn needs_backend_cmm(self) -> bool {
        matches!(self.category(), BlendCategory::NonSeparable)
    }

    /// Return a stable five-bit encoding of this blend mode.
    pub const fn bits(self) -> u8 {
        match self {
            Self::Normal => 0,
            Self::Multiply => 1,
            Self::Screen => 2,
            Self::Overlay => 3,
            Self::Darken => 4,
            Self::Lighten => 5,
            Self::ColorDodge => 6,
            Self::ColorBurn => 7,
            Self::HardLight => 8,
            Self::SoftLight => 9,
            Self::Difference => 10,
            Self::Exclusion => 11,
            Self::Hue => 12,
            Self::Saturation => 13,
            Self::Color => 14,
            Self::Luminosity => 15,
            Self::Compatible => 16,
        }
    }
}

impl std::fmt::Display for BlendMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

/// Whether a transparency group is isolated from its backdrop.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum GroupIsolation {
    /// The group's backdrop is the initial backdrop (transparent black).
    Isolated,
    /// The group is composited against the enclosing backdrop.
    #[default]
    NonIsolated,
}

impl GroupIsolation {
    /// Parse an isolation policy from a case-insensitive token.
    ///
    /// Accepts `isolated`/`isolate`/`true` and
    /// `nonisolated`/`non-isolated`/`false`.
    ///
    /// # Errors
    ///
    /// Returns [`BlendError::UnknownIsolation`] for an unrecognised token.
    pub fn parse(value: &str) -> Result<Self, BlendError> {
        match normalize_token(value).as_str() {
            "isolated" | "isolate" | "true" => Ok(Self::Isolated),
            "nonisolated" | "false" => Ok(Self::NonIsolated),
            other => Err(BlendError::UnknownIsolation(other.to_owned())),
        }
    }

    /// Return the canonical token for this isolation policy.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Isolated => "isolated",
            Self::NonIsolated => "non-isolated",
        }
    }

    /// Return `true` when the group requires isolated backdrop handling.
    pub const fn needs_isolated_backdrop(self) -> bool {
        matches!(self, Self::Isolated)
    }

    /// Return a single bit encoding.
    pub const fn bits(self) -> u8 {
        match self {
            Self::Isolated => 1,
            Self::NonIsolated => 0,
        }
    }
}

impl std::fmt::Display for GroupIsolation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

/// Whether a transparency group knocks out its backdrop.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum GroupKnockout {
    /// Elements inside the group knock out earlier elements.
    Knockout,
    /// Elements accumulate over earlier elements.
    #[default]
    NonKnockout,
}

impl GroupKnockout {
    /// Parse a knockout policy from a case-insensitive token.
    ///
    /// Accepts `knockout`/`ko`/`true` and
    /// `nonknockout`/`non-knockout`/`false`.
    ///
    /// # Errors
    ///
    /// Returns [`BlendError::UnknownKnockout`] for an unrecognised token.
    pub fn parse(value: &str) -> Result<Self, BlendError> {
        match normalize_token(value).as_str() {
            "knockout" | "ko" | "true" => Ok(Self::Knockout),
            "nonknockout" | "false" => Ok(Self::NonKnockout),
            other => Err(BlendError::UnknownKnockout(other.to_owned())),
        }
    }

    /// Return the canonical token for this knockout policy.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Knockout => "knockout",
            Self::NonKnockout => "non-knockout",
        }
    }

    /// Return a single bit encoding.
    pub const fn bits(self) -> u8 {
        match self {
            Self::Knockout => 1,
            Self::NonKnockout => 0,
        }
    }
}

impl std::fmt::Display for GroupKnockout {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

/// A validated transparency-group descriptor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TransparencyGroup {
    name: String,
    isolation: GroupIsolation,
    knockout: GroupKnockout,
}

impl TransparencyGroup {
    /// Create and validate a transparency group.
    ///
    /// # Errors
    ///
    /// Returns [`BlendError::EmptyGroupName`] or
    /// [`BlendError::GroupNameTooLong`] for an invalid label.
    pub fn new(
        name: &str,
        isolation: GroupIsolation,
        knockout: GroupKnockout,
    ) -> Result<Self, BlendError> {
        if name.trim().is_empty() {
            return Err(BlendError::EmptyGroupName);
        }
        let len = name.len();
        if len > MAX_GROUP_NAME_BYTES {
            return Err(BlendError::GroupNameTooLong {
                len,
                limit: MAX_GROUP_NAME_BYTES,
            });
        }
        Ok(Self {
            name: name.to_owned(),
            isolation,
            knockout,
        })
    }

    /// Return the group label.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Return the group's isolation policy.
    pub const fn isolation(&self) -> GroupIsolation {
        self.isolation
    }

    /// Return the group's knockout policy.
    pub const fn knockout(&self) -> GroupKnockout {
        self.knockout
    }
}

/// A validated description of one blend operation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BlendSpec {
    mode: BlendMode,
    source_alpha: f32,
    backdrop_alpha: f32,
}

impl BlendSpec {
    /// Create and validate a blend operation.
    ///
    /// Both alpha values must be finite and in `0.0..=1.0`.
    ///
    /// # Errors
    ///
    /// Returns [`BlendError::InvalidAlpha`] for either out-of-range alpha.
    pub fn new(
        mode: BlendMode,
        source_alpha: f32,
        backdrop_alpha: f32,
    ) -> Result<Self, BlendError> {
        if !source_alpha.is_finite() || !(0.0..=1.0).contains(&source_alpha) {
            return Err(BlendError::InvalidAlpha {
                field: "source",
                value: source_alpha,
            });
        }
        if !backdrop_alpha.is_finite() || !(0.0..=1.0).contains(&backdrop_alpha) {
            return Err(BlendError::InvalidAlpha {
                field: "backdrop",
                value: backdrop_alpha,
            });
        }
        Ok(Self {
            mode,
            source_alpha,
            backdrop_alpha,
        })
    }

    /// Return the blend mode.
    pub const fn mode(&self) -> BlendMode {
        self.mode
    }

    /// Return the source alpha.
    pub const fn source_alpha(&self) -> f32 {
        self.source_alpha
    }

    /// Return the backdrop alpha.
    pub const fn backdrop_alpha(&self) -> f32 {
        self.backdrop_alpha
    }
}

impl Default for BlendSpec {
    fn default() -> Self {
        Self {
            mode: BlendMode::Normal,
            source_alpha: 1.0,
            backdrop_alpha: 1.0,
        }
    }
}

/// A deterministic compositing description a native renderer can consume.
#[derive(Clone, Debug, PartialEq)]
pub struct BlendPlan {
    /// The resolved blend mode.
    pub mode: BlendMode,
    /// Whether the mode is separable or non-separable.
    pub category: BlendCategory,
    /// The resolved source alpha.
    pub source_alpha: f32,
    /// The resolved backdrop alpha.
    pub backdrop_alpha: f32,
    /// The resolved group isolation (default `NonIsolated` without a group).
    pub isolation: GroupIsolation,
    /// The resolved group knockout (default `NonKnockout` without a group).
    pub knockout: GroupKnockout,
    /// Whether the operation belongs to a named transparency group.
    pub has_transparency_group: bool,
    /// The transparency-group label, when one was supplied.
    pub group_name: Option<String>,
    /// Whether a native colour-management module is required.
    pub needs_backend_cmm: bool,
    /// Whether native alpha/group compositing is required.
    pub needs_backend_alpha: bool,
    /// Deterministic bit encoding of the plan's categorical choices.
    pub bits: u32,
    /// Human-readable notes about backend requirements.
    pub warnings: Vec<String>,
}

impl BlendPlan {
    /// Build a deterministic plan from a validated spec and optional group.
    ///
    /// The function is pure and infallible: both inputs are already validated
    /// newtypes. It returns an identical plan for identical inputs.
    pub fn build(spec: &BlendSpec, group: Option<&TransparencyGroup>) -> Self {
        let isolation = group.map_or(GroupIsolation::NonIsolated, TransparencyGroup::isolation);
        let knockout = group.map_or(GroupKnockout::NonKnockout, TransparencyGroup::knockout);
        let needs_backend_cmm = spec.mode.needs_backend_cmm();
        let needs_backend_alpha = spec.source_alpha < 1.0
            || spec.backdrop_alpha < 1.0
            || isolation.needs_isolated_backdrop()
            || knockout == GroupKnockout::Knockout;

        let mut warnings = Vec::new();
        if needs_backend_cmm {
            warnings.push(format!(
                "non-separable blend mode '{}' requires a backend colour transform",
                spec.mode.name()
            ));
        }
        if spec.source_alpha < 1.0 || spec.backdrop_alpha < 1.0 {
            warnings.push("partial alpha requires backend alpha compositing".to_owned());
        }
        if isolation.needs_isolated_backdrop() {
            warnings.push("isolated group requires backend transparent-black backdrop".to_owned());
        }
        if knockout == GroupKnockout::Knockout {
            warnings.push("knockout group requires backend knockout compositing".to_owned());
        }

        let category = spec.mode.category();
        let bits = u32::from(spec.mode.bits())
            | (u32::from(category.bits()) << 5)
            | (u32::from(isolation.bits()) << 6)
            | (u32::from(knockout.bits()) << 7);

        Self {
            mode: spec.mode,
            category,
            source_alpha: spec.source_alpha,
            backdrop_alpha: spec.backdrop_alpha,
            isolation,
            knockout,
            has_transparency_group: group.is_some(),
            group_name: group.map(|g| g.name.to_owned()),
            needs_backend_cmm,
            needs_backend_alpha,
            bits,
            warnings,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_standard_blend_modes_parse_and_display() {
        assert_eq!(BlendMode::parse("Normal"), Ok(BlendMode::Normal));
        assert_eq!(
            BlendMode::parse(" color - dodge "),
            Ok(BlendMode::ColorDodge)
        );
        assert_eq!(BlendMode::parse("burn"), Ok(BlendMode::ColorBurn));
        assert_eq!(BlendMode::parse("colour"), Ok(BlendMode::Color));
        assert_eq!(BlendMode::parse("compatibility"), Ok(BlendMode::Compatible));
        assert!(matches!(
            BlendMode::parse("nonsense"),
            Err(BlendError::UnknownBlendMode(_))
        ));
        assert_eq!(BlendMode::SoftLight.to_string(), "soft-light");
        assert_eq!(BlendMode::Luminosity.bits(), 15);
        assert_eq!(BlendMode::Compatible.bits(), 16);
    }

    #[test]
    fn blend_category_separates_cmy_modes() {
        assert_eq!(BlendMode::Multiply.category(), BlendCategory::Separable);
        assert_eq!(BlendMode::Difference.category(), BlendCategory::Separable);
        for mode in [
            BlendMode::Hue,
            BlendMode::Saturation,
            BlendMode::Color,
            BlendMode::Luminosity,
        ] {
            assert_eq!(mode.category(), BlendCategory::NonSeparable);
            assert!(mode.needs_backend_cmm());
        }
        assert!(!BlendMode::Normal.needs_backend_cmm());
        assert_eq!(BlendCategory::NonSeparable.bits(), 1);
    }

    #[test]
    fn isolation_and_knockout_parse_and_default() {
        assert_eq!(
            GroupIsolation::parse("isolated"),
            Ok(GroupIsolation::Isolated)
        );
        assert_eq!(
            GroupIsolation::parse("non-isolated"),
            Ok(GroupIsolation::NonIsolated)
        );
        assert_eq!(GroupIsolation::parse("true"), Ok(GroupIsolation::Isolated));
        assert!(matches!(
            GroupIsolation::parse("maybe"),
            Err(BlendError::UnknownIsolation(_))
        ));
        assert_eq!(GroupKnockout::parse("ko"), Ok(GroupKnockout::Knockout));
        assert_eq!(
            GroupKnockout::parse("non-knockout"),
            Ok(GroupKnockout::NonKnockout)
        );
        assert!(matches!(
            GroupKnockout::parse("bogus"),
            Err(BlendError::UnknownKnockout(_))
        ));
        assert_eq!(GroupIsolation::default(), GroupIsolation::NonIsolated);
        assert_eq!(GroupKnockout::default(), GroupKnockout::NonKnockout);
        assert_eq!(GroupIsolation::Isolated.to_string(), "isolated");
        assert_eq!(GroupKnockout::Knockout.to_string(), "knockout");
    }

    #[test]
    fn transparency_group_validation() {
        let group =
            TransparencyGroup::new("Artwork", GroupIsolation::Isolated, GroupKnockout::Knockout)
                .expect("valid group");
        assert_eq!(group.name(), "Artwork");
        assert_eq!(group.isolation(), GroupIsolation::Isolated);
        assert_eq!(group.knockout(), GroupKnockout::Knockout);
        assert_eq!(
            TransparencyGroup::new("  ", GroupIsolation::Isolated, GroupKnockout::Knockout),
            Err(BlendError::EmptyGroupName)
        );
        assert_eq!(
            TransparencyGroup::new(
                &"x".repeat(MAX_GROUP_NAME_BYTES + 1),
                GroupIsolation::Isolated,
                GroupKnockout::Knockout
            ),
            Err(BlendError::GroupNameTooLong {
                len: MAX_GROUP_NAME_BYTES + 1,
                limit: MAX_GROUP_NAME_BYTES
            })
        );
    }

    #[test]
    fn blend_spec_alpha_validation() {
        assert_eq!(
            BlendSpec::new(BlendMode::Normal, 1.5, 1.0),
            Err(BlendError::InvalidAlpha {
                field: "source",
                value: 1.5
            })
        );
        assert!(matches!(
            BlendSpec::new(BlendMode::Normal, 1.0, f32::NAN),
            Err(BlendError::InvalidAlpha {
                field: "backdrop",
                ..
            })
        ));
        let spec = BlendSpec::new(BlendMode::Multiply, 0.5, 1.0).expect("valid spec");
        assert_eq!(spec.mode(), BlendMode::Multiply);
        assert_eq!(spec.source_alpha(), 0.5);
        assert_eq!(spec.backdrop_alpha(), 1.0);
        assert_eq!(BlendSpec::default().source_alpha(), 1.0);
    }

    #[test]
    fn plan_is_deterministic_and_delegates_honestly() {
        let group = TransparencyGroup::new(
            "IsolatedArt",
            GroupIsolation::Isolated,
            GroupKnockout::Knockout,
        )
        .expect("group");
        let spec = BlendSpec::new(BlendMode::Hue, 0.8, 1.0).expect("spec");
        let a = BlendPlan::build(&spec, Some(&group));
        let b = BlendPlan::build(&spec, Some(&group));
        assert_eq!(a, b);
        assert_eq!(a.mode, BlendMode::Hue);
        assert_eq!(a.category, BlendCategory::NonSeparable);
        assert!(a.needs_backend_cmm);
        assert!(a.needs_backend_alpha);
        assert!(a.has_transparency_group);
        assert_eq!(a.group_name.as_deref(), Some("IsolatedArt"));
        assert!(a.warnings.iter().any(|w| w.contains("colour transform")));
        assert!(a.warnings.iter().any(|w| w.contains("alpha")));
        assert!(a.warnings.iter().any(|w| w.contains("isolated")));
        assert!(a.warnings.iter().any(|w| w.contains("knockout")));
    }

    #[test]
    fn opaque_normal_without_group_needs_no_backend() {
        let spec = BlendSpec::default();
        let plan = BlendPlan::build(&spec, None);
        assert_eq!(plan.mode, BlendMode::Normal);
        assert_eq!(plan.isolation, GroupIsolation::NonIsolated);
        assert_eq!(plan.knockout, GroupKnockout::NonKnockout);
        assert!(!plan.has_transparency_group);
        assert_eq!(plan.group_name, None);
        assert!(!plan.needs_backend_cmm);
        assert!(!plan.needs_backend_alpha);
        assert!(plan.warnings.is_empty());
    }

    #[test]
    fn plan_bits_encode_categorical_choices() {
        let spec = BlendSpec::new(BlendMode::ColorBurn, 1.0, 1.0).expect("spec");
        let group =
            TransparencyGroup::new("G", GroupIsolation::Isolated, GroupKnockout::NonKnockout)
                .expect("group");
        let plan = BlendPlan::build(&spec, Some(&group));
        let expected = u32::from(BlendMode::ColorBurn.bits())
            | (u32::from(BlendCategory::Separable.bits()) << 5)
            | (u32::from(GroupIsolation::Isolated.bits()) << 6)
            | (u32::from(GroupKnockout::NonKnockout.bits()) << 7);
        assert_eq!(plan.bits, expected);
    }
}
