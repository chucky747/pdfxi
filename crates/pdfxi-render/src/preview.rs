//! Output preview, separations, ink management, and overprint policy.
//!
//! This module is the backend-agnostic model behind Acrobat-style *Output
//! Preview* and *Separations* panels. It does not rasterize anything; instead it
//! produces a small, deterministic [`PreviewPlan`] that a renderer backend can
//! consume, together with an [`InkManager`] that models the plate/ink list.
//!
//! # Scope
//!
//! Two related concerns live here:
//!
//! * **Output preview** (feature PRINT-004): a [`PreviewMode`] (composite,
//!   single separation, or all separations), per-plate visibility toggles, an
//!   overprint-simulation flag, and a [`SoftProofIntent`] selection. Inks and
//!   soft-proof defaults are derived deterministically from the
//!   [`IccProfile`]/[`OutputIntent`] structures in
//!   [`crate::color`].
//! * **Ink manager** (feature PRINT-005): [`InkEntry`] records carrying a name,
//!   a process-CMYK-versus-spot indicator, a normalised coverage/density value,
//!   and an enabled flag. Entries are kept in a canonical order (C, M, Y, K,
//!   then spots by name), totals are computed deterministically, and an
//!   [`OverprintPolicy`] carries the overprint-preview policy flags.
//!
//! # Honest unsupported conversions
//!
//! This model deliberately does **not** emulate a full colour-management
//! module. Colour spaces that cannot yield process plates (RGB, Lab, DeviceN,
//! …) are reported through [`PreviewError::UnsupportedConversion`] when a
//! caller asks for derivation, and as a human-readable warning inside a
//! [`PreviewPlan`] during preview building. Soft-proofing and spot-ink preview
//! set [`PreviewPlan::needs_backend_cmm`] so the caller knows a native backend
//! must perform the real transform.
//!
//! # Determinism and safety
//!
//! Inputs are bounded (ink count and name length are capped), all totals are
//! accumulated in `f64` and rounded to `f32` once, and ordering is total and
//! stable. There is no randomness, wall-clock dependence, unsafe code, or new
//! dependency. The same inputs always yield byte-identical output.

use crate::color::{
    ColorError, ConversionPlan, IccColorSpace, IccProfile, OutputIntent, RenderingIntent,
};

/// Maximum number of ink/plate entries accepted by an [`InkManager`].
pub const MAX_INKS: usize = 64;

/// Maximum length, in bytes, of an ink/plate name.
pub const MAX_INK_NAME_BYTES: usize = 128;

/// Structured errors produced while configuring previews or managing inks.
#[derive(Debug, thiserror::Error, PartialEq)]
pub enum PreviewError {
    /// A preview-mode token was not recognised.
    #[error("unknown preview mode: {0}")]
    UnknownPreviewMode(String),
    /// A soft-proof intent token was not recognised.
    #[error("unknown soft-proof intent: {0}")]
    UnknownSoftProofIntent(String),
    /// An ink name was empty or whitespace only.
    #[error("ink name must not be empty")]
    EmptyInkName,
    /// An ink name exceeded the byte-length cap.
    #[error("ink name length {len} exceeds the {limit}-byte limit")]
    InkNameTooLong {
        /// Actual byte length of the name.
        len: usize,
        /// Enforced byte-length limit.
        limit: usize,
    },
    /// Adding an ink would exceed the configured cap.
    #[error("ink count {count} exceeds the {limit}-ink limit")]
    TooManyInks {
        /// Ink count that would result from the add.
        count: usize,
        /// Enforced ink-count limit.
        limit: usize,
    },
    /// Adding a visible-plate toggle would exceed the configured cap.
    #[error("visible-plate count {count} exceeds the {limit}-plate limit")]
    TooManyVisiblePlates {
        /// Visible-plate count that would result from the add.
        count: usize,
        /// Enforced visible-plate limit.
        limit: usize,
    },
    /// A coverage/density value was not finite or was outside `0.0..=1.0`.
    #[error("coverage for ink '{name}' must be finite and in 0.0..=1.0, got {value}")]
    InvalidCoverage {
        /// Name of the offending ink.
        name: String,
        /// The rejected value.
        value: f32,
    },
    /// Two inks shared the same name.
    #[error("duplicate ink name: {0}")]
    DuplicateInk(String),
    /// A named plate was not present in the manager.
    #[error("unknown plate: {0}")]
    UnknownPlate(String),
    /// A requested conversion cannot be modelled analytically by this module.
    #[error("unsupported conversion: {0}")]
    UnsupportedConversion(String),
    /// An output intent's colour data could not be decoded.
    #[error(transparent)]
    Color(#[from] ColorError),
}

/// The high-level page preview mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PreviewMode {
    /// A normal full-colour composite preview.
    ColorComposite,
    /// A single separation: only the selected plate is shown.
    Separation,
    /// All separations listed together.
    Separations,
}

impl PreviewMode {
    /// Parse a preview mode from a case-insensitive token.
    ///
    /// Accepts `composite`/`color`/`colour`/`rgb` for [`Self::ColorComposite`],
    /// `separation`/`plate`/`spot` for [`Self::Separation`], and
    /// `separations`/`plates`/`all-plates` for [`Self::Separations`].
    ///
    /// # Errors
    ///
    /// Returns [`PreviewError::UnknownPreviewMode`] for an unrecognised token.
    pub fn parse(value: &str) -> Result<Self, PreviewError> {
        match value.trim().to_ascii_lowercase().as_str() {
            "composite" | "color" | "colour" | "rgb" => Ok(Self::ColorComposite),
            "separation" | "plate" | "spot" => Ok(Self::Separation),
            "separations" | "plates" | "all-plates" => Ok(Self::Separations),
            other => Err(PreviewError::UnknownPreviewMode(other.to_owned())),
        }
    }

