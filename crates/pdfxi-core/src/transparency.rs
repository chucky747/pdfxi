//! Deterministic transparency and blend-mode planning at library level.
//!
//! The render crate owns pixel compositing. This module deliberately does not
//! touch pixels: it supplies the *contract* a renderer must honour -- validated
//! blend descriptors, backend requirements and a deterministic plan whose bit
//! encoding is stable across runs. Cross-crate capability checks are expressed
//! through [`BlendBackend`], a trait any renderer can implement, so this crate
//! stays rendering-free and never imports another crate's renderer.
//!
//! All inputs are bounded. Alpha is stored as a 9-bit step count in `0..=256`,
//! descriptor tokens have a maximum length, and a transparency stack has a
//! maximum depth. Unsupported backend features are reported honestly through
//! [`BlendPlan::check_support`] as [`TransparencyError::UnsupportedBackend`]
//! rather than being silently dropped.
//!
//! # Render-backend contract (pixel compositing stays outside this crate)
//!
//! This module supplies *plans*, never pixels. A renderer consumes a
//! [`CompositingPlan`] by iterating [`CompositingPlan::steps`] in back-to-front
//! order, honouring each step's [`BlendSpec`], resolved [`GroupColorSpace`],
//! optional [`SoftMask`] and [`BackendRequirement`] set. The renderer must
//! refuse a plan whose [`CompositingPlan::check_support`] reports an unmet
//! [`BackendRequirement`], and must perform colour-space conversion and
//! source-over rasterization itself. This crate performs no colour conversion
//! and touches no pixel buffer.
//!
//! `pdfxi-render` already provides the raster/colour boundary through its
//! `color` and `blend` modules (`BlendMode`, `BlendPlan`, `DeviceColorSpace`,
//! `DeviceColor`). That crate is intentionally **not** modified here: the core
//! plan is a backend-agnostic intermediate whose requirements a renderer maps
//! onto those helpers. Non-separable modes require a colour-management
//! transform (`BackendRequirement::ColorManagement`), partial alpha and
//! isolated/knockout groups declare their own requirements, and applying soft
//! masks to pixels is likewise a renderer concern.

use std::fmt;

/// Fully opaque alpha value in the 9-bit `0..=256` representation.
pub const MAX_ALPHA: u16 = 256;
/// Maximum accepted length of a single descriptor token.
pub const MAX_TOKEN_LEN: usize = 64;
/// Maximum number of nested transparency groups in a stack.
pub const MAX_STACK_DEPTH: u8 = 8;

/// A typed reason a transparency descriptor or plan was rejected.
///
/// This error intentionally derives only `PartialEq`: the invalid-backdrop
/// variant carries an `f32`, for which exact `Eq` is not meaningful.
#[derive(Debug, Clone, PartialEq)]
pub enum TransparencyError {
    /// The blend-mode token is not an ISO 32000 standard mode.
    UnknownBlendMode(String),
    /// The isolation token is not recognised.
    UnknownIsolation(String),
    /// The knockout token is not recognised.
    UnknownKnockout(String),
    /// An alpha value lies outside `0..=256`.
    InvalidAlpha {
        /// The field that was rejected (`ca` or `CA`).
        field: &'static str,
        /// The rejected value.
        value: u16,
    },
    /// A caller-supplied token exceeded the bounded length.
    TokenTooLong {
        /// The field that was rejected.
        field: &'static str,
        /// The maximum accepted length.
        limit: usize,
    },
    /// A transparency stack exceeded its bounded nesting depth.
    StackTooDeep {
        /// The maximum accepted depth.
        limit: u8,
    },
    /// A descriptor did not have the expected field layout.
    MalformedDescriptor(String),
    /// The backend cannot honour a required compositing feature.
    UnsupportedBackend(BackendRequirement),
    /// The transparency-group colour space is not recognised.
    UnknownColourSpace(String),
    /// An ICC-based group colour space declared an out-of-range component count.
    InvalidColourSpaceComponents {
        /// The rejected component count.
        value: u8,
    },
    /// A soft-mask subtype token is not one of the ISO 32000 subtypes.
    UnknownSoftMaskSubtype(String),
    /// A soft-mask backdrop component was non-finite or outside `0.0..=1.0`.
    InvalidBackdropComponent {
        /// Zero-based component index.
        index: usize,
        /// The rejected component value.
        value: f32,
    },
    /// A soft-mask backdrop had more components than the colour space allows.
    TooManyBackdropComponents {
        /// Components supplied.
        count: usize,
        /// Maximum accepted components.
        limit: usize,
    },
    /// The group colour space is incompatible with the backdrop colour space.
    ColourSpaceMismatch {
        /// Zero-based compositing layer whose space was rejected.
        layer: usize,
        /// Component count of the backdrop space.
        backdrop: u8,
        /// Component count of the resolved group space.
        group: u8,
    },
    /// A compositing plan was built with no layers.
    EmptyCompositingPlan,
    /// A compositing plan contained more layers than the bound allows.
    TooManyLayers {
        /// Layers supplied.
        count: usize,
        /// Maximum accepted layers.
        limit: usize,
    },
}

impl fmt::Display for TransparencyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownBlendMode(token) => write!(f, "unknown blend mode '{token}'"),
            Self::UnknownIsolation(token) => write!(f, "unknown isolation '{token}'"),
            Self::UnknownKnockout(token) => write!(f, "unknown knockout '{token}'"),
            Self::InvalidAlpha { field, value } => {
                write!(f, "alpha /{field} value {value} is outside 0..=256")
            }
            Self::TokenTooLong { field, limit } => {
                write!(f, "{field} token exceeds the {limit} byte limit")
            }
            Self::StackTooDeep { limit } => {
                write!(f, "transparency stack exceeds the {limit} group limit")
            }
            Self::MalformedDescriptor(reason) => write!(f, "malformed descriptor: {reason}"),
            Self::UnsupportedBackend(requirement) => {
                write!(f, "backend does not support {requirement}")
            }
            Self::UnknownColourSpace(token) => write!(f, "unknown colour space '{token}'"),
            Self::InvalidColourSpaceComponents { value } => {
                write!(f, "colour space component count {value} is outside 1..=15")
            }
            Self::UnknownSoftMaskSubtype(token) => {
                write!(f, "unknown soft-mask subtype '{token}'")
            }
            Self::InvalidBackdropComponent { index, value } => {
                write!(
                    f,
                    "soft-mask backdrop component {index} is out of range: {value}"
                )
            }
            Self::TooManyBackdropComponents { count, limit } => {
                write!(
                    f,
                    "soft-mask backdrop has {count} components, above the {limit} limit"
                )
            }
            Self::ColourSpaceMismatch {
                layer,
                backdrop,
                group,
            } => write!(
                f,
                "layer {layer} group space has {group} components but backdrop has {backdrop}"
            ),
            Self::EmptyCompositingPlan => f.write_str("compositing plan has no layers"),
            Self::TooManyLayers { count, limit } => {
                write!(
                    f,
                    "compositing plan has {count} layers, above the {limit} limit"
                )
            }
        }
    }
}

impl std::error::Error for TransparencyError {}

/// The 16 standard ISO 32000 extended blend modes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BlendMode {
    /// Normal (the default).
    Normal,
    /// Multiply.
    Multiply,
    /// Screen.
    Screen,
    /// Overlay.
    Overlay,
    /// Darken.
    Darken,
    /// Lighten.
    Lighten,
    /// Color dodge.
    ColorDodge,
    /// Color burn.
    ColorBurn,
    /// Hard light.
    HardLight,
    /// Soft light.
    SoftLight,
    /// Difference.
    Difference,
    /// Exclusion.
    Exclusion,
    /// Hue (non-separable).
    Hue,
    /// Saturation (non-separable).
    Saturation,
    /// Color (non-separable).
    Color,
    /// Luminosity (non-separable).
    Luminosity,
}

/// Whether a blend mode can be applied per colour component.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BlendCategory {
    /// The mode is separable and can be applied component-by-component.
    Separable,
    /// The mode needs a colour-management transform on the host backend.
    NonSeparable,
}

