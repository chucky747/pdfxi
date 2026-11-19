//! Dependency-light ICC colour management and PDF output intents.
//!
//! This module is deliberately **not** a full colour-management engine. It
//! provides the three backend-agnostic pieces that a renderer needs before it
//! can apply a colour transform:
//!
//! 1. A bounded, hostile-input-safe [`IccProfile`] parser. It reads the 128-byte
//!    header, the tag table, and only the tags required for intent, description,
//!    white-point and tone-reproduction-curve (TRC) queries. Profiles are capped
//!    in size and every offset is bounds-checked; malformed input produces a
//!    typed [`ColorError`] and never panics.
//! 2. An output-intent model ([`OutputIntentSet`]) that parses the dictionary
//!    fields of `/OutputIntents` entries, detects missing, multiple or
//!    unsupported intents, and validates a single usable intent.
//! 3. A rendering colour contract ([`DeviceColor`]) in which every device
//!    colour carries an explicit colour space, plus a deterministic
//!    [`ConversionPlan`] builder that extracts an RGB matrix/TRC plan or a gray
//!    gamma from a profile.
//!
//! # Backend boundary
//!
//! The matrix/TRC and gray-gamma plans are intentionally small: they encode the
//! common, analytically describable part of a colour transform. Full ICC
//! colour-management-module (CMM) rendering — arbitrary LUT tags, parametric
//! curves beyond a single gamma, chromatic adaptation, and multi-process
//! profiles — is **delegated to the renderer backend**. Plans that cannot be
//! expressed analytically are reported as [`ConversionPlan::NeedsBackend`] with
//! a human-readable reason rather than approximated. The optional PDFium
//! backend is the intended consumer of that boundary.
//!
//! # Determinism and safety
//!
//! All arithmetic is performed in `f64` and rounded to `f32` only at the end;
//! there is no randomness, wall-clock dependence, image decoding, or unsafe
//! code. Parsing the same bytes and building the same plan twice yields
//! byte-identical results.

use std::fmt;

/// Maximum accepted ICC profile size in bytes (16 MiB).
///
/// Profiles larger than this are rejected with [`ColorError::OversizeProfile`]
/// before any tag data is read, so a hostile document cannot force unbounded
/// allocation.
pub const MAX_ICC_PROFILE_BYTES: usize = 16 * 1024 * 1024;

/// Maximum number of tag-table entries accepted in one profile.
pub const MAX_ICC_TAG_COUNT: usize = 1024;

/// The fixed size of an ICC profile header in bytes.
pub const ICC_HEADER_SIZE: usize = 128;

/// Minimum size of a structurally valid profile: header plus a 4-byte tag count.
pub const MIN_ICC_PROFILE_BYTES: usize = ICC_HEADER_SIZE + 4;

/// The signature identifying an ICC profile, `acsp`.
const ICC_SIGNATURE: [u8; 4] = *b"acsp";

/// Structured errors produced while parsing ICC profiles, validating output
/// intents, or building/applying a colour-conversion plan.
#[derive(Debug, thiserror::Error, PartialEq)]
pub enum ColorError {
    /// A required region of a profile or tag was shorter than expected.
    #[error("ICC data is truncated: need {needed} bytes, have {available} ({field})")]
    Truncated {
        /// Number of bytes required.
        needed: usize,
        /// Number of bytes actually available.
        available: usize,
        /// Name of the field being read.
        field: &'static str,
    },
    /// The profile exceeds the configured size cap.
    #[error("ICC profile size {size} exceeds the {limit}-byte limit")]
    OversizeProfile {
        /// Declared or actual profile size.
        size: usize,
        /// The enforced limit.
        limit: usize,
    },
    /// The `acsp` profile signature was absent or corrupt.
    #[error("invalid ICC signature: expected 'acsp', found {found:?}")]
    InvalidSignature {
        /// The four bytes found at the signature offset.
        found: [u8; 4],
    },
    /// The header's declared size does not fit in the supplied buffer.
    #[error("ICC header declares {declared} bytes but only {actual} are available")]
    SizeMismatch {
        /// Size declared in the profile header.
        declared: usize,
        /// Number of bytes actually supplied.
        actual: usize,
    },
    /// The tag table declares more entries than the cap allows.
    #[error("ICC tag table declares {count} tags, above the {limit}-tag limit")]
    TooManyTags {
        /// Declared tag count.
        count: usize,
        /// Enforced tag-count limit.
        limit: usize,
    },
    /// A tag's offset/size pair does not lie inside the profile.
    #[error(
        "ICC tag {tag:?} at offset {offset} with size {size} is outside a {profile_size}-byte profile"
    )]
    TagOutOfBounds {
        /// The offending tag signature.
        tag: [u8; 4],
        /// Declared data offset.
        offset: usize,
        /// Declared data size.
        size: usize,
        /// Total profile size.
        profile_size: usize,
    },
    /// A queried tag exists but carries no data.
    #[error("ICC tag {tag:?} has zero length")]
    ZeroLengthTag {
        /// The offending tag signature.
        tag: [u8; 4],
    },
    /// A tag's payload is structurally malformed for its type.
    #[error("ICC tag {tag:?} is malformed: {reason}")]
    InvalidTagData {
        /// The offending tag signature.
        tag: [u8; 4],
        /// Why the data is unusable.
        reason: &'static str,
    },
    /// A required tag is absent from the profile.
    #[error("ICC profile is missing the required {tag:?} tag")]
    MissingTag {
        /// The absent tag signature.
        tag: [u8; 4],
    },
    /// A device colour has the wrong number of components for its space.
    #[error("colour space expects {expected} components but got {got}")]
    ComponentCountMismatch {
        /// Required component count.
        expected: usize,
        /// Supplied component count.
        got: usize,
    },
    /// No `/OutputIntents` entry was supplied.
    #[error("no PDF output intent was supplied")]
    MissingOutputIntent,
    /// More than one `/OutputIntents` entry was supplied.
    #[error("expected exactly one PDF output intent but found {count}")]
    MultipleOutputIntents {
        /// Number of supplied intents.
        count: usize,
    },
    /// The output intent subtype is not one this crate recognises.
    #[error("unsupported output intent subtype: {0}")]
    UnsupportedOutputIntentSubtype(String),
    /// The output intent lacks a `/DestOutputProfile`.
    #[error("PDF output intent has no /DestOutputProfile")]
    MissingDestOutputProfile,
    /// The requested conversion needs a full CMM in the renderer backend.
    #[error("conversion requires a renderer backend: {0}")]
    BackendRequired(String),
}

/// Read a big-endian `u32` at `offset`, with bounds checking.
fn be_u32(bytes: &[u8], offset: usize) -> Result<u32, ColorError> {
    let end = offset.checked_add(4).ok_or(ColorError::Truncated {
        needed: 4,
        available: bytes.len(),
        field: "u32 offset",
    })?;
    let slice = bytes.get(offset..end).ok_or(ColorError::Truncated {
        needed: end,
        available: bytes.len(),
        field: "u32",
    })?;
    Ok(u32::from_be_bytes([slice[0], slice[1], slice[2], slice[3]]))
}

