//! Minimal ASN.1 DER encoder and decoder for the CMS/PKCS#7 subset.
//!
//! This module implements the definite-length DER subset needed to build and
//! parse CMS `SignedData`: [`Tag`] identifier octets, `SEQUENCE`, `SET`,
//! `INTEGER`, `OCTET STRING`, `NULL`, object identifiers, `UTCTime` /
//! `GeneralizedTime`, and context-specific tags. It is written from the public
//! X.690 BER/DER rules and RFC 5652; no external crate is used.
//!
//! Decoding is deliberately strict and hostile-input aware:
//!
//! - indefinite lengths and high-tag-number identifiers are rejected;
//! - length and integer encodings must be minimal;
//! - every read is bounds-checked and returns a structured
//!   [`SecurityError`] rather than panicking;
//! - trailing bytes are detected by [`DerReader::expect_end`];
//! - `GeneralizedTime` accepts a one-to-nine-digit fractional-seconds suffix
//!   (`.sss...Z`, as used by RFC 3161 `genTime`); `UTCTime` and malformed
//!   fractions are rejected. Fractions are validated but not retained because
//!   the decoded time is whole Unix seconds.
//!
//! Values are never logged. Errors report only tags, lengths, and OIDs.
//!
//! [`SecurityError`]: crate::SecurityError

use crate::error::{Result, SecurityError};

/// An ASN.1 identifier octet (the tag).
///
/// Only the low-tag-number form (tag numbers `0..=30`) is supported, which is
/// sufficient for the CMS subset. The constructor [`Tag::context`] builds the
/// context-specific tags used by CMS implicit/explicit fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tag(u8);

impl Tag {
    /// `BOOLEAN` (universal, primitive, tag 1).
    pub const BOOLEAN: Self = Self(0x01);
    /// `INTEGER` (universal, primitive, tag 2).
    pub const INTEGER: Self = Self(0x02);
    /// `BIT STRING` (universal, primitive, tag 3).
    pub const BIT_STRING: Self = Self(0x03);
    /// `OCTET STRING` (universal, primitive, tag 4).
    pub const OCTET_STRING: Self = Self(0x04);
    /// `NULL` (universal, primitive, tag 5).
    pub const NULL: Self = Self(0x05);
    /// `OBJECT IDENTIFIER` (universal, primitive, tag 6).
    pub const OBJECT_IDENTIFIER: Self = Self(0x06);
    /// `UTF8String` (universal, primitive, tag 12).
    pub const UTF8_STRING: Self = Self(0x0c);
    /// `NumericString` (universal, primitive, tag 18).
    pub const NUMERIC_STRING: Self = Self(0x12);
    /// `PrintableString` (universal, primitive, tag 19).
    pub const PRINTABLE_STRING: Self = Self(0x13);
    /// `TeletexString`/`T61String` (universal, primitive, tag 20).
    pub const T61_STRING: Self = Self(0x14);
    /// `IA5String` (universal, primitive, tag 22).
    pub const IA5_STRING: Self = Self(0x16);
    /// `BMPString` (universal, primitive, tag 30).
    pub const BMP_STRING: Self = Self(0x1e);
    /// `UTCTime` (universal, primitive, tag 23).
    pub const UTC_TIME: Self = Self(0x17);
    /// `GeneralizedTime` (universal, primitive, tag 24).
    pub const GENERALIZED_TIME: Self = Self(0x18);
    /// `SEQUENCE` (universal, constructed, tag 16).
    pub const SEQUENCE: Self = Self(0x30);
    /// `SET` / `SET OF` (universal, constructed, tag 17).
    pub const SET: Self = Self(0x31);

    /// Build a context-specific tag with the given number `0..=30`.
    ///
    /// Returns `None` for numbers above 30, which would require the unsupported
    /// high-tag-number form.
    pub fn context(number: u8, constructed: bool) -> Option<Self> {
        if number > 30 {
            return None;
        }
        let bit = if constructed { 0x20 } else { 0x00 };
        Some(Self(0x80 | bit | number))
    }

    /// Return the raw identifier octet.
    pub const fn byte(self) -> u8 {
        self.0
    }

    /// Return `true` when the constructed bit is set.
    pub const fn is_constructed(self) -> bool {
        self.0 & 0x20 != 0
    }

    const fn from_byte(byte: u8) -> Self {
        Self(byte)
    }
}

