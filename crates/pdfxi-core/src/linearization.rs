//! Parsing of the PDF linearization hint dictionary (ISO 32000-1, Annex F).
//!
//! A linearized PDF begins with a small parameter dictionary whose key
//! `/Linearized` marks the file as optimized for "first page at a time" network
//! delivery. This module locates that dictionary, validates its fields against
//! the actual file length, and exposes a small summary with the first-page
//! object number, the declared file length and the hint-stream ranges.
//!
//! This is a **reader for the hint dictionary only**. It does not verify that
//! the rest of the file really is laid out in linearized order, and it does not
//! write linearized files. Use [`crate::Document::linearization`] to inspect a
//! loaded document, or [`parse_linearization`] on a raw byte slice.

use crate::lexer::Lexer;
use crate::object::{Dictionary, Object, ObjectId};
use crate::{Error, Result};

/// Maximum number of leading indirect objects examined for a hint dictionary.
const MAX_SCAN_OBJECTS: usize = 8;
/// Accepted deviation of the `/Linearized` marker from `1.0`.
const MARKER_TOLERANCE: f64 = 1e-6;

/// Offset and length of one of the two hint streams described by `/H`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HintStreamRange {
    /// Byte offset of the hint stream from the start of the file.
    pub offset: u64,
    /// Number of bytes in the hint stream.
    pub length: u64,
}

impl HintStreamRange {
    /// Return the exclusive end offset of this range using checked addition.
    pub fn end(&self) -> Option<u64> {
        self.offset.checked_add(self.length)
    }
}

/// Validated summary of a linearization parameter dictionary.
///
/// All byte offsets are checked to lie inside the file, and every required
/// field must be present with its specified type before this value is built.
#[derive(Debug, Clone, PartialEq)]
pub struct Linearization {
    /// Identifier of the indirect object holding the hint dictionary.
    pub object_id: ObjectId,
    /// Object number of the first page object (`/O`).
    pub first_page_object: u32,
    /// Declared total file length in bytes (`/L`).
    pub file_length: u64,
    /// Offset of the end of the first-page section (`/E`), when present.
    pub first_page_end: Option<u64>,
    /// Declared page count (`/N`), when present.
    pub page_count: Option<u32>,
    /// Offset of the main xref section (`/T`), when present.
    pub main_xref_offset: Option<u64>,
    /// Declared PDF version (`/V`), when present.
    pub version: Option<f64>,
    /// Location of the first hint stream.
    pub hint: HintStreamRange,
    /// Location of the optional shared-object hint stream.
    pub shared_hint: Option<HintStreamRange>,
}