/// Read a big-endian `u16` at `offset`, with bounds checking.
fn be_u16(bytes: &[u8], offset: usize) -> Result<u16, ColorError> {
    let end = offset.checked_add(2).ok_or(ColorError::Truncated {
        needed: 2,
        available: bytes.len(),
        field: "u16 offset",
    })?;
    let slice = bytes.get(offset..end).ok_or(ColorError::Truncated {
        needed: end,
        available: bytes.len(),
        field: "u16",
    })?;
    Ok(u16::from_be_bytes([slice[0], slice[1]]))
}

/// Read a four-byte signature at `offset`, with bounds checking.
fn signature(bytes: &[u8], offset: usize) -> Result<[u8; 4], ColorError> {
    let end = offset.checked_add(4).ok_or(ColorError::Truncated {
        needed: 4,
        available: bytes.len(),
        field: "signature offset",
    })?;
    let slice = bytes.get(offset..end).ok_or(ColorError::Truncated {
        needed: end,
        available: bytes.len(),
        field: "signature",
    })?;
    Ok([slice[0], slice[1], slice[2], slice[3]])
}

/// Read a signed `s15Fixed16` number at `offset`.
fn s15_fixed16(bytes: &[u8], offset: usize) -> Result<f32, ColorError> {
    let raw = be_u32(bytes, offset)? as i32;
    Ok((raw as f64 / 65536.0_f64) as f32)
}

/// Render a four-byte ICC signature as a printable ASCII string.
fn sig_str(sig: [u8; 4]) -> String {
    String::from_utf8_lossy(&sig).into_owned()
}

/// The ICC device-class field (header bytes 12..16).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeviceClass {
    /// Input device profile, such as a scanner or camera.
    Input,
    /// Display device profile, such as a monitor.
    Display,
    /// Output device profile, such as a printer.
    Output,
    /// Device-link (transform) profile.
    Link,
    /// Colour-space conversion profile.
    ColorSpace,
    /// Abstract profile, such as a colour-temperature transform.
    Abstract,
    /// Named colour profile.
    Named,
    /// A class not recognised by this crate; the raw signature is preserved.
    Unknown([u8; 4]),
}

impl DeviceClass {
    /// Map a raw four-byte signature to a device class.
    pub fn from_sig(sig: [u8; 4]) -> Self {
        match &sig {
            b"scnr" => Self::Input,
            b"mntr" => Self::Display,
            b"prtr" => Self::Output,
            b"link" => Self::Link,
            b"spac" => Self::ColorSpace,
            b"abst" => Self::Abstract,
            b"nmcl" => Self::Named,
            _ => Self::Unknown(sig),
        }
    }
}

/// The ICC data colour space (header bytes 16..20).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IccColorSpace {
    /// CIE XYZ.
    Xyz,
    /// CIE Lab.
    Lab,
    /// CIE Luv.
    Luv,
    /// CIE Yxy.
    Yxy,
    /// RGB.
    Rgb,
    /// Gray.
    Gray,
    /// HSV.
    Hsv,
    /// CMY.
    Cmy,
    /// CMYK.
    Cmyk,
    /// A two-channel colour space.
    TwoChannel,
    /// A three-channel colour space.
    ThreeChannel,
    /// A four-channel colour space.
    FourChannel,
    /// A colour space not recognised by this crate; the signature is preserved.
    Unknown([u8; 4]),
}

impl IccColorSpace {
    /// Map a raw four-byte signature to a colour space.
    pub fn from_sig(sig: [u8; 4]) -> Self {
        match &sig {
            b"XYZ " => Self::Xyz,
            b"Lab " => Self::Lab,
            b"Luv " => Self::Luv,
            b"Yxy " => Self::Yxy,
            b"RGB " => Self::Rgb,
            b"GRAY" => Self::Gray,
            b"HSV " => Self::Hsv,
            b"CMY " => Self::Cmy,
            b"CMYK" => Self::Cmyk,
            b"2CLR" => Self::TwoChannel,
            b"3CLR" => Self::ThreeChannel,
            b"4CLR" => Self::FourChannel,
            _ => Self::Unknown(sig),
        }
    }

    /// Return the canonical four-character signature for this space.
    pub fn name(self) -> String {
        match self {
            Self::Xyz => "XYZ ".to_owned(),
            Self::Lab => "Lab ".to_owned(),
            Self::Luv => "Luv ".to_owned(),
            Self::Yxy => "Yxy ".to_owned(),
            Self::Rgb => "RGB ".to_owned(),
            Self::Gray => "GRAY".to_owned(),
            Self::Hsv => "HSV ".to_owned(),
            Self::Cmy => "CMY ".to_owned(),
            Self::Cmyk => "CMYK".to_owned(),
            Self::TwoChannel => "2CLR".to_owned(),
            Self::ThreeChannel => "3CLR".to_owned(),
            Self::FourChannel => "4CLR".to_owned(),
            Self::Unknown(sig) => sig_str(sig),
        }
    }
}

impl fmt::Display for IccColorSpace {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.name())
    }
}

/// The ICC profile connection space (header bytes 20..24).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pcs {
    /// PCS is CIE XYZ.
    Xyz,
    /// PCS is CIE Lab.
    Lab,
    /// A PCS not recognised by this crate; the signature is preserved.
    Unknown([u8; 4]),
}

impl Pcs {
    /// Map a raw four-byte signature to a profile connection space.
    pub fn from_sig(sig: [u8; 4]) -> Self {
        match &sig {
            b"XYZ " => Self::Xyz,
            b"Lab " => Self::Lab,
            _ => Self::Unknown(sig),
        }
    }
}

/// The rendering intent recorded in the ICC header (byte 64).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RenderingIntent {
    /// Perceptual intent.
    Perceptual,
    /// Media-relative colorimetric intent.
    RelativeColorimetric,
    /// Saturation intent.
    Saturation,
    /// Absolute colorimetric intent.
    AbsoluteColorimetric,
    /// A value outside the standard `0..=3` range.
    Unknown(u32),
}

impl RenderingIntent {
    /// Map a raw 32-bit value to a rendering intent.
    pub fn from_u32(value: u32) -> Self {
        match value {
            0 => Self::Perceptual,
            1 => Self::RelativeColorimetric,
            2 => Self::Saturation,
            3 => Self::AbsoluteColorimetric,
            _ => Self::Unknown(value),
        }
    }
}

/// A CIE XYZ triple used for white points and colourants.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Xyz {
    /// The X component.
    pub x: f32,
    /// The Y (luminance) component.
    pub y: f32,
    /// The Z component.
    pub z: f32,
}

impl Xyz {
    /// Read three consecutive `s15Fixed16` numbers at `offset`.
    fn read_numbers(bytes: &[u8], offset: usize) -> Result<Self, ColorError> {
        Ok(Self {
            x: s15_fixed16(bytes, offset)?,
            y: s15_fixed16(bytes, offset.saturating_add(4))?,
            z: s15_fixed16(bytes, offset.saturating_add(8))?,
        })
    }

    /// Read an ICC `XYZType` tag payload (signature, reserved word, three
    /// fixed-point numbers).
    fn read_tag(bytes: &[u8], tag: [u8; 4]) -> Result<Self, ColorError> {
        let sig = signature(bytes, 0)?;
        if &sig != b"XYZ " {
            return Err(ColorError::InvalidTagData {
                tag,
                reason: "expected 'XYZ ' type",
            });
        }
        Self::read_numbers(bytes, 8)
    }
}

