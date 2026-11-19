//! Repair paths for common malformed PDFs.
//!
//! [`repair_document`] scans an untrusted byte stream for indirect object
//! headers, rebuilds a correct classic cross-reference table, and returns a
//! standalone PDF that reparses through [`crate::Document`]. Every deviation
//! from a well-formed file is reported through [`RepairWarning`] in a
//! deterministic order; a damaged file is never silently accepted.
//!
//! Unknown object bodies and stream bytes are copied verbatim whenever they
//! are intact, so unrelated content survives the repair. Only structural
//! pieces (the header, xref table, trailer, `startxref`, `%%EOF` and incorrect
//! stream `/Length` values) are rewritten.

use std::collections::BTreeMap;

use crate::compatibility::{self, CompatibilityWarning, PdfVersion};
use crate::lexer::Lexer;
use crate::object::{Dictionary, Object, ObjectId};
use crate::parser;
use crate::writer;

/// A fatal condition that prevents any meaningful repair.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RepairError {
    /// No indirect object header could be found in the input.
    #[error("no indirect objects found; the input is not a recoverable PDF")]
    NoObjects,
    /// Neither a trailer `/Root` nor a catalog object could be identified.
    #[error("no document catalog found; cannot determine /Root")]
    NoCatalog,
    /// The rebuilt byte stream failed structural verification.
    #[error("repair produced a structurally invalid PDF: {0}")]
    VerificationFailed(String),
    /// A structural table could not be serialized.
    #[error("repair could not serialize a structural table: {0}")]
    SerializationFailed(String),
}

/// A non-fatal repair that was applied to produce a valid document.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RepairWarning {
    /// The header was missing or unreadable and a canonical one was written.
    #[error("header normalized to version {version} ({reason})")]
    HeaderNormalized {
        /// The version written into the repaired header.
        version: PdfVersion,
        /// Why the original header was replaced.
        reason: &'static str,
    },
    /// The `%PDF-` marker was present but not at byte zero.
    #[error("%PDF- header at byte {offset} was moved to the front")]
    HeaderNotAtStart {
        /// Byte offset of the original `%PDF-` marker.
        offset: usize,
    },
    /// No `startxref` marker was present, so objects were scanned instead.
    #[error("startxref marker is missing; xref rebuilt from an object scan")]
    StartXrefMissing,
    /// The `startxref` value did not point at a readable xref section.
    #[error("startxref offset {offset} did not point at an xref section")]
    StartXrefMisplaced {
        /// The unusable offset read from `startxref`.
        offset: usize,
    },
    /// The xref section was unreadable and was rebuilt from a scan.
    #[error("xref section was unreadable; rebuilt from an object scan")]
    XrefRebuiltByScan,
    /// One or more xref entries disagreed with the real object positions.
    #[error("{count} xref entry offset(s) did not match the scanned objects")]
    WrongXrefOffsets {
        /// Number of incorrect in-use xref entries.
        count: usize,
    },
    /// A stream's declared `/Length` did not match its actual payload.
    #[error("object {object} declared /Length {declared} but the stream is {actual} bytes")]
    StreamLengthCorrected {
        /// The object number whose `/Length` was rewritten.
        object: u32,
        /// The incorrect declared length.
        declared: i64,
        /// The corrected length in bytes.
        actual: usize,
    },
    /// An object body ended without an `endobj` keyword.
    #[error("object {object} was missing endobj; one was synthesized")]
    TruncatedObjectEnd {
        /// The object number that was closed.
        object: u32,
    },
    /// The final `%%EOF` marker was absent.
    #[error("the final %%EOF marker was missing; a new trailer and EOF were written")]
    TruncatedEof,
    /// An incremental revision repeated an object number; the newest won.
    #[error("object number {number} appeared more than once; the latest revision was kept")]
    DuplicateObject {
        /// The duplicated object number.
        number: u32,
    },
}

/// The ordered list of repairs applied to a damaged document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepairReport {
    warnings: Vec<RepairWarning>,
}

impl RepairReport {
    /// All applied repairs in deterministic order: header issues, then xref
    /// issues, then per-object issues ordered by object number.
    pub fn warnings(&self) -> &[RepairWarning] {
        &self.warnings
    }

