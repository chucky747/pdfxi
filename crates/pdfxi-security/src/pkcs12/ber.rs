//! A small BER reader for PKCS#12 containers.
//!
//! RFC 7292 archives are BER, not DER: Windows and OpenSSL emit
//! indefinite-length constructed `OCTET STRING` values for `authSafe` and
//! `safeContents`. [`crate::der`] is deliberately strict DER and rejects those,
//! so this module implements only the BER subset a PKCS#12 container can
//! legally use:
//!
//! - definite lengths, in short or long form (up to four length octets);
//! - indefinite lengths (`0x80`) terminated by end-of-contents (`00 00`),
//!   accepted only on constructed values;
//! - constructed and primitive values, so `OCTET STRING` chunks can be
//!   flattened.
//!
//! High-tag-number form is rejected: no PKCS#12 structure needs it. Every read
//! is bounds-checked and returns a typed [`SecurityError`]; nothing here
//! allocates, logs, or panics on attacker-controlled input.

use crate::error::{Result, SecurityError};

/// The identifier octet of a constructed value (bit 6 of the first octet).
const CONSTRUCTED: u8 = 0x20;
/// The `END-OF-CONTENTS` identifier octet used to close indefinite lengths.
const END_OF_CONTENTS: u8 = 0x00;
/// `SEQUENCE`, as used by every PKCS#12 structure this module reads.
pub(super) const SEQUENCE: u8 = 0x30;

/// `INTEGER`.
pub(super) const INTEGER: u8 = 0x02;
/// `OCTET STRING`.
pub(super) const OCTET_STRING: u8 = 0x04;
/// `OBJECT IDENTIFIER`.
pub(super) const OBJECT_IDENTIFIER: u8 = 0x06;

fn malformed(what: &'static str) -> SecurityError {
    SecurityError::PfxMalformed(what)
}

/// One decoded BER value: its identifier octet, content octets, and encoding.
#[derive(Debug, Clone, Copy)]
pub(super) struct Element<'a> {
    tag: u8,
    content: &'a [u8],
    encoded: &'a [u8],
}

impl<'a> Element<'a> {
    /// The identifier octet.
    pub(super) const fn tag(&self) -> u8 {
        self.tag
    }