/// Encode a DER length in the minimal definite form.
///
/// Lengths below 128 use the single-byte short form. Larger lengths use the
/// long form with the fewest significant big-endian bytes.
pub fn encode_length(length: usize) -> Vec<u8> {
    if length < 0x80 {
        return vec![length as u8];
    }
    let bytes = length.to_be_bytes();
    let mut first = 0usize;
    while first + 1 < bytes.len() && bytes[first] == 0 {
        first += 1;
    }
    let significant = &bytes[first..];
    let mut out = Vec::with_capacity(significant.len() + 1);
    out.push(0x80 | significant.len() as u8);
    out.extend_from_slice(significant);
    out
}

/// Append one tag-length-value triple to `out`.
pub fn write_tlv(out: &mut Vec<u8>, tag: Tag, content: &[u8]) {
    out.push(tag.byte());
    out.extend_from_slice(&encode_length(content.len()));
    out.extend_from_slice(content);
}

/// Encode one tag-length-value triple as a fresh byte vector.
pub fn encode_tlv(tag: Tag, content: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(content.len() + 3);
    write_tlv(&mut out, tag, content);
    out
}

/// Wrap already-encoded children in a `SEQUENCE`.
pub fn encode_sequence(children: &[u8]) -> Vec<u8> {
    encode_tlv(Tag::SEQUENCE, children)
}

/// Wrap already-encoded children in a `SET` / `SET OF`.
pub fn encode_set(children: &[u8]) -> Vec<u8> {
    encode_tlv(Tag::SET, children)
}

/// Encode an `OCTET STRING`.
pub fn encode_octet_string(bytes: &[u8]) -> Vec<u8> {
    encode_tlv(Tag::OCTET_STRING, bytes)
}

/// Encode a `NULL` value.
pub fn encode_null() -> Vec<u8> {
    encode_tlv(Tag::NULL, &[])
}

/// Encode a non-negative `INTEGER` using minimal two's-complement DER bytes.
pub fn encode_integer(value: u64) -> Vec<u8> {
    let bytes = value.to_be_bytes();
    let mut first = 0usize;
    while first + 1 < bytes.len() && bytes[first] == 0 {
        first += 1;
    }
    let mut content = Vec::with_capacity(9);
    if bytes[first] & 0x80 != 0 {
        content.push(0u8);
    }
    content.extend_from_slice(&bytes[first..]);
    encode_tlv(Tag::INTEGER, &content)
}

/// Encode a context-specific field of the given number.
///
/// Returns `None` for tag numbers above 30.
pub fn encode_context(number: u8, constructed: bool, content: &[u8]) -> Option<Vec<u8>> {
    Tag::context(number, constructed).map(|tag| encode_tlv(tag, content))
}

fn encode_base128(out: &mut Vec<u8>, mut value: u64) {
    let mut groups = [0u8; 10];
    let mut count = 0usize;
    loop {
        groups[count] = (value & 0x7f) as u8;
        count += 1;
        value >>= 7;
        if value == 0 {
            break;
        }
    }
    for index in (0..count).rev() {
        let byte = groups[index];
        if index != 0 {
            out.push(byte | 0x80);
        } else {
            out.push(byte);
        }
    }
}

/// Encode an object identifier from its integer arcs.
///
/// Requires at least two arcs, a first arc in `0..=2`, and a second arc no
/// greater than 39 when the first arc is less than 2.
pub fn encode_oid(arcs: &[u32]) -> Result<Vec<u8>> {
    if arcs.len() < 2 || arcs[0] > 2 || (arcs[0] < 2 && arcs[1] > 39) {
        return Err(SecurityError::DerInvalid);
    }
    let mut content = Vec::with_capacity(arcs.len() + 2);
    encode_base128(&mut content, u64::from(arcs[0]) * 40 + u64::from(arcs[1]));
    for arc in &arcs[2..] {
        encode_base128(&mut content, u64::from(*arc));
    }
    Ok(encode_tlv(Tag::OBJECT_IDENTIFIER, &content))
}

/// Convert a Unix timestamp (seconds since 1970-01-01 UTC) to a calendar date.
///
/// Uses Howard Hinnant's civil-from-days algorithm. Returns `None` when the
/// timestamp lies outside years `0..=9999`.
fn civil_from_days(days: i64) -> Option<(i64, u32, u32)> {
    let z = days.checked_add(719_468)?;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = if month_prime < 10 {
        month_prime + 3
    } else {
        month_prime - 9
    };
    if month <= 2 {
        year += 1;
    }
    if !(0..=9999).contains(&year) {
        return None;
    }
    Some((year, month as u32, day as u32))
}