/// A tone reproduction curve (ICC `curveType` or parametric type 0).
#[derive(Clone, Debug, PartialEq)]
pub enum Trc {
    /// An identity curve (`y = x`).
    Identity,
    /// A simple power curve (`y = x^gamma`).
    Gamma(f32),
    /// A sampled curve with values linearly interpolated between samples.
    Table(Vec<f32>),
}

impl Trc {
    /// Evaluate the curve at normalised input `value`.
    ///
    /// Inputs outside `0.0..=1.0` are clamped. Table curves use deterministic
    /// linear interpolation between adjacent 16-bit samples.
    pub fn evaluate(&self, value: f32) -> f32 {
        let v = value as f64;
        let clamped = v.clamp(0.0, 1.0);
        let result = match self {
            Self::Identity => clamped,
            Self::Gamma(g) => clamped.powf(f64::from(*g)),
            Self::Table(samples) => {
                if samples.len() == 1 {
                    f64::from(samples[0])
                } else if samples.is_empty() {
                    clamped
                } else {
                    let scaled = clamped * (samples.len() as f64 - 1.0);
                    let lower = scaled.floor() as usize;
                    let upper = (lower + 1).min(samples.len() - 1);
                    let fraction = scaled - lower as f64;
                    let a = f64::from(samples[lower]);
                    let b = f64::from(samples[upper]);
                    a + (b - a) * fraction
                }
            }
        };
        result as f32
    }

    /// Read a TRC from an ICC tag payload.
    fn read_tag(bytes: &[u8], tag: [u8; 4]) -> Result<Self, ColorError> {
        let sig = signature(bytes, 0)?;
        match &sig {
            b"curv" => {
                let count = be_u32(bytes, 8)? as usize;
                match count {
                    0 => Ok(Self::Identity),
                    1 => {
                        let raw = be_u16(bytes, 12)?;
                        Ok(Self::Gamma((f64::from(raw) / 256.0_f64) as f32))
                    }
                    n => {
                        let end = 12usize
                            .checked_add(n.checked_mul(2).ok_or(ColorError::InvalidTagData {
                                tag,
                                reason: "TRC table size overflow",
                            })?)
                            .ok_or(ColorError::InvalidTagData {
                                tag,
                                reason: "TRC table size overflow",
                            })?;
                        if end > bytes.len() {
                            return Err(ColorError::Truncated {
                                needed: end,
                                available: bytes.len(),
                                field: "curv table",
                            });
                        }
                        let mut samples = Vec::with_capacity(n);
                        for index in 0..n {
                            let raw = be_u16(bytes, 12 + index * 2)?;
                            samples.push((f64::from(raw) / 65535.0_f64) as f32);
                        }
                        Ok(Self::Table(samples))
                    }
                }
            }
            b"para" => {
                // Only the simplest parametric form (Y = X^g) is analytic.
                let function = be_u16(bytes, 8)?;
                if function != 0 {
                    return Err(ColorError::InvalidTagData {
                        tag,
                        reason: "unsupported parametric TRC function",
                    });
                }
                let gamma = s15_fixed16(bytes, 12)?;
                Ok((gamma > 0.0).then_some(Self::Gamma(gamma)).ok_or(
                    ColorError::InvalidTagData {
                        tag,
                        reason: "non-positive parametric gamma",
                    },
                )?)
            }
            _ => Err(ColorError::InvalidTagData {
                tag,
                reason: "unsupported TRC type",
            }),
        }
    }
}

/// A row-major 3x3 matrix mapping linear device RGB to PCS XYZ.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RgbMatrix {
    /// Nine row-major coefficients: `[r.x, g.x, b.x, r.y, g.y, b.y, r.z, g.z, b.z]`.
    pub m: [f32; 9],
}

impl RgbMatrix {
    /// Build a matrix from the three colourant XYZ values, used as columns.
    pub fn from_colorants(r: Xyz, g: Xyz, b: Xyz) -> Self {
        Self {
            m: [
                r.x, g.x, b.x, //
                r.y, g.y, b.y, //
                r.z, g.z, b.z, //
            ],
        }
    }

    /// Apply the matrix to a linear RGB triple, returning XYZ.
    ///
    /// Computation is performed in `f64` and rounded once to `f32`.
    pub fn apply(&self, rgb: [f32; 3]) -> [f32; 3] {
        let r = f64::from(rgb[0]);
        let g = f64::from(rgb[1]);
        let b = f64::from(rgb[2]);
        let m = self.m.map(f64::from);
        [
            (m[0] * r + m[1] * g + m[2] * b) as f32,
            (m[3] * r + m[4] * g + m[5] * b) as f32,
            (m[6] * r + m[7] * g + m[8] * b) as f32,
        ]
    }
}

/// An analytic RGB-to-PCS plan: per-channel TRCs followed by a 3x3 matrix.
#[derive(Clone, Debug, PartialEq)]
pub struct RgbMatrixTrc {
    /// The linear-RGB to PCS-XYZ matrix.
    pub matrix: RgbMatrix,
    /// Red-channel tone curve.
    pub r: Trc,
    /// Green-channel tone curve.
    pub g: Trc,
    /// Blue-channel tone curve.
    pub b: Trc,
}

/// A deterministic, backend-independent colour conversion plan.
///
/// A plan is produced by [`ConversionPlan::from_profile`]. Plans that cannot be
/// expressed analytically are reported as [`Self::NeedsBackend`] so the caller
/// can route them to a full CMM rather than silently approximating the result.
#[derive(Clone, Debug, PartialEq)]
pub enum ConversionPlan {
    /// No transform is needed (for example an XYZ or Lab PCS profile).
    Identity,
    /// Apply RGB TRCs then a 3x3 matrix.
    RgbMatrixTrc(RgbMatrixTrc),
    /// Apply a single gamma to a gray channel.
    GrayGamma(f32),
    /// A transform that needs the renderer backend's colour-management module.
    NeedsBackend {
        /// Why an analytic plan was not possible.
        reason: String,
    },
}

impl ConversionPlan {
    /// Derive an analytic plan from an ICC profile.
    ///
    /// RGB profiles with a colourant matrix and `rTRC`/`gTRC`/`bTRC` tags yield
    /// [`Self::RgbMatrixTrc`]. Gray profiles with a gamma `kTRC` yield
    /// [`Self::GrayGamma`]. XYZ/Lab profiles are [`Self::Identity`]; everything
    /// else is [`Self::NeedsBackend`]. This function is deterministic and never
    /// fails: unsupported profiles are reported as a plan, not an error.
    pub fn from_profile(profile: &IccProfile) -> Self {
        match profile.header().data_color_space {
            IccColorSpace::Rgb => {
                let matrix = profile.colorant_matrix();
                let r = profile.trc(*b"rTRC").ok();
                let g = profile.trc(*b"gTRC").ok();
                let b = profile.trc(*b"bTRC").ok();
                match (matrix, r, g, b) {
                    (Some(matrix), Some(r), Some(g), Some(b)) => {
                        Self::RgbMatrixTrc(RgbMatrixTrc { matrix, r, g, b })
                    }
                    _ => Self::NeedsBackend {
                        reason: "RGB profile lacks a colourant matrix or RGB TRC tags".to_owned(),
                    },
                }
            }
            IccColorSpace::Gray => match profile.trc(*b"kTRC") {
                Ok(Trc::Gamma(gamma)) => Self::GrayGamma(gamma),
                Ok(Trc::Identity) => Self::Identity,
                _ => Self::NeedsBackend {
                    reason: "gray profile lacks an analytic kTRC gamma".to_owned(),
                },
            },
            IccColorSpace::Xyz | IccColorSpace::Lab => Self::Identity,
            other => Self::NeedsBackend {
                reason: format!("no analytic plan for colour space {}", other.name()),
            },
        }
    }

