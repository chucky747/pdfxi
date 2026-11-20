//! Bounded validation of ICC profile bytes used by output intents.
//!
//! The validator parses only the fixed 128-byte ICC header plus the tag table
//! that follows it (ICC.1 / ISO 15076-1). It never decodes tag payloads, never
//! allocates proportional to a declared size, and never invokes an external
//! ICC library. Every rejection is a typed [`IccError`] with a stable machine
//! [`IccError::code`], so callers can filter or document findings without
//! matching on free-form text.
//!
//! A successful check means "the header and tag table are structurally
//! consistent"; it is **not** a colourimetric certification. Independent
//! validators such as veraPDF remain CI oracles, not runtime dependencies.

use crate::input::PreflightInput;
use std::fmt;

/// Length of the fixed ICC profile header, in bytes.
pub const ICC_HEADER_LEN: usize = 128;

/// Byte offset of the 32-bit tag count, immediately after the header.
pub const ICC_TAG_COUNT_OFFSET: usize = ICC_HEADER_LEN;

/// Byte offset of the first 12-byte tag-table entry.
pub const ICC_TAG_TABLE_OFFSET: usize = ICC_TAG_COUNT_OFFSET + 4;

/// Length of one tag-table entry (signature, offset, size).
pub const ICC_TAG_ENTRY_LEN: usize = 12;

/// Smallest profile that can be fully checked (header plus tag count).
pub const ICC_MIN_PROFILE_LEN: usize = ICC_TAG_TABLE_OFFSET;

/// Largest profile this bounded validator will inspect.
///
/// A real output-intent profile is typically a few megabytes at most. The cap
/// keeps a hostile `/DestOutputProfile` stream from driving unbounded work.
pub const MAX_ICC_BYTES: usize = 64 * 1024 * 1024;

/// Largest number of tag-table entries this validator will walk.
pub const MAX_ICC_TAGS: usize = 4096;

const SIGNATURE_OFFSET: usize = 36;
const CMM_OFFSET: usize = 4;
const VERSION_OFFSET: usize = 8;
const CLASS_OFFSET: usize = 12;
const COLOUR_SPACE_OFFSET: usize = 16;
const PCS_OFFSET: usize = 20;
const RENDERING_INTENT_OFFSET: usize = 64;

/// The data colour space declared by an ICC profile header.
///
/// Only the four spaces meaningful for PDF output intents are accepted; an
/// unknown four-character code is a typed [`IccError::UnsupportedColourSpace`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IccColourSpace {
    /// Calibrated gray (`GRAY`).
    Gray,
    /// Calibrated RGB (`RGB `).
    Rgb,
    /// Calibrated CMYK (`CMYK`).
    Cmyk,
    /// CIE Lab (`Lab `).
    Lab,
}

impl IccColourSpace {
    /// Parses a four-character colour-space code.
    #[must_use]
    pub fn from_four_cc(code: &[u8; 4]) -> Option<Self> {
        match code {
            b"GRAY" => Some(Self::Gray),
            b"RGB " => Some(Self::Rgb),
            b"CMYK" => Some(Self::Cmyk),
            b"Lab " => Some(Self::Lab),
            _ => None,
        }
    }

    /// Returns the canonical four-character code.
    #[must_use]
    pub const fn four_cc(self) -> &'static [u8; 4] {
        match self {
            Self::Gray => b"GRAY",
            Self::Rgb => b"RGB ",
            Self::Cmyk => b"CMYK",
            Self::Lab => b"Lab ",
        }
    }
}

impl fmt::Display for IccColourSpace {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&four_cc_text(self.four_cc()))
    }
}

/// Facts read from a structurally valid ICC header and tag table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IccProfileInfo {
    /// Size declared in the header; equal to the inspected byte length.
    pub declared_size: usize,
    /// Preferred CMM four-character code (for example `ADBE`).
    pub cmm: [u8; 4],
    /// Four-byte ICC version, most significant byte first.
    pub version: [u8; 4],
    /// Device class four-character code (for example `mntr` or `prtr`).
    pub device_class: [u8; 4],
    /// Parsed data colour space.
    pub colour_space: IccColourSpace,
    /// Profile connection space four-character code (`XYZ ` or `Lab `).
    pub pcs: [u8; 4],
    /// Rendering intent declared in the header.
    pub rendering_intent: u32,
    /// Number of tag-table entries inspected.
    pub tag_count: usize,
}