    /// Return `true` when at least one repair was necessary.
    pub fn is_empty(&self) -> bool {
        self.warnings.is_empty()
    }

    /// Return the number of applied repairs.
    pub fn len(&self) -> usize {
        self.warnings.len()
    }
}

/// A repaired PDF together with the effective version and repair report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepairedDocument {
    bytes: Vec<u8>,
    version: PdfVersion,
    report: RepairReport,
}

impl RepairedDocument {
    /// The repaired, standalone PDF byte stream.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// The effective version written into the repaired header.
    pub fn version(&self) -> PdfVersion {
        self.version
    }

    /// The repairs that were applied to produce [`Self::bytes`].
    pub fn report(&self) -> &RepairReport {
        &self.report
    }
}

/// Return `true` when a byte separates PDF tokens.
fn is_delimiter(byte: u8) -> bool {
    byte.is_ascii_whitespace()
        || matches!(
            byte,
            b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%'
        )
}

/// Find a standalone keyword at or after `from`.
fn find_keyword(data: &[u8], from: usize, keyword: &[u8]) -> Option<usize> {
    let mut search = from.min(data.len());
    loop {
        let tail = data.get(search..)?;
        let relative = tail
            .windows(keyword.len())
            .position(|window| window == keyword)?;
        let position = search + relative;
        let before_ok = position == 0 || is_delimiter(data[position - 1]);
        let after = position + keyword.len();
        let after_ok = after >= data.len() || is_delimiter(data[after]);
        if before_ok && after_ok {
            return Some(position);
        }
        search = position + 1;
    }
}

/// Read a decimal run starting at `at`, returning its value and end offset.
fn decimal_at(data: &[u8], at: usize) -> Option<(u64, usize)> {
    let mut cursor = at;
    let start = cursor;
    let mut value = 0u64;
    while let Some(byte) = data.get(cursor).copied() {
        if !byte.is_ascii_digit() {
            break;
        }
        value = value.checked_mul(10)?.checked_add(u64::from(byte - b'0'))?;
        cursor += 1;
    }
    if cursor == start {
        None
    } else {
        Some((value, cursor))
    }
}

/// Parse an indirect object header (`N G obj`) at byte position `i`.
fn parse_object_header(data: &[u8], i: usize) -> Option<(u32, u16, usize)> {
    if i > 0 && !data[i - 1].is_ascii_whitespace() {
        return None;
    }
    let (number, after_number) = decimal_at(data, i)?;
    if number > u64::from(u32::MAX)
        || data
            .get(after_number)
            .is_none_or(|b| !b.is_ascii_whitespace())
    {
        return None;
    }
    let mut cursor = after_number;
    while data.get(cursor).is_some_and(u8::is_ascii_whitespace) {
        cursor += 1;
    }
    let (generation, after_generation) = decimal_at(data, cursor)?;
    if generation > u64::from(u16::MAX)
        || data
            .get(after_generation)
            .is_none_or(|b| !b.is_ascii_whitespace())
    {
        return None;
    }
    cursor = after_generation;
    while data.get(cursor).is_some_and(u8::is_ascii_whitespace) {
        cursor += 1;
    }
    if !data.get(cursor..)?.starts_with(b"obj") {
        return None;
    }
    let after_obj = cursor + 3;
    if data.get(after_obj).is_some_and(|b| !is_delimiter(*b)) {
        return None;
    }
    Some((number as u32, generation as u16, after_obj))
}

/// A direct integer `/Length` value and the byte span of its number token.
#[derive(Debug, Clone, Copy)]
struct LengthToken {
    value: i64,
    span: (usize, usize),
}