    /// Return the canonical token for this mode.
    pub const fn name(self) -> &'static str {
        match self {
            Self::ColorComposite => "composite",
            Self::Separation => "separation",
            Self::Separations => "separations",
        }
    }
}

impl std::fmt::Display for PreviewMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

/// The soft-proofing rendering intent selected for a preview.
///
/// [`Self::Auto`] asks the plan builder to derive the intent from the output
/// intent's destination ICC profile header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SoftProofIntent {
    /// Derive the intent from the output profile.
    Auto,
    /// No soft proofing.
    None,
    /// Perceptual rendering intent.
    Perceptual,
    /// Media-relative colorimetric rendering intent.
    RelativeColorimetric,
    /// Saturation rendering intent.
    Saturation,
    /// Absolute colorimetric rendering intent.
    AbsoluteColorimetric,
}

impl SoftProofIntent {
    /// Parse a soft-proof intent from a case-insensitive token.
    ///
    /// Accepts `auto`, `none`/`off`, `perceptual`,
    /// `relative`/`relative-colorimetric`/`rel`, `saturation`, and
    /// `absolute`/`absolute-colorimetric`/`abs`.
    ///
    /// # Errors
    ///
    /// Returns [`PreviewError::UnknownSoftProofIntent`] for an unrecognised
    /// token.
    pub fn parse(value: &str) -> Result<Self, PreviewError> {
        match value.trim().to_ascii_lowercase().as_str() {
            "auto" => Ok(Self::Auto),
            "none" | "off" | "no" => Ok(Self::None),
            "perceptual" => Ok(Self::Perceptual),
            "relative" | "relative-colorimetric" | "rel" => Ok(Self::RelativeColorimetric),
            "saturation" => Ok(Self::Saturation),
            "absolute" | "absolute-colorimetric" | "abs" => Ok(Self::AbsoluteColorimetric),
            other => Err(PreviewError::UnknownSoftProofIntent(other.to_owned())),
        }
    }

    /// Return the canonical token for this intent.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::None => "none",
            Self::Perceptual => "perceptual",
            Self::RelativeColorimetric => "relative",
            Self::Saturation => "saturation",
            Self::AbsoluteColorimetric => "absolute",
        }
    }

    /// Derive a soft-proof intent from an ICC header rendering intent.
    ///
    /// An out-of-range [`RenderingIntent::Unknown`] maps to [`Self::None`]
    /// rather than being silently coerced to a concrete intent.
    pub const fn from_rendering_intent(intent: RenderingIntent) -> Self {
        match intent {
            RenderingIntent::Perceptual => Self::Perceptual,
            RenderingIntent::RelativeColorimetric => Self::RelativeColorimetric,
            RenderingIntent::Saturation => Self::Saturation,
            RenderingIntent::AbsoluteColorimetric => Self::AbsoluteColorimetric,
            RenderingIntent::Unknown(_) => Self::None,
        }
    }

    /// Return `true` when a real soft-proof transform must be applied.
    pub const fn needs_transform(self) -> bool {
        matches!(
            self,
            Self::Perceptual
                | Self::RelativeColorimetric
                | Self::Saturation
                | Self::AbsoluteColorimetric
        )
    }
}

impl std::fmt::Display for SoftProofIntent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

/// Whether an ink is a CMYK process plate or a named spot colour.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InkKind {
    /// Cyan process plate.
    Cyan,
    /// Magenta process plate.
    Magenta,
    /// Yellow process plate.
    Yellow,
    /// Black (key) process plate.
    Black,
    /// A named spot colour, outside the process set.
    Spot,
}

impl InkKind {
    /// Return `true` for one of the four CMYK process plates.
    pub const fn is_process(self) -> bool {
        matches!(
            self,
            Self::Cyan | Self::Magenta | Self::Yellow | Self::Black
        )
    }

    /// Return `true` for a spot colour.
    pub const fn is_spot(self) -> bool {
        matches!(self, Self::Spot)
    }

    /// Return the canonical name of this ink kind.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Cyan => "cyan",
            Self::Magenta => "magenta",
            Self::Yellow => "yellow",
            Self::Black => "black",
            Self::Spot => "spot",
        }
    }

    /// Return the canonical order rank used to sort ink entries.
    const fn order_rank(self) -> u8 {
        match self {
            Self::Cyan => 0,
            Self::Magenta => 1,
            Self::Yellow => 2,
            Self::Black => 3,
            Self::Spot => 4,
        }
    }
}

/// One ink/plate entry in the ink manager.
#[derive(Clone, Debug, PartialEq)]
pub struct InkEntry {
    name: String,
    kind: InkKind,
    coverage: f32,
    enabled: bool,
}

impl InkEntry {
    /// Create a validated ink entry.
    ///
    /// `coverage` must be finite and in `0.0..=1.0`, and `name` must be
    /// non-empty and within [`MAX_INK_NAME_BYTES`].
    ///
    /// # Errors
    ///
    /// Returns a typed [`PreviewError`] for an invalid name or coverage.
    pub fn new(name: &str, kind: InkKind, coverage: f32) -> Result<Self, PreviewError> {
        let trimmed = name.trim();
        if trimmed.is_empty() {
            return Err(PreviewError::EmptyInkName);
        }
        let len = name.len();
        if len > MAX_INK_NAME_BYTES {
            return Err(PreviewError::InkNameTooLong {
                len,
                limit: MAX_INK_NAME_BYTES,
            });
        }
        if !coverage.is_finite() || !(0.0..=1.0).contains(&coverage) {
            return Err(PreviewError::InvalidCoverage {
                name: name.to_owned(),
                value: coverage,
            });
        }
        Ok(Self {
            name: name.to_owned(),
            kind,
            coverage,
            enabled: true,
        })
    }