/// A typed reason an ICC byte sequence was rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IccError {
    /// The buffer is shorter than the fixed header plus tag count.
    TooShort {
        /// Actual byte length.
        len: usize,
        /// Minimum byte length required for validation.
        minimum: usize,
    },
    /// The buffer exceeds the caller's or crate's byte budget.
    TooLarge {
        /// Actual byte length.
        len: usize,
        /// Largest byte length the caller permits.
        maximum: usize,
    },
    /// The `acsp` signature at offset 36 is missing or wrong.
    BadSignature {
        /// The four bytes actually found.
        found: [u8; 4],
    },
    /// The declared profile size is smaller than the minimum header.
    SizeFieldTooSmall {
        /// The declared size.
        declared: usize,
    },
    /// The declared size does not match the number of bytes supplied.
    SizeMismatch {
        /// Size declared in the header.
        declared: usize,
        /// Number of bytes actually supplied.
        actual: usize,
    },
    /// The data colour space is not one this validator accepts.
    UnsupportedColourSpace {
        /// The unsupported four-character code.
        four_cc: [u8; 4],
    },
    /// The profile connection space is neither `XYZ ` nor `Lab `.
    UnsupportedPcs {
        /// The unsupported four-character code.
        four_cc: [u8; 4],
    },
    /// The tag count exceeds the bounded walk limit.
    TagCountTooLarge {
        /// Declared tag count.
        count: usize,
        /// Largest accepted tag count.
        maximum: usize,
    },
    /// The tag table does not fit inside the declared and supplied bytes.
    TagTableOutOfBounds {
        /// Declared tag count.
        count: usize,
        /// First byte after the tag table.
        end: usize,
        /// Largest valid table end.
        limit: usize,
    },
    /// A tag's offset or extent leaves the profile or overlaps the header.
    TagOutOfRange {
        /// Zero-based tag-table entry index.
        index: usize,
        /// Declared tag data offset.
        offset: usize,
        /// Declared tag data size.
        size: usize,
        /// Largest valid end offset.
        limit: usize,
    },
    /// Two tag data ranges overlap.
    TagOverlap {
        /// Tag-table index of the earlier range in sorted order.
        first: usize,
        /// Tag-table index of the later range in sorted order.
        second: usize,
    },
}

impl IccError {
    /// Returns a stable machine code for this error.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::TooShort { .. } => "ICC.TOO_SHORT",
            Self::TooLarge { .. } => "ICC.TOO_LARGE",
            Self::BadSignature { .. } => "ICC.BAD_SIGNATURE",
            Self::SizeFieldTooSmall { .. } => "ICC.SIZE_FIELD_TOO_SMALL",
            Self::SizeMismatch { .. } => "ICC.SIZE_MISMATCH",
            Self::UnsupportedColourSpace { .. } => "ICC.UNSUPPORTED_COLOUR_SPACE",
            Self::UnsupportedPcs { .. } => "ICC.UNSUPPORTED_PCS",
            Self::TagCountTooLarge { .. } => "ICC.TAG_COUNT_TOO_LARGE",
            Self::TagTableOutOfBounds { .. } => "ICC.TAG_TABLE_OUT_OF_BOUNDS",
            Self::TagOutOfRange { .. } => "ICC.TAG_OUT_OF_RANGE",
            Self::TagOverlap { .. } => "ICC.TAG_OVERLAP",
        }
    }
}