    /// Convert normalised colour components through this plan.
    ///
    /// RGB plans expect three components and return a PCS XYZ triple. Gray
    /// plans expect one component and return the linearised luminance. Identity
    /// plans pass components through unchanged. A [`Self::NeedsBackend`] plan
    /// returns [`ColorError::BackendRequired`].
    pub fn convert(&self, components: &[f32]) -> Result<Vec<f32>, ColorError> {
        match self {
            Self::Identity => Ok(components.to_vec()),
            Self::RgbMatrixTrc(plan) => {
                if components.len() != 3 {
                    return Err(ColorError::ComponentCountMismatch {
                        expected: 3,
                        got: components.len(),
                    });
                }
                let linear = [
                    plan.r.evaluate(components[0]),
                    plan.g.evaluate(components[1]),
                    plan.b.evaluate(components[2]),
                ];
                Ok(plan.matrix.apply(linear).to_vec())
            }
            Self::GrayGamma(gamma) => {
                if components.len() != 1 {
                    return Err(ColorError::ComponentCountMismatch {
                        expected: 1,
                        got: components.len(),
                    });
                }
                let curve = Trc::Gamma(*gamma);
                Ok(vec![curve.evaluate(components[0])])
            }
            Self::NeedsBackend { reason } => Err(ColorError::BackendRequired(reason.clone())),
        }
    }
}

/// The colour space a device colour is defined in.
///
/// Encoding the space alongside the components avoids the common bug of
/// interpreting, for example, a CMYK quadruple as RGB.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DeviceColorSpace {
    /// Device gray, one component (`0.0` black to `1.0` white).
    DeviceGray,
    /// Device RGB, three components.
    DeviceRgb,
    /// Device CMYK, four components.
    DeviceCmyk,
    /// Calibrated gray, one component.
    CalGray,
    /// Calibrated RGB, three components.
    CalRgb,
    /// CIE Lab, three components.
    Lab,
    /// An ICCBased space; the count depends on the embedded profile.
    IccBased {
        /// The profile description, when it was decoded successfully.
        description: Option<String>,
    },
    /// An indexed (palette) space, one component.
    Indexed {
        /// Highest palette index, inclusive.
        max_index: usize,
    },
    /// A separation (spot) colour, one component.
    Separation,
    /// A DeviceN space with an explicit component count.
    DeviceN {
        /// Number of colourants.
        n: usize,
    },
    /// A pattern colour space, whose component count is not fixed here.
    Pattern,
}

impl DeviceColorSpace {
    /// Return the expected component count when it is fixed by the space.
    pub fn expected_components(&self) -> Option<usize> {
        match self {
            Self::DeviceGray | Self::CalGray | Self::Indexed { .. } | Self::Separation => Some(1),
            Self::DeviceRgb | Self::CalRgb | Self::Lab => Some(3),
            Self::DeviceCmyk => Some(4),
            Self::DeviceN { n } => Some(*n),
            Self::IccBased { .. } | Self::Pattern => None,
        }
    }
}

/// A device colour: numeric components paired with an explicit colour space.
#[derive(Clone, Debug, PartialEq)]
pub struct DeviceColor {
    /// The colour space the components are expressed in.
    pub space: DeviceColorSpace,
    /// Normalised component values.
    pub components: Vec<f32>,
}

impl DeviceColor {
    /// Create a device colour, validating the component count when the space
    /// fixes one.
    pub fn new(space: DeviceColorSpace, components: Vec<f32>) -> Result<Self, ColorError> {
        if let Some(expected) = space.expected_components()
            && expected != components.len()
        {
            return Err(ColorError::ComponentCountMismatch {
                expected,
                got: components.len(),
            });
        }
        Ok(Self { space, components })
    }

    /// Return the number of stored components.
    pub fn component_count(&self) -> usize {
        self.components.len()
    }

    /// Convert this colour through `plan`, delegating the interpretation of the
    /// source space to the caller (the plan describes the target transform).
    pub fn convert(&self, plan: &ConversionPlan) -> Result<Vec<f32>, ColorError> {
        plan.convert(&self.components)
    }
}

/// The parsed 128-byte ICC header.
#[derive(Clone, Debug, PartialEq)]
pub struct IccHeader {
    /// Profile size as declared in bytes `0..4`.
    pub size: u32,
    /// Preferred CMM signature from bytes `4..8`.
    pub cmm: [u8; 4],
    /// Raw version bytes `8..12` (major, minor/flags, and two reserved bytes).
    pub version: [u8; 4],
    /// Device class from bytes `12..16`.
    pub device_class: DeviceClass,
    /// Data colour space from bytes `16..20`.
    pub data_color_space: IccColorSpace,
    /// Profile connection space from bytes `20..24`.
    pub pcs: Pcs,
    /// Rendering intent from byte `64`.
    pub rendering_intent: RenderingIntent,
    /// The profile illuminant from bytes `68..80`.
    pub illuminant: Xyz,
}

impl IccHeader {
    /// Return the major version number (first version byte).
    pub fn version_major(&self) -> u8 {
        self.version[0]
    }

    /// Return the minor version number (high nibble of the second version byte).
    pub fn version_minor(&self) -> u8 {
        self.version[1] >> 4
    }

    /// Return the preferred CMM as a printable string, or `"?"` if it is not
    /// valid ASCII.
    pub fn cmm_name(&self) -> String {
        sig_str(self.cmm)
    }
}

/// One entry of an ICC tag table: a signature plus an offset/size pair into the
/// profile.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TagEntry {
    /// Four-byte tag signature, for example `*b"desc"`.
    pub signature: [u8; 4],
    /// Byte offset of the tag payload within the profile.
    pub offset: usize,
    /// Byte length of the tag payload.
    pub size: usize,
}

/// A bounded, parsed ICC profile.
///
/// Construct one with [`Self::parse`]. The parser caps the profile size and tag
/// count, validates the `acsp` signature, and bounds-checks every tag so that
/// later queries cannot read outside the buffer.
#[derive(Clone, Debug, PartialEq)]
pub struct IccProfile {
    bytes: Vec<u8>,
    header: IccHeader,
    tags: Vec<TagEntry>,
}