    /// The complete encoding, including tag and length.
    pub(super) const fn encoded(&self) -> &'a [u8] {
        self.encoded
    }

    /// The content octets, without tag or length.
    ///
    /// Only the module's unit tests inspect raw content directly; production
    /// code uses the typed accessors below.
    #[cfg(test)]
    pub(super) const fn content(&self) -> &'a [u8] {
        self.content
    }

    /// Return `true` when the constructed bit is set.
    pub(super) const fn is_constructed(&self) -> bool {
        self.tag & CONSTRUCTED != 0
    }

    /// Interpret the value as a sequence of children.
    pub(super) fn children(&self) -> Result<BerReader<'a>> {
        if self.tag != SEQUENCE {
            return Err(malformed("expected a SEQUENCE"));
        }
        Ok(BerReader::new(self.content))
    }

    /// Interpret any constructed value as a sequence of children.
    ///
    /// Used for the `[0] EXPLICIT` wrappers in `ContentInfo`, `SafeBag`, and
    /// `CertBag`, whose tag is context-specific rather than `SEQUENCE`.
    pub(super) fn elements(&self) -> Result<BerReader<'a>> {
        if !self.is_constructed() {
            return Err(malformed("expected a constructed value"));
        }
        Ok(BerReader::new(self.content))
    }

    /// Flatten an `OCTET STRING`, concatenating constructed chunks.
    ///
    /// Returns an owned buffer because a constructed value can span many
    /// chunks. The caller is responsible for not retaining secrets longer than
    /// necessary.
    pub(super) fn octets(&self) -> Result<Vec<u8>> {
        if self.tag & 0x1f != OCTET_STRING {
            return Err(malformed("expected an OCTET STRING"));
        }
        if !self.is_constructed() {
            return Ok(self.content.to_vec());
        }
        let mut out = Vec::new();
        let mut reader = BerReader::new(self.content);
        while !reader.is_empty() {
            let chunk = reader.read()?;
            out.extend_from_slice(&chunk.octets()?);
        }
        Ok(out)
    }

    /// Flatten an implicitly tagged `OCTET STRING`.
    ///
    /// CMS `EncryptedContentInfo.encryptedContent` is `[0] IMPLICIT OCTET
    /// STRING`, so its identifier is context-specific rather than
    /// `OCTET STRING`; the content octets and optional chunking are the same.
    pub(super) fn implicit_octets(&self) -> Result<Vec<u8>> {
        if !self.is_constructed() {
            return Ok(self.content.to_vec());
        }
        let mut out = Vec::new();
        let mut reader = BerReader::new(self.content);
        while !reader.is_empty() {
            out.extend_from_slice(&reader.read()?.octets()?);
        }
        Ok(out)
    }

    /// Interpret the value as an `OBJECT IDENTIFIER` and return its arcs.
    pub(super) fn oid(&self) -> Result<Vec<u32>> {
        if self.tag != OBJECT_IDENTIFIER {
            return Err(malformed("expected an OBJECT IDENTIFIER"));
        }
        decode_oid(self.content)
    }

    /// Interpret the value as a non-negative `INTEGER` that fits a `u32`.
    pub(super) fn integer_u32(&self) -> Result<u32> {
        if self.tag != INTEGER {
            return Err(malformed("expected an INTEGER"));
        }
        if self.content.is_empty() || self.content.len() > 5 {
            return Err(malformed("INTEGER has an unusable length"));
        }
        if self.content.len() > 1 && self.content[0] == 0 && self.content[1] & 0x80 == 0 {
            return Err(malformed("INTEGER is not minimally encoded"));
        }
        if self.content[0] & 0x80 != 0 {
            return Err(malformed("INTEGER is negative"));
        }
        let mut value: u32 = 0;
        for byte in self.content {
            value = value
                .checked_mul(256)
                .and_then(|shifted| shifted.checked_add(u32::from(*byte)))
                .ok_or(malformed("INTEGER is too large"))?;
        }
        Ok(value)
    }
}

/// Decode the content octets of an `OBJECT IDENTIFIER` into its arcs.
///
/// PKCS#12 never uses an OID arc above `u32`, so a wider arc is a malformed
/// value rather than something to truncate.
fn decode_oid(content: &[u8]) -> Result<Vec<u32>> {
    if content.is_empty() {
        return Err(malformed("OBJECT IDENTIFIER is empty"));
    }
    if content.last().is_some_and(|byte| byte & 0x80 != 0) {
        return Err(malformed("OBJECT IDENTIFIER is truncated"));
    }
    let mut arcs = Vec::new();
    let mut value: u64 = 0;
    let mut pending = false;
    for byte in content {
        value = value
            .checked_mul(128)
            .and_then(|shifted| shifted.checked_add(u64::from(byte & 0x7f)))
            .ok_or(malformed("OBJECT IDENTIFIER arc overflows"))?;
        pending = byte & 0x80 != 0;
        if !pending {
            if arcs.is_empty() {
                if value > 2 * 40 + 39 {
                    return Err(malformed("OBJECT IDENTIFIER first arc is invalid"));
                }
                let first = if value < 40 {
                    0
                } else if value < 80 {
                    1
                } else {
                    2
                };
                arcs.push(first);
                arcs.push((value - u64::from(first) * 40) as u32);
            } else {
                arcs.push(u32::try_from(value).map_err(|_| {
                    SecurityError::PfxMalformed("OBJECT IDENTIFIER arc is too large")
                })?);
            }
            value = 0;
        }
    }
    if pending {
        return Err(malformed("OBJECT IDENTIFIER is truncated"));
    }
    Ok(arcs)
}