    /// Return the ink's name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Return whether this ink is process or spot.
    pub const fn kind(&self) -> InkKind {
        self.kind
    }

    /// Return the normalised coverage/density value in `0.0..=1.0`.
    pub const fn coverage(&self) -> f32 {
        self.coverage
    }

    /// Return `true` when this ink is enabled.
    pub const fn enabled(&self) -> bool {
        self.enabled
    }

    /// Enable or disable this ink.
    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
    }

    /// Update the coverage/density value.
    ///
    /// # Errors
    ///
    /// Returns [`PreviewError::InvalidCoverage`] when the value is not finite
    /// or outside `0.0..=1.0`.
    pub fn set_coverage(&mut self, coverage: f32) -> Result<(), PreviewError> {
        if !coverage.is_finite() || !(0.0..=1.0).contains(&coverage) {
            return Err(PreviewError::InvalidCoverage {
                name: self.name.clone(),
                value: coverage,
            });
        }
        self.coverage = coverage;
        Ok(())
    }

    /// A total ordering key: process rank first, then name.
    fn order_key(&self) -> (u8, &str) {
        (self.kind.order_rank(), self.name.as_str())
    }
}

/// Overprint-preview policy flags carried by an [`InkManager`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OverprintPolicy {
    /// Whether overprint simulation is permitted at all.
    pub simulate: bool,
    /// Whether knockout behaviour should be honoured.
    pub honour_knockout: bool,
    /// Whether spot inks participate in overprint simulation.
    pub overprint_spots: bool,
}

impl Default for OverprintPolicy {
    fn default() -> Self {
        Self {
            simulate: true,
            honour_knockout: true,
            overprint_spots: false,
        }
    }
}

impl OverprintPolicy {
    /// Encode the flags into a deterministic bit mask: bit 0 `simulate`, bit 1
    /// `honour_knockout`, bit 2 `overprint_spots`.
    pub fn to_bits(self) -> u8 {
        let mut bits = 0u8;
        if self.simulate {
            bits |= 1;
        }
        if self.honour_knockout {
            bits |= 2;
        }
        if self.overprint_spots {
            bits |= 4;
        }
        bits
    }

    /// Decode flags from a bit mask produced by [`Self::to_bits`].
    ///
    /// Unknown high bits are ignored, so decoding is idempotent.
    pub const fn from_bits(bits: u8) -> Self {
        Self {
            simulate: bits & 1 != 0,
            honour_knockout: bits & 2 != 0,
            overprint_spots: bits & 4 != 0,
        }
    }
}

/// A bounded, deterministically ordered collection of ink/plate entries.
#[derive(Clone, Debug, PartialEq)]
pub struct InkManager {
    inks: Vec<InkEntry>,
    policy: OverprintPolicy,
}

impl Default for InkManager {
    fn default() -> Self {
        Self::new()
    }
}

impl InkManager {
    /// Create an empty ink manager with the default overprint policy.
    pub fn new() -> Self {
        Self {
            inks: Vec::new(),
            policy: OverprintPolicy::default(),
        }
    }

    /// Create an empty ink manager with an explicit overprint policy.
    pub fn with_policy(policy: OverprintPolicy) -> Self {
        Self {
            inks: Vec::new(),
            policy,
        }
    }

    /// Add an ink, keeping entries in canonical order.
    ///
    /// # Errors
    ///
    /// Returns a typed [`PreviewError`] for an invalid entry, a duplicate name,
    /// or when the ink count would exceed [`MAX_INKS`].
    pub fn add(&mut self, name: &str, kind: InkKind, coverage: f32) -> Result<(), PreviewError> {
        let entry = InkEntry::new(name, kind, coverage)?;
        self.add_entry(entry)
    }

    /// Add a pre-built ink entry.
    ///
    /// # Errors
    ///
    /// Returns [`PreviewError::DuplicateInk`] for a repeated name or
    /// [`PreviewError::TooManyInks`] when the cap would be exceeded.
    pub fn add_entry(&mut self, entry: InkEntry) -> Result<(), PreviewError> {
        if self.inks.iter().any(|ink| ink.name == entry.name) {
            return Err(PreviewError::DuplicateInk(entry.name));
        }
        let count = self.inks.len().saturating_add(1);
        if count > MAX_INKS {
            return Err(PreviewError::TooManyInks {
                count,
                limit: MAX_INKS,
            });
        }
        self.inks.push(entry);
        // Names are unique, so the order key is total and the sort deterministic.
        self.inks.sort_by(|a, b| a.order_key().cmp(&b.order_key()));
        Ok(())
    }

    /// Return the number of ink entries.
    pub fn len(&self) -> usize {
        self.inks.len()
    }

    /// Return `true` when there are no ink entries.
    pub fn is_empty(&self) -> bool {
        self.inks.is_empty()
    }

    /// Return the ink entries in canonical order.
    pub fn inks(&self) -> &[InkEntry] {
        &self.inks
    }

    /// Return the names of all inks in canonical order.
    pub fn names(&self) -> Vec<&str> {
        self.inks.iter().map(|ink| ink.name.as_str()).collect()
    }

    /// Find an ink by exact name.
    pub fn find(&self, name: &str) -> Option<&InkEntry> {
        self.inks.iter().find(|ink| ink.name == name)
    }