impl BlendMode {
    /// All canonical mode names, in plan-bit order.
    #[must_use]
    pub const fn all() -> &'static [BlendMode] {
        &[
            Self::Normal,
            Self::Multiply,
            Self::Screen,
            Self::Overlay,
            Self::Darken,
            Self::Lighten,
            Self::ColorDodge,
            Self::ColorBurn,
            Self::HardLight,
            Self::SoftLight,
            Self::Difference,
            Self::Exclusion,
            Self::Hue,
            Self::Saturation,
            Self::Color,
            Self::Luminosity,
        ]
    }

    /// Parse a tolerant name token, ignoring case and an optional leading slash.
    ///
    /// # Errors
    ///
    /// Returns [`TransparencyError::UnknownBlendMode`] for any token that is not
    /// one of the 16 standard names, including over-long tokens.
    pub fn parse(token: &str) -> Result<Self, TransparencyError> {
        check_token("blend mode", token)?;
        let trimmed = token.strip_prefix('/').unwrap_or(token);
        for mode in Self::all() {
            if trimmed.eq_ignore_ascii_case(mode.name()) {
                return Ok(*mode);
            }
        }
        Err(TransparencyError::UnknownBlendMode(token.to_owned()))
    }

    /// Return the canonical, slash-free name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Normal => "Normal",
            Self::Multiply => "Multiply",
            Self::Screen => "Screen",
            Self::Overlay => "Overlay",
            Self::Darken => "Darken",
            Self::Lighten => "Lighten",
            Self::ColorDodge => "ColorDodge",
            Self::ColorBurn => "ColorBurn",
            Self::HardLight => "HardLight",
            Self::SoftLight => "SoftLight",
            Self::Difference => "Difference",
            Self::Exclusion => "Exclusion",
            Self::Hue => "Hue",
            Self::Saturation => "Saturation",
            Self::Color => "Color",
            Self::Luminosity => "Luminosity",
        }
    }

    /// Return the separable/non-separable category.
    #[must_use]
    pub const fn category(self) -> BlendCategory {
        match self {
            Self::Hue | Self::Saturation | Self::Color | Self::Luminosity => {
                BlendCategory::NonSeparable
            }
            _ => BlendCategory::Separable,
        }
    }

    /// Return the stable index used in [`BlendPlan::bits`].
    #[must_use]
    pub const fn index(self) -> u8 {
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
        }
    }

    /// Recover a mode from its stable plan index.
    ///
    /// # Errors
    ///
    /// Returns [`TransparencyError::UnknownBlendMode`] for indices above 15.
    pub fn from_index(index: u8) -> Result<Self, TransparencyError> {
        Self::all()
            .get(usize::from(index))
            .copied()
            .ok_or_else(|| TransparencyError::UnknownBlendMode(index.to_string()))
    }
}

impl fmt::Display for BlendMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// Transparency-group isolation behaviour.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GroupIsolation {
    /// The group is isolated from its backdrop.
    Isolated,
    /// The group blends with its backdrop.
    NonIsolated,
}

impl GroupIsolation {
    /// Parse an isolation token, defaulting to non-isolated when unrecognised.
    ///
    /// # Errors
    ///
    /// Returns [`TransparencyError::TokenTooLong`] for over-long tokens; an
    /// unknown but bounded token is treated as the specification default so a
    /// forward-compatible producer does not block the whole plan.
    pub fn parse(token: &str) -> Result<Self, TransparencyError> {
        check_token("isolation", token)?;
        let trimmed = token.strip_prefix('/').unwrap_or(token);
        Ok(
            if trimmed.eq_ignore_ascii_case("true") || trimmed.eq_ignore_ascii_case("isolated") {
                Self::Isolated
            } else {
                Self::NonIsolated
            },
        )
    }

    /// Return the canonical token.
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::Isolated => "Isolated",
            Self::NonIsolated => "NonIsolated",
        }
    }
}

impl fmt::Display for GroupIsolation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.token())
    }
}

/// Transparency-group knockout behaviour.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GroupKnockout {
    /// Later shapes knock out earlier ones in the group.
    Knockout,
    /// Later shapes accumulate over earlier ones.
    NonKnockout,
}

impl GroupKnockout {
    /// Parse a knockout token, defaulting to non-knockout when unrecognised.
    ///
    /// # Errors
    ///
    /// Returns [`TransparencyError::TokenTooLong`] for over-long tokens.
    pub fn parse(token: &str) -> Result<Self, TransparencyError> {
        check_token("knockout", token)?;
        let trimmed = token.strip_prefix('/').unwrap_or(token);
        Ok(
            if trimmed.eq_ignore_ascii_case("true") || trimmed.eq_ignore_ascii_case("knockout") {
                Self::Knockout
            } else {
                Self::NonKnockout
            },
        )
    }

    /// Return the canonical token.
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::Knockout => "Knockout",
            Self::NonKnockout => "NonKnockout",
        }
    }
}

impl fmt::Display for GroupKnockout {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.token())
    }
}

/// A validated transparency-group description.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TransparencyGroup {
    isolation: GroupIsolation,
    knockout: GroupKnockout,
    mode: BlendMode,
}

impl TransparencyGroup {
    /// Create a group from its three attributes.
    #[must_use]
    pub const fn new(isolation: GroupIsolation, knockout: GroupKnockout, mode: BlendMode) -> Self {
        Self {
            isolation,
            knockout,
            mode,
        }
    }

    /// Return the default group: non-isolated, non-knockout, normal blending.
    #[must_use]
    pub const fn default_group() -> Self {
        Self::new(
            GroupIsolation::NonIsolated,
            GroupKnockout::NonKnockout,
            BlendMode::Normal,
        )
    }

    /// Return the isolation mode.
    #[must_use]
    pub const fn isolation(self) -> GroupIsolation {
        self.isolation
    }

    /// Return the knockout mode.
    #[must_use]
    pub const fn knockout(self) -> GroupKnockout {
        self.knockout
    }

    /// Return the group blend mode.
    #[must_use]
    pub const fn mode(self) -> BlendMode {
        self.mode
    }

    /// Serialise the group to a canonical token.
    #[must_use]
    pub fn to_token(self) -> String {
        format!(
            "{}|{}|{}",
            self.isolation.token(),
            self.knockout.token(),
            self.mode.name()
        )
    }

    /// Parse a canonical group token.
    ///
    /// # Errors
    ///
    /// Returns a typed error for a wrong field count, an over-long token or an
    /// unknown blend mode.
    pub fn from_token(token: &str) -> Result<Self, TransparencyError> {
        check_token("group", token)?;
        let parts: Vec<&str> = token.split('|').collect();
        if parts.len() != 3 {
            return Err(TransparencyError::MalformedDescriptor(
                "a group token needs isolation|knockout|mode".to_owned(),
            ));
        }
        Ok(Self::new(
            GroupIsolation::parse(parts[0])?,
            GroupKnockout::parse(parts[1])?,
            BlendMode::parse(parts[2])?,
        ))
    }
}

impl Default for TransparencyGroup {
    fn default() -> Self {
        Self::default_group()
    }
}

/// A validated blend operation with optional group attributes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlendSpec {
    mode: BlendMode,
    source_alpha: u16,
    backdrop_alpha: u16,
    group: Option<TransparencyGroup>,
}

impl BlendSpec {
    /// Create a spec and validate its alpha bounds.
    ///
    /// # Errors
    ///
    /// Returns [`TransparencyError::InvalidAlpha`] when either alpha exceeds
    /// [`MAX_ALPHA`].
    pub fn new(
        mode: BlendMode,
        source_alpha: u16,
        backdrop_alpha: u16,
    ) -> Result<Self, TransparencyError> {
        let spec = Self {
            mode,
            source_alpha,
            backdrop_alpha,
            group: None,
        };
        spec.validate()?;
        Ok(spec)
    }

    /// Attach a transparency group.
    #[must_use]
    pub const fn with_group(mut self, group: TransparencyGroup) -> Self {
        self.group = Some(group);
        self
    }

    /// Return the blend mode.
    #[must_use]
    pub const fn mode(self) -> BlendMode {
        self.mode
    }

    /// Return the source alpha step count.
    #[must_use]
    pub const fn source_alpha(self) -> u16 {
        self.source_alpha
    }

    /// Return the backdrop alpha step count.
    #[must_use]
    pub const fn backdrop_alpha(self) -> u16 {
        self.backdrop_alpha
    }

    /// Return the attached group, if any.
    #[must_use]
    pub const fn group(self) -> Option<TransparencyGroup> {
        self.group
    }