impl IccProfile {
    /// Parse an ICC profile from `bytes`.
    ///
    /// # Errors
    ///
    /// Returns a typed [`ColorError`] when the buffer is truncated, exceeds
    /// [`MAX_ICC_PROFILE_BYTES`], has a bad signature, declares an inconsistent
    /// size, declares too many tags, or contains a tag outside the profile.
    pub fn parse(bytes: &[u8]) -> Result<Self, ColorError> {
        if bytes.len() > MAX_ICC_PROFILE_BYTES {
            return Err(ColorError::OversizeProfile {
                size: bytes.len(),
                limit: MAX_ICC_PROFILE_BYTES,
            });
        }
        if bytes.len() < MIN_ICC_PROFILE_BYTES {
            return Err(ColorError::Truncated {
                needed: MIN_ICC_PROFILE_BYTES,
                available: bytes.len(),
                field: "ICC header and tag count",
            });
        }
        let declared = be_u32(bytes, 0)? as usize;
        if declared > MAX_ICC_PROFILE_BYTES {
            return Err(ColorError::OversizeProfile {
                size: declared,
                limit: MAX_ICC_PROFILE_BYTES,
            });
        }
        if declared < MIN_ICC_PROFILE_BYTES {
            return Err(ColorError::Truncated {
                needed: MIN_ICC_PROFILE_BYTES,
                available: declared,
                field: "declared ICC size",
            });
        }
        if declared > bytes.len() {
            return Err(ColorError::SizeMismatch {
                declared,
                actual: bytes.len(),
            });
        }
        let profile = &bytes[..declared];

        let sig = signature(profile, 36)?;
        if sig != ICC_SIGNATURE {
            return Err(ColorError::InvalidSignature { found: sig });
        }

        let header = IccHeader {
            size: declared as u32,
            cmm: signature(profile, 4)?,
            version: [profile[8], profile[9], profile[10], profile[11]],
            device_class: DeviceClass::from_sig(signature(profile, 12)?),
            data_color_space: IccColorSpace::from_sig(signature(profile, 16)?),
            pcs: Pcs::from_sig(signature(profile, 20)?),
            rendering_intent: RenderingIntent::from_u32(be_u32(profile, 64)?),
            illuminant: Xyz::read_numbers(profile, 68)?,
        };

        let count = be_u32(profile, ICC_HEADER_SIZE)? as usize;
        if count > MAX_ICC_TAG_COUNT {
            return Err(ColorError::TooManyTags {
                count,
                limit: MAX_ICC_TAG_COUNT,
            });
        }
        let table_end = ICC_HEADER_SIZE
            .checked_add(4)
            .and_then(|base| base.checked_add(count.checked_mul(12)?))
            .ok_or(ColorError::Truncated {
                needed: declared.saturating_add(1),
                available: declared,
                field: "tag table size",
            })?;
        if table_end > declared {
            return Err(ColorError::Truncated {
                needed: table_end,
                available: declared,
                field: "tag table",
            });
        }

        let mut tags = Vec::with_capacity(count);
        for index in 0..count {
            let base = ICC_HEADER_SIZE + 4 + index * 12;
            let tag_sig = signature(profile, base)?;
            let offset = be_u32(profile, base + 4)? as usize;
            let size = be_u32(profile, base + 8)? as usize;
            let end = offset.checked_add(size).ok_or(ColorError::TagOutOfBounds {
                tag: tag_sig,
                offset,
                size,
                profile_size: declared,
            })?;
            if offset > declared || end > declared {
                return Err(ColorError::TagOutOfBounds {
                    tag: tag_sig,
                    offset,
                    size,
                    profile_size: declared,
                });
            }
            tags.push(TagEntry {
                signature: tag_sig,
                offset,
                size,
            });
        }

        Ok(Self {
            bytes: profile.to_vec(),
            header,
            tags,
        })
    }

    /// Return the parsed header.
    pub fn header(&self) -> &IccHeader {
        &self.header
    }

    /// Return every tag entry in declaration order.
    pub fn tags(&self) -> &[TagEntry] {
        &self.tags
    }

    /// Find the first tag with signature `sig`.
    pub fn find_tag(&self, sig: [u8; 4]) -> Option<&TagEntry> {
        self.tags.iter().find(|entry| entry.signature == sig)
    }

    /// Return a tag's payload, re-validating its bounds and rejecting zero-length
    /// data.
    pub fn tag_data(&self, entry: &TagEntry) -> Result<&[u8], ColorError> {
        if entry.size == 0 {
            return Err(ColorError::ZeroLengthTag {
                tag: entry.signature,
            });
        }
        let end = entry
            .offset
            .checked_add(entry.size)
            .ok_or(ColorError::TagOutOfBounds {
                tag: entry.signature,
                offset: entry.offset,
                size: entry.size,
                profile_size: self.bytes.len(),
            })?;
        self.bytes
            .get(entry.offset..end)
            .ok_or(ColorError::TagOutOfBounds {
                tag: entry.signature,
                offset: entry.offset,
                size: entry.size,
                profile_size: self.bytes.len(),
            })
    }

    /// Return the payload of the tag with signature `sig`, or a typed error.
    pub fn tag_bytes(&self, sig: [u8; 4]) -> Result<&[u8], ColorError> {
        let entry = self
            .find_tag(sig)
            .ok_or(ColorError::MissingTag { tag: sig })?;
        self.tag_data(entry)
    }

    /// Read the `desc` tag as a human-readable description.
    pub fn description(&self) -> Result<String, ColorError> {
        let data = self.tag_bytes(*b"desc")?;
        let count = be_u32(data, 0)? as usize;
        let end = 4usize
            .checked_add(count)
            .ok_or(ColorError::InvalidTagData {
                tag: *b"desc",
                reason: "description length overflow",
            })?;
        if end > data.len() {
            return Err(ColorError::InvalidTagData {
                tag: *b"desc",
                reason: "description ASCII count exceeds tag size",
            });
        }
        let ascii = data.get(4..end).ok_or(ColorError::InvalidTagData {
            tag: *b"desc",
            reason: "description ASCII slice unavailable",
        })?;
        Ok(String::from_utf8_lossy(ascii).into_owned())
    }

    /// Read the `wtpt` (media white point) tag.
    pub fn whitepoint(&self) -> Result<Xyz, ColorError> {
        self.xyz_tag(*b"wtpt")
    }

    /// Read an `XYZType` tag with signature `sig`.
    pub fn xyz_tag(&self, sig: [u8; 4]) -> Result<Xyz, ColorError> {
        let data = self.tag_bytes(sig)?;
        Xyz::read_tag(data, sig)
    }

    /// Read a TRC tag with signature `sig`.
    pub fn trc(&self, sig: [u8; 4]) -> Result<Trc, ColorError> {
        let data = self.tag_bytes(sig)?;
        Trc::read_tag(data, sig)
    }

    /// Build the RGB-to-PCS matrix from the `rXYZ`, `gXYZ` and `bXYZ` tags.
    ///
    /// The three colourants are used as matrix columns. This is the analytic
    /// part of the transform; chromatic adaptation is left to the backend.
    pub fn colorant_matrix(&self) -> Option<RgbMatrix> {
        let r = self.xyz_tag(*b"rXYZ").ok()?;
        let g = self.xyz_tag(*b"gXYZ").ok()?;
        let b = self.xyz_tag(*b"bXYZ").ok()?;
        Some(RgbMatrix::from_colorants(r, g, b))
    }

    /// Derive a deterministic [`ConversionPlan`] from this profile.
    pub fn conversion_plan(&self) -> ConversionPlan {
        ConversionPlan::from_profile(self)
    }
}

/// A recognised PDF output-intent subtype.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OutputIntentSubtype {
    /// The PDF/X generalized subtype.
    GtsPdfx,
    /// The PDF/A-1 output intent subtype.
    GtsPdfa1,
    /// The PDF/E-1 output intent subtype.
    IsoPdfE1,
    /// The PPIF (PDF Print Interchange Format) subtype.
    GtsPpif,
    /// A subtype not recognised by this crate; the raw name is preserved.
    Unknown(String),
}