/// Find the direct integer `/Length` entry of a stream dictionary in a range.
fn length_token(data: &[u8], from: usize, to: usize) -> Option<LengthToken> {
    let region = data.get(from..to)?;
    let relative = region.windows(7).position(|window| window == b"/Length")?;
    let name_pos = from + relative;
    if name_pos > 0 && !is_delimiter(data[name_pos - 1]) {
        return None;
    }
    let mut cursor = name_pos + 7;
    while data.get(cursor).is_some_and(u8::is_ascii_whitespace) {
        cursor += 1;
    }
    let start = cursor;
    if matches!(data.get(cursor), Some(b'+') | Some(b'-')) {
        cursor += 1;
    }
    let digits_start = cursor;
    while data.get(cursor).is_some_and(u8::is_ascii_digit) {
        cursor += 1;
    }
    if cursor == digits_start {
        // Indirect or non-integer /Length: the normal parser scans to
        // endstream in that case, so no repair is required here.
        return None;
    }
    let text = std::str::from_utf8(data.get(start..cursor)?).ok()?;
    let value = text.parse::<i64>().ok()?;
    // Reject an indirect reference such as `/Length 12 0 R`.
    let mut probe = cursor;
    while data.get(probe).is_some_and(u8::is_ascii_whitespace) {
        probe += 1;
    }
    if data.get(probe..).is_some_and(|tail| tail.starts_with(b"R")) {
        return None;
    }
    Some(LengthToken {
        value,
        span: (start, cursor),
    })
}

/// A correction that must be applied to a stream object body.
#[derive(Debug, Clone, Copy)]
struct LengthFix {
    declared: i64,
    actual: usize,
    span: (usize, usize),
}

/// The byte boundary of an object and any correction it needs.
#[derive(Debug, Clone, Copy)]
struct ObjectEnd {
    end: usize,
    truncated: bool,
    fix: Option<LengthFix>,
}

/// Determine where an object ends, accounting for stream payloads.
fn object_end(data: &[u8], value_start: usize) -> ObjectEnd {
    let endobj = find_keyword(data, value_start, b"endobj");
    let stream = find_keyword(data, value_start, b"stream");
    if let Some(stream_pos) = stream
        && endobj.is_none_or(|end| stream_pos < end)
    {
        return stream_object_end(data, value_start, stream_pos);
    }
    match endobj {
        Some(position) => ObjectEnd {
            end: position + 6,
            truncated: false,
            fix: None,
        },
        None => ObjectEnd {
            end: data.len(),
            truncated: true,
            fix: None,
        },
    }
}

/// Compute the true payload length of a stream object and locate its end.
fn stream_object_end(data: &[u8], value_start: usize, stream_pos: usize) -> ObjectEnd {
    let mut cursor = stream_pos + 6;
    if data.get(cursor) == Some(&b'\r') {
        cursor += 1;
        if data.get(cursor) == Some(&b'\n') {
            cursor += 1;
        }
    } else if data.get(cursor) == Some(&b'\n') {
        cursor += 1;
    }
    let data_start = cursor;

    let endstream = find_keyword(data, data_start, b"endstream");
    let (actual, after_stream) = match endstream {
        Some(position) => {
            let mut payload_end = position;
            if payload_end > data_start && data[payload_end - 1] == b'\n' {
                payload_end -= 1;
                if payload_end > data_start && data[payload_end - 1] == b'\r' {
                    payload_end -= 1;
                }
            } else if payload_end > data_start && data[payload_end - 1] == b'\r' {
                payload_end -= 1;
            }
            (payload_end.saturating_sub(data_start), position + 9)
        }
        None => (data.len().saturating_sub(data_start), data.len()),
    };

    let token = length_token(data, value_start, stream_pos);
    let fix = match token {
        Some(token) if token.value >= 0 => {
            let declared = usize::try_from(token.value).unwrap_or(usize::MAX);
            let candidate = data_start.saturating_add(declared);
            let valid = data.get(candidate..).is_some_and(|tail| {
                let mut probe = 0;
                while probe < tail.len() && tail[probe].is_ascii_whitespace() {
                    probe += 1;
                }
                tail.get(probe..)
                    .is_some_and(|rest| rest.starts_with(b"endstream"))
            });
            if valid {
                None
            } else {
                Some(LengthFix {
                    declared: token.value,
                    actual,
                    span: token.span,
                })
            }
        }
        _ => None,
    };

    match find_keyword(data, after_stream, b"endobj") {
        Some(position) => ObjectEnd {
            end: position + 6,
            truncated: false,
            fix,
        },
        None => ObjectEnd {
            end: data.len(),
            truncated: true,
            fix,
        },
    }
}

/// One scanned indirect object, copied (and possibly corrected) from the input.
#[derive(Debug, Clone)]
struct ScannedObject {
    number: u32,
    generation: u16,
    start: usize,
    body: Vec<u8>,
}