    /// Re-check alpha and token bounds before planning.
    ///
    /// # Errors
    ///
    /// Returns a typed error for out-of-range alpha or an over-long group token.
    pub fn validate(&self) -> Result<(), TransparencyError> {
        if self.source_alpha > MAX_ALPHA {
            return Err(TransparencyError::InvalidAlpha {
                field: "ca",
                value: self.source_alpha,
            });
        }
        if self.backdrop_alpha > MAX_ALPHA {
            return Err(TransparencyError::InvalidAlpha {
                field: "CA",
                value: self.backdrop_alpha,
            });
        }
        if let Some(group) = self.group {
            check_token("group", &group.to_token())?;
        }
        Ok(())
    }

    /// Serialise to a canonical descriptor for round-trip callers.
    #[must_use]
    pub fn to_descriptor(self) -> String {
        let mut text = format!(
            "{} ca={} CA={}",
            self.mode.name(),
            self.source_alpha,
            self.backdrop_alpha
        );
        if let Some(group) = self.group {
            text.push_str(" group=");
            text.push_str(&group.to_token());
        }
        text
    }

    /// Parse a canonical descriptor produced by [`Self::to_descriptor`].
    ///
    /// # Errors
    ///
    /// Returns a typed error for a malformed descriptor, over-long field,
    /// unknown mode or out-of-range alpha.
    pub fn from_descriptor(text: &str) -> Result<Self, TransparencyError> {
        check_token("descriptor", text)?;
        let fields: Vec<&str> = text.split_whitespace().collect();
        if fields.is_empty() || fields.len() > 4 {
            return Err(TransparencyError::MalformedDescriptor(
                "expected 1 to 4 whitespace-separated fields".to_owned(),
            ));
        }
        let mode = BlendMode::parse(fields[0])?;
        let mut source_alpha = MAX_ALPHA;
        let mut backdrop_alpha = MAX_ALPHA;
        let mut group = None;
        for field in &fields[1..] {
            let (key, value) = field.split_once('=').ok_or_else(|| {
                TransparencyError::MalformedDescriptor(format!("field '{field}' has no '='"))
            })?;
            match key {
                "ca" => source_alpha = parse_alpha(value, "ca")?,
                "CA" => backdrop_alpha = parse_alpha(value, "CA")?,
                "group" => group = Some(TransparencyGroup::from_token(value)?),
                _ => {
                    return Err(TransparencyError::MalformedDescriptor(format!(
                        "unknown field '{key}'"
                    )));
                }
            }
        }
        Self::new(mode, source_alpha, backdrop_alpha).map(|spec| match group {
            Some(group) => spec.with_group(group),
            None => spec,
        })
    }

    /// Return whether this spec needs partial-alpha support from a backend.
    #[must_use]
    pub const fn needs_partial_alpha(self) -> bool {
        self.source_alpha < MAX_ALPHA || self.backdrop_alpha < MAX_ALPHA
    }
}

fn parse_alpha(value: &str, field: &'static str) -> Result<u16, TransparencyError> {
    check_token(field, value)?;
    value
        .parse::<u16>()
        .map_err(|_| TransparencyError::MalformedDescriptor(format!("{field} is not a number")))
}

fn check_token(field: &'static str, token: &str) -> Result<(), TransparencyError> {
    if token.len() > MAX_TOKEN_LEN {
        return Err(TransparencyError::TokenTooLong {
            field,
            limit: MAX_TOKEN_LEN,
        });
    }
    Ok(())
}

/// A compositing feature the caller's backend must provide.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BackendRequirement {
    /// The exact blend mode must be supported.
    Mode(BlendMode),
    /// A colour-management transform is needed for a non-separable mode.
    ColorManagement,
    /// Source or backdrop alpha is partially transparent.
    PartialAlpha,
    /// The group is isolated.
    Isolation,
    /// The group knocks out earlier shapes.
    Knockout,
}

impl fmt::Display for BackendRequirement {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Mode(mode) => write!(f, "blend mode {mode}"),
            Self::ColorManagement => f.write_str("colour-management transform"),
            Self::PartialAlpha => f.write_str("partial alpha compositing"),
            Self::Isolation => f.write_str("group isolation"),
            Self::Knockout => f.write_str("group knockout"),
        }
    }
}

/// Caller-supplied description of what a renderer can actually composite.
///
/// Implement this trait in the render crate (or a test double) and pass it to
/// [`BlendPlan::build`]. This crate never guesses another crate's capabilities.
pub trait BlendBackend {
    /// Return `true` when the exact mode can be composited.
    fn supports_mode(&self, mode: BlendMode) -> bool;
    /// Return `true` when the named generic feature is available.
    fn supports(&self, requirement: BackendRequirement) -> bool;
}

/// An honest backend that supports nothing; useful as the default case.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NoBackend;

impl BlendBackend for NoBackend {
    fn supports_mode(&self, _mode: BlendMode) -> bool {
        false
    }

    fn supports(&self, _requirement: BackendRequirement) -> bool {
        false
    }
}

/// A deterministic, validated blend plan for one operation.
///
/// The plan is pure with respect to its spec: repeated builds produce the same
/// bit encoding, requirement order and warning order. A backend that cannot
/// honour a requirement is recorded in [`Self::blocked`] and surfaced by
/// [`Self::check_support`] as a typed error; no requirement is silently ignored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlendPlan {
    spec: BlendSpec,
    bits: u64,
    requirements: Vec<BackendRequirement>,
    blocked: Vec<BackendRequirement>,
    warnings: Vec<String>,
}

impl BlendPlan {
    /// Build a plan from a spec and caller-supplied backend capabilities.
    ///
    /// # Errors
    ///
    /// Returns a typed [`TransparencyError`] when the spec is invalid. Missing
    /// backend features are recorded, not rejected here; call
    /// [`Self::check_support`] before attempting a composite.
    pub fn build<B: BlendBackend>(spec: BlendSpec, backend: &B) -> Result<Self, TransparencyError> {
        spec.validate()?;
        let mut requirements = vec![BackendRequirement::Mode(spec.mode())];
        if spec.mode().category() == BlendCategory::NonSeparable {
            requirements.push(BackendRequirement::ColorManagement);
        }
        if spec.needs_partial_alpha() {
            requirements.push(BackendRequirement::PartialAlpha);
        }
        if let Some(group) = spec.group() {
            if group.isolation() == GroupIsolation::Isolated {
                requirements.push(BackendRequirement::Isolation);
            }
            if group.knockout() == GroupKnockout::Knockout {
                requirements.push(BackendRequirement::Knockout);
            }
        }
        let mut blocked = Vec::new();
        let mut warnings = Vec::new();
        for &requirement in &requirements {
            let supported = match requirement {
                BackendRequirement::Mode(mode) => backend.supports_mode(mode),
                other => backend.supports(other),
            };
            if !supported {
                blocked.push(requirement);
                warnings.push(format!("backend does not support {requirement}"));
            }
        }

        let mut bits = u64::from(spec.mode().index());
        bits |= u64::from(spec.source_alpha()) << 4;
        bits |= u64::from(spec.backdrop_alpha()) << 13;
        if let Some(group) = spec.group() {
            bits |= 1 << 22;
            if group.isolation() == GroupIsolation::Isolated {
                bits |= 1 << 23;
            }
            if group.knockout() == GroupKnockout::Knockout {
                bits |= 1 << 24;
            }
        }
        Ok(Self {
            spec,
            bits,
            requirements,
            blocked,
            warnings,
        })
    }

    /// Reject a plan whose backend requirements are not all satisfied.
    ///
    /// # Errors
    ///
    /// Returns [`TransparencyError::UnsupportedBackend`] naming the first
    /// unsupported feature in deterministic requirement order.
    pub fn check_support(&self) -> Result<(), TransparencyError> {
        match self.blocked.first() {
            Some(&requirement) => Err(TransparencyError::UnsupportedBackend(requirement)),
            None => Ok(()),
        }
    }

    /// Return the validated source spec.
    #[must_use]
    pub const fn spec(&self) -> BlendSpec {
        self.spec
    }

    /// Return the stable bit encoding.
    ///
    /// Mode occupies bits `0..4`, source alpha bits `4..13` and backdrop alpha
    /// bits `13..22`. The alpha fields hold the raw `0..=256` step counts, so a
    /// fully opaque alpha ([`MAX_ALPHA`]) encodes as `256`, not `zero`. Group
    /// flags occupy bits 22 (group present), 23 (isolated) and 24 (knockout).
    #[must_use]
    pub const fn bits(&self) -> u64 {
        self.bits
    }

    /// Return all requirements in deterministic order.
    #[must_use]
    pub fn requirements(&self) -> &[BackendRequirement] {
        &self.requirements
    }