    /// Enable or disable an ink by name.
    ///
    /// # Errors
    ///
    /// Returns [`PreviewError::UnknownPlate`] when the name is not present.
    pub fn set_enabled(&mut self, name: &str, enabled: bool) -> Result<(), PreviewError> {
        let ink = self
            .inks
            .iter_mut()
            .find(|ink| ink.name == name)
            .ok_or_else(|| PreviewError::UnknownPlate(name.to_owned()))?;
        ink.set_enabled(enabled);
        Ok(())
    }

    /// Update an ink's coverage by name.
    ///
    /// # Errors
    ///
    /// Returns [`PreviewError::UnknownPlate`] or an invalid-coverage error.
    pub fn set_coverage(&mut self, name: &str, coverage: f32) -> Result<(), PreviewError> {
        let ink = self
            .inks
            .iter_mut()
            .find(|ink| ink.name == name)
            .ok_or_else(|| PreviewError::UnknownPlate(name.to_owned()))?;
        ink.set_coverage(coverage)
    }

    /// Enable every ink.
    pub fn enable_all(&mut self) {
        for ink in &mut self.inks {
            ink.set_enabled(true);
        }
    }

    /// Disable every ink.
    pub fn disable_all(&mut self) {
        for ink in &mut self.inks {
            ink.set_enabled(false);
        }
    }

    /// Return the number of enabled inks.
    pub fn enabled_count(&self) -> usize {
        self.inks.iter().filter(|ink| ink.enabled).count()
    }

    /// Return the sum of coverage across enabled inks.
    ///
    /// Accumulation happens in `f64` and is rounded to `f32` once, so the
    /// result is deterministic and independent of ink order.
    pub fn total_coverage(&self) -> f32 {
        let total: f64 = self
            .inks
            .iter()
            .filter(|ink| ink.enabled)
            .map(|ink| f64::from(ink.coverage))
            .sum();
        total as f32
    }

    /// Return only the process inks, in canonical order.
    pub fn process_inks(&self) -> Vec<&InkEntry> {
        self.inks
            .iter()
            .filter(|ink| ink.kind.is_process())
            .collect()
    }

    /// Return only the spot inks, in canonical order.
    pub fn spot_inks(&self) -> Vec<&InkEntry> {
        self.inks.iter().filter(|ink| ink.kind.is_spot()).collect()
    }

    /// Return the overprint policy.
    pub const fn policy(&self) -> &OverprintPolicy {
        &self.policy
    }

    /// Replace the overprint policy.
    pub fn set_policy(&mut self, policy: OverprintPolicy) {
        self.policy = policy;
    }

    /// Derive process plates deterministically from an ICC colour space.
    ///
    /// CMYK yields the four process plates (C, M, Y, K); Gray yields K. Any
    /// other space is reported through
    /// [`PreviewError::UnsupportedConversion`] rather than approximated.
    pub fn from_icc_color_space(space: IccColorSpace) -> Result<Self, PreviewError> {
        let mut manager = Self::new();
        for (name, kind) in derive_process_inks(space)? {
            manager.add(name, kind, 0.0)?;
        }
        Ok(manager)
    }

    /// Derive process plates from an output intent's destination profile.
    ///
    /// A missing or unparseable destination profile is reported as a typed
    /// error; use [`PreviewPlan::build`] when warnings are preferred instead.
    ///
    /// # Errors
    ///
    /// Returns [`PreviewError::Color`] for a bad profile,
    /// [`PreviewError::UnsupportedConversion`] for a profile whose colour space
    /// has no process plates, or a missing destination profile.
    pub fn derive_from_output_intent(intent: &OutputIntent) -> Result<Self, PreviewError> {
        let profile = intent.parse_dest_profile()?.ok_or_else(|| {
            PreviewError::UnsupportedConversion(
                "output intent has no destination profile".to_owned(),
            )
        })?;
        Self::from_icc_color_space(profile.header().data_color_space)
    }
}

/// Derive the canonical process-plate descriptors for an ICC colour space.
fn derive_process_inks(space: IccColorSpace) -> Result<Vec<(&'static str, InkKind)>, PreviewError> {
    match space {
        IccColorSpace::Cmyk => Ok(vec![
            ("Cyan", InkKind::Cyan),
            ("Magenta", InkKind::Magenta),
            ("Yellow", InkKind::Yellow),
            ("Black", InkKind::Black),
        ]),
        IccColorSpace::Gray => Ok(vec![("Black", InkKind::Black)]),
        other => Err(PreviewError::UnsupportedConversion(format!(
            "no process plates for colour space {}",
            other.name()
        ))),
    }
}

/// A backend-friendly snapshot of one visible ink.
#[derive(Clone, Debug, PartialEq)]
pub struct InkSummary {
    /// The ink's name.
    pub name: String,
    /// Whether this ink is a process plate or a spot colour.
    pub kind: InkKind,
    /// The normalised coverage/density value in `0.0..=1.0`.
    pub coverage: f32,
    /// Whether the ink is enabled.
    pub enabled: bool,
}

impl From<&InkEntry> for InkSummary {
    fn from(ink: &InkEntry) -> Self {
        Self {
            name: ink.name.clone(),
            kind: ink.kind,
            coverage: ink.coverage,
            enabled: ink.enabled,
        }
    }
}

/// User-controlled settings that shape a [`PreviewPlan`].
#[derive(Clone, Debug, PartialEq)]
pub struct PreviewConfig {
    /// The high-level preview mode.
    pub mode: PreviewMode,
    /// The plate shown in [`PreviewMode::Separation`] mode.
    pub selected_plate: Option<String>,
    /// Plates that are visible; empty means "all plates".
    pub visible_plates: Vec<String>,
    /// Whether overprint simulation is requested.
    pub overprint: bool,
    /// The soft-proof intent selection.
    pub soft_proof: SoftProofIntent,
}