/// An object-scoped warning paired with a stable ordering rank.
struct RankedWarning {
    number: u32,
    rank: u8,
    warning: RepairWarning,
}

/// Scan all indirect objects and correct the recoverable ones.
fn scan_objects(data: &[u8]) -> (BTreeMap<u32, ScannedObject>, Vec<RepairWarning>) {
    let mut objects = BTreeMap::new();
    let mut ranked = Vec::new();
    let mut cursor = 0usize;
    while cursor < data.len() {
        if let Some((number, generation, value_start)) = parse_object_header(data, cursor) {
            let object_end = object_end(data, value_start);
            if objects.contains_key(&number) {
                ranked.push(RankedWarning {
                    number,
                    rank: 0,
                    warning: RepairWarning::DuplicateObject { number },
                });
            }
            let mut body = data.get(cursor..object_end.end).unwrap_or(&[]).to_vec();
            if let Some(fix) = object_end.fix {
                let (start, end) = fix.span;
                if start >= cursor && end <= object_end.end {
                    let relative_start = start - cursor;
                    let relative_end = end - cursor;
                    body.splice(relative_start..relative_end, fix.actual.to_string().bytes());
                    ranked.push(RankedWarning {
                        number,
                        rank: 1,
                        warning: RepairWarning::StreamLengthCorrected {
                            object: number,
                            declared: fix.declared,
                            actual: fix.actual,
                        },
                    });
                }
            }
            if object_end.truncated {
                body.extend_from_slice(b"\nendobj");
                ranked.push(RankedWarning {
                    number,
                    rank: 2,
                    warning: RepairWarning::TruncatedObjectEnd { object: number },
                });
            }
            objects.insert(
                number,
                ScannedObject {
                    number,
                    generation,
                    start: cursor,
                    body,
                },
            );
            cursor = object_end.end.max(cursor + 1);
        } else {
            cursor += 1;
        }
    }
    ranked.sort_by_key(|item| (item.number, item.rank));
    let warnings = ranked.into_iter().map(|item| item.warning).collect();
    (objects, warnings)
}

/// Parse the most recent trailer dictionary, if one is readable.
fn trailer_dictionary(data: &[u8]) -> Option<Dictionary> {
    let mut from = 0usize;
    loop {
        let tail = data.get(from..)?;
        let relative = tail.windows(7).position(|window| window == b"trailer")?;
        let position = from + relative;
        let delimiter_ok = position == 0 || is_delimiter(data[position - 1]);
        let newline_ok = data.get(position + 7).is_some_and(u8::is_ascii_whitespace);
        if delimiter_ok && newline_ok {
            let mut lexer = Lexer::new(data, position + 7).ok()?;
            if let Ok(Object::Dictionary(dictionary)) = lexer.parse_value(0) {
                return Some(dictionary);
            }
        }
        from = position + 1;
    }
}

/// Read the numeric value following the final `startxref` marker.
fn raw_startxref(data: &[u8]) -> Option<usize> {
    let position = data.windows(9).rposition(|window| window == b"startxref")?;
    let mut cursor = position + 9;
    while data.get(cursor).is_some_and(u8::is_ascii_whitespace) {
        cursor += 1;
    }
    let (value, _) = decimal_at(data, cursor)?;
    usize::try_from(value).ok()
}

/// Return `true` when a body contains a `/Type /Catalog` entry.
fn is_catalog(body: &[u8]) -> bool {
    let Some(relative) = body.windows(8).position(|window| window == b"/Catalog") else {
        return false;
    };
    let before = &body[..relative];
    let mut end = before.len();
    while end > 0 && before[end - 1].is_ascii_whitespace() {
        end -= 1;
    }
    before
        .get(..end)
        .is_some_and(|prefix| prefix.ends_with(b"/Type"))
}