impl fmt::Display for IccError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooShort { len, minimum } => {
                write!(
                    f,
                    "ICC profile is truncated: {len} bytes, need at least {minimum}"
                )
            }
            Self::TooLarge { len, maximum } => {
                write!(
                    f,
                    "ICC profile is too large: {len} bytes, limit is {maximum}"
                )
            }
            Self::BadSignature { found } => {
                write!(
                    f,
                    "ICC profile is missing the 'acsp' signature (found '{}')",
                    four_cc_text(found)
                )
            }
            Self::SizeFieldTooSmall { declared } => {
                write!(
                    f,
                    "ICC profile declares size {declared}, smaller than the {ICC_MIN_PROFILE_LEN}-byte minimum"
                )
            }
            Self::SizeMismatch { declared, actual } => {
                write!(
                    f,
                    "ICC profile declares size {declared} but {actual} bytes were supplied"
                )
            }
            Self::UnsupportedColourSpace { four_cc } => {
                write!(
                    f,
                    "ICC profile uses unsupported data colour space '{}'",
                    four_cc_text(four_cc)
                )
            }
            Self::UnsupportedPcs { four_cc } => {
                write!(
                    f,
                    "ICC profile uses unsupported profile connection space '{}'",
                    four_cc_text(four_cc)
                )
            }
            Self::TagCountTooLarge { count, maximum } => {
                write!(f, "ICC profile declares {count} tags, limit is {maximum}")
            }
            Self::TagTableOutOfBounds { count, end, limit } => {
                write!(
                    f,
                    "ICC tag table for {count} tags ends at {end}, beyond the {limit}-byte limit"
                )
            }
            Self::TagOutOfRange {
                index,
                offset,
                size,
                limit,
            } => {
                write!(
                    f,
                    "ICC tag {index} at offset {offset} with size {size} exceeds the {limit}-byte limit"
                )
            }
            Self::TagOverlap { first, second } => {
                write!(
                    f,
                    "ICC tags {first} and {second} have overlapping data ranges"
                )
            }
        }
    }
}

impl std::error::Error for IccError {}

/// Renders a four-character code with printable characters and hex escapes.
fn four_cc_text(code: &[u8; 4]) -> String {
    code.iter()
        .map(|byte| {
            if byte.is_ascii_graphic() {
                (*byte as char).to_string()
            } else {
                format!("\\x{byte:02x}")
            }
        })
        .collect()
}

/// Reads a big-endian `u32` at `offset`, returning `None` when out of bounds.
fn read_u32(bytes: &[u8], offset: usize) -> Option<u32> {
    let slice = bytes.get(offset..offset.checked_add(4)?)?;
    let array: [u8; 4] = slice.try_into().ok()?;
    Some(u32::from_be_bytes(array))
}

/// Reads a four-character code at `offset`, returning `None` when out of bounds.
fn read_four_cc(bytes: &[u8], offset: usize) -> Option<[u8; 4]> {
    let slice = bytes.get(offset..offset.checked_add(4)?)?;
    Some([slice[0], slice[1], slice[2], slice[3]])
}

/// Reads a fixed header field, using truncation as the typed failure.
fn header_u32(bytes: &[u8], offset: usize) -> Result<u32, IccError> {
    read_u32(bytes, offset).ok_or(IccError::TooShort {
        len: bytes.len(),
        minimum: ICC_MIN_PROFILE_LEN,
    })
}

/// Reads a fixed header four-character code, using truncation as the failure.
fn header_four_cc(bytes: &[u8], offset: usize) -> Result<[u8; 4], IccError> {
    read_four_cc(bytes, offset).ok_or(IccError::TooShort {
        len: bytes.len(),
        minimum: ICC_MIN_PROFILE_LEN,
    })
}