/// A cursor over a BER byte slice.
#[derive(Debug, Clone)]
pub(super) struct BerReader<'a> {
    data: &'a [u8],
    position: usize,
}

impl<'a> BerReader<'a> {
    /// Create a reader over `data`.
    pub(super) const fn new(data: &'a [u8]) -> Self {
        Self { data, position: 0 }
    }

    /// Return `true` when no bytes remain.
    pub(super) const fn is_empty(&self) -> bool {
        self.position >= self.data.len()
    }

    /// Return the bytes that have not been read yet.
    pub(super) fn remaining_bytes(&self) -> &'a [u8] {
        &self.data[self.position..]
    }

    /// Read one complete BER value.
    pub(super) fn read(&mut self) -> Result<Element<'a>> {
        let start = self.position;
        let tag = *self.data.get(start).ok_or(malformed("truncated tag"))?;
        if tag & 0x1f == 0x1f {
            return Err(malformed("high-tag-number form is not accepted"));
        }
        if tag == END_OF_CONTENTS {
            return Err(malformed("unexpected end-of-contents"));
        }
        let length_byte = *self
            .data
            .get(start + 1)
            .ok_or(malformed("truncated length"))?;
        if length_byte == 0x80 {
            if tag & CONSTRUCTED == 0 {
                return Err(malformed("indefinite length on a primitive value"));
            }
            let content_start = start + 2;
            let content_end = self.find_end(content_start)?;
            self.position = content_end + 2;
            return Ok(Element {
                tag,
                content: &self.data[content_start..content_end],
                encoded: &self.data[start..self.position],
            });
        }
        let (length, header) = if length_byte < 0x80 {
            (usize::from(length_byte), 2usize)
        } else {
            let count = usize::from(length_byte & 0x7f);
            if count == 0 || count > 4 {
                return Err(malformed("unsupported length form"));
            }
            let length_start = start + 2;
            let length_end = length_start
                .checked_add(count)
                .ok_or(malformed("length overflows"))?;
            let bytes = self
                .data
                .get(length_start..length_end)
                .ok_or(malformed("truncated length"))?;
            let mut decoded = 0usize;
            for byte in bytes {
                decoded = decoded
                    .checked_mul(256)
                    .and_then(|shifted| shifted.checked_add(usize::from(*byte)))
                    .ok_or(malformed("length overflows"))?;
            }
            (decoded, 2 + count)
        };
        let content_start = start
            .checked_add(header)
            .ok_or(malformed("length overflows"))?;
        let content_end = content_start
            .checked_add(length)
            .ok_or(malformed("length overflows"))?;
        let content = self
            .data
            .get(content_start..content_end)
            .ok_or(malformed("truncated value"))?;
        self.position = content_end;
        Ok(Element {
            tag,
            content,
            encoded: &self.data[start..content_end],
        })
    }

    /// Locate the end-of-contents marker that closes an indefinite length.
    ///
    /// The scan walks nested values so an inner `00 00` inside a definite-length
    /// value cannot terminate an outer indefinite length early.
    fn find_end(&self, mut cursor: usize) -> Result<usize> {
        while cursor < self.data.len() {
            let tag = self.data[cursor];
            if tag == END_OF_CONTENTS {
                if self.data.get(cursor + 1) == Some(&0) {
                    return Ok(cursor);
                }
                return Err(malformed("malformed end-of-contents"));
            }
            if tag & 0x1f == 0x1f {
                return Err(malformed("high-tag-number form is not accepted"));
            }
            let length_byte = *self
                .data
                .get(cursor + 1)
                .ok_or(malformed("truncated length"))?;
            if length_byte == 0x80 {
                if tag & CONSTRUCTED == 0 {
                    return Err(malformed("indefinite length on a primitive value"));
                }
                cursor = self.find_end(cursor + 2)? + 2;
                continue;
            }
            let (length, header) = if length_byte < 0x80 {
                (usize::from(length_byte), 2usize)
            } else {
                let count = usize::from(length_byte & 0x7f);
                if count == 0 || count > 4 {
                    return Err(malformed("unsupported length form"));
                }
                let bytes = self
                    .data
                    .get(cursor + 2..cursor + 2 + count)
                    .ok_or(malformed("truncated length"))?;
                let mut decoded = 0usize;
                for byte in bytes {
                    decoded = decoded
                        .checked_mul(256)
                        .and_then(|shifted| shifted.checked_add(usize::from(*byte)))
                        .ok_or(malformed("length overflows"))?;
                }
                (decoded, 2 + count)
            };
            cursor = cursor
                .checked_add(header)
                .and_then(|value| value.checked_add(length))
                .ok_or(malformed("length overflows"))?;
        }
        Err(malformed("indefinite length is never closed"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read_one(bytes: &[u8]) -> Element<'_> {
        let mut reader = BerReader::new(bytes);
        reader.read().expect("read BER value")
    }

    #[test]
    fn definite_and_indefinite_lengths_are_read() {
        let definite = [0x30, 0x03, 0x02, 0x01, 0x07];
        let element = read_one(&definite);
        assert_eq!(element.tag(), SEQUENCE);
        assert_eq!(element.content(), &[0x02, 0x01, 0x07]);

        let indefinite = [0x30, 0x80, 0x02, 0x01, 0x07, 0x00, 0x00];
        let element = read_one(&indefinite);
        assert_eq!(element.tag(), SEQUENCE);
        assert_eq!(element.content(), &[0x02, 0x01, 0x07]);
    }

    #[test]
    fn an_inner_end_of_contents_does_not_close_the_outer_value() {
        // Outer indefinite SEQUENCE holding one definite OCTET STRING that
        // contains the bytes 00 00 in its payload.
        let bytes = [
            0x30, 0x80, 0x04, 0x02, 0x00, 0x00, 0x02, 0x01, 0x01, 0x00, 0x00,
        ];
        let element = read_one(&bytes);
        assert_eq!(
            element.content(),
            &[0x04, 0x02, 0x00, 0x00, 0x02, 0x01, 0x01]
        );
        let mut children = element.children().expect("children");
        let first = children.read().expect("octet string");
        assert_eq!(first.tag(), OCTET_STRING);
        assert_eq!(first.octets().expect("flatten"), vec![0, 0]);
    }

    #[test]
    fn constructed_octet_strings_are_flattened() {
        let bytes = [0x24, 0x08, 0x04, 0x02, 0xde, 0xad, 0x04, 0x02, 0xbe, 0xef];
        let element = read_one(&bytes);
        assert_eq!(
            element.octets().expect("flatten"),
            vec![0xde, 0xad, 0xbe, 0xef]
        );
    }

    #[test]
    fn malformed_input_is_a_typed_error() {
        for bytes in [
            &[0x30][..],
            &[0x30, 0x80, 0x02, 0x01, 0x01][..],
            &[0x1f, 0x01][..],
            &[0x04, 0x80, 0x00, 0x00][..],
            &[0x30, 0x85, 1, 2, 3, 4, 5][..],
            &[0x30, 0x05, 0x02, 0x01, 0x01][..],
        ] {
            let mut reader = BerReader::new(bytes);
            assert!(reader.read().is_err(), "expected an error for {bytes:?}");
        }
    }

    #[test]
    fn oids_decode_including_two_byte_arcs() {
        let element = read_one(&[
            0x06, 0x09, 0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x07, 0x02,
        ]);
        assert_eq!(
            element.oid().expect("oid"),
            vec![1, 2, 840, 113_549, 1, 7, 2]
        );
        let large = read_one(&[0x06, 0x03, 0x2a, 0x03, 0x04]);
        assert_eq!(large.oid().expect("oid"), vec![1, 2, 3, 4]);
        assert!(read_one(&[0x06, 0x02, 0x2a, 0x86]).oid().is_err());
    }
}