impl Default for PreviewConfig {
    fn default() -> Self {
        Self::new()
    }
}

impl PreviewConfig {
    /// Create a default config: composite mode, all plates, no overprint, auto
    /// soft proofing.
    pub fn new() -> Self {
        Self {
            mode: PreviewMode::ColorComposite,
            selected_plate: None,
            visible_plates: Vec::new(),
            overprint: false,
            soft_proof: SoftProofIntent::Auto,
        }
    }

    /// Add a plate to the visible set (deduplicating).
    ///
    /// # Errors
    ///
    /// Returns [`PreviewError::TooManyVisiblePlates`] if the set is full.
    pub fn show_plate(&mut self, name: &str) -> Result<(), PreviewError> {
        if self.visible_plates.iter().any(|plate| plate == name) {
            return Ok(());
        }
        let count = self.visible_plates.len().saturating_add(1);
        if count > MAX_INKS {
            return Err(PreviewError::TooManyVisiblePlates {
                count,
                limit: MAX_INKS,
            });
        }
        self.visible_plates.push(name.to_owned());
        Ok(())
    }

    /// Remove a plate from the visible set.
    pub fn hide_plate(&mut self, name: &str) {
        self.visible_plates.retain(|plate| plate != name);
    }

    /// Toggle a plate's visibility, adding it when absent.
    ///
    /// # Errors
    ///
    /// Propagates [`PreviewError::TooManyVisiblePlates`] from [`Self::show_plate`].
    pub fn toggle_plate(&mut self, name: &str) -> Result<(), PreviewError> {
        if self.visible_plates.iter().any(|plate| plate == name) {
            self.hide_plate(name);
            Ok(())
        } else {
            self.show_plate(name)
        }
    }

    /// Return `true` when `name` is visible (no visible set means all plates).
    pub fn is_plate_visible(&self, name: &str) -> bool {
        self.visible_plates.is_empty() || self.visible_plates.iter().any(|plate| plate == name)
    }
}

/// A deterministic preview description a renderer backend can consume.
#[derive(Clone, Debug, PartialEq)]
pub struct PreviewPlan {
    /// The resolved preview mode.
    pub mode: PreviewMode,
    /// The resolved soft-proof intent (never [`SoftProofIntent::Auto`]).
    pub soft_proof: SoftProofIntent,
    /// Whether overprint simulation will be applied.
    pub simulate_overprint: bool,
    /// The overprint policy flags encoded by [`OverprintPolicy::to_bits`].
    pub overprint_policy_bits: u8,
    /// Enabled, visible inks in canonical order.
    pub visible_inks: Vec<InkSummary>,
    /// The output condition label, when an output intent supplied one.
    pub output_condition: Option<String>,
    /// Human-readable notes about conversions that need a native backend.
    pub warnings: Vec<String>,
    /// Whether a native colour-management backend is required to render.
    pub needs_backend_cmm: bool,
}

impl PreviewPlan {
    /// Build a preview plan from `config`, `manager`, and an optional output
    /// intent.
    ///
    /// The function is pure and deterministic: it never mutates its inputs and
    /// returns the same plan for the same arguments. Unsupported conversions
    /// are reported as warnings rather than silently approximated.
    pub fn build(
        config: &PreviewConfig,
        manager: &InkManager,
        intent: Option<&OutputIntent>,
    ) -> Result<Self, PreviewError> {
        let mut warnings = Vec::new();
        let mut needs_backend_cmm = false;

        // Resolve the soft-proof intent, deriving it from the destination
        // profile when the caller selected Auto.
        let soft_proof = match config.soft_proof {
            SoftProofIntent::Auto => intent
                .and_then(|i| i.parse_dest_profile().ok().flatten())
                .map(|profile| {
                    SoftProofIntent::from_rendering_intent(profile.header().rendering_intent)
                })
                .unwrap_or(SoftProofIntent::None),
            explicit => explicit,
        };

        let output_condition = intent.and_then(|i| {
            i.output_condition_identifier
                .clone()
                .or_else(|| i.info.clone())
        });

        if let Some(intent) = intent {
            match intent.parse_dest_profile() {
                Ok(Some(profile)) => {
                    collect_profile_warnings(
                        &profile,
                        soft_proof,
                        &mut warnings,
                        &mut needs_backend_cmm,
                    );
                }
                Ok(None) => warnings.push(
                    "output intent has no destination profile; ink derivation is unavailable"
                        .to_owned(),
                ),
                Err(err) => warnings.push(format!(
                    "output intent destination profile could not be parsed: {err}"
                )),
            }
        }

        // Collect enabled, visible inks in the manager's canonical order.
        let mut visible_inks = Vec::new();
        for ink in manager.inks() {
            if !ink.enabled || !config.is_plate_visible(ink.name()) {
                continue;
            }
            if config.mode == PreviewMode::Separation {
                let selected = config.selected_plate.as_deref();
                if selected != Some(ink.name()) {
                    continue;
                }
            }
            visible_inks.push(InkSummary::from(ink));
        }

        if config.mode == PreviewMode::Separation {
            match config.selected_plate.as_deref() {
                None => warnings.push(
                    "separation mode has no selected plate; no separation is shown".to_owned(),
                ),
                Some(name) if manager.find(name).is_none() => warnings.push(format!(
                    "selected plate '{name}' is not present in the ink manager"
                )),
                Some(_) => {}
            }
        }

        if soft_proof.needs_transform() {
            needs_backend_cmm = true;
        }
        if visible_inks.iter().any(|ink| ink.kind.is_spot()) {
            needs_backend_cmm = true;
        }

        let simulate_overprint = config.overprint && manager.policy().simulate;

        Ok(Self {
            mode: config.mode,
            soft_proof,
            simulate_overprint,
            overprint_policy_bits: manager.policy().to_bits(),
            visible_inks,
            output_condition,
            warnings,
            needs_backend_cmm,
        })
    }
}