/// Repair `input` into a standalone, structurally valid PDF.
///
/// The scanner locates indirect objects directly, so it does not trust the
/// `startxref` value or any xref entry. It rebuilds a classic xref table with
/// byte-accurate offsets, fixes incorrect stream `/Length` integers, synthesizes
/// missing `endobj`/`%%EOF` markers, and keeps the newest revision of duplicate
/// object numbers. The result is verified by re-parsing its structure before
/// it is returned.
///
/// # Errors
///
/// Returns [`RepairError::NoObjects`] when no indirect object can be found,
/// [`RepairError::NoCatalog`] when neither the trailer nor a catalog object
/// identifies `/Root`, [`RepairError::SerializationFailed`] for a table that
/// cannot be written, and [`RepairError::VerificationFailed`] when the rebuilt
/// bytes do not parse. No repair is reported as a success without verification.
pub fn repair_document(input: &[u8]) -> std::result::Result<RepairedDocument, RepairError> {
    let compatibility = compatibility::analyze_compatibility(input);
    let version = compatibility.effective_version();
    let mut warnings = Vec::new();

    for warning in compatibility.warnings() {
        match warning {
            CompatibilityWarning::HeaderMissing { assumed } => {
                warnings.push(RepairWarning::HeaderNormalized {
                    version: *assumed,
                    reason: "header missing",
                })
            }
            CompatibilityWarning::HeaderVersionUnreadable { .. } => {
                warnings.push(RepairWarning::HeaderNormalized {
                    version,
                    reason: "version unreadable",
                })
            }
            CompatibilityWarning::HeaderNotAtStart { offset } => {
                warnings.push(RepairWarning::HeaderNotAtStart { offset: *offset });
            }
            _ => {}
        }
    }

    let (objects, object_warnings) = scan_objects(input);
    if objects.is_empty() {
        return Err(RepairError::NoObjects);
    }

    match parser::xref_offsets(input) {
        Some(entries) => {
            let mut wrong = 0usize;
            for (number, offset) in entries {
                if objects
                    .get(&number)
                    .is_some_and(|object| object.start != offset)
                {
                    wrong += 1;
                }
            }
            if wrong > 0 {
                warnings.push(RepairWarning::WrongXrefOffsets { count: wrong });
            }
        }
        None => {
            match raw_startxref(input) {
                None => warnings.push(RepairWarning::StartXrefMissing),
                Some(offset) => warnings.push(RepairWarning::StartXrefMisplaced { offset }),
            }
            warnings.push(RepairWarning::XrefRebuiltByScan);
        }
    }

    if !input.windows(5).any(|window| window == b"%%EOF") {
        warnings.push(RepairWarning::TruncatedEof);
    }
    warnings.extend(object_warnings);

    let mut trailer = trailer_dictionary(input).unwrap_or_default();
    let root = match trailer.get("Root").and_then(Object::as_reference) {
        Some(id) => id,
        None => objects
            .values()
            .find(|object| is_catalog(&object.body))
            .map(|object| ObjectId::new(object.number, object.generation))
            .ok_or(RepairError::NoCatalog)?,
    };
    for key in ["Prev", "XRefStm", "Size", "Root"] {
        trailer.remove(key);
    }
    let size = objects
        .keys()
        .next_back()
        .map_or(1, |number| number.saturating_add(1));
    trailer.insert("Size".to_owned(), Object::Integer(i64::from(size)));
    trailer.insert("Root".to_owned(), Object::Reference(root));

    let mut out = Vec::with_capacity(input.len().saturating_add(128));
    out.extend_from_slice(format!("%PDF-{version}\n%PDFXI repaired\n").as_bytes());
    let mut offsets = BTreeMap::new();
    for (number, object) in &objects {
        offsets.insert(*number, out.len());
        out.extend_from_slice(&object.body);
        out.push(b'\n');
    }
    let xref_offset = out.len();
    writer::write_xref_section(&mut out, &offsets, true);
    writer::write_trailer(&mut out, &trailer, xref_offset)
        .map_err(|error| RepairError::SerializationFailed(error.to_string()))?;

    parser::parse_document(&out)
        .map_err(|error| RepairError::VerificationFailed(error.to_string()))?;

    Ok(RepairedDocument {
        bytes: out,
        version,
        report: RepairReport { warnings },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Document;

    /// Build a deterministic one-page PDF with a stream object whose declared
    /// `/Length` can be overridden by `declared`.
    fn one_page_pdf(declared: Option<usize>) -> Vec<u8> {
        let content = b"BT (Hello, repair!) Tj ET";
        let stated = declared.unwrap_or(content.len());
        let mut objects: Vec<Vec<u8>> = vec![
            b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
            b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R >>".to_vec(),
        ];
        let mut fourth = format!("<< /Length {stated} >>\nstream\n").into_bytes();
        fourth.extend_from_slice(content);
        fourth.extend_from_slice(b"\nendstream");
        objects.push(fourth);

        let mut out = b"%PDF-1.7\n".to_vec();
        let mut offsets = Vec::new();
        for (index, object) in objects.iter().enumerate() {
            offsets.push(out.len());
            out.extend_from_slice(format!("{} 0 obj\n", index + 1).as_bytes());
            out.extend_from_slice(object);
            out.extend_from_slice(b"\nendobj\n");
        }
        let xref = out.len();
        out.extend_from_slice(format!("xref\n0 {}\n", objects.len() + 1).as_bytes());
        out.extend_from_slice(b"0000000000 65535 f \n");
        for offset in &offsets {
            out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        out.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
                objects.len() + 1
            )
            .as_bytes(),
        );
        out
    }

    fn set_startxref(pdf: &[u8], value: &str) -> Vec<u8> {
        let text = String::from_utf8(pdf.to_vec()).expect("ASCII fixture");
        let marker = "startxref\n";
        let start = text.find(marker).expect("marker") + marker.len();
        let end = start + text[start..].find('\n').expect("newline");
        let mut out = String::with_capacity(text.len());
        out.push_str(&text[..start]);
        out.push_str(value);
        out.push_str(&text[end..]);
        out.into_bytes()
    }

    fn remove_startxref(pdf: &[u8]) -> Vec<u8> {
        let text = String::from_utf8(pdf.to_vec()).expect("ASCII fixture");
        let marker = "startxref\n";
        let start = text.find(marker).expect("marker");
        let end = text[start..].find("%%EOF").expect("eof") + start;
        let mut out = String::with_capacity(text.len());
        out.push_str(&text[..start]);
        out.push_str(&text[end..]);
        out.into_bytes()
    }

    #[test]
    fn missing_startxref_is_rescanned() {
        let damaged = remove_startxref(&one_page_pdf(None));
        let repaired = repair_document(&damaged).expect("repair succeeds");
        let report = repaired.report();
        assert!(
            report
                .warnings()
                .iter()
                .any(|w| matches!(w, RepairWarning::StartXrefMissing))
        );
        assert!(
            report
                .warnings()
                .iter()
                .any(|w| matches!(w, RepairWarning::XrefRebuiltByScan))
        );
        let doc = Document::from_bytes(repaired.bytes().to_vec()).expect("reparses");
        assert_eq!(doc.page_count().expect("pages"), 1);
    }

    #[test]
    fn misplaced_startxref_is_reported() {
        let damaged = set_startxref(&one_page_pdf(None), "2");
        let repaired = repair_document(&damaged).expect("repair succeeds");
        assert!(
            repaired
                .report()
                .warnings()
                .iter()
                .any(|w| matches!(w, RepairWarning::StartXrefMisplaced { .. }))
        );
    }

    #[test]
    fn wrong_xref_offset_is_corrected() {
        let source = one_page_pdf(None);
        let text = String::from_utf8(source.clone()).expect("ASCII");
        // Corrupt the xref offset of object 1 (the first 10-digit in-use entry)
        // while keeping the field width so the table still parses.
        let needle = "0000000009 00000 n";
        let position = text.find(needle).expect("first object offset");
        let mut damaged = text.clone();
        damaged.replace_range(position..position + needle.len(), "0000000099 00000 n");
        let repaired = repair_document(damaged.as_bytes()).expect("repair succeeds");
        assert!(
            repaired
                .report()
                .warnings()
                .iter()
                .any(|w| matches!(w, RepairWarning::WrongXrefOffsets { count } if *count >= 1))
        );
        let doc = Document::from_bytes(repaired.bytes().to_vec()).expect("reparses");
        assert_eq!(doc.page_count().expect("pages"), 1);
    }

    #[test]
    fn broken_stream_length_is_corrected() {
        // Declared 4096 while the payload is 25 bytes; the field widths match.
        let damaged = one_page_pdf(Some(4096));
        let repaired = repair_document(&damaged).expect("repair succeeds");
        assert!(repaired.report().warnings().iter().any(|w| matches!(
            w,
            RepairWarning::StreamLengthCorrected {
                object: 4,
                declared: 4096,
                ..
            }
        )));
        let doc = Document::from_bytes(repaired.bytes().to_vec()).expect("reparses");
        assert_eq!(doc.page_text(0).expect("text"), "Hello, repair!");
    }

    #[test]
    fn truncated_eof_is_reported_and_repaired() {
        let source = one_page_pdf(None);
        let marker = b"startxref\n";
        let cut = source
            .windows(marker.len())
            .position(|window| window == marker)
            .expect("startxref");
        let damaged = &source[..cut];
        let repaired = repair_document(damaged).expect("repair succeeds");
        assert!(
            repaired
                .report()
                .warnings()
                .iter()
                .any(|w| matches!(w, RepairWarning::TruncatedEof))
        );
        let doc = Document::from_bytes(repaired.bytes().to_vec()).expect("reparses");
        assert_eq!(doc.page_count().expect("pages"), 1);
    }

    #[test]
    fn unrecoverable_input_is_a_typed_error() {
        assert_eq!(
            repair_document(b"not a pdf at all").expect_err("must fail"),
            RepairError::NoObjects
        );
        assert_eq!(
            repair_document(b"1 0 obj << /Foo /Bar >> endobj").expect_err("must fail"),
            RepairError::NoCatalog
        );
    }

    #[test]
    fn repair_report_warnings_are_ordered_and_deterministic() {
        let mut damaged = remove_startxref(&one_page_pdf(Some(4096)));
        let cut = damaged
            .windows(5)
            .position(|window| window == b"%%EOF")
            .expect("eof");
        damaged.truncate(cut);
        let first = repair_document(&damaged).expect("repair succeeds");
        let second = repair_document(&damaged).expect("repair succeeds");
        assert_eq!(first.report(), second.report());
        let warnings = first.report().warnings();
        // Header, then startxref/xref, then EOF, then per-object warnings.
        let kinds: Vec<&str> = warnings
            .iter()
            .map(|w| match w {
                RepairWarning::StartXrefMissing => "startxref",
                RepairWarning::XrefRebuiltByScan => "xref",
                RepairWarning::TruncatedEof => "eof",
                RepairWarning::StreamLengthCorrected { .. } => "length",
                _ => "other",
            })
            .collect();
        assert_eq!(kinds, vec!["startxref", "xref", "eof", "length"]);
        assert!(!first.report().is_empty());
        assert_eq!(first.report().len(), warnings.len());
    }

    #[test]
    fn repaired_document_round_trips_with_original_content() {
        let source = one_page_pdf(None);
        let damaged = remove_startxref(&source);
        let original = Document::from_bytes(source).expect("source parses");
        let repaired = repair_document(&damaged).expect("repair succeeds");
        assert_eq!(repaired.version(), PdfVersion::V1_7);
        let after = Document::from_bytes(repaired.bytes().to_vec()).expect("reparses");
        assert_eq!(after.version(), "1.7");
        assert_eq!(
            after.page_count().expect("pages"),
            original.page_count().expect("pages")
        );
        assert_eq!(
            after.page_text(0).expect("text"),
            original.page_text(0).expect("text")
        );
        assert_eq!(after.root().expect("root"), original.root().expect("root"));
    }

    #[test]
    fn duplicate_objects_keep_the_latest_revision() {
        let mut source = one_page_pdf(None);
        // Append a newer incremental revision of object 1 (the catalog).
        source.extend_from_slice(
            b"\n1 0 obj\n<< /Type /Catalog /Pages 2 0 R /Version /1.7 >>\nendobj\n",
        );
        let repaired = repair_document(&source).expect("repair succeeds");
        assert!(
            repaired
                .report()
                .warnings()
                .iter()
                .any(|w| matches!(w, RepairWarning::DuplicateObject { number: 1 }))
        );
        let doc = Document::from_bytes(repaired.bytes().to_vec()).expect("reparses");
        assert_eq!(doc.page_count().expect("pages"), 1);
        // The newest revision carried the catalog /Version override.
        let report = crate::compatibility::analyze_compatibility(repaired.bytes());
        assert_eq!(report.effective_version(), PdfVersion::V1_7);
        assert_eq!(
            report.source(),
            crate::compatibility::VersionSource::Catalog
        );
    }
}