/// Days since 1970-01-01 for a proleptic Gregorian date (Hinnant's algorithm).
fn days_from_civil(year: i64, month: u32, day: u32) -> Option<i64> {
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let mut adjusted_year = year;
    if month <= 2 {
        adjusted_year -= 1;
    }
    let era = if adjusted_year >= 0 {
        adjusted_year
    } else {
        adjusted_year - 399
    } / 400;
    let year_of_era = adjusted_year - era * 400;
    let month_prime = if month > 2 {
        month as i64 - 3
    } else {
        month as i64 + 9
    };
    let day_of_year = (153 * month_prime + 2) / 5 + i64::from(day) - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era.checked_mul(146_097)?
        .checked_add(day_of_era)?
        .checked_sub(719_468)
}

/// Number of days in a given month, accounting for leap years.
fn days_in_month(year: i64, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
            if leap { 29 } else { 28 }
        }
        _ => 0,
    }
}

/// Encode a signing time as `UTCTime` (1950–2049) or `GeneralizedTime`.
///
/// DER requires the canonical `YYMMDDHHMMSSZ` / `YYYYMMDDHHMMSSZ` form with no
/// fractional seconds and an explicit `Z` (UTC) zone.
pub fn encode_time(unix_seconds: u64) -> Result<Vec<u8>> {
    let days = (unix_seconds / 86_400) as i64;
    let seconds_of_day = unix_seconds % 86_400;
    let (year, month, day) = civil_from_days(days).ok_or(SecurityError::DerInvalid)?;
    let hour = seconds_of_day / 3600;
    let minute = (seconds_of_day % 3600) / 60;
    let second = seconds_of_day % 60;
    if day > days_in_month(year, month) {
        return Err(SecurityError::DerInvalid);
    }
    let mut text = Vec::with_capacity(15);
    if (1950..=2049).contains(&year) {
        let short_year = (year % 100) as u32;
        push_two(&mut text, short_year);
    } else {
        push_four(&mut text, year as u32);
    }
    push_two(&mut text, month);
    push_two(&mut text, day);
    push_two(&mut text, hour as u32);
    push_two(&mut text, minute as u32);
    push_two(&mut text, second as u32);
    text.push(b'Z');
    let tag = if (1950..=2049).contains(&year) {
        Tag::UTC_TIME
    } else {
        Tag::GENERALIZED_TIME
    };
    Ok(encode_tlv(tag, &text))
}

fn push_two(out: &mut Vec<u8>, value: u32) {
    out.push(b'0' + ((value / 10) % 10) as u8);
    out.push(b'0' + (value % 10) as u8);
}

fn push_four(out: &mut Vec<u8>, value: u32) {
    out.push(b'0' + ((value / 1000) % 10) as u8);
    out.push(b'0' + ((value / 100) % 10) as u8);
    push_two(out, value);
}

fn parse_two(bytes: &[u8]) -> Option<u32> {
    if bytes.len() != 2 || !bytes[0].is_ascii_digit() || !bytes[1].is_ascii_digit() {
        return None;
    }
    Some(u32::from(bytes[0] - b'0') * 10 + u32::from(bytes[1] - b'0'))
}

fn parse_four(bytes: &[u8]) -> Option<u32> {
    if bytes.len() != 4 || bytes.iter().any(|b| !b.is_ascii_digit()) {
        return None;
    }
    let mut value = 0u32;
    for byte in bytes {
        value = value.checked_mul(10)?.checked_add(u32::from(byte - b'0'))?;
    }
    Some(value)
}