/// Validates ICC bytes with a caller-supplied byte budget.
///
/// The checks are, in order: byte budget, minimum length, `acsp` signature,
/// declared size (minimum and exact match), data colour space, profile
/// connection space, tag count, tag-table bounds, per-tag bounds, and tag
/// overlap. All arithmetic is saturating or checked so hostile values cannot
/// overflow. The same bytes always yield the same result.
///
/// Use [`validate_icc`] for the crate-wide byte budget.
pub fn validate_icc_with_limit(bytes: &[u8], max_bytes: usize) -> Result<IccProfileInfo, IccError> {
    let len = bytes.len();
    if len > max_bytes {
        return Err(IccError::TooLarge {
            len,
            maximum: max_bytes,
        });
    }
    if len < ICC_MIN_PROFILE_LEN {
        return Err(IccError::TooShort {
            len,
            minimum: ICC_MIN_PROFILE_LEN,
        });
    }

    let signature = header_four_cc(bytes, SIGNATURE_OFFSET)?;
    if signature != *b"acsp" {
        return Err(IccError::BadSignature { found: signature });
    }

    let declared = header_u32(bytes, 0)? as usize;
    if declared < ICC_MIN_PROFILE_LEN {
        return Err(IccError::SizeFieldTooSmall { declared });
    }
    if declared != len {
        return Err(IccError::SizeMismatch {
            declared,
            actual: len,
        });
    }

    let colour_cc = header_four_cc(bytes, COLOUR_SPACE_OFFSET)?;
    let colour_space = IccColourSpace::from_four_cc(&colour_cc)
        .ok_or(IccError::UnsupportedColourSpace { four_cc: colour_cc })?;

    let pcs = header_four_cc(bytes, PCS_OFFSET)?;
    if pcs != *b"XYZ " && pcs != *b"Lab " {
        return Err(IccError::UnsupportedPcs { four_cc: pcs });
    }

    let count = header_u32(bytes, ICC_TAG_COUNT_OFFSET)? as usize;
    if count > MAX_ICC_TAGS {
        return Err(IccError::TagCountTooLarge {
            count,
            maximum: MAX_ICC_TAGS,
        });
    }
    let table_bytes = count.saturating_mul(ICC_TAG_ENTRY_LEN);
    let table_end = ICC_TAG_TABLE_OFFSET.saturating_add(table_bytes);
    let limit = declared.min(len);
    if table_end > limit {
        return Err(IccError::TagTableOutOfBounds {
            count,
            end: table_end,
            limit,
        });
    }

    // `count` is bounded by MAX_ICC_TAGS, so this walk is bounded too.
    let mut ranges: Vec<(usize, usize, usize)> = Vec::with_capacity(count);
    for index in 0..count {
        let entry = ICC_TAG_TABLE_OFFSET.saturating_add(index.saturating_mul(ICC_TAG_ENTRY_LEN));
        let offset = match read_u32(bytes, entry.saturating_add(4)) {
            Some(value) => value as usize,
            None => {
                return Err(IccError::TagOutOfRange {
                    index,
                    offset: 0,
                    size: 0,
                    limit,
                });
            }
        };
        let size = match read_u32(bytes, entry.saturating_add(8)) {
            Some(value) => value as usize,
            None => {
                return Err(IccError::TagOutOfRange {
                    index,
                    offset,
                    size: 0,
                    limit,
                });
            }
        };
        let end = offset.saturating_add(size);
        if offset < table_end || end > limit {
            return Err(IccError::TagOutOfRange {
                index,
                offset,
                size,
                limit,
            });
        }
        ranges.push((offset, end, index));
    }

    ranges.sort_unstable();
    for pair in ranges.windows(2) {
        if pair[1].0 < pair[0].1 {
            return Err(IccError::TagOverlap {
                first: pair[0].2,
                second: pair[1].2,
            });
        }
    }

    Ok(IccProfileInfo {
        declared_size: declared,
        cmm: header_four_cc(bytes, CMM_OFFSET)?,
        version: header_four_cc(bytes, VERSION_OFFSET)?,
        device_class: header_four_cc(bytes, CLASS_OFFSET)?,
        colour_space,
        pcs,
        rendering_intent: header_u32(bytes, RENDERING_INTENT_OFFSET)?,
        tag_count: count,
    })
}

/// Validates ICC bytes against the crate-wide [`MAX_ICC_BYTES`] budget.
pub fn validate_icc(bytes: &[u8]) -> Result<IccProfileInfo, IccError> {
    validate_icc_with_limit(bytes, MAX_ICC_BYTES)
}