    /// Return the requirements the backend cannot satisfy.
    #[must_use]
    pub fn blocked(&self) -> &[BackendRequirement] {
        &self.blocked
    }

    /// Return stable, human-readable warnings for blocked requirements.
    #[must_use]
    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }
}

/// A bounded stack of nested transparency operations.
///
/// Plans are produced in insertion order, and the stack never grows past
/// [`MAX_STACK_DEPTH`] groups. This bounds work for hostile content streams
/// that open deeply nested transparency groups.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TransparencyStack {
    specs: Vec<BlendSpec>,
}

impl TransparencyStack {
    /// Create an empty stack.
    #[must_use]
    pub fn new() -> Self {
        Self { specs: Vec::new() }
    }

    /// Push a spec, rejecting stacks deeper than [`MAX_STACK_DEPTH`].
    ///
    /// # Errors
    ///
    /// Returns [`TransparencyError::StackTooDeep`] at the bound and any spec
    /// validation error.
    pub fn push(&mut self, spec: BlendSpec) -> Result<(), TransparencyError> {
        if self.specs.len() >= usize::from(MAX_STACK_DEPTH) {
            return Err(TransparencyError::StackTooDeep {
                limit: MAX_STACK_DEPTH,
            });
        }
        spec.validate()?;
        self.specs.push(spec);
        Ok(())
    }

    /// Return the number of stacked operations.
    #[must_use]
    pub fn len(&self) -> usize {
        self.specs.len()
    }

    /// Return `true` when no operations are stacked.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.specs.is_empty()
    }

    /// Return the spec at `index`.
    #[must_use]
    pub fn get(&self, index: usize) -> Option<BlendSpec> {
        self.specs.get(index).copied()
    }

    /// Validate every stacked spec.
    ///
    /// # Errors
    ///
    /// Returns the first spec error in stack order.
    pub fn validate(&self) -> Result<(), TransparencyError> {
        self.specs.iter().try_for_each(BlendSpec::validate)
    }

    /// Build one deterministic plan per stacked spec.
    ///
    /// # Errors
    ///
    /// Returns a typed error if any spec is invalid. Backend gaps remain
    /// recorded in each plan rather than aborting the build.
    pub fn plans<B: BlendBackend>(&self, backend: &B) -> Result<Vec<BlendPlan>, TransparencyError> {
        self.validate()?;
        self.specs
            .iter()
            .copied()
            .map(|spec| BlendPlan::build(spec, backend))
            .collect()
    }
}

// ---------------------------------------------------------------------------
// STD-005 closure: colour spaces, soft masks and an ordered compositing plan.
// ---------------------------------------------------------------------------

/// Maximum number of components a soft-mask backdrop colour may hold.
pub const MAX_BACKDROP_COMPONENTS: usize = 4;
/// Maximum number of layers in one compositing plan.
pub const MAX_COMPOSITING_LAYERS: usize = 64;
/// Highest component count accepted for an ICC-based group colour space.
pub const MAX_COLOUR_COMPONENTS: u8 = 15;

/// The colour space in which a transparency group composites.
///
/// This resolves the group `/CS` entry of ISO 32000. Device spaces carry a
/// fixed component count; an ICC-based space is identified by a bounded name
/// and its explicit component count. Pixel conversion between spaces belongs
/// to the render backend, not to this crate.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum GroupColorSpace {
    /// The `/DeviceGray` space (one component).
    DeviceGray,
    /// The `/DeviceRGB` space (three components).
    DeviceRgb,
    /// The `/DeviceCMYK` space (four components).
    DeviceCmyk,
    /// An ICC-based space with a bounded name and explicit component count.
    IccBased {
        /// Bounded name identifying the ICC profile.
        name: String,
        /// Number of colour components declared by the profile.
        components: u8,
    },
}

impl GroupColorSpace {
    /// Parse a colour-space token, ignoring case and an optional leading slash.
    ///
    /// Accepted forms are `DeviceGray`, `DeviceRGB`, `DeviceCMYK`, and
    /// `ICCBased:<components>:<name>` where the component count is in
    /// `1..=15` and the name is bounded by [`MAX_TOKEN_LEN`].
    ///
    /// # Errors
    ///
    /// Returns [`TransparencyError::UnknownColourSpace`] for an unrecognised
    /// token, [`TransparencyError::InvalidColourSpaceComponents`] for an
    /// out-of-range component count, and [`TransparencyError::TokenTooLong`]
    /// for an over-long token or name.
    pub fn parse(token: &str) -> Result<Self, TransparencyError> {
        check_token("colour space", token)?;
        let trimmed = token.strip_prefix('/').unwrap_or(token);
        if trimmed.eq_ignore_ascii_case("DeviceGray") || trimmed.eq_ignore_ascii_case("G") {
            return Ok(Self::DeviceGray);
        }
        if trimmed.eq_ignore_ascii_case("DeviceRGB") {
            return Ok(Self::DeviceRgb);
        }
        if trimmed.eq_ignore_ascii_case("DeviceCMYK") {
            return Ok(Self::DeviceCmyk);
        }
        if let Some(rest) = trimmed.strip_prefix("ICCBased:") {
            let mut parts = rest.splitn(2, ':');
            let count = parts.next().unwrap_or("");
            let name = parts.next().unwrap_or("");
            if let Ok(components) = count.parse::<u8>() {
                if components == 0 || components > MAX_COLOUR_COMPONENTS {
                    return Err(TransparencyError::InvalidColourSpaceComponents {
                        value: components,
                    });
                }
                if name.is_empty() {
                    return Err(TransparencyError::UnknownColourSpace(token.to_owned()));
                }
                check_token("colour space name", name)?;
                return Ok(Self::IccBased {
                    name: name.to_owned(),
                    components,
                });
            }
        }
        Err(TransparencyError::UnknownColourSpace(token.to_owned()))
    }

    /// Return the number of colour components in this space.
    #[must_use]
    pub fn component_count(&self) -> u8 {
        match self {
            Self::DeviceGray => 1,
            Self::DeviceRgb => 3,
            Self::DeviceCmyk => 4,
            Self::IccBased { components, .. } => *components,
        }
    }

    /// Serialise to the canonical token accepted by [`Self::parse`].
    #[must_use]
    pub fn to_token(&self) -> String {
        match self {
            Self::DeviceGray => "DeviceGray".to_owned(),
            Self::DeviceRgb => "DeviceRGB".to_owned(),
            Self::DeviceCmyk => "DeviceCMYK".to_owned(),
            Self::IccBased { name, components } => format!("ICCBased:{components}:{name}"),
        }
    }
}

impl fmt::Display for GroupColorSpace {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_token())
    }
}

/// Resolve a layer's group colour space against its backdrop space.
///
/// A layer without an explicit `/CS` inherits the backdrop space. When a group
/// space is present its component count must equal the backdrop's, otherwise
/// the layer cannot composite and a typed [`TransparencyError::ColourSpaceMismatch`]
/// is returned. No colour conversion is performed here.
///
/// # Errors
///
/// Returns a typed colour-space mismatch when the component counts differ.
pub fn resolve_group_colour_space(
    group: Option<GroupColorSpace>,
    backdrop: GroupColorSpace,
    layer: usize,
) -> Result<GroupColorSpace, TransparencyError> {
    match group {
        None => Ok(backdrop),
        Some(space) => {
            if space.component_count() != backdrop.component_count() {
                Err(TransparencyError::ColourSpaceMismatch {
                    layer,
                    backdrop: backdrop.component_count(),
                    group: space.component_count(),
                })
            } else {
                Ok(space)
            }
        }
    }
}

/// The subtype of an ISO 32000 soft mask (`/SMask`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SoftMaskSubtype {
    /// Alpha soft mask: the mask alpha is taken directly.
    Alpha,
    /// Luminosity soft mask: the mask luminance becomes alpha.
    Luminosity,
}

impl SoftMaskSubtype {
    /// Parse a soft-mask subtype token, ignoring case and a leading slash.
    ///
    /// # Errors
    ///
    /// Returns [`TransparencyError::UnknownSoftMaskSubtype`] for an unrecognised
    /// token and [`TransparencyError::TokenTooLong`] for an over-long token.
    pub fn parse(token: &str) -> Result<Self, TransparencyError> {
        check_token("soft-mask subtype", token)?;
        let trimmed = token.strip_prefix('/').unwrap_or(token);
        if trimmed.eq_ignore_ascii_case("Alpha") {
            Ok(Self::Alpha)
        } else if trimmed.eq_ignore_ascii_case("Luminosity") {
            Ok(Self::Luminosity)
        } else {
            Err(TransparencyError::UnknownSoftMaskSubtype(token.to_owned()))
        }
    }