fn decode_time_content(tag: Tag, content: &[u8]) -> Result<u64> {
    let (year, offset) = if tag == Tag::UTC_TIME {
        let short = parse_two(content.get(0..2).ok_or(SecurityError::DerTruncated)?)
            .ok_or(SecurityError::DerInvalid)?;
        let full = if short >= 50 {
            i64::from(short) + 1900
        } else {
            i64::from(short) + 2000
        };
        (full, 2usize)
    } else if tag == Tag::GENERALIZED_TIME {
        let full = parse_four(content.get(0..4).ok_or(SecurityError::DerTruncated)?)
            .ok_or(SecurityError::DerInvalid)?;
        (i64::from(full), 4usize)
    } else {
        return Err(SecurityError::DerUnexpectedTag {
            expected: Tag::UTC_TIME.byte(),
            found: tag.byte(),
        });
    };
    // Remaining layout: `MMDDHHMMSS` followed by `Z`, or, for GeneralizedTime
    // only, `.sss...Z` (RFC 3161 `genTime` permits fractional seconds).
    let clock = content
        .get(offset..offset + 10)
        .ok_or(SecurityError::DerTruncated)?;
    if clock.iter().any(|byte| !byte.is_ascii_digit()) {
        return Err(SecurityError::DerInvalid);
    }
    let tail = content
        .get(offset + 10..)
        .ok_or(SecurityError::DerTruncated)?;
    if tail.first() == Some(&b'.') {
        if tag != Tag::GENERALIZED_TIME {
            return Err(SecurityError::DerInvalid);
        }
        let zone_index = tail
            .iter()
            .position(|byte| *byte == b'Z')
            .ok_or(SecurityError::DerInvalid)?;
        if zone_index != tail.len() - 1 {
            return Err(SecurityError::DerInvalid);
        }
        let fraction = &tail[1..zone_index];
        if fraction.is_empty()
            || fraction.len() > 9
            || fraction.iter().any(|byte| !byte.is_ascii_digit())
        {
            return Err(SecurityError::DerInvalid);
        }
        // Fraction digits are validated but intentionally discarded: the public
        // time type is whole Unix seconds and carries no sub-second precision.
    } else if !(tail.len() == 1 && tail[0] == b'Z') {
        return Err(SecurityError::DerInvalid);
    }
    let month = parse_two(&clock[0..2]).ok_or(SecurityError::DerInvalid)?;
    let day = parse_two(&clock[2..4]).ok_or(SecurityError::DerInvalid)?;
    let hour = parse_two(&clock[4..6]).ok_or(SecurityError::DerInvalid)?;
    let minute = parse_two(&clock[6..8]).ok_or(SecurityError::DerInvalid)?;
    let second = parse_two(&clock[8..10]).ok_or(SecurityError::DerInvalid)?;
    if month == 0
        || day == 0
        || day > days_in_month(year, month)
        || hour > 23
        || minute > 59
        || second > 59
    {
        return Err(SecurityError::DerInvalid);
    }
    let days = days_from_civil(year, month, day).ok_or(SecurityError::DerInvalid)?;
    if days < 0 {
        return Err(SecurityError::DerInvalid);
    }
    (days as u64)
        .checked_mul(86_400)
        .and_then(|value| value.checked_add(u64::from(hour) * 3600))
        .and_then(|value| value.checked_add(u64::from(minute) * 60))
        .and_then(|value| value.checked_add(u64::from(second)))
        .ok_or(SecurityError::DerInvalid)
}

/// A decoded tag-length-value view into a byte slice.
///
/// The view borrows its input and never allocates. Use [`DerReader`] to obtain
/// values, then the typed accessors to validate their content.
#[derive(Debug, Clone, Copy)]
pub struct Tlv<'a> {
    tag: Tag,
    content: &'a [u8],
    encoded: &'a [u8],
}

impl<'a> Tlv<'a> {
    /// Return the identifier octet of this value.
    pub const fn tag(&self) -> Tag {
        self.tag
    }