/// Validates the output intent's ICC bytes carried by `input`, when supplied.
///
/// `Ok(None)` means the caller did not supply bytes; profiles then fall back to
/// the existing output-intent findings and do not claim ICC validation.
pub fn validated_output_intent_icc(
    input: &PreflightInput,
) -> Result<Option<IccProfileInfo>, IccError> {
    match input.output_intent_icc.as_deref() {
        Some(bytes) => validate_icc(bytes).map(Some),
        None => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn put_u32(bytes: &mut [u8], offset: usize, value: u32) {
        bytes[offset..offset + 4].copy_from_slice(&value.to_be_bytes());
    }

    fn put_cc(bytes: &mut [u8], offset: usize, code: &[u8; 4]) {
        bytes[offset..offset + 4].copy_from_slice(code);
    }

    /// Builds a profile whose declared size matches its byte length.
    fn base_icc(len: usize, tag_count: u32) -> Vec<u8> {
        let mut bytes = vec![0u8; len];
        put_u32(&mut bytes, 0, len as u32);
        put_cc(&mut bytes, CMM_OFFSET, b"ADBE");
        put_u32(&mut bytes, VERSION_OFFSET, 0x0210_0000);
        put_cc(&mut bytes, CLASS_OFFSET, b"mntr");
        put_cc(&mut bytes, COLOUR_SPACE_OFFSET, b"RGB ");
        put_cc(&mut bytes, PCS_OFFSET, b"XYZ ");
        put_cc(&mut bytes, SIGNATURE_OFFSET, b"acsp");
        put_u32(&mut bytes, RENDERING_INTENT_OFFSET, 1);
        put_u32(&mut bytes, ICC_TAG_COUNT_OFFSET, tag_count);
        bytes
    }

    /// A minimal but structurally valid profile with no tags.
    fn valid_icc() -> Vec<u8> {
        base_icc(ICC_MIN_PROFILE_LEN, 0)
    }

    /// A valid profile with one four-byte `desc` tag.
    fn valid_icc_with_one_tag() -> Vec<u8> {
        let mut bytes = base_icc(ICC_TAG_TABLE_OFFSET + ICC_TAG_ENTRY_LEN + 4, 1);
        put_cc(&mut bytes, ICC_TAG_TABLE_OFFSET, b"desc");
        put_u32(&mut bytes, ICC_TAG_TABLE_OFFSET + 4, 144);
        put_u32(&mut bytes, ICC_TAG_TABLE_OFFSET + 8, 4);
        bytes
    }

    #[test]
    fn minimal_profile_is_accepted_with_extracted_facts() {
        let info = validate_icc(&valid_icc()).expect("test: minimal profile must validate");
        assert_eq!(info.declared_size, 132);
        assert_eq!(info.cmm, *b"ADBE");
        // ICC v2.1 encodes the minor version in the high nibble of byte 1.
        assert_eq!(info.version, [2, 16, 0, 0]);
        assert_eq!(info.device_class, *b"mntr");
        assert_eq!(info.colour_space, IccColourSpace::Rgb);
        assert_eq!(info.pcs, *b"XYZ ");
        assert_eq!(info.rendering_intent, 1);
        assert_eq!(info.tag_count, 0);
    }

    #[test]
    fn one_tag_profile_is_accepted() {
        let info =
            validate_icc(&valid_icc_with_one_tag()).expect("test: one-tag profile must validate");
        assert_eq!(info.tag_count, 1);
    }

    #[test]
    fn header_constants_are_stable() {
        assert_eq!(ICC_HEADER_LEN, 128);
        assert_eq!(ICC_TAG_COUNT_OFFSET, 128);
        assert_eq!(ICC_TAG_TABLE_OFFSET, 132);
        assert_eq!(ICC_MIN_PROFILE_LEN, 132);
        assert_eq!(ICC_TAG_ENTRY_LEN, 12);
    }

    #[test]
    fn truncation_is_a_typed_too_short_error() {
        for len in [0, 1, 36, 40, 127, 131] {
            let error = validate_icc(&vec![0u8; len]).expect_err("test: short buffer must fail");
            assert_eq!(error.code(), "ICC.TOO_SHORT");
            assert!(matches!(
                error,
                IccError::TooShort {
                    minimum: ICC_MIN_PROFILE_LEN,
                    ..
                }
            ));
        }
    }

    #[test]
    fn empty_and_short_buffers_never_panic() {
        for len in 0..ICC_MIN_PROFILE_LEN {
            let buffer = vec![0x41u8; len];
            assert!(validate_icc(&buffer).is_err());
        }
    }

    #[test]
    fn bad_signature_is_a_typed_error() {
        let mut bytes = valid_icc();
        bytes[SIGNATURE_OFFSET] = b'x';
        let error = validate_icc(&bytes).expect_err("test: bad signature must fail");
        assert_eq!(error.code(), "ICC.BAD_SIGNATURE");
        assert!(matches!(error, IccError::BadSignature { .. }));
        assert!(error.to_string().contains("acsp"));
    }

    #[test]
    fn declared_size_smaller_than_minimum_is_typed() {
        let mut bytes = valid_icc();
        put_u32(&mut bytes, 0, 100);
        let error = validate_icc(&bytes).expect_err("test: small declared size must fail");
        assert_eq!(error.code(), "ICC.SIZE_FIELD_TOO_SMALL");
    }

    #[test]
    fn declared_size_mismatch_is_typed() {
        let mut bytes = valid_icc();
        put_u32(&mut bytes, 0, 200);
        let error = validate_icc(&bytes).expect_err("test: size mismatch must fail");
        assert_eq!(error.code(), "ICC.SIZE_MISMATCH");
        assert!(matches!(
            error,
            IccError::SizeMismatch {
                declared: 200,
                actual: 132
            }
        ));
    }

    #[test]
    fn unsupported_colour_space_and_pcs_are_typed() {
        let mut bytes = valid_icc();
        put_cc(&mut bytes, COLOUR_SPACE_OFFSET, b"2CLR");
        let error = validate_icc(&bytes).expect_err("test: unknown colour space must fail");
        assert_eq!(error.code(), "ICC.UNSUPPORTED_COLOUR_SPACE");

        let mut bytes = valid_icc();
        put_cc(&mut bytes, PCS_OFFSET, b"RGB ");
        let error = validate_icc(&bytes).expect_err("test: invalid PCS must fail");
        assert_eq!(error.code(), "ICC.UNSUPPORTED_PCS");
    }

    #[test]
    fn every_supported_colour_space_round_trips() {
        for space in [
            IccColourSpace::Gray,
            IccColourSpace::Rgb,
            IccColourSpace::Cmyk,
            IccColourSpace::Lab,
        ] {
            assert_eq!(IccColourSpace::from_four_cc(space.four_cc()), Some(space));
            let mut bytes = valid_icc();
            put_cc(&mut bytes, COLOUR_SPACE_OFFSET, space.four_cc());
            let info = validate_icc(&bytes).expect("test: supported space must validate");
            assert_eq!(info.colour_space, space);
        }
        assert_eq!(IccColourSpace::from_four_cc(b"ABCD"), None);
    }

    #[test]
    fn tag_count_limit_is_enforced() {
        let mut bytes = valid_icc();
        put_u32(&mut bytes, ICC_TAG_COUNT_OFFSET, (MAX_ICC_TAGS + 1) as u32);
        let error = validate_icc(&bytes).expect_err("test: large tag count must fail");
        assert_eq!(error.code(), "ICC.TAG_COUNT_TOO_LARGE");
    }

    #[test]
    fn tag_table_out_of_bounds_is_typed() {
        let mut bytes = valid_icc_with_one_tag();
        put_u32(&mut bytes, ICC_TAG_COUNT_OFFSET, 2);
        let error = validate_icc(&bytes).expect_err("test: truncated tag table must fail");
        assert_eq!(error.code(), "ICC.TAG_TABLE_OUT_OF_BOUNDS");
    }

    #[test]
    fn tag_offset_out_of_range_is_typed() {
        let mut bytes = valid_icc_with_one_tag();
        put_u32(&mut bytes, ICC_TAG_TABLE_OFFSET + 8, 8);
        let error = validate_icc(&bytes).expect_err("test: oversized tag must fail");
        assert_eq!(error.code(), "ICC.TAG_OUT_OF_RANGE");
    }

    #[test]
    fn tag_offset_into_header_is_typed() {
        let mut bytes = valid_icc_with_one_tag();
        put_u32(&mut bytes, ICC_TAG_TABLE_OFFSET + 4, 0);
        let error = validate_icc(&bytes).expect_err("test: tag in header must fail");
        assert_eq!(error.code(), "ICC.TAG_OUT_OF_RANGE");
    }

    #[test]
    fn overlapping_tags_are_typed() {
        let mut bytes = base_icc(ICC_TAG_TABLE_OFFSET + 2 * ICC_TAG_ENTRY_LEN + 8, 2);
        put_cc(&mut bytes, ICC_TAG_TABLE_OFFSET, b"desc");
        put_u32(&mut bytes, ICC_TAG_TABLE_OFFSET + 4, 156);
        put_u32(&mut bytes, ICC_TAG_TABLE_OFFSET + 8, 8);
        put_cc(
            &mut bytes,
            ICC_TAG_TABLE_OFFSET + ICC_TAG_ENTRY_LEN,
            b"cprt",
        );
        put_u32(
            &mut bytes,
            ICC_TAG_TABLE_OFFSET + ICC_TAG_ENTRY_LEN + 4,
            160,
        );
        put_u32(&mut bytes, ICC_TAG_TABLE_OFFSET + ICC_TAG_ENTRY_LEN + 8, 4);
        let error = validate_icc(&bytes).expect_err("test: overlapping tags must fail");
        assert_eq!(error.code(), "ICC.TAG_OVERLAP");
        assert!(matches!(
            error,
            IccError::TagOverlap {
                first: 0,
                second: 1
            }
        ));
    }

    #[test]
    fn oversized_buffer_is_rejected_before_parsing() {
        let bytes = base_icc(200, 0);
        let error = validate_icc_with_limit(&bytes, ICC_MIN_PROFILE_LEN)
            .expect_err("test: must be bounded");
        assert_eq!(error.code(), "ICC.TOO_LARGE");
        assert!(matches!(
            error,
            IccError::TooLarge {
                len: 200,
                maximum: 132
            }
        ));
    }

    #[test]
    fn error_codes_are_stable_and_unique() {
        let errors = [
            IccError::TooShort {
                len: 1,
                minimum: 132,
            },
            IccError::TooLarge { len: 2, maximum: 1 },
            IccError::BadSignature { found: [0; 4] },
            IccError::SizeFieldTooSmall { declared: 1 },
            IccError::SizeMismatch {
                declared: 1,
                actual: 2,
            },
            IccError::UnsupportedColourSpace { four_cc: [0; 4] },
            IccError::UnsupportedPcs { four_cc: [0; 4] },
            IccError::TagCountTooLarge {
                count: 1,
                maximum: 2,
            },
            IccError::TagTableOutOfBounds {
                count: 1,
                end: 2,
                limit: 1,
            },
            IccError::TagOutOfRange {
                index: 0,
                offset: 1,
                size: 2,
                limit: 3,
            },
            IccError::TagOverlap {
                first: 0,
                second: 1,
            },
        ];
        let mut codes: Vec<&str> = errors.iter().map(IccError::code).collect();
        codes.sort_unstable();
        let before = codes.len();
        codes.dedup();
        assert_eq!(codes.len(), before, "every typed error has a unique code");
        for error in &errors {
            assert!(error.code().starts_with("ICC."));
            assert!(!error.to_string().is_empty());
        }
    }

    #[test]
    fn validation_is_deterministic() {
        let bytes = valid_icc_with_one_tag();
        let first = validate_icc(&bytes);
        let second = validate_icc(&bytes);
        assert_eq!(first, second);
    }

    #[test]
    fn arbitrary_bytes_yield_errors_without_panicking() {
        for seed in 0..64u8 {
            let mut bytes = vec![seed; ICC_MIN_PROFILE_LEN];
            for (index, byte) in bytes.iter_mut().enumerate() {
                *byte = byte.wrapping_add((index as u8).wrapping_mul(seed));
            }
            bytes[SIGNATURE_OFFSET..SIGNATURE_OFFSET + 4].copy_from_slice(b"zzzz");
            assert!(validate_icc(&bytes).is_err());
        }
    }

    #[test]
    fn output_intent_helper_distinguishes_absent_and_invalid_bytes() {
        let mut input = PreflightInput::default();
        assert_eq!(validated_output_intent_icc(&input), Ok(None));
        input.output_intent_icc = Some(valid_icc());
        let info = validated_output_intent_icc(&input)
            .expect("test: valid bytes must validate")
            .expect("test: info must be present");
        assert_eq!(info.colour_space, IccColourSpace::Rgb);
        input.output_intent_icc = Some(vec![0u8; 3]);
        assert_eq!(
            validated_output_intent_icc(&input)
                .expect_err("test: invalid bytes must fail")
                .code(),
            "ICC.TOO_SHORT"
        );
    }
}