    /// Return the canonical token.
    #[must_use]
    pub const fn token(self) -> &'static str {
        match self {
            Self::Alpha => "Alpha",
            Self::Luminosity => "Luminosity",
        }
    }

    /// Return a stable one-bit encoding used in compositing-plan summaries.
    #[must_use]
    pub const fn bits(self) -> u8 {
        match self {
            Self::Alpha => 0,
            Self::Luminosity => 1,
        }
    }
}

impl fmt::Display for SoftMaskSubtype {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.token())
    }
}

/// A validated soft-mask descriptor for one compositing layer.
///
/// The backdrop colour (`/BC`) is bounded to [`MAX_BACKDROP_COMPONENTS`]
/// components, each finite and in `0.0..=1.0`. An optional transparency group
/// describes the mask's own compositing behaviour. Applying the mask to pixels
/// is a render-backend operation.
#[derive(Debug, Clone, PartialEq)]
pub struct SoftMask {
    subtype: SoftMaskSubtype,
    backdrop: Vec<f32>,
    group: Option<TransparencyGroup>,
}

impl SoftMask {
    /// Create a soft mask with a subtype and backdrop colour.
    ///
    /// # Errors
    ///
    /// Returns a typed error when the backdrop has too many components or a
    /// component is non-finite or outside `0.0..=1.0`.
    pub fn new(subtype: SoftMaskSubtype, backdrop: Vec<f32>) -> Result<Self, TransparencyError> {
        let mask = Self {
            subtype,
            backdrop,
            group: None,
        };
        mask.validate()?;
        Ok(mask)
    }

    /// Attach a transparency group that describes the mask's compositing.
    #[must_use]
    pub const fn with_group(mut self, group: TransparencyGroup) -> Self {
        self.group = Some(group);
        self
    }

    /// Return the soft-mask subtype.
    #[must_use]
    pub const fn subtype(&self) -> SoftMaskSubtype {
        self.subtype
    }

    /// Return the backdrop colour components.
    #[must_use]
    pub fn backdrop(&self) -> &[f32] {
        &self.backdrop
    }

    /// Return the attached transparency group, if any.
    #[must_use]
    pub const fn group(&self) -> Option<TransparencyGroup> {
        self.group
    }

    /// Re-check bounds and component ranges.
    ///
    /// # Errors
    ///
    /// Returns a typed [`TransparencyError`] for an out-of-range backdrop.
    pub fn validate(&self) -> Result<(), TransparencyError> {
        if self.backdrop.len() > MAX_BACKDROP_COMPONENTS {
            return Err(TransparencyError::TooManyBackdropComponents {
                count: self.backdrop.len(),
                limit: MAX_BACKDROP_COMPONENTS,
            });
        }
        for (index, &value) in self.backdrop.iter().enumerate() {
            if !value.is_finite() || !(0.0..=1.0).contains(&value) {
                return Err(TransparencyError::InvalidBackdropComponent { index, value });
            }
        }
        Ok(())
    }

    /// Serialise to a canonical descriptor for round-trip callers.
    #[must_use]
    pub fn to_descriptor(&self) -> String {
        let components = self
            .backdrop
            .iter()
            .map(|value| value.to_string())
            .collect::<Vec<_>>()
            .join(",");
        let group = match self.group {
            Some(group) => group.to_token(),
            None => "-".to_owned(),
        };
        format!("SMask:{}:[{components}]:{group}", self.subtype.token())
    }

    /// Parse a canonical descriptor produced by [`Self::to_descriptor`].
    ///
    /// # Errors
    ///
    /// Returns a typed [`TransparencyError`] for a malformed descriptor, an
    /// unknown subtype, an out-of-range backdrop or an invalid group token.
    pub fn from_descriptor(text: &str) -> Result<Self, TransparencyError> {
        check_token("soft mask", text)?;
        let fields: Vec<&str> = text.split(':').collect();
        if fields.len() != 4 || fields[0] != "SMask" {
            return Err(TransparencyError::MalformedDescriptor(
                "expected SMask:<subtype>:[components]:<group>".to_owned(),
            ));
        }
        let subtype = SoftMaskSubtype::parse(fields[1])?;
        let inner = fields[2]
            .strip_prefix('[')
            .and_then(|value| value.strip_suffix(']'))
            .ok_or_else(|| {
                TransparencyError::MalformedDescriptor("backdrop must be bracketed".to_owned())
            })?;
        let mut backdrop = Vec::new();
        if !inner.is_empty() {
            for value in inner.split(',') {
                let parsed = value.parse::<f32>().map_err(|_| {
                    TransparencyError::MalformedDescriptor(
                        "backdrop component is not a number".to_owned(),
                    )
                })?;
                backdrop.push(parsed);
            }
        }
        let group = if fields[3] == "-" {
            None
        } else {
            Some(TransparencyGroup::from_token(fields[3])?)
        };
        Self::new(subtype, backdrop).map(|mask| match group {
            Some(group) => mask.with_group(group),
            None => mask,
        })
    }
}

/// One layer to be composited, with optional soft mask and group colour space.
///
/// Layers are supplied to [`CompositingPlan::build`] in painting order: the
/// backmost layer first, the frontmost layer last. That is exactly the order a
/// render backend executes source-over compositing.
#[derive(Debug, Clone, PartialEq)]
pub struct BlendLayer {
    spec: BlendSpec,
    soft_mask: Option<SoftMask>,
    group_space: Option<GroupColorSpace>,
}

impl BlendLayer {
    /// Create a layer from a validated blend spec with no mask or explicit space.
    #[must_use]
    pub const fn new(spec: BlendSpec) -> Self {
        Self {
            spec,
            soft_mask: None,
            group_space: None,
        }
    }

    /// Attach a soft mask to this layer.
    #[must_use]
    pub fn with_soft_mask(mut self, mask: SoftMask) -> Self {
        self.soft_mask = Some(mask);
        self
    }

    /// Attach an explicit group colour space (`/CS`) to this layer.
    #[must_use]
    pub fn with_group_space(mut self, space: GroupColorSpace) -> Self {
        self.group_space = Some(space);
        self
    }

    /// Return the layer's blend spec.
    #[must_use]
    pub const fn spec(&self) -> BlendSpec {
        self.spec
    }

    /// Return the attached soft mask, if any.
    #[must_use]
    pub fn soft_mask(&self) -> Option<&SoftMask> {
        self.soft_mask.as_ref()
    }

    /// Return the explicit group colour space, if any.
    #[must_use]
    pub fn group_space(&self) -> Option<&GroupColorSpace> {
        self.group_space.as_ref()
    }
}

/// One executed step of a [`CompositingPlan`], in back-to-front order.
#[derive(Debug, Clone, PartialEq)]
pub struct CompositingStep {
    index: usize,
    spec: BlendSpec,
    soft_mask: Option<SoftMask>,
    resolved_space: GroupColorSpace,
    plan: BlendPlan,
}

impl CompositingStep {
    /// Zero-based position of this step in painting order (zero is backmost).
    #[must_use]
    pub const fn index(&self) -> usize {
        self.index
    }

    /// Return the validated blend spec for this step.
    #[must_use]
    pub const fn spec(&self) -> BlendSpec {
        self.spec
    }

    /// Return the soft mask applied at this step, if any.
    #[must_use]
    pub fn soft_mask(&self) -> Option<&SoftMask> {
        self.soft_mask.as_ref()
    }

    /// Return the colour space resolved for this step's transparency group.
    #[must_use]
    pub fn resolved_space(&self) -> &GroupColorSpace {
        &self.resolved_space
    }

    /// Return the underlying deterministic blend plan.
    #[must_use]
    pub const fn plan(&self) -> &BlendPlan {
        &self.plan
    }
}

/// A deterministic, back-to-front compositing plan a render backend can execute.
///
/// Steps are stored in painting order (index 0 is the backmost layer). Both the
/// encoded [`Self::bits`] and the human-readable [`Self::summary`] are pure
/// functions of the layers, backdrop space and backend, so repeated builds are
/// byte-identical. Missing backend features are recorded in each step's
/// [`BlendPlan`] and surfaced by [`Self::check_support`]; no pixel is blended.
#[derive(Debug, Clone, PartialEq)]
pub struct CompositingPlan {
    backdrop_space: GroupColorSpace,
    steps: Vec<CompositingStep>,
    bits: u64,
}