impl OutputIntentSubtype {
    /// Parse a subtype from its PDF name, with or without a leading slash.
    pub fn parse(value: &str) -> Self {
        let trimmed = value.trim_start_matches('/');
        match trimmed {
            "GTS_PDFX" => Self::GtsPdfx,
            "GTS_PDFA1" => Self::GtsPdfa1,
            "ISO_PDFE1" => Self::IsoPdfE1,
            "GTS_PPIF" => Self::GtsPpif,
            _ => Self::Unknown(trimmed.to_owned()),
        }
    }

    /// Return `true` when this is a subtype this crate recognises.
    pub fn is_supported(&self) -> bool {
        !matches!(self, Self::Unknown(_))
    }

    /// Return the canonical subtype name.
    pub fn name(&self) -> String {
        match self {
            Self::GtsPdfx => "GTS_PDFX".to_owned(),
            Self::GtsPdfa1 => "GTS_PDFA1".to_owned(),
            Self::IsoPdfE1 => "ISO_PDFE1".to_owned(),
            Self::GtsPpif => "GTS_PPIF".to_owned(),
            Self::Unknown(name) => name.clone(),
        }
    }
}

impl fmt::Display for OutputIntentSubtype {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.name())
    }
}

/// A decoded but unvalidated `/OutputIntents` dictionary entry.
///
/// This crate does not depend on a PDF object model, so callers pass the
/// dictionary's fields already decoded into strings and bytes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RawOutputIntent {
    /// The `/S` subtype name, for example `"/GTS_PDFX"`.
    pub subtype: String,
    /// The human-readable `/OutputConditionIdentifier`.
    pub output_condition_identifier: Option<String>,
    /// The free-form `/Info` string.
    pub info: Option<String>,
    /// The `/RegistryName` string.
    pub registry_name: Option<String>,
    /// Raw bytes of the `/DestOutputProfile` stream, when present.
    pub dest_output_profile: Option<Vec<u8>>,
}

/// A validated-shape output intent carrying its destination profile bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutputIntent {
    /// Parsed `/S` subtype.
    pub subtype: OutputIntentSubtype,
    /// The human-readable `/OutputConditionIdentifier`.
    pub output_condition_identifier: Option<String>,
    /// The free-form `/Info` string.
    pub info: Option<String>,
    /// The `/RegistryName` string.
    pub registry_name: Option<String>,
    /// Raw bytes of the `/DestOutputProfile` stream, when present.
    pub dest_output_profile: Option<Vec<u8>>,
}

impl OutputIntent {
    /// Parse the embedded `/DestOutputProfile` into an [`IccProfile`].
    ///
    /// Returns `Ok(None)` when the intent has no destination profile and a
    /// typed [`ColorError`] when the bytes present are not a valid profile.
    pub fn parse_dest_profile(&self) -> Result<Option<IccProfile>, ColorError> {
        match &self.dest_output_profile {
            Some(bytes) => IccProfile::parse(bytes).map(Some),
            None => Ok(None),
        }
    }
}

/// A detected problem with a set of output intents.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OutputIntentIssue {
    /// No intent was supplied.
    Missing,
    /// More than one intent was supplied; the count is attached.
    Multiple(usize),
    /// An intent's subtype is not recognised.
    UnsupportedSubtype(String),
    /// The single intent has no `/DestOutputProfile`.
    MissingDestOutputProfile,
}

/// A collection of output intents with detection and validation helpers.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OutputIntentSet {
    intents: Vec<OutputIntent>,
}

impl OutputIntentSet {
    /// Decode a list of raw dictionary entries into an intent set.
    ///
    /// This performs no validation; call [`Self::issues`] to detect all problems
    /// or [`Self::validate`] to require a single usable intent.
    pub fn from_entries(entries: &[RawOutputIntent]) -> Self {
        let intents = entries
            .iter()
            .map(|raw| OutputIntent {
                subtype: OutputIntentSubtype::parse(&raw.subtype),
                output_condition_identifier: raw.output_condition_identifier.clone(),
                info: raw.info.clone(),
                registry_name: raw.registry_name.clone(),
                dest_output_profile: raw.dest_output_profile.clone(),
            })
            .collect();
        Self { intents }
    }

    /// Return the decoded intents.
    pub fn intents(&self) -> &[OutputIntent] {
        &self.intents
    }

    /// Return the number of intents.
    pub fn len(&self) -> usize {
        self.intents.len()
    }

    /// Return `true` when no intents were supplied.
    pub fn is_empty(&self) -> bool {
        self.intents.is_empty()
    }

    /// Detect all problems with this intent set, in a stable order.
    pub fn issues(&self) -> Vec<OutputIntentIssue> {
        let mut issues = Vec::new();
        if self.intents.is_empty() {
            issues.push(OutputIntentIssue::Missing);
        }
        if self.intents.len() > 1 {
            issues.push(OutputIntentIssue::Multiple(self.intents.len()));
        }
        for intent in &self.intents {
            if !intent.subtype.is_supported() {
                issues.push(OutputIntentIssue::UnsupportedSubtype(intent.subtype.name()));
            }
        }
        if self.intents.len() == 1 && self.intents[0].dest_output_profile.is_none() {
            issues.push(OutputIntentIssue::MissingDestOutputProfile);
        }
        issues
    }