    /// Return the content octets (without the tag and length).
    pub const fn content(&self) -> &'a [u8] {
        self.content
    }

    /// Return the complete encoded value, including tag and length.
    pub const fn encoded(&self) -> &'a [u8] {
        self.encoded
    }

    /// Interpret this value as a `SEQUENCE`, returning a reader over its content.
    pub fn as_sequence(&self) -> Result<DerReader<'a>> {
        self.as_constructed(Tag::SEQUENCE)
    }

    /// Interpret this value as a `SET`, returning a reader over its content.
    pub fn as_set(&self) -> Result<DerReader<'a>> {
        self.as_constructed(Tag::SET)
    }

    fn as_constructed(&self, expected: Tag) -> Result<DerReader<'a>> {
        expect_tag(expected, self.tag)?;
        if !self.tag.is_constructed() {
            return Err(SecurityError::DerInvalid);
        }
        Ok(DerReader::new(self.content))
    }

    /// Interpret this value as an `OCTET STRING` and return its bytes.
    pub fn as_octets(&self) -> Result<&'a [u8]> {
        expect_tag(Tag::OCTET_STRING, self.tag)?;
        Ok(self.content)
    }

    /// Validate that this value is an empty `NULL`.
    pub fn as_null(&self) -> Result<()> {
        expect_tag(Tag::NULL, self.tag)?;
        if self.content.is_empty() {
            Ok(())
        } else {
            Err(SecurityError::DerInvalid)
        }
    }

    /// Validate that this value is a `BOOLEAN` and return its logical value.
    ///
    /// DER permits only `0x00` for false and `0xff` for true; other encodings
    /// (notably BER's `0x01`) are rejected.
    pub fn as_bool(&self) -> Result<bool> {
        expect_tag(Tag::BOOLEAN, self.tag)?;
        match self.content {
            [0x00] => Ok(false),
            [0xff] => Ok(true),
            _ => Err(SecurityError::DerInvalid),
        }
    }

    /// Validate a `BIT STRING` and return `(unused_bits, payload)`.
    ///
    /// The first content octet counts the unused bits in the final octet. DER
    /// requires it to be `0..=7` and to be zero when the payload is empty.
    pub fn as_bit_string(&self) -> Result<(u8, &'a [u8])> {
        expect_tag(Tag::BIT_STRING, self.tag)?;
        let (unused, payload) = self
            .content
            .split_first()
            .ok_or(SecurityError::DerTruncated)?;
        if *unused > 7 || (payload.is_empty() && *unused != 0) {
            return Err(SecurityError::DerInvalid);
        }
        Ok((*unused, payload))
    }

    /// Decode a non-negative `INTEGER` into a `u64`.
    pub fn as_integer_u64(&self) -> Result<u64> {
        expect_tag(Tag::INTEGER, self.tag)?;
        let content = self.content;
        if content.is_empty() || content[0] & 0x80 != 0 {
            return Err(SecurityError::DerInvalid);
        }
        if content.len() > 1 && content[0] == 0 && content[1] & 0x80 == 0 {
            return Err(SecurityError::DerInvalid);
        }
        if content.len() > 9 {
            return Err(SecurityError::DerInvalid);
        }
        let significant = if content.len() == 9 {
            if content[0] != 0 {
                return Err(SecurityError::DerInvalid);
            }
            &content[1..]
        } else {
            content
        };
        let mut value = 0u64;
        for byte in significant {
            value = value
                .checked_mul(256)
                .and_then(|number| number.checked_add(u64::from(*byte)))
                .ok_or(SecurityError::DerInvalid)?;
        }
        Ok(value)
    }

    /// Decode an object identifier into its integer arcs.
    pub fn as_oid(&self) -> Result<Vec<u32>> {
        expect_tag(Tag::OBJECT_IDENTIFIER, self.tag)?;
        decode_oid_arcs(self.content)
    }

    /// Decode a `UTCTime` or `GeneralizedTime` into Unix seconds.
    ///
    /// `GeneralizedTime` may carry a fractional-seconds suffix (`.sss...Z`);
    /// its digits are validated and then discarded because the result is whole
    /// seconds. `UTCTime` has no fractional form and malformed suffixes are
    /// rejected with [`SecurityError::DerInvalid`].
    pub fn as_time(&self) -> Result<u64> {
        decode_time_content(self.tag, self.content)
    }
}

fn expect_tag(expected: Tag, found: Tag) -> Result<()> {
    if expected == found {
        Ok(())
    } else {
        Err(SecurityError::DerUnexpectedTag {
            expected: expected.byte(),
            found: found.byte(),
        })
    }
}

/// Decode the content octets of an `OBJECT IDENTIFIER` into integer arcs.
///
/// Useful when an OID appears behind an implicit context tag (for example a
/// SAN `registeredID`), where the identifier octet is not `0x06`.
pub fn decode_oid_content(content: &[u8]) -> Result<Vec<u32>> {
    decode_oid_arcs(content)
}