/// Parse the linearization hint from `data`.
///
/// Returns `Ok(None)` when no `/Linearized` dictionary appears among the first
/// few objects. Returns an error when a hint dictionary is present but invalid,
/// so a damaged hint is never silently treated as an ordinary PDF.
///
/// # Errors
///
/// Returns [`Error::MalformedLinearization`] for a missing field, wrong type,
/// negative value, out-of-range offset or a `/Linearized` marker other than 1.
pub fn parse_linearization(data: &[u8]) -> Result<Option<Linearization>> {
    let Some((dictionary, object_id)) = find_hint_dictionary(data)? else {
        return Ok(None);
    };

    let marker = dictionary
        .get("Linearized")
        .and_then(Object::as_f64)
        .ok_or_else(|| Error::MalformedLinearization("/Linearized is not numeric".to_owned()))?;
    if !marker.is_finite() || (marker - 1.0).abs() > MARKER_TOLERANCE {
        return Err(Error::MalformedLinearization(format!(
            "/Linearized marker must be 1.0, found {marker}"
        )));
    }

    let file_length = required_non_negative(&dictionary, "L")?;
    if file_length == 0 {
        return Err(Error::MalformedLinearization(
            "/L file length must be positive".to_owned(),
        ));
    }
    if file_length > data.len() as u64 {
        return Err(Error::MalformedLinearization(format!(
            "/L declares {file_length} bytes but the file is {} bytes",
            data.len()
        )));
    }

    let first_page_object = u32::try_from(required_non_negative(&dictionary, "O")?)
        .ok()
        .filter(|number| *number != 0)
        .ok_or_else(|| {
            Error::MalformedLinearization("/O first-page object number is invalid".to_owned())
        })?;

    let (hint, shared_hint) = parse_hint_array(&dictionary, file_length)?;

    let first_page_end = optional_non_negative(&dictionary, "E")?;
    if first_page_end.is_some_and(|end| end >= file_length) {
        return Err(Error::MalformedLinearization(
            "/E first-page end lies outside the declared file length".to_owned(),
        ));
    }
    let main_xref_offset = optional_non_negative(&dictionary, "T")?;
    if main_xref_offset.is_some_and(|offset| offset >= file_length) {
        return Err(Error::MalformedLinearization(
            "/T xref offset lies outside the declared file length".to_owned(),
        ));
    }
    let page_count =
        match optional_non_negative(&dictionary, "N")? {
            Some(value) => Some(u32::try_from(value).map_err(|_| {
                Error::MalformedLinearization("/N page count exceeds u32".to_owned())
            })?),
            None => None,
        };
    let version =
        match dictionary.get("V") {
            Some(value) => Some(value.as_f64().filter(|number| *number > 0.0).ok_or_else(
                || Error::MalformedLinearization("/V must be a positive number".to_owned()),
            )?),
            None => None,
        };

    Ok(Some(Linearization {
        object_id,
        first_page_object,
        file_length,
        first_page_end,
        page_count,
        main_xref_offset,
        version,
        hint,
        shared_hint,
    }))
}

/// Locate the first indirect object whose dictionary contains `/Linearized`.
fn find_hint_dictionary(data: &[u8]) -> Result<Option<(Dictionary, ObjectId)>> {
    let mut position = 0usize;
    for _ in 0..MAX_SCAN_OBJECTS {
        let mut lexer = Lexer::new(data, position)?;
        lexer.skip_ws();
        let Some(number_token) = lexer.read_regular() else {
            return Ok(None);
        };
        if number_token.is_empty() || !number_token.iter().all(u8::is_ascii_digit) {
            return Ok(None);
        }
        lexer.skip_ws();
        let Some(generation_token) = lexer.read_regular() else {
            return Ok(None);
        };
        if generation_token.is_empty() || !generation_token.iter().all(u8::is_ascii_digit) {
            return Ok(None);
        }
        let number = parse_decimal(number_token, "linearization object number")?;
        let generation = parse_decimal(generation_token, "linearization generation")?;
        let number = u32::try_from(number).map_err(|_| {
            Error::MalformedLinearization("linearization object number exceeds u32".to_owned())
        })?;
        let generation = u16::try_from(generation).map_err(|_| {
            Error::MalformedLinearization("linearization generation exceeds 65535".to_owned())
        })?;
        if lexer.expect_keyword(b"obj").is_err() {
            return Ok(None);
        }
        let object = lexer.parse_value(0)?;
        if let Some(dictionary) = object.as_dictionary()
            && dictionary.contains_key("Linearized")
        {
            return Ok(Some((
                dictionary.clone(),
                ObjectId::new(number, generation),
            )));
        }
        let tail = data.get(lexer.position()..).unwrap_or(&[]);
        let Some(relative_end) = tail.windows(6).position(|window| window == b"endobj") else {
            return Ok(None);
        };
        position = lexer.position() + relative_end + 6;
    }
    Ok(None)
}

/// Read a required non-negative integer field.
fn required_non_negative(dictionary: &Dictionary, key: &str) -> Result<u64> {
    let value = dictionary
        .get(key)
        .and_then(Object::as_i64)
        .ok_or_else(|| Error::MalformedLinearization(format!("missing or non-integer /{key}")))?;
    u64::try_from(value)
        .map_err(|_| Error::MalformedLinearization(format!("/{key} must be non-negative")))
}