/// Add warnings for colour spaces and conversions this module cannot model.
fn collect_profile_warnings(
    profile: &IccProfile,
    soft_proof: SoftProofIntent,
    warnings: &mut Vec<String>,
    needs_backend_cmm: &mut bool,
) {
    match profile.header().data_color_space {
        IccColorSpace::Cmyk | IccColorSpace::Gray => {}
        other => {
            warnings.push(format!(
                "unsupported conversion: separation preview for colour space {} requires a backend",
                other.name()
            ));
            *needs_backend_cmm = true;
        }
    }
    if let ConversionPlan::NeedsBackend { reason } = profile.conversion_plan() {
        warnings.push(format!("backend colour management required: {reason}"));
        *needs_backend_cmm = true;
    }
    if soft_proof.needs_transform() {
        warnings.push("soft-proof rendering intent requires a backend colour transform".to_owned());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal but structurally valid RGB ICC profile with a given
    /// rendering intent, using the same layout as the `color` module tests.
    fn rgb_profile_bytes(intent: u32) -> Vec<u8> {
        // header (128) + tag count (4) + one desc tag (12) + payload (16).
        let mut buf = vec![0u8; 128 + 4 + 12 + 16];
        let size = buf.len() as u32;
        buf[0..4].copy_from_slice(&size.to_be_bytes());
        buf[4..8].copy_from_slice(b"ADBE");
        buf[12..16].copy_from_slice(b"mntr");
        buf[16..20].copy_from_slice(b"RGB ");
        buf[20..24].copy_from_slice(b"XYZ ");
        buf[36..40].copy_from_slice(b"acsp");
        buf[64..68].copy_from_slice(&intent.to_be_bytes());
        // Tag count = 1, one 'desc' table entry pointing at payload offset 144.
        buf[128..132].copy_from_slice(&1u32.to_be_bytes());
        buf[132..136].copy_from_slice(b"desc");
        buf[136..140].copy_from_slice(&144u32.to_be_bytes());
        buf[140..144].copy_from_slice(&16u32.to_be_bytes());
        // A valid minimal 'desc' payload (signature + reserved + ASCII count).
        buf[144..148].copy_from_slice(b"desc");
        buf[156..160].copy_from_slice(&1u32.to_be_bytes());
        buf
    }

    fn pdfx_intent(intent: u32) -> OutputIntent {
        OutputIntent {
            subtype: crate::color::OutputIntentSubtype::GtsPdfx,
            output_condition_identifier: Some("FOGRA39".to_owned()),
            info: Some("Coated FOGRA39".to_owned()),
            registry_name: None,
            dest_output_profile: Some(rgb_profile_bytes(intent)),
        }
    }

    #[test]
    fn preview_mode_parsing() {
        assert_eq!(
            PreviewMode::parse("composite"),
            Ok(PreviewMode::ColorComposite)
        );
        assert_eq!(
            PreviewMode::parse(" COLOR "),
            Ok(PreviewMode::ColorComposite)
        );
        assert_eq!(PreviewMode::parse("rgb"), Ok(PreviewMode::ColorComposite));
        assert_eq!(PreviewMode::parse("plate"), Ok(PreviewMode::Separation));
        assert_eq!(PreviewMode::parse("Spot"), Ok(PreviewMode::Separation));
        assert_eq!(
            PreviewMode::parse("separations"),
            Ok(PreviewMode::Separations)
        );
        assert_eq!(
            PreviewMode::parse("nonsense"),
            Err(PreviewError::UnknownPreviewMode("nonsense".to_owned()))
        );
        assert_eq!(PreviewMode::ColorComposite.name(), "composite");
    }

    #[test]
    fn soft_proof_intent_parsing_and_mapping() {
        assert_eq!(SoftProofIntent::parse("auto"), Ok(SoftProofIntent::Auto));
        assert_eq!(SoftProofIntent::parse("OFF"), Ok(SoftProofIntent::None));
        assert_eq!(
            SoftProofIntent::parse("relative-colorimetric"),
            Ok(SoftProofIntent::RelativeColorimetric)
        );
        assert_eq!(
            SoftProofIntent::parse("abs"),
            Ok(SoftProofIntent::AbsoluteColorimetric)
        );
        assert_eq!(
            SoftProofIntent::parse("bogus"),
            Err(PreviewError::UnknownSoftProofIntent("bogus".to_owned()))
        );
        assert_eq!(
            SoftProofIntent::from_rendering_intent(RenderingIntent::Saturation),
            SoftProofIntent::Saturation
        );
        assert_eq!(
            SoftProofIntent::from_rendering_intent(RenderingIntent::Unknown(9)),
            SoftProofIntent::None
        );
        assert!(SoftProofIntent::Perceptual.needs_transform());
        assert!(!SoftProofIntent::None.needs_transform());
    }

    #[test]
    fn plate_list_and_toggles() {
        let mut config = PreviewConfig::new();
        assert!(config.is_plate_visible("Cyan"));
        config.show_plate("Cyan").expect("show Cyan");
        config.show_plate("Cyan").expect("deduplicate");
        assert_eq!(config.visible_plates.len(), 1);
        assert!(config.is_plate_visible("Cyan"));
        assert!(!config.is_plate_visible("Magenta"));
        config.toggle_plate("Magenta").expect("toggle on");
        assert!(config.is_plate_visible("Magenta"));
        config.toggle_plate("Magenta").expect("toggle off");
        assert!(!config.is_plate_visible("Magenta"));
        config.hide_plate("Cyan");
        assert!(
            config.is_plate_visible("anything"),
            "empty means all visible"
        );
    }

    #[test]
    fn ink_ordering_and_totals() {
        let mut manager = InkManager::new();
        manager.add("Spot Red", InkKind::Spot, 0.10).expect("spot");
        manager.add("Black", InkKind::Black, 0.30).expect("black");
        manager.add("Cyan", InkKind::Cyan, 0.50).expect("cyan");
        assert_eq!(manager.names(), vec!["Cyan", "Black", "Spot Red"]);
        assert_eq!(manager.process_inks().len(), 2);
        assert_eq!(manager.spot_inks().len(), 1);
        // 0.50 + 0.30 + 0.10 = 0.90; accumulation in f64 then cast to f32.
        assert!((manager.total_coverage() - 0.90).abs() < 1e-6);
        assert_eq!(manager.enabled_count(), 3);
    }

    #[test]
    fn spot_vs_process_and_enabling() {
        let mut manager = InkManager::new();
        manager.add("Cyan", InkKind::Cyan, 0.5).expect("cyan");
        manager
            .add("PANTONE 485", InkKind::Spot, 0.25)
            .expect("spot");
        assert!(manager.find("Cyan").is_some_and(|i| i.kind().is_process()));
        assert!(
            manager
                .find("PANTONE 485")
                .is_some_and(|i| i.kind().is_spot())
        );
        assert_eq!(manager.total_coverage(), 0.75);
        manager.set_enabled("Cyan", false).expect("disable cyan");
        assert_eq!(manager.enabled_count(), 1);
        assert!((manager.total_coverage() - 0.25).abs() < 1e-6);
        manager.disable_all();
        assert_eq!(manager.enabled_count(), 0);
        manager.enable_all();
        assert_eq!(manager.enabled_count(), 2);
    }

    #[test]
    fn coverage_updates_are_validated() {
        let mut manager = InkManager::new();
        manager.add("Cyan", InkKind::Cyan, 0.0).expect("cyan");
        assert!(manager.set_coverage("Cyan", 0.75).is_ok());
        assert!((manager.find("Cyan").map_or(0.0, InkEntry::coverage) - 0.75).abs() < 1e-6);
        assert_eq!(
            manager.set_coverage("Cyan", 1.5),
            Err(PreviewError::InvalidCoverage {
                name: "Cyan".to_owned(),
                value: 1.5,
            })
        );
        assert!(matches!(
            manager.set_coverage("Cyan", f32::NAN),
            Err(PreviewError::InvalidCoverage { .. })
        ));
    }

    #[test]
    fn overprint_policy_flags_and_bits() {
        let default = OverprintPolicy::default();
        assert_eq!(default.to_bits(), 0b011);
        let custom = OverprintPolicy {
            simulate: true,
            honour_knockout: false,
            overprint_spots: true,
        };
        let bits = custom.to_bits();
        assert_eq!(bits, 0b101);
        assert_eq!(OverprintPolicy::from_bits(bits), custom);
        assert_eq!(OverprintPolicy::from_bits(0b1111_1101), custom);
    }

    #[test]
    fn process_plate_derivation_from_icc_space() {
        let cmyk = InkManager::from_icc_color_space(IccColorSpace::Cmyk).expect("CMYK plates");
        assert_eq!(cmyk.names(), vec!["Cyan", "Magenta", "Yellow", "Black"]);
        assert!(cmyk.inks().iter().all(|i| i.kind().is_process()));
        let gray = InkManager::from_icc_color_space(IccColorSpace::Gray).expect("gray plate");
        assert_eq!(gray.names(), vec!["Black"]);
        assert_eq!(
            InkManager::from_icc_color_space(IccColorSpace::Rgb),
            Err(PreviewError::UnsupportedConversion(
                "no process plates for colour space RGB ".to_owned()
            ))
        );
    }

    #[test]
    fn derive_from_output_intent() {
        let intent = OutputIntent {
            subtype: crate::color::OutputIntentSubtype::GtsPdfx,
            output_condition_identifier: None,
            info: None,
            registry_name: None,
            dest_output_profile: None,
        };
        assert!(matches!(
            InkManager::derive_from_output_intent(&intent),
            Err(PreviewError::UnsupportedConversion(_))
        ));

        // An RGB destination profile cannot yield process plates.
        let rgb = pdfx_intent(1);
        assert!(matches!(
            InkManager::derive_from_output_intent(&rgb),
            Err(PreviewError::UnsupportedConversion(_))
        ));
    }

    #[test]
    fn plan_is_deterministic() {
        let mut manager = InkManager::new();
        manager.add("Cyan", InkKind::Cyan, 0.4).expect("cyan");
        manager.add("Spot", InkKind::Spot, 0.2).expect("spot");
        let config = PreviewConfig {
            mode: PreviewMode::Separations,
            overprint: true,
            soft_proof: SoftProofIntent::RelativeColorimetric,
            ..PreviewConfig::new()
        };
        let intent = pdfx_intent(1);
        let a = PreviewPlan::build(&config, &manager, Some(&intent)).expect("plan a");
        let b = PreviewPlan::build(&config, &manager, Some(&intent)).expect("plan b");
        assert_eq!(a, b);
    }

    #[test]
    fn plan_resolves_soft_proof_and_reports_unsupported_conversion() {
        // Perceptual intent from the profile (token 0).
        let intent = pdfx_intent(0);
        let manager = InkManager::new();
        let config = PreviewConfig::new(); // soft_proof = Auto
        let plan = PreviewPlan::build(&config, &manager, Some(&intent)).expect("plan");
        assert_eq!(plan.soft_proof, SoftProofIntent::Perceptual);
        assert_eq!(plan.output_condition.as_deref(), Some("FOGRA39"));
        assert!(plan.needs_backend_cmm);
        assert!(
            plan.warnings
                .iter()
                .any(|w| w.contains("unsupported conversion"))
        );
        assert_eq!(plan.soft_proof, SoftProofIntent::Perceptual);
    }

    #[test]
    fn plan_respects_mode_overprint_and_visibility() {
        let mut manager = InkManager::new();
        manager.add("Cyan", InkKind::Cyan, 0.4).expect("cyan");
        manager
            .add("Magenta", InkKind::Magenta, 0.3)
            .expect("magenta");
        let mut config = PreviewConfig::new();
        config.mode = PreviewMode::Separation;
        config.selected_plate = Some("Cyan".to_owned());
        config.overprint = true;
        config.show_plate("Cyan").expect("show cyan");
        // Magenta is hidden by the visible set; Cyan is the selected separation.
        let plan = PreviewPlan::build(&config, &manager, None).expect("plan");
        assert_eq!(plan.mode, PreviewMode::Separation);
        assert!(plan.simulate_overprint);
        assert_eq!(plan.visible_inks.len(), 1);
        assert_eq!(plan.visible_inks[0].name, "Cyan");
        assert_eq!(plan.soft_proof, SoftProofIntent::None);
    }

    #[test]
    fn plan_honours_policy_and_disabled_inks() {
        let mut manager = InkManager::new();
        manager.add("Cyan", InkKind::Cyan, 0.4).expect("cyan");
        manager.set_enabled("Cyan", false).expect("disable");
        let policy = OverprintPolicy {
            simulate: false,
            honour_knockout: true,
            overprint_spots: false,
        };
        manager.set_policy(policy);
        let mut config = PreviewConfig::new();
        config.overprint = true;
        let plan = PreviewPlan::build(&config, &manager, None).expect("plan");
        assert!(!plan.simulate_overprint, "policy vetoes simulation");
        assert_eq!(plan.overprint_policy_bits, policy.to_bits());
        assert!(plan.visible_inks.is_empty(), "disabled ink is omitted");
    }

    #[test]
    fn empty_inputs_produce_valid_plan() {
        let config = PreviewConfig::new();
        let manager = InkManager::new();
        let plan = PreviewPlan::build(&config, &manager, None).expect("plan");
        assert!(plan.visible_inks.is_empty());
        assert!(plan.warnings.is_empty());
        assert!(!plan.needs_backend_cmm);
        assert!(!plan.simulate_overprint);
        assert_eq!(plan.soft_proof, SoftProofIntent::None);
        assert!(manager.is_empty());
        assert_eq!(manager.len(), 0);
    }

    #[test]
    fn typed_errors() {
        assert_eq!(
            InkEntry::new("  ", InkKind::Cyan, 0.0),
            Err(PreviewError::EmptyInkName)
        );
        assert_eq!(
            InkEntry::new(&"x".repeat(MAX_INK_NAME_BYTES + 1), InkKind::Cyan, 0.0),
            Err(PreviewError::InkNameTooLong {
                len: MAX_INK_NAME_BYTES + 1,
                limit: MAX_INK_NAME_BYTES,
            })
        );
        assert!(matches!(
            InkEntry::new("Cyan", InkKind::Cyan, -0.1),
            Err(PreviewError::InvalidCoverage { .. })
        ));
        let mut manager = InkManager::new();
        manager.add("Cyan", InkKind::Cyan, 0.0).expect("cyan");
        assert_eq!(
            manager.add("Cyan", InkKind::Cyan, 0.0),
            Err(PreviewError::DuplicateInk("Cyan".to_owned()))
        );
        assert_eq!(
            manager.set_enabled("Missing", true),
            Err(PreviewError::UnknownPlate("Missing".to_owned()))
        );
    }

    #[test]
    fn bounded_ink_and_visible_plate_counts() {
        let mut manager = InkManager::new();
        for i in 0..MAX_INKS {
            manager
                .add(&format!("Spot {i}"), InkKind::Spot, 0.0)
                .expect("within cap");
        }
        assert_eq!(
            manager.add("Overflow", InkKind::Spot, 0.0),
            Err(PreviewError::TooManyInks {
                count: MAX_INKS + 1,
                limit: MAX_INKS,
            })
        );
        let mut config = PreviewConfig::new();
        for i in 0..MAX_INKS {
            config.show_plate(&format!("P{i}")).expect("within cap");
        }
        assert_eq!(
            config.show_plate("overflow"),
            Err(PreviewError::TooManyVisiblePlates {
                count: MAX_INKS + 1,
                limit: MAX_INKS,
            })
        );
    }

    #[test]
    fn bad_destination_profile_is_reported_as_warning() {
        let intent = OutputIntent {
            subtype: crate::color::OutputIntentSubtype::GtsPdfx,
            output_condition_identifier: None,
            info: None,
            registry_name: None,
            dest_output_profile: Some(vec![0u8; 12]),
        };
        let plan = PreviewPlan::build(&PreviewConfig::new(), &InkManager::new(), Some(&intent))
            .expect("build succeeds");
        assert!(
            plan.warnings
                .iter()
                .any(|w| w.contains("could not be parsed"))
        );
    }
}