impl CompositingPlan {
    /// Build an ordered compositing plan from layers in painting order.
    ///
    /// # Errors
    ///
    /// Returns [`TransparencyError::EmptyCompositingPlan`] for no layers, a
    /// typed limit error for more than [`MAX_COMPOSITING_LAYERS`] layers, and
    /// any spec, mask, colour-space or backend-planning error. Backend gaps
    /// remain recorded in the steps rather than aborting the build.
    pub fn build<B: BlendBackend>(
        layers: &[BlendLayer],
        backdrop_space: GroupColorSpace,
        backend: &B,
    ) -> Result<Self, TransparencyError> {
        if layers.is_empty() {
            return Err(TransparencyError::EmptyCompositingPlan);
        }
        if layers.len() > MAX_COMPOSITING_LAYERS {
            return Err(TransparencyError::TooManyLayers {
                count: layers.len(),
                limit: MAX_COMPOSITING_LAYERS,
            });
        }
        let mut steps = Vec::with_capacity(layers.len());
        let mut bits = 0xcbf2_9ce4_8422_2325u64;
        const PRIME: u64 = 0x1000_0000_01b3;
        for (index, layer) in layers.iter().enumerate() {
            layer.spec.validate()?;
            if let Some(mask) = &layer.soft_mask {
                mask.validate()?;
            }
            let resolved = resolve_group_colour_space(
                layer.group_space.clone(),
                backdrop_space.clone(),
                index,
            )?;
            let plan = BlendPlan::build(layer.spec, backend)?;
            bits = (bits ^ plan.bits()).wrapping_mul(PRIME);
            bits = (bits ^ u64::from(resolved.component_count())).wrapping_mul(PRIME);
            if let Some(mask) = &layer.soft_mask {
                bits = (bits ^ u64::from(mask.subtype().bits())).wrapping_mul(PRIME);
            }
            steps.push(CompositingStep {
                index,
                spec: layer.spec,
                soft_mask: layer.soft_mask.clone(),
                resolved_space: resolved,
                plan,
            });
        }
        Ok(Self {
            backdrop_space,
            steps,
            bits,
        })
    }

    /// Return the steps in back-to-front painting order.
    #[must_use]
    pub fn steps(&self) -> &[CompositingStep] {
        &self.steps
    }

    /// Return the number of compositing steps.
    #[must_use]
    pub fn len(&self) -> usize {
        self.steps.len()
    }