/// Read an optional non-negative integer field.
fn optional_non_negative(dictionary: &Dictionary, key: &str) -> Result<Option<u64>> {
    match dictionary.get(key) {
        None => Ok(None),
        Some(value) => {
            let number = value.as_i64().ok_or_else(|| {
                Error::MalformedLinearization(format!("/{key} must be an integer"))
            })?;
            let number = u64::try_from(number).map_err(|_| {
                Error::MalformedLinearization(format!("/{key} must be non-negative"))
            })?;
            Ok(Some(number))
        }
    }
}

/// Parse the `/H` hint array into one or two validated ranges.
fn parse_hint_array(
    dictionary: &Dictionary,
    file_length: u64,
) -> Result<(HintStreamRange, Option<HintStreamRange>)> {
    let values = dictionary
        .get("H")
        .and_then(Object::as_array)
        .ok_or_else(|| Error::MalformedLinearization("missing or invalid /H array".to_owned()))?;
    if values.len() != 2 && values.len() != 4 {
        return Err(Error::MalformedLinearization(format!(
            "/H must contain two or four integers, found {}",
            values.len()
        )));
    }
    let integers = values
        .iter()
        .map(|value| {
            value.as_i64().ok_or_else(|| {
                Error::MalformedLinearization("/H entries must be integers".to_owned())
            })
        })
        .collect::<Result<Vec<i64>>>()?;
    let first = range_from_fields(integers[0], integers[1], file_length, "first hint stream")?;
    let shared = if integers.len() == 4 {
        // A value of -1 for the shared-object hint offset means the stream is
        // absent; Annex F requires its length to be zero in that case.
        if integers[2] == -1 {
            if integers[3] != 0 {
                return Err(Error::MalformedLinearization(
                    "absent shared hint must have zero length".to_owned(),
                ));
            }
            None
        } else {
            Some(range_from_fields(
                integers[2],
                integers[3],
                file_length,
                "shared hint stream",
            )?)
        }
    } else {
        None
    };
    Ok((first, shared))
}

/// Build and bounds-check one hint range from an offset/length pair.
fn range_from_fields(
    offset: i64,
    length: i64,
    file_length: u64,
    label: &str,
) -> Result<HintStreamRange> {
    let offset = u64::try_from(offset).map_err(|_| {
        Error::MalformedLinearization(format!("{label} offset must be non-negative"))
    })?;
    let length = u64::try_from(length).map_err(|_| {
        Error::MalformedLinearization(format!("{label} length must be non-negative"))
    })?;
    if offset == 0 || length == 0 {
        return Err(Error::MalformedLinearization(format!(
            "{label} has a zero offset or length"
        )));
    }
    let end = offset
        .checked_add(length)
        .ok_or_else(|| Error::MalformedLinearization(format!("{label} range overflows")))?;
    if end > file_length {
        return Err(Error::MalformedLinearization(format!(
            "{label} ends beyond the declared file length"
        )));
    }
    Ok(HintStreamRange { offset, length })
}