fn decode_oid_arcs(content: &[u8]) -> Result<Vec<u32>> {
    if content.is_empty() {
        return Err(SecurityError::DerInvalid);
    }
    let mut subids = Vec::new();
    let mut current = 0u64;
    let mut at_arc_start = true;
    for byte in content {
        if at_arc_start && *byte == 0x80 {
            return Err(SecurityError::DerInvalid);
        }
        current = current
            .checked_mul(128)
            .and_then(|value| value.checked_add(u64::from(*byte & 0x7f)))
            .ok_or(SecurityError::DerInvalid)?;
        at_arc_start = false;
        if byte & 0x80 == 0 {
            if current > u64::from(u32::MAX) {
                return Err(SecurityError::DerInvalid);
            }
            subids.push(current as u32);
            current = 0;
            at_arc_start = true;
        }
    }
    if !at_arc_start {
        return Err(SecurityError::DerTruncated);
    }
    if subids.is_empty() {
        return Err(SecurityError::DerInvalid);
    }
    // The first two arcs are packed into a single base-128 subidentifier.
    let first_value = subids[0];
    let first = if first_value < 40 {
        0
    } else if first_value < 80 {
        1
    } else {
        2
    };
    let second = first_value - first * 40;
    let mut arcs = Vec::with_capacity(subids.len());
    arcs.push(first);
    arcs.push(second);
    arcs.extend_from_slice(&subids[1..]);
    Ok(arcs)
}

/// A bounds-checked cursor over a DER-encoded byte slice.
///
/// Every method returns a structured error on malformed input; none panic on
/// attacker-controlled data.
#[derive(Debug, Clone)]
pub struct DerReader<'a> {
    data: &'a [u8],
    position: usize,
}

impl<'a> DerReader<'a> {
    /// Create a reader over `data`.
    pub const fn new(data: &'a [u8]) -> Self {
        Self { data, position: 0 }
    }

    /// Return the current byte offset.
    pub const fn position(&self) -> usize {
        self.position
    }

    /// Return the number of unread bytes.
    pub const fn remaining(&self) -> usize {
        self.data.len() - self.position
    }

    /// Return `true` when no bytes remain.
    pub const fn is_empty(&self) -> bool {
        self.position == self.data.len()
    }

    /// Peek at the next identifier octet without consuming it.
    pub fn peek_tag(&self) -> Option<Tag> {
        self.data.get(self.position).copied().map(Tag::from_byte)
    }