    /// Return `true` when the plan has no steps (only possible for a default).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.steps.is_empty()
    }

    /// Return the backdrop colour space this plan composites onto.
    #[must_use]
    pub fn backdrop_space(&self) -> &GroupColorSpace {
        &self.backdrop_space
    }

    /// Return the stable bit encoding of the whole plan.
    #[must_use]
    pub const fn bits(&self) -> u64 {
        self.bits
    }

    /// Return a deterministic, human-readable back-to-front summary.
    #[must_use]
    pub fn summary(&self) -> String {
        let mut out = format!(
            "compositing[{}] back-to-front onto {}",
            self.steps.len(),
            self.backdrop_space.to_token()
        );
        for step in &self.steps {
            out.push_str(&format!(
                " | {}:{}/{}",
                step.index,
                step.spec.mode().name(),
                step.resolved_space.to_token()
            ));
        }
        out
    }

    /// Reject the plan when any step's backend requirements are unmet.
    ///
    /// # Errors
    ///
    /// Returns the first [`TransparencyError::UnsupportedBackend`] in step and
    /// requirement order.
    pub fn check_support(&self) -> Result<(), TransparencyError> {
        for step in &self.steps {
            step.plan.check_support()?;
        }
        Ok(())
    }

    /// Return all backend requirements that are blocked, in deterministic order.
    #[must_use]
    pub fn blocked_requirements(&self) -> Vec<BackendRequirement> {
        let mut out = Vec::new();
        for step in &self.steps {
            for &requirement in step.plan.blocked() {
                if !out.contains(&requirement) {
                    out.push(requirement);
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Clone, Copy)]
    struct FullBackend;

    impl BlendBackend for FullBackend {
        fn supports_mode(&self, _mode: BlendMode) -> bool {
            true
        }

        fn supports(&self, _requirement: BackendRequirement) -> bool {
            true
        }
    }

    /// A backend that can composite every mode but lacks a colour-management
    /// transform, so non-separable modes cannot be honoured. It isolates the
    /// `/ColorManagement` requirement from the mode requirement.
    #[derive(Debug, Clone, Copy)]
    struct NoCmmBackend;

    impl BlendBackend for NoCmmBackend {
        fn supports_mode(&self, _mode: BlendMode) -> bool {
            true
        }

        fn supports(&self, requirement: BackendRequirement) -> bool {
            requirement != BackendRequirement::ColorManagement
        }
    }

    #[test]
    fn every_standard_mode_round_trips_name_and_category() {
        assert_eq!(BlendMode::all().len(), 16);
        for (index, &mode) in BlendMode::all().iter().enumerate() {
            assert_eq!(mode.index(), index as u8);
            assert_eq!(BlendMode::from_index(mode.index()).expect("index"), mode);
            assert_eq!(BlendMode::parse(mode.name()).expect("canonical name"), mode);
            assert_eq!(
                BlendMode::parse(&format!("/{}", mode.name().to_ascii_lowercase()))
                    .expect("tolerant name"),
                mode
            );
            assert_eq!(mode.to_string(), mode.name());
        }
        assert_eq!(BlendMode::Hue.category(), BlendCategory::NonSeparable);
        assert_eq!(
            BlendMode::Luminosity.category(),
            BlendCategory::NonSeparable
        );
        assert_eq!(
            BlendMode::parse("not-a-mode").expect_err("unknown"),
            TransparencyError::UnknownBlendMode("not-a-mode".to_owned())
        );
    }

    #[test]
    fn group_tokens_round_trip_and_default_to_spec_values() {
        let group = TransparencyGroup::new(
            GroupIsolation::Isolated,
            GroupKnockout::Knockout,
            BlendMode::Multiply,
        );
        assert_eq!(
            TransparencyGroup::from_token(&group.to_token()).expect("round trip"),
            group
        );
        assert_eq!(
            GroupIsolation::parse("false").expect("parse"),
            GroupIsolation::NonIsolated
        );
        assert_eq!(
            GroupKnockout::parse("/true").expect("parse"),
            GroupKnockout::Knockout
        );
        assert_eq!(
            TransparencyGroup::default(),
            TransparencyGroup::default_group()
        );
        assert!(
            TransparencyGroup::from_token("only|two")
                .is_err_and(|error| matches!(error, TransparencyError::MalformedDescriptor(_)))
        );
    }

    #[test]
    fn descriptor_round_trips_with_and_without_a_group() {
        let group = TransparencyGroup::new(
            GroupIsolation::Isolated,
            GroupKnockout::NonKnockout,
            BlendMode::Overlay,
        );
        for spec in [
            BlendSpec::new(BlendMode::Multiply, 128, 256).expect("plain spec"),
            BlendSpec::new(BlendMode::Overlay, 0, 96)
                .expect("grouped spec")
                .with_group(group),
        ] {
            let text = spec.to_descriptor();
            assert_eq!(BlendSpec::from_descriptor(&text).expect("round trip"), spec);
        }
    }

    #[test]
    fn alpha_boundaries_are_valid_and_overflow_is_typed() {
        assert!(BlendSpec::new(BlendMode::Normal, 0, MAX_ALPHA).is_ok());
        assert!(BlendSpec::new(BlendMode::Normal, MAX_ALPHA, 0).is_ok());
        assert_eq!(
            BlendSpec::new(BlendMode::Normal, MAX_ALPHA + 1, 0).expect_err("source overflow"),
            TransparencyError::InvalidAlpha {
                field: "ca",
                value: 257
            }
        );
        assert_eq!(
            BlendSpec::new(BlendMode::Normal, 0, MAX_ALPHA + 1).expect_err("backdrop overflow"),
            TransparencyError::InvalidAlpha {
                field: "CA",
                value: 257
            }
        );
    }

    #[test]
    fn over_long_tokens_are_rejected_with_a_typed_error() {
        let long = "x".repeat(MAX_TOKEN_LEN + 1);
        assert_eq!(
            BlendMode::parse(&long).expect_err("long mode"),
            TransparencyError::TokenTooLong {
                field: "blend mode",
                limit: MAX_TOKEN_LEN
            }
        );
        assert!(
            BlendSpec::from_descriptor(&long)
                .is_err_and(|error| matches!(error, TransparencyError::TokenTooLong { .. }))
        );
    }

    #[test]
    fn plan_is_deterministic_for_a_spec_and_backend() {
        let group = TransparencyGroup::new(
            GroupIsolation::Isolated,
            GroupKnockout::Knockout,
            BlendMode::Screen,
        );
        let spec = BlendSpec::new(BlendMode::Screen, 64, 256)
            .expect("spec")
            .with_group(group);
        let first = BlendPlan::build(spec, &FullBackend).expect("first plan");
        let second = BlendPlan::build(spec, &FullBackend).expect("second plan");
        assert_eq!(first, second);
        assert!(first.check_support().is_ok());
        assert!(first.warnings().is_empty());
    }

    #[test]
    fn plan_bits_encode_mode_alpha_and_group_flags() {
        let plain = BlendPlan::build(
            BlendSpec::new(BlendMode::Normal, MAX_ALPHA, MAX_ALPHA).expect("plain"),
            &FullBackend,
        )
        .expect("plain plan");
        // Alpha fields store the raw 0..=256 step count, so opaque (256) is not
        // zero; check each field explicitly rather than assuming a zero word.
        assert_eq!(plain.bits() & 0xf, 0);
        assert_eq!((plain.bits() >> 4) & 0x1ff, u64::from(MAX_ALPHA));
        assert_eq!((plain.bits() >> 13) & 0x1ff, u64::from(MAX_ALPHA));
        let group = TransparencyGroup::new(
            GroupIsolation::Isolated,
            GroupKnockout::Knockout,
            BlendMode::Luminosity,
        );
        let complex = BlendPlan::build(
            BlendSpec::new(BlendMode::Luminosity, 1, 2)
                .expect("complex")
                .with_group(group),
            &FullBackend,
        )
        .expect("complex plan");
        assert_eq!(complex.bits() & 0xf, 15);
        assert_eq!((complex.bits() >> 4) & 0x1ff, 1);
        assert_eq!((complex.bits() >> 13) & 0x1ff, 2);
        assert_ne!(complex.bits() & (1 << 22), 0);
        assert_ne!(complex.bits() & (1 << 23), 0);
        assert_ne!(complex.bits() & (1 << 24), 0);
    }

    #[test]
    fn nonseparable_mode_requires_color_management() {
        let spec = BlendSpec::new(BlendMode::Hue, MAX_ALPHA, MAX_ALPHA).expect("hue spec");
        let plan = BlendPlan::build(spec, &FullBackend).expect("full plan");
        assert!(
            plan.requirements()
                .contains(&BackendRequirement::ColorManagement)
        );
        let blocked = BlendPlan::build(spec, &NoCmmBackend).expect("plan without a CMM");
        assert!(
            blocked
                .blocked()
                .contains(&BackendRequirement::ColorManagement)
        );
        assert_eq!(
            blocked.check_support().expect_err("unsupported cmm"),
            TransparencyError::UnsupportedBackend(BackendRequirement::ColorManagement)
        );
    }

    #[test]
    fn partial_alpha_and_group_flags_declare_their_requirements() {
        let group = TransparencyGroup::new(
            GroupIsolation::Isolated,
            GroupKnockout::Knockout,
            BlendMode::Normal,
        );
        let spec = BlendSpec::new(BlendMode::Normal, 255, 256)
            .expect("spec")
            .with_group(group);
        let plan = BlendPlan::build(spec, &FullBackend).expect("plan");
        assert_eq!(
            plan.requirements(),
            &[
                BackendRequirement::Mode(BlendMode::Normal),
                BackendRequirement::PartialAlpha,
                BackendRequirement::Isolation,
                BackendRequirement::Knockout,
            ]
        );
    }

    #[test]
    fn no_backend_returns_an_honest_typed_error() {
        let spec = BlendSpec::new(BlendMode::Multiply, MAX_ALPHA, MAX_ALPHA).expect("spec");
        let plan = BlendPlan::build(spec, &NoBackend).expect("plan records gaps");
        assert_eq!(
            plan.check_support().expect_err("nothing is supported"),
            TransparencyError::UnsupportedBackend(BackendRequirement::Mode(BlendMode::Multiply))
        );
        assert_eq!(plan.warnings().len(), 1);
        assert!(plan.warnings()[0].contains("Multiply"));
    }

    #[test]
    fn stack_depth_is_bounded_and_plans_keep_order() {
        let mut stack = TransparencyStack::new();
        // Use a different mode at each depth so plan order is observable.
        for &mode in BlendMode::all().iter().take(usize::from(MAX_STACK_DEPTH)) {
            stack
                .push(BlendSpec::new(mode, MAX_ALPHA, MAX_ALPHA).expect("spec"))
                .expect("within bound");
        }
        assert_eq!(stack.len(), usize::from(MAX_STACK_DEPTH));
        assert_eq!(
            stack
                .push(BlendSpec::new(BlendMode::Normal, MAX_ALPHA, MAX_ALPHA).expect("spec"))
                .expect_err("over bound"),
            TransparencyError::StackTooDeep {
                limit: MAX_STACK_DEPTH
            }
        );
        let plans = stack.plans(&FullBackend).expect("plans");
        assert_eq!(plans.len(), usize::from(MAX_STACK_DEPTH));
        for (index, plan) in plans.iter().enumerate() {
            let expected_mode = BlendMode::from_index(index as u8).expect("mode at depth");
            assert_eq!(plan.spec().mode(), expected_mode);
            assert_eq!(plan.bits() & 0xf, u64::from(expected_mode.index()));
            assert_eq!((plan.bits() >> 4) & 0x1ff, u64::from(MAX_ALPHA));
            assert_eq!((plan.bits() >> 13) & 0x1ff, u64::from(MAX_ALPHA));
        }
    }

    #[test]
    fn stack_plans_are_deterministic_across_repeated_builds() {
        let mut stack = TransparencyStack::new();
        let group = TransparencyGroup::new(
            GroupIsolation::Isolated,
            GroupKnockout::NonKnockout,
            BlendMode::Multiply,
        );
        stack
            .push(BlendSpec::new(BlendMode::Normal, MAX_ALPHA, MAX_ALPHA).expect("plain spec"))
            .expect("first push");
        stack
            .push(
                BlendSpec::new(BlendMode::Hue, 128, 256)
                    .expect("grouped spec")
                    .with_group(group),
            )
            .expect("second push");
        let first = stack.plans(&FullBackend).expect("first build");
        let second = stack.plans(&FullBackend).expect("second build");
        assert_eq!(first, second);
        assert_eq!(first.len(), 2);
        assert!(first.iter().all(|plan| plan.check_support().is_ok()));
        // A partial backend records the same gaps, in the same order, every time.
        let partial_a = stack.plans(&NoCmmBackend).expect("partial build");
        let partial_b = stack.plans(&NoCmmBackend).expect("partial build");
        assert_eq!(partial_a, partial_b);
        assert!(
            partial_a[1]
                .blocked()
                .contains(&BackendRequirement::ColorManagement)
        );
    }

    #[test]
    fn malformed_descriptors_return_typed_errors() {
        assert!(
            BlendSpec::from_descriptor("")
                .is_err_and(|error| matches!(error, TransparencyError::MalformedDescriptor(_)))
        );
        assert!(
            BlendSpec::from_descriptor("Normal ca=abc")
                .is_err_and(|error| matches!(error, TransparencyError::MalformedDescriptor(_)))
        );
        assert!(
            BlendSpec::from_descriptor("Normal bogus=1")
                .is_err_and(|error| matches!(error, TransparencyError::MalformedDescriptor(_)))
        );
        assert!(
            BlendSpec::from_descriptor("Normal group=A|B|C")
                .is_err_and(|error| matches!(error, TransparencyError::UnknownBlendMode(_)))
        );
    }

    #[test]
    fn colour_space_tokens_round_trip_with_component_counts() {
        for (token, space, components) in [
            ("DeviceGray", GroupColorSpace::DeviceGray, 1),
            ("/devicergb", GroupColorSpace::DeviceRgb, 3),
            ("DeviceCMYK", GroupColorSpace::DeviceCmyk, 4),
            (
                "ICCBased:3:MyRGB",
                GroupColorSpace::IccBased {
                    name: "MyRGB".to_owned(),
                    components: 3,
                },
                3,
            ),
        ] {
            let parsed = GroupColorSpace::parse(token).expect("parse");
            assert_eq!(parsed, space);
            assert_eq!(parsed.component_count(), components);
            assert_eq!(
                GroupColorSpace::parse(&space.to_token()).expect("canonical round trip"),
                space
            );
        }
    }

    #[test]
    fn invalid_colour_spaces_are_typed_errors() {
        assert_eq!(
            GroupColorSpace::parse("Lab").expect_err("unknown"),
            TransparencyError::UnknownColourSpace("Lab".to_owned())
        );
        assert!(matches!(
            GroupColorSpace::parse("ICCBased:0:x"),
            Err(TransparencyError::InvalidColourSpaceComponents { value: 0 })
        ));
        assert!(matches!(
            GroupColorSpace::parse("ICCBased:16:x"),
            Err(TransparencyError::InvalidColourSpaceComponents { value: 16 })
        ));
        assert!(matches!(
            GroupColorSpace::parse(&"x".repeat(MAX_TOKEN_LEN + 1)),
            Err(TransparencyError::TokenTooLong { .. })
        ));
    }

    #[test]
    fn group_space_resolution_inherits_backdrop_and_rejects_mismatch() {
        let backdrop = GroupColorSpace::DeviceRgb;
        assert_eq!(
            resolve_group_colour_space(None, backdrop.clone(), 0).expect("inherit"),
            backdrop
        );
        let same = GroupColorSpace::IccBased {
            name: "sRGB".to_owned(),
            components: 3,
        };
        assert_eq!(
            resolve_group_colour_space(Some(same.clone()), backdrop.clone(), 0).expect("resolve"),
            same
        );
        assert_eq!(
            resolve_group_colour_space(Some(GroupColorSpace::DeviceCmyk), backdrop, 2)
                .expect_err("mismatch"),
            TransparencyError::ColourSpaceMismatch {
                layer: 2,
                backdrop: 3,
                group: 4,
            }
        );
    }

    #[test]
    fn soft_masks_round_trip_and_validate_backdrop() {
        let group = TransparencyGroup::new(
            GroupIsolation::Isolated,
            GroupKnockout::NonKnockout,
            BlendMode::Multiply,
        );
        let mask = SoftMask::new(SoftMaskSubtype::Luminosity, vec![0.0, 0.5, 1.0])
            .expect("mask")
            .with_group(group);
        assert_eq!(
            SoftMask::from_descriptor(&mask.to_descriptor()).expect("round trip"),
            mask
        );
        let plain = SoftMask::new(SoftMaskSubtype::Alpha, vec![]).expect("empty backdrop");
        assert_eq!(
            SoftMask::from_descriptor(&plain.to_descriptor()).expect("round trip"),
            plain
        );
        assert_eq!(
            SoftMaskSubtype::parse("/alpha").expect("alpha"),
            SoftMaskSubtype::Alpha
        );
        assert_eq!(
            SoftMaskSubtype::parse("bogus").expect_err("bad subtype"),
            TransparencyError::UnknownSoftMaskSubtype("bogus".to_owned())
        );
    }

    #[test]
    fn soft_mask_out_of_range_backdrop_is_typed() {
        assert_eq!(
            SoftMask::new(SoftMaskSubtype::Alpha, vec![1.5]).expect_err("over one"),
            TransparencyError::InvalidBackdropComponent {
                index: 0,
                value: 1.5
            }
        );
        assert!(matches!(
            SoftMask::new(SoftMaskSubtype::Alpha, vec![f32::NAN]),
            Err(TransparencyError::InvalidBackdropComponent { index: 0, .. })
        ));
        assert_eq!(
            SoftMask::new(
                SoftMaskSubtype::Alpha,
                vec![0.0; MAX_BACKDROP_COMPONENTS + 1]
            )
            .expect_err("too many"),
            TransparencyError::TooManyBackdropComponents { count: 5, limit: 4 }
        );
    }

    #[test]
    fn compositing_plan_is_ordered_back_to_front_and_deterministic() {
        let layers = vec![
            BlendLayer::new(BlendSpec::new(BlendMode::Normal, MAX_ALPHA, MAX_ALPHA).expect("back")),
            BlendLayer::new(BlendSpec::new(BlendMode::Multiply, 128, MAX_ALPHA).expect("mid"))
                .with_soft_mask(SoftMask::new(SoftMaskSubtype::Alpha, vec![1.0]).expect("mask"))
                .with_group_space(GroupColorSpace::DeviceRgb),
            BlendLayer::new(BlendSpec::new(BlendMode::Hue, MAX_ALPHA, MAX_ALPHA).expect("front")),
        ];
        let a =
            CompositingPlan::build(&layers, GroupColorSpace::DeviceRgb, &FullBackend).expect("a");
        let b =
            CompositingPlan::build(&layers, GroupColorSpace::DeviceRgb, &FullBackend).expect("b");
        assert_eq!(a, b);
        assert_eq!(a.bits(), b.bits());
        assert_eq!(a.summary(), b.summary());
        assert_eq!(a.len(), 3);
        assert_eq!(a.steps()[0].index(), 0);
        assert_eq!(a.steps()[0].spec().mode(), BlendMode::Normal);
        assert_eq!(a.steps()[2].spec().mode(), BlendMode::Hue);
        assert!(a.steps()[1].soft_mask().is_some());
        assert!(
            a.summary()
                .starts_with("compositing[3] back-to-front onto DeviceRGB")
        );
        assert!(a.check_support().is_ok());
    }

    #[test]
    fn compositing_plan_summary_reflects_painting_order() {
        let back =
            BlendLayer::new(BlendSpec::new(BlendMode::Normal, MAX_ALPHA, MAX_ALPHA).expect("a"));
        let front =
            BlendLayer::new(BlendSpec::new(BlendMode::Screen, MAX_ALPHA, MAX_ALPHA).expect("b"));
        let forward = CompositingPlan::build(
            &[back.clone(), front.clone()],
            GroupColorSpace::DeviceRgb,
            &FullBackend,
        )
        .expect("forward");
        let reversed =
            CompositingPlan::build(&[front, back], GroupColorSpace::DeviceRgb, &FullBackend)
                .expect("reversed");
        assert_ne!(
            forward.summary(),
            reversed.summary(),
            "painting order must be observable in the summary"
        );
        let summary = forward.summary();
        let at_normal = summary.find("0:Normal").expect("back step first");
        let at_screen = summary.find("1:Screen").expect("front step second");
        assert!(at_normal < at_screen);
    }

    #[test]
    fn compositing_plan_rejects_empty_and_bound_violations() {
        assert_eq!(
            CompositingPlan::build(&[], GroupColorSpace::DeviceRgb, &FullBackend)
                .expect_err("empty"),
            TransparencyError::EmptyCompositingPlan
        );
        let layer = BlendLayer::new(
            BlendSpec::new(BlendMode::Normal, MAX_ALPHA, MAX_ALPHA).expect("layer"),
        );
        let many = vec![layer; MAX_COMPOSITING_LAYERS + 1];
        assert!(matches!(
            CompositingPlan::build(&many, GroupColorSpace::DeviceRgb, &FullBackend),
            Err(TransparencyError::TooManyLayers { .. })
        ));
    }

    #[test]
    fn compositing_plan_reports_backend_gaps_and_space_mismatch() {
        let hue =
            BlendLayer::new(BlendSpec::new(BlendMode::Hue, MAX_ALPHA, MAX_ALPHA).expect("hue"));
        let plan = CompositingPlan::build(&[hue], GroupColorSpace::DeviceRgb, &NoCmmBackend)
            .expect("gaps");
        assert!(
            plan.blocked_requirements()
                .contains(&BackendRequirement::ColorManagement)
        );
        assert!(matches!(
            plan.check_support(),
            Err(TransparencyError::UnsupportedBackend(
                BackendRequirement::ColorManagement
            ))
        ));
        let cmyk_layer = BlendLayer::new(
            BlendSpec::new(BlendMode::Normal, MAX_ALPHA, MAX_ALPHA).expect("normal"),
        )
        .with_group_space(GroupColorSpace::DeviceCmyk);
        assert!(matches!(
            CompositingPlan::build(&[cmyk_layer], GroupColorSpace::DeviceRgb, &FullBackend),
            Err(TransparencyError::ColourSpaceMismatch { layer: 0, .. })
        ));
    }
}