    /// Require exactly one, supported intent with a destination profile.
    ///
    /// # Errors
    ///
    /// Returns the first applicable [`ColorError`] for a missing, multiple,
    /// unsupported or profile-less intent. On success, returns the single
    /// usable intent.
    pub fn validate(&self) -> Result<&OutputIntent, ColorError> {
        if self.intents.is_empty() {
            return Err(ColorError::MissingOutputIntent);
        }
        if self.intents.len() > 1 {
            return Err(ColorError::MultipleOutputIntents {
                count: self.intents.len(),
            });
        }
        let intent = &self.intents[0];
        if !intent.subtype.is_supported() {
            return Err(ColorError::UnsupportedOutputIntentSubtype(
                intent.subtype.name(),
            ));
        }
        if intent.dest_output_profile.is_none() {
            return Err(ColorError::MissingDestOutputProfile);
        }
        Ok(intent)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Write a big-endian `u32` into a buffer at `offset`.
    fn put_u32(buf: &mut [u8], offset: usize, value: u32) {
        buf[offset..offset + 4].copy_from_slice(&value.to_be_bytes());
    }

    /// Write a big-endian `u16` into a buffer at `offset`.
    fn put_u16(buf: &mut [u8], offset: usize, value: u16) {
        buf[offset..offset + 2].copy_from_slice(&value.to_be_bytes());
    }

    /// Write a four-byte signature into a buffer at `offset`.
    fn put_sig(buf: &mut [u8], offset: usize, sig: &[u8; 4]) {
        buf[offset..offset + 4].copy_from_slice(sig);
    }

    /// Write an `s15Fixed16` value into a buffer at `offset`.
    fn put_fixed(buf: &mut [u8], offset: usize, value: f32) {
        put_u32(buf, offset, (f64::from(value) * 65536.0) as u32);
    }

    /// An XYZType tag payload (signature, reserved, three fixed numbers).
    fn xyz_payload(x: f32, y: f32, z: f32) -> Vec<u8> {
        let mut data = vec![0u8; 20];
        put_sig(&mut data, 0, b"XYZ ");
        put_fixed(&mut data, 8, x);
        put_fixed(&mut data, 12, y);
        put_fixed(&mut data, 16, z);
        data
    }

    /// A `curv` tag payload with a single-entry gamma curve.
    fn gamma_payload(gamma: f32) -> Vec<u8> {
        let mut data = vec![0u8; 14];
        put_sig(&mut data, 0, b"curv");
        put_u32(&mut data, 8, 1);
        put_u16(&mut data, 12, (f64::from(gamma) * 256.0) as u16);
        data
    }

    /// A `desc` tag payload carrying an ASCII description.
    fn desc_payload(text: &str) -> Vec<u8> {
        let bytes = text.as_bytes();
        let mut data = vec![0u8; 4 + bytes.len()];
        put_u32(&mut data, 0, bytes.len() as u32);
        data[4..].copy_from_slice(bytes);
        data
    }

    /// A synthetic ICC profile builder used by the parser/conversion tests.
    struct ProfileBuilder {
        class: [u8; 4],
        space: [u8; 4],
        pcs: [u8; 4],
        tags: Vec<([u8; 4], Vec<u8>)>,
    }

    impl ProfileBuilder {
        fn rgb() -> Self {
            Self {
                class: *b"mntr",
                space: *b"RGB ",
                pcs: *b"XYZ ",
                tags: Vec::new(),
            }
        }

        fn gray() -> Self {
            Self {
                class: *b"prtr",
                space: *b"GRAY",
                pcs: *b"XYZ ",
                tags: Vec::new(),
            }
        }

        fn tag(mut self, sig: &[u8; 4], data: Vec<u8>) -> Self {
            self.tags.push((*sig, data));
            self
        }

        fn build(self) -> Vec<u8> {
            let count = self.tags.len();
            let mut cursor = ICC_HEADER_SIZE + 4 + count * 12;
            let mut offsets = Vec::with_capacity(count);
            for (_, data) in &self.tags {
                offsets.push(cursor);
                cursor += data.len();
            }
            let mut buf = vec![0u8; cursor];
            put_u32(&mut buf, 0, cursor as u32);
            put_sig(&mut buf, 4, b"ADBE");
            put_sig(&mut buf, 8, b"\x02\x40\x00\x00");
            put_sig(&mut buf, 12, &self.class);
            put_sig(&mut buf, 16, &self.space);
            put_sig(&mut buf, 20, &self.pcs);
            put_sig(&mut buf, 36, b"acsp");
            put_u32(&mut buf, 64, 1); // relative colorimetric
            put_fixed(&mut buf, 68, 0.9642); // D50 white point
            put_fixed(&mut buf, 72, 1.0);
            put_fixed(&mut buf, 76, 0.8249);
            put_u32(&mut buf, ICC_HEADER_SIZE, count as u32);
            for (index, ((sig, data), offset)) in self.tags.iter().zip(offsets).enumerate() {
                let base = ICC_HEADER_SIZE + 4 + index * 12;
                put_sig(&mut buf, base, sig);
                put_u32(&mut buf, base + 4, offset as u32);
                put_u32(&mut buf, base + 8, data.len() as u32);
                buf[offset..offset + data.len()].copy_from_slice(data);
            }
            buf
        }
    }

    fn rgb_profile() -> Vec<u8> {
        ProfileBuilder::rgb()
            .tag(b"desc", desc_payload("Synthetic sRGB-like"))
            .tag(b"wtpt", xyz_payload(0.9642, 1.0, 0.8249))
            .tag(b"rXYZ", xyz_payload(0.4360, 0.2225, 0.0139))
            .tag(b"gXYZ", xyz_payload(0.3851, 0.7169, 0.0971))
            .tag(b"bXYZ", xyz_payload(0.1431, 0.0606, 0.7141))
            .tag(b"rTRC", gamma_payload(2.2))
            .tag(b"gTRC", gamma_payload(2.2))
            .tag(b"bTRC", gamma_payload(2.2))
            .build()
    }

    fn gray_profile() -> Vec<u8> {
        ProfileBuilder::gray()
            .tag(b"desc", desc_payload("Synthetic gray gamma 2.2"))
            .tag(b"wtpt", xyz_payload(0.9642, 1.0, 0.8249))
            .tag(b"kTRC", gamma_payload(2.2))
            .build()
    }

    #[test]
    fn header_fields_are_parsed() {
        let profile = IccProfile::parse(&rgb_profile()).expect("valid profile");
        let header = profile.header();
        assert_eq!(header.device_class, DeviceClass::Display);
        assert_eq!(header.data_color_space, IccColorSpace::Rgb);
        assert_eq!(header.pcs, Pcs::Xyz);
        assert_eq!(
            header.rendering_intent,
            RenderingIntent::RelativeColorimetric
        );
        assert_eq!(header.cmm_name(), "ADBE");
        assert_eq!(header.version_major(), 2);
        assert_eq!(header.version_minor(), 4);
        assert!((header.illuminant.y - 1.0).abs() < 1e-6);
    }

    #[test]
    fn tag_lookup_by_signature() {
        let profile = IccProfile::parse(&rgb_profile()).expect("valid profile");
        assert_eq!(profile.tags().len(), 8);
        assert!(profile.find_tag(*b"desc").is_some());
        assert!(profile.find_tag(*b"zzzz").is_none());
        assert_eq!(&profile.tag_bytes(*b"rTRC").expect("rTRC")[0..4], b"curv");
        assert_eq!(
            profile.tag_bytes(*b"zzzz"),
            Err(ColorError::MissingTag { tag: *b"zzzz" })
        );
    }

    #[test]
    fn description_and_whitepoint_queries() {
        let profile = IccProfile::parse(&rgb_profile()).expect("valid profile");
        assert_eq!(profile.description().as_deref(), Ok("Synthetic sRGB-like"));
        let wtpt = profile.whitepoint().expect("white point");
        // s15Fixed16 has a resolution of 1/65536 ~= 1.5e-5.
        assert!((wtpt.x - 0.9642).abs() < 1e-4);
        assert!((wtpt.z - 0.8249).abs() < 1e-4);
    }

    #[test]
    fn truncated_profile_is_rejected() {
        let full = rgb_profile();
        for len in [0, 4, 36, ICC_HEADER_SIZE, ICC_HEADER_SIZE + 3] {
            assert!(
                matches!(
                    IccProfile::parse(&full[..len]),
                    Err(ColorError::Truncated { .. }) | Err(ColorError::SizeMismatch { .. })
                ),
                "len {len} should be rejected as truncated/incomplete"
            );
        }
    }

    #[test]
    fn oversize_profile_is_rejected() {
        // A declared size above the cap is rejected before reading tag data.
        let mut bytes = rgb_profile();
        put_u32(&mut bytes, 0, (MAX_ICC_PROFILE_BYTES + 1) as u32);
        assert_eq!(
            IccProfile::parse(&bytes),
            Err(ColorError::OversizeProfile {
                size: MAX_ICC_PROFILE_BYTES + 1,
                limit: MAX_ICC_PROFILE_BYTES,
            })
        );
    }

    #[test]
    fn invalid_signature_is_rejected() {
        let mut bytes = rgb_profile();
        put_sig(&mut bytes, 36, b"bad!");
        assert_eq!(
            IccProfile::parse(&bytes),
            Err(ColorError::InvalidSignature { found: *b"bad!" })
        );
    }

    #[test]
    fn srgb_like_matrix_and_trc_are_decoded() {
        let profile = IccProfile::parse(&rgb_profile()).expect("valid profile");
        let matrix = profile.colorant_matrix().expect("colourant matrix");
        assert!((matrix.m[0] - 0.4360).abs() < 1e-4);
        assert!((matrix.m[4] - 0.7169).abs() < 1e-4);
        assert!((matrix.m[8] - 0.7141).abs() < 1e-4);
        let r_trc = profile.trc(*b"rTRC").expect("red TRC");
        // u8Fixed8 gamma has a resolution of 1/256 ~= 0.0039.
        assert!(matches!(r_trc, Trc::Gamma(g) if (g - 2.2).abs() < 2e-3));
        assert!(matches!(
            profile.conversion_plan(),
            ConversionPlan::RgbMatrixTrc(_)
        ));
    }

    #[test]
    fn gray_trc_is_decoded() {
        let profile = IccProfile::parse(&gray_profile()).expect("valid profile");
        assert_eq!(profile.header().data_color_space, IccColorSpace::Gray);
        assert!(matches!(profile.trc(*b"kTRC"), Ok(Trc::Gamma(g)) if (g - 2.2).abs() < 2e-3));
        assert!(
            matches!(profile.conversion_plan(), ConversionPlan::GrayGamma(g) if (g - 2.2).abs() < 2e-3)
        );
    }

    #[test]
    fn output_intent_parse() {
        let raw = RawOutputIntent {
            subtype: "/GTS_PDFX".to_owned(),
            output_condition_identifier: Some("FOGRA39".to_owned()),
            info: Some("Coated FOGRA39".to_owned()),
            registry_name: Some("CGATS".to_owned()),
            dest_output_profile: Some(rgb_profile()),
        };
        let set = OutputIntentSet::from_entries(&[raw]);
        assert_eq!(set.len(), 1);
        let intent = set.validate().expect("valid intent");
        assert_eq!(intent.subtype, OutputIntentSubtype::GtsPdfx);
        assert_eq!(
            intent.output_condition_identifier.as_deref(),
            Some("FOGRA39")
        );
        assert_eq!(intent.registry_name.as_deref(), Some("CGATS"));
        assert!(intent.parse_dest_profile().expect("profile").is_some());
    }

    #[test]
    fn missing_output_intent_is_detected() {
        let set = OutputIntentSet::from_entries(&[]);
        assert!(set.is_empty());
        assert_eq!(set.issues(), vec![OutputIntentIssue::Missing]);
        assert_eq!(set.validate(), Err(ColorError::MissingOutputIntent));
    }

    #[test]
    fn multiple_output_intents_are_detected() {
        let raw = RawOutputIntent {
            subtype: "/GTS_PDFX".to_owned(),
            dest_output_profile: Some(rgb_profile()),
            ..RawOutputIntent::default()
        };
        let set = OutputIntentSet::from_entries(&[raw.clone(), raw]);
        assert_eq!(set.issues(), vec![OutputIntentIssue::Multiple(2)]);
        assert_eq!(
            set.validate(),
            Err(ColorError::MultipleOutputIntents { count: 2 })
        );
    }

    #[test]
    fn unsupported_intent_subtype_is_detected() {
        let raw = RawOutputIntent {
            subtype: "/GTS_FUTURE".to_owned(),
            dest_output_profile: Some(rgb_profile()),
            ..RawOutputIntent::default()
        };
        let set = OutputIntentSet::from_entries(&[raw]);
        assert_eq!(
            set.issues(),
            vec![OutputIntentIssue::UnsupportedSubtype(
                "GTS_FUTURE".to_owned()
            )]
        );
        assert_eq!(
            set.validate(),
            Err(ColorError::UnsupportedOutputIntentSubtype(
                "GTS_FUTURE".to_owned()
            ))
        );
    }

    #[test]
    fn conversion_plan_is_deterministic() {
        let bytes = rgb_profile();
        let a = IccProfile::parse(&bytes).expect("profile a");
        let b = IccProfile::parse(&bytes).expect("profile b");
        let plan_a = a.conversion_plan();
        let plan_b = b.conversion_plan();
        assert_eq!(plan_a, plan_b);
        let input = [0.5_f32, 0.25, 0.75];
        let out_a = plan_a.convert(&input).expect("convert a");
        let out_b = plan_b.convert(&input).expect("convert b");
        assert_eq!(out_a, out_b);
        // Linearising with gamma 2.2 gives monotone XYZ output.
        assert!(out_a.iter().all(|v| v.is_finite() && *v >= 0.0));
        assert_eq!(
            plan_a.convert(&[0.5]),
            Err(ColorError::ComponentCountMismatch {
                expected: 3,
                got: 1
            })
        );
    }

    #[test]
    fn needs_backend_plan_returns_typed_error() {
        let plan = ConversionPlan::NeedsBackend {
            reason: "test".to_owned(),
        };
        assert_eq!(
            plan.convert(&[0.5]),
            Err(ColorError::BackendRequired("test".to_owned()))
        );
        let cmyk = ConversionPlan::from_profile(
            &IccProfile::parse(
                &ProfileBuilder::rgb()
                    .tag(b"desc", desc_payload("cmyk"))
                    .build(),
            )
            .expect("profile"),
        );
        assert!(matches!(cmyk, ConversionPlan::NeedsBackend { .. }));
    }

    #[test]
    fn hostile_and_zero_length_tag_data_rejected() {
        // A tag offset beyond the profile is rejected during parsing.
        let mut bytes = rgb_profile();
        let tag_base = ICC_HEADER_SIZE + 4;
        let beyond = (bytes.len() + 1) as u32;
        put_u32(&mut bytes, tag_base + 4, beyond);
        assert!(matches!(
            IccProfile::parse(&bytes),
            Err(ColorError::TagOutOfBounds { .. })
        ));

        // A zero-length tag is accepted structurally but rejected on query.
        let profile = IccProfile::parse(&rgb_profile()).expect("valid profile");
        let zero = TagEntry {
            signature: *b"zzzz",
            offset: 0,
            size: 0,
        };
        assert_eq!(
            profile.tag_data(&zero),
            Err(ColorError::ZeroLengthTag { tag: *b"zzzz" })
        );
    }

    #[test]
    fn device_color_component_validation() {
        let rgb = DeviceColor::new(DeviceColorSpace::DeviceRgb, vec![0.1, 0.2, 0.3]).expect("rgb");
        assert_eq!(rgb.component_count(), 3);
        assert_eq!(
            DeviceColor::new(DeviceColorSpace::DeviceRgb, vec![0.1]),
            Err(ColorError::ComponentCountMismatch {
                expected: 3,
                got: 1
            })
        );
        assert_eq!(DeviceColorSpace::DeviceCmyk.expected_components(), Some(4));
        assert_eq!(
            DeviceColorSpace::IccBased { description: None }.expected_components(),
            None
        );
    }
}