    /// Read one complete tag-length-value triple.
    pub fn read_tlv(&mut self) -> Result<Tlv<'a>> {
        let identifier = *self
            .data
            .get(self.position)
            .ok_or(SecurityError::DerTruncated)?;
        if identifier & 0x1f == 0x1f {
            return Err(SecurityError::DerInvalid);
        }
        let length_byte = *self
            .data
            .get(self.position + 1)
            .ok_or(SecurityError::DerTruncated)?;
        let (length, header) = if length_byte < 0x80 {
            (usize::from(length_byte), 2usize)
        } else {
            let count = usize::from(length_byte & 0x7f);
            if count == 0 {
                return Err(SecurityError::DerInvalid);
            }
            if count > (usize::BITS as usize / 8) {
                return Err(SecurityError::DerLengthOverflow);
            }
            let length_start = self
                .position
                .checked_add(2)
                .ok_or(SecurityError::DerLengthOverflow)?;
            let length_end = length_start
                .checked_add(count)
                .ok_or(SecurityError::DerLengthOverflow)?;
            let bytes = self
                .data
                .get(length_start..length_end)
                .ok_or(SecurityError::DerTruncated)?;
            if bytes[0] == 0 {
                return Err(SecurityError::DerInvalid);
            }
            let mut decoded = 0usize;
            for byte in bytes {
                decoded = decoded
                    .checked_mul(256)
                    .and_then(|value| value.checked_add(usize::from(*byte)))
                    .ok_or(SecurityError::DerLengthOverflow)?;
            }
            if decoded < 128 {
                return Err(SecurityError::DerInvalid);
            }
            (decoded, 2 + count)
        };
        let content_start = self
            .position
            .checked_add(header)
            .ok_or(SecurityError::DerLengthOverflow)?;
        let content_end = content_start
            .checked_add(length)
            .ok_or(SecurityError::DerLengthOverflow)?;
        let content = self
            .data
            .get(content_start..content_end)
            .ok_or(SecurityError::DerTruncated)?;
        let encoded = &self.data[self.position..content_end];
        self.position = content_end;
        Ok(Tlv {
            tag: Tag::from_byte(identifier),
            content,
            encoded,
        })
    }

    /// Read a value and require it to be a `SEQUENCE`.
    pub fn read_sequence(&mut self) -> Result<DerReader<'a>> {
        self.read_tlv()?.as_sequence()
    }

    /// Read a value and require it to be a `SET`.
    pub fn read_set(&mut self) -> Result<DerReader<'a>> {
        self.read_tlv()?.as_set()
    }

    /// Read a value and require it to match `expected`.
    pub fn read_tagged(&mut self, expected: Tag) -> Result<DerReader<'a>> {
        let tlv = self.read_tlv()?;
        tlv.as_constructed(expected)
    }

    /// Require that no bytes remain, reporting [`SecurityError::DerTrailingBytes`]
    /// otherwise.
    pub fn expect_end(&self) -> Result<()> {
        if self.is_empty() {
            Ok(())
        } else {
            Err(SecurityError::DerTrailingBytes)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    #[test]
    fn length_encoding_uses_short_minimal_long_and_long_forms() {
        assert_eq!(encode_length(0), vec![0x00]);
        assert_eq!(encode_length(127), vec![0x7f]);
        assert_eq!(encode_length(128), vec![0x81, 0x80]);
        assert_eq!(encode_length(255), vec![0x81, 0xff]);
        assert_eq!(encode_length(256), vec![0x82, 0x01, 0x00]);
        assert_eq!(encode_length(65_536), vec![0x83, 0x01, 0x00, 0x00]);
    }

    #[test]
    fn integer_round_trips_and_rejects_non_minimal_forms() {
        for value in [0u64, 1, 127, 128, 255, 256, 65_536, u64::MAX] {
            let encoded = encode_integer(value);
            let tlv = DerReader::new(&encoded).read_tlv().expect("valid integer");
            assert_eq!(tlv.as_integer_u64(), Ok(value));
        }
        assert_eq!(encode_integer(0), vec![0x02, 0x01, 0x00]);
        assert_eq!(encode_integer(127), vec![0x02, 0x01, 0x7f]);
        assert_eq!(encode_integer(128), vec![0x02, 0x02, 0x00, 0x80]);
        let non_minimal = [0x02, 0x02, 0x00, 0x01];
        let tlv = DerReader::new(&non_minimal).read_tlv().expect("valid tlv");
        assert_eq!(tlv.as_integer_u64(), Err(SecurityError::DerInvalid));
    }

    #[test]
    fn oid_round_trips_and_matches_known_encodings() {
        let data = [1u32, 2, 840, 113_549, 1, 7, 1];
        let encoded = encode_oid(&data).expect("valid oid");
        assert_eq!(hex(&encoded), "06092a864886f70d010701");
        let tlv = DerReader::new(&encoded).read_tlv().expect("valid tlv");
        assert_eq!(tlv.as_oid(), Ok(data.to_vec()));
        // A first arc of 2 permits an arbitrarily large second arc.
        let large_second = encode_oid(&[2, 999]).expect("valid oid");
        let large_tlv = DerReader::new(&large_second).read_tlv().expect("valid tlv");
        assert_eq!(large_tlv.as_oid(), Ok(vec![2, 999]));
        assert_eq!(encode_oid(&[3, 1]), Err(SecurityError::DerInvalid));
        assert_eq!(encode_oid(&[1, 40]), Err(SecurityError::DerInvalid));
    }

    #[test]
    fn time_round_trips_utc_and_generalized() {
        for seconds in [0u64, 1_600_000_000, 2_147_483_647, 4_102_444_800] {
            let encoded = encode_time(seconds).expect("valid time");
            let tlv = DerReader::new(&encoded).read_tlv().expect("valid tlv");
            assert_eq!(tlv.as_time(), Ok(seconds));
        }
        let utc = encode_time(0).expect("valid time");
        assert_eq!(utc.first(), Some(&Tag::UTC_TIME.byte()));
        assert_eq!(&utc[2..], b"700101000000Z");
        let generalized = encode_time(4_102_444_800).expect("valid time");
        assert_eq!(generalized.first(), Some(&Tag::GENERALIZED_TIME.byte()));
        assert_eq!(&generalized[2..], b"21000101000000Z");
        let invalid = encode_tlv(Tag::UTC_TIME, b"991332000000Z");
        let tlv = DerReader::new(&invalid).read_tlv().expect("valid tlv");
        assert_eq!(tlv.as_time(), Err(SecurityError::DerInvalid));
    }

    #[test]
    fn octet_string_null_sequence_and_set_round_trip() {
        let octets = encode_octet_string(b"abc");
        assert_eq!(octets, vec![0x04, 0x03, b'a', b'b', b'c']);
        let mut children = Vec::new();
        write_tlv(&mut children, Tag::OCTET_STRING, b"x");
        write_tlv(&mut children, Tag::NULL, &[]);
        let sequence = encode_sequence(&children);
        let mut reader = DerReader::new(&sequence).read_sequence().expect("sequence");
        assert_eq!(
            reader.read_tlv().expect("octets").as_octets(),
            Ok(b"x".as_slice())
        );
        reader.read_tlv().expect("null").as_null().expect("null");
        reader.expect_end().expect("end");
        assert_eq!(encode_set(&children), encode_tlv(Tag::SET, &children));
    }

    #[test]
    fn context_tags_round_trip() {
        let explicit = encode_context(0, true, &encode_integer(1)).expect("context 0");
        assert_eq!(explicit.first(), Some(&0xa0));
        let mut reader = DerReader::new(&explicit)
            .read_tagged(Tag::context(0, true).expect("tag"))
            .expect("tagged");
        let value = reader.read_tlv().expect("integer");
        assert_eq!(value.as_integer_u64(), Ok(1));
        reader.expect_end().expect("end");
        assert_eq!(Tag::context(31, true), None);
    }

    #[test]
    fn reader_rejects_truncated_indefinite_and_trailing_data() {
        let encoded = encode_integer(258);
        let truncated = &encoded[..encoded.len() - 1];
        assert_eq!(
            DerReader::new(truncated).read_tlv().map(|_| ()),
            Err(SecurityError::DerTruncated)
        );
        let mut with_trailing = encoded.clone();
        with_trailing.push(0);
        let mut reader = DerReader::new(&with_trailing);
        reader.read_tlv().expect("valid");
        assert_eq!(reader.expect_end(), Err(SecurityError::DerTrailingBytes));
        let indefinite = [0x30, 0x80, 0x00, 0x00];
        assert_eq!(
            DerReader::new(&indefinite).read_tlv().map(|_| ()),
            Err(SecurityError::DerInvalid)
        );
        let non_minimal = [0x04, 0x81, 0x01, b'x'];
        assert_eq!(
            DerReader::new(&non_minimal).read_tlv().map(|_| ()),
            Err(SecurityError::DerInvalid)
        );
    }

    #[test]
    fn reader_rejects_oversized_length_declaration() {
        let oversized = [0x04, 0x88, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff];
        assert_eq!(
            DerReader::new(&oversized).read_tlv().map(|_| ()),
            Err(SecurityError::DerLengthOverflow)
        );
    }

    #[test]
    fn generalized_time_accepts_fractional_seconds() {
        let plain = encode_tlv(Tag::GENERALIZED_TIME, b"20261119000000Z");
        let plain_seconds = DerReader::new(&plain)
            .read_tlv()
            .expect("plain generalized time")
            .as_time()
            .expect("valid plain time");
        let fractions: [&[u8]; 4] = [b".5Z", b".123Z", b".123456789Z", b".000Z"];
        for fraction in fractions {
            let mut content = b"20261119000000".to_vec();
            content.extend_from_slice(fraction);
            let encoded = encode_tlv(Tag::GENERALIZED_TIME, &content);
            let seconds = DerReader::new(&encoded)
                .read_tlv()
                .expect("fractional generalized time")
                .as_time()
                .expect("valid fractional time");
            assert_eq!(
                seconds, plain_seconds,
                "fractional seconds must not change whole-second time"
            );
        }
    }

    #[test]
    fn time_rejects_malformed_fractional_seconds() {
        let invalid: [(Tag, &[u8]); 8] = [
            (Tag::GENERALIZED_TIME, b"20261119000000.Z"),
            (Tag::GENERALIZED_TIME, b"20261119000000.abcZ"),
            (Tag::GENERALIZED_TIME, b"20261119000000.1234567890Z"),
            (Tag::GENERALIZED_TIME, b"20261119000000.12.3Z"),
            (Tag::GENERALIZED_TIME, b"20261119000000.123"),
            (Tag::GENERALIZED_TIME, b"20261119000000.123Zx"),
            (Tag::GENERALIZED_TIME, b"20261119000000.123+0200"),
            (Tag::UTC_TIME, b"261119000000.123Z"),
        ];
        for (tag, content) in invalid {
            let encoded = encode_tlv(tag, content);
            assert_eq!(
                DerReader::new(&encoded)
                    .read_tlv()
                    .expect("time tlv")
                    .as_time(),
                Err(SecurityError::DerInvalid),
                "malformed time content must be rejected: {content:?}"
            );
        }
    }
}