/// Parse a decimal token into a `usize` with a structured error.
fn parse_decimal(token: &[u8], context: &str) -> Result<usize> {
    let text = std::str::from_utf8(token)
        .map_err(|_| Error::MalformedLinearization(format!("{context} is not UTF-8")))?;
    text.parse::<usize>()
        .map_err(|_| Error::MalformedLinearization(format!("invalid {context}: {text}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal file whose first object is a linearization dictionary.
    ///
    /// Every numeric placeholder is exactly ten digits wide, so replacing the
    /// zero placeholders with the computed length never changes the byte count.
    fn linearized_pdf(hint_offset: u64, hint_length: u64, first_page: u32, pages: u32) -> Vec<u8> {
        let base = format!(
            "%PDF-1.7\n1 0 obj\n<< /Linearized 1 /L 0000000000 /H [ {hint_offset:010} {hint_length:010} ] /O {first_page:010} /E 0000000000 /N {pages:010} /T 0000000000 /V 1.7 >>\nendobj\n%%EOF\n"
        );
        let length = base.len() as u64;
        let first_page_end = length / 2;
        let main_xref = length.saturating_sub(6);
        base.replacen("0000000000", &format!("{length:010}"), 1)
            .replacen("0000000000", &format!("{first_page_end:010}"), 1)
            .replacen("0000000000", &format!("{main_xref:010}"), 1)
            .into_bytes()
    }

    /// Replace the first occurrence of `from` with an equal-length `to`.
    fn patch(bytes: &mut [u8], from: &[u8], to: &[u8]) {
        assert_eq!(from.len(), to.len(), "patch must preserve length");
        let at = bytes
            .windows(from.len())
            .position(|window| window == from)
            .expect("pattern exists");
        bytes[at..at + to.len()].copy_from_slice(to);
    }

    #[test]
    fn parses_valid_linearization_hint() {
        let bytes = linearized_pdf(60, 40, 5, 2);
        let summary = parse_linearization(&bytes)
            .expect("valid hint parses")
            .expect("hint present");
        assert_eq!(summary.object_id, ObjectId::new(1, 0));
        assert_eq!(summary.first_page_object, 5);
        assert_eq!(summary.file_length, bytes.len() as u64);
        assert_eq!(summary.page_count, Some(2));
        assert_eq!(
            summary.hint,
            HintStreamRange {
                offset: 60,
                length: 40
            }
        );
        assert_eq!(summary.hint.end(), Some(100));
        assert_eq!(summary.shared_hint, None);
        assert_eq!(summary.version, Some(1.7));
        assert!(summary.first_page_end.is_some());
        assert!(summary.main_xref_offset.is_some());

        let document = crate::Document::from_bytes(bytes).expect("header parses");
        assert_eq!(
            document.linearization().expect("document hint"),
            Some(summary)
        );
    }

    #[test]
    fn parses_shared_hint_stream_range() {
        let bytes = linearized_pdf(60, 40, 5, 1);
        // Growing /H makes the physical file longer than the declared /L, which
        // is still valid because /L must not exceed the real file length.
        let bytes = String::from_utf8_lossy(&bytes)
            .replace(
                "/H [ 0000000060 0000000040 ]",
                "/H [ 0000000060 0000000040 0000000120 0000000010 ]",
            )
            .into_bytes();
        let summary = parse_linearization(&bytes)
            .expect("shared hint parses")
            .expect("hint present");
        assert_eq!(
            summary.shared_hint,
            Some(HintStreamRange {
                offset: 120,
                length: 10
            })
        );
    }

    #[test]
    fn rejects_malformed_linearization_hint() {
        let valid = linearized_pdf(60, 40, 5, 1);

        let mut wrong_marker = valid.clone();
        patch(&mut wrong_marker, b"/Linearized 1", b"/Linearized 2");
        assert!(matches!(
            parse_linearization(&wrong_marker),
            Err(Error::MalformedLinearization(_))
        ));

        let mut huge_length = valid.clone();
        patch(
            &mut huge_length,
            format!("/L {:010}", valid.len()).as_bytes(),
            b"/L 9999999999",
        );
        assert!(matches!(
            parse_linearization(&huge_length),
            Err(Error::MalformedLinearization(_))
        ));

        let mut zero_first_page = valid.clone();
        patch(&mut zero_first_page, b"/O 0000000005", b"/O 0000000000");
        assert!(matches!(
            parse_linearization(&zero_first_page),
            Err(Error::MalformedLinearization(_))
        ));

        let mut bad_hint = valid.clone();
        patch(&mut bad_hint, b"/H [ 0000000060", b"/H [ -000000060");
        assert!(matches!(
            parse_linearization(&bad_hint),
            Err(Error::MalformedLinearization(_))
        ));

        let mut out_of_range_hint = valid.clone();
        patch(&mut out_of_range_hint, b"0000000040 ]", b"9999999999 ]");
        assert!(matches!(
            parse_linearization(&out_of_range_hint),
            Err(Error::MalformedLinearization(_))
        ));
    }

    #[test]
    fn non_linearized_document_returns_none() {
        let bytes = pdfxi_testkit::minimal_pdf();
        let document = crate::Document::from_bytes(bytes.clone()).expect("valid fixture");
        assert!(matches!(parse_linearization(&bytes), Ok(None)));
        assert!(matches!(document.linearization(), Ok(None)));
    }
}
