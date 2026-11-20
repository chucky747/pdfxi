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

use std::fmt;

/// Fully opaque alpha value in the 9-bit `0..=256` representation.
pub const MAX_ALPHA: u16 = 256;
/// Maximum accepted length of a single descriptor token.
pub const MAX_TOKEN_LEN: usize = 64;
/// Maximum number of nested transparency groups in a stack.
pub const MAX_STACK_DEPTH: u8 = 8;

/// A typed reason a transparency descriptor or plan was rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
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
}
