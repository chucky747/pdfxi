//! Minimal, deterministic stored-ZIP container used by the Office adapters.
//!
//! OOXML files (`.docx`, `.xlsx`, `.pptx`) are ZIP archives of XML parts. This
//! module implements just enough ZIP to write and read those archives without
//! an external crate: uncompressed ("stored") entries, a central directory,
//! deterministic DOS timestamps, and CRC-32 integrity checks. Reading is used
//! both by the adapters (to validate their own output before returning it) and
//! by the tests (to assert part sets and round-trip text).

use std::collections::HashSet;
use thiserror::Error;

const LOCAL_HEADER_SIG: u32 = 0x0403_4b50;
const CENTRAL_HEADER_SIG: u32 = 0x0201_4b50;
const EOCD_SIG: u32 = 0x0605_4b50;
const VERSION_NEEDED: u16 = 20;
const FLAGS: u16 = 0;
const METHOD_STORED: u16 = 0;
/// Fixed DOS time (00:00:00) so archives are byte-for-byte reproducible.
const DOS_TIME: u16 = 0;
/// Fixed DOS date: 1980-01-01, the ZIP epoch.
const DOS_DATE: u16 = 0x0021;
const CRC_INIT: u32 = 0xFFFF_FFFF;
const CRC_POLY: u32 = 0xEDB8_8320;
const EOCD_LEN: usize = 22;

/// A typed failure while writing, reading, or validating a ZIP archive.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ZipError {
    /// No entries were supplied to the writer.
    #[error("ZIP archive is empty")]
    EmptyArchive,
    /// The byte stream is truncated or a header field is inconsistent.
    #[error("invalid ZIP structure: {0}")]
    InvalidStructure(&'static str),
    /// The same entry name appears more than once.
    #[error("duplicate ZIP entry '{0}'")]
    DuplicateEntry(String),
    /// A stored entry's payload does not match its recorded CRC-32.
    #[error("ZIP entry '{name}' failed its CRC-32 check")]
    CrcMismatch {
        /// Name of the corrupted entry.
        name: String,
    },
    /// An entry name is not valid UTF-8.
    #[error("ZIP entry name is not valid UTF-8")]
    InvalidName,
    /// A required OOXML part is absent from the archive.
    #[error("ZIP archive is missing required part '{0}'")]
    MissingPart(String),
    /// An entry name exceeds the 16-bit ZIP length field.
    #[error("ZIP entry name exceeds 65535 bytes")]
    NameTooLong,
    /// The archive exceeds a 32-bit size or 16-bit entry count.
    #[error("ZIP archive exceeds 2^32 bytes or 65535 entries")]
    TooLarge,
}

/// One uncompressed entry to place in a ZIP archive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ZipEntry<'a> {
    /// Archive-relative name, for example `word/document.xml`.
    pub name: &'a str,
    /// Raw bytes of the part.
    pub data: &'a [u8],
}

impl<'a> ZipEntry<'a> {
    /// Creates an entry from a name and its bytes.
    #[must_use]
    pub const fn new(name: &'a str, data: &'a [u8]) -> Self {
        Self { name, data }
    }
}

/// A validated entry read back from a ZIP archive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ZipEntryView<'a> {
    /// Archive-relative name.
    pub name: &'a str,
    /// Raw bytes of the part.
    pub data: &'a [u8],
}

/// Continues a CRC-32 (IEEE 802.3) register over `data`.
fn crc32_continue(mut crc: u32, data: &[u8]) -> u32 {
    for &byte in data {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            let mask = 0u32.wrapping_sub(crc & 1);
            crc = (crc >> 1) ^ (CRC_POLY & mask);
        }
    }
    crc
}

/// Returns the standard CRC-32 checksum of `data`.
fn crc32(data: &[u8]) -> u32 {
    crc32_continue(CRC_INIT, data) ^ CRC_INIT
}

fn push_u16(out: &mut Vec<u8>, value: u16) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn push_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_le_bytes());
}

/// Writes a deterministic, stored-only ZIP archive from `entries`.
///
/// Entries appear in the local-header and central-directory sections in the
/// exact order supplied, so two equal inputs produce byte-identical archives.
///
/// # Errors
///
/// Returns [`ZipError`] for an empty archive, an oversized name/archive, or a
/// duplicate entry name.
pub fn write_zip(entries: &[ZipEntry<'_>]) -> Result<Vec<u8>, ZipError> {
    if entries.is_empty() {
        return Err(ZipError::EmptyArchive);
    }
    let count = u16::try_from(entries.len()).map_err(|_| ZipError::TooLarge)?;

    let mut out = Vec::new();
    let mut central = Vec::new();
    let mut seen: HashSet<&str> = HashSet::with_capacity(entries.len());

    for entry in entries {
        let name = entry.name.as_bytes();
        let name_len = u16::try_from(name.len()).map_err(|_| ZipError::NameTooLong)?;
        let data_len = u32::try_from(entry.data.len()).map_err(|_| ZipError::TooLarge)?;
        let crc = crc32(entry.data);
        if !seen.insert(entry.name) {
            return Err(ZipError::DuplicateEntry(entry.name.to_owned()));
        }
        let local_offset = u32::try_from(out.len()).map_err(|_| ZipError::TooLarge)?;

        push_u32(&mut out, LOCAL_HEADER_SIG);
        push_u16(&mut out, VERSION_NEEDED);
        push_u16(&mut out, FLAGS);
        push_u16(&mut out, METHOD_STORED);
        push_u16(&mut out, DOS_TIME);
        push_u16(&mut out, DOS_DATE);
        push_u32(&mut out, crc);
        push_u32(&mut out, data_len);
        push_u32(&mut out, data_len);
        push_u16(&mut out, name_len);
        push_u16(&mut out, 0);
        out.extend_from_slice(name);
        out.extend_from_slice(entry.data);

        push_u32(&mut central, CENTRAL_HEADER_SIG);
        push_u16(&mut central, VERSION_NEEDED);
        push_u16(&mut central, VERSION_NEEDED);
        push_u16(&mut central, FLAGS);
        push_u16(&mut central, METHOD_STORED);
        push_u16(&mut central, DOS_TIME);
        push_u16(&mut central, DOS_DATE);
        push_u32(&mut central, crc);
        push_u32(&mut central, data_len);
        push_u32(&mut central, data_len);
        push_u16(&mut central, name_len);
        push_u16(&mut central, 0);
        push_u16(&mut central, 0);
        push_u16(&mut central, 0);
        push_u16(&mut central, 0);
        push_u32(&mut central, 0);
        push_u32(&mut central, local_offset);
        central.extend_from_slice(name);
    }

    let central_offset = u32::try_from(out.len()).map_err(|_| ZipError::TooLarge)?;
    let central_size = u32::try_from(central.len()).map_err(|_| ZipError::TooLarge)?;
    out.extend_from_slice(&central);

    push_u32(&mut out, EOCD_SIG);
    push_u16(&mut out, 0);
    push_u16(&mut out, 0);
    push_u16(&mut out, count);
    push_u16(&mut out, count);
    push_u32(&mut out, central_size);
    push_u32(&mut out, central_offset);
    push_u16(&mut out, 0);
    Ok(out)
}

/// A little-endian byte cursor used by the ZIP reader.
struct Cursor<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    fn take(&mut self, len: usize) -> Result<&'a [u8], ZipError> {
        let end = self
            .pos
            .checked_add(len)
            .ok_or(ZipError::InvalidStructure("offset overflow"))?;
        let slice = self
            .data
            .get(self.pos..end)
            .ok_or(ZipError::InvalidStructure("truncated header"))?;
        self.pos = end;
        Ok(slice)
    }

    fn u16(&mut self) -> Result<u16, ZipError> {
        let bytes = self.take(2)?;
        Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
    }

    fn u32(&mut self) -> Result<u32, ZipError> {
        let bytes = self.take(4)?;
        Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }
}

fn find_eocd(data: &[u8]) -> Result<usize, ZipError> {
    if data.len() < EOCD_LEN {
        return Err(ZipError::InvalidStructure("too short for end record"));
    }
    let mut index = data.len() - EOCD_LEN;
    loop {
        if data[index..index + 4] == EOCD_SIG.to_le_bytes()[..] {
            let mut cursor = Cursor::new(&data[index..]);
            let _ = cursor.u32()?;
            let _ = cursor.u16()?;
            let _ = cursor.u16()?;
            let _ = cursor.u16()?;
            let _ = cursor.u16()?;
            let _ = cursor.u32()?;
            let _ = cursor.u32()?;
            let comment_len = usize::from(cursor.u16()?);
            if index + EOCD_LEN + comment_len == data.len() {
                return Ok(index);
            }
        }
        if index == 0 {
            break;
        }
        index -= 1;
    }
    Err(ZipError::InvalidStructure(
        "end of central directory not found",
    ))
}

/// Validates `data` as a stored-only ZIP archive and returns its entries.
///
/// Every central-directory record is cross-checked against its local header,
/// and every payload's CRC-32 is recomputed. This makes the function a real
/// structural validator rather than a superficial signature scan.
///
/// # Errors
///
/// Returns [`ZipError`] on a malformed archive, a CRC mismatch, a duplicate
/// name, or a non-stored entry.
pub fn read_zip(data: &[u8]) -> Result<Vec<ZipEntryView<'_>>, ZipError> {
    let eocd = find_eocd(data)?;
    let mut end = Cursor::new(&data[eocd..]);
    if end.u32()? != EOCD_SIG {
        return Err(ZipError::InvalidStructure("bad end record signature"));
    }
    let _disk = end.u16()?;
    let _cd_disk = end.u16()?;
    let entries_disk = usize::from(end.u16()?);
    let entries_total = usize::from(end.u16()?);
    let cd_size = end.u32()? as usize;
    let cd_offset = end.u32()? as usize;
    let comment_len = usize::from(end.u16()?);
    if eocd + EOCD_LEN + comment_len != data.len() {
        return Err(ZipError::InvalidStructure(
            "trailing bytes after end record",
        ));
    }
    if entries_disk != entries_total {
        return Err(ZipError::InvalidStructure(
            "multi-disk archives unsupported",
        ));
    }
    let cd_end = cd_offset
        .checked_add(cd_size)
        .ok_or(ZipError::InvalidStructure("central directory overflow"))?;
    if cd_end > eocd {
        return Err(ZipError::InvalidStructure(
            "central directory overlaps end record",
        ));
    }
    let central = data
        .get(cd_offset..cd_end)
        .ok_or(ZipError::InvalidStructure(
            "central directory out of bounds",
        ))?;

    let mut reader = Cursor::new(central);
    let mut views = Vec::with_capacity(entries_total);
    let mut seen: HashSet<&str> = HashSet::with_capacity(entries_total);

    for _ in 0..entries_total {
        if reader.u32()? != CENTRAL_HEADER_SIG {
            return Err(ZipError::InvalidStructure("bad central header signature"));
        }
        let _made_by = reader.u16()?;
        let _needed = reader.u16()?;
        let _flags = reader.u16()?;
        let method = reader.u16()?;
        let _time = reader.u16()?;
        let _date = reader.u16()?;
        let crc = reader.u32()?;
        let compressed = reader.u32()? as usize;
        let uncompressed = reader.u32()? as usize;
        let name_len = usize::from(reader.u16()?);
        let extra_len = usize::from(reader.u16()?);
        let comment_len = usize::from(reader.u16()?);
        let _disk_start = reader.u16()?;
        let _internal = reader.u16()?;
        let _external = reader.u32()?;
        let local_offset = reader.u32()? as usize;
        let name_bytes = reader.take(name_len)?;
        let _ = reader.take(extra_len)?;
        let _ = reader.take(comment_len)?;

        if method != METHOD_STORED || compressed != uncompressed {
            return Err(ZipError::InvalidStructure(
                "only stored entries are supported",
            ));
        }
        let name = std::str::from_utf8(name_bytes).map_err(|_| ZipError::InvalidName)?;
        if !seen.insert(name) {
            return Err(ZipError::DuplicateEntry(name.to_owned()));
        }

        let mut local = Cursor::new(data);
        local.pos = local_offset;
        if local.u32()? != LOCAL_HEADER_SIG {
            return Err(ZipError::InvalidStructure("bad local header signature"));
        }
        let _needed = local.u16()?;
        let _flags = local.u16()?;
        let local_method = local.u16()?;
        let _time = local.u16()?;
        let _date = local.u16()?;
        let local_crc = local.u32()?;
        let local_compressed = local.u32()? as usize;
        let local_uncompressed = local.u32()? as usize;
        let local_name_len = usize::from(local.u16()?);
        let local_extra_len = usize::from(local.u16()?);
        let local_name = local.take(local_name_len)?;
        let _ = local.take(local_extra_len)?;
        if local_name != name_bytes
            || local_method != METHOD_STORED
            || local_crc != crc
            || local_compressed != compressed
            || local_uncompressed != uncompressed
        {
            return Err(ZipError::InvalidStructure(
                "local header disagrees with central directory",
            ));
        }
        let start = local.pos;
        let end = start
            .checked_add(compressed)
            .ok_or(ZipError::InvalidStructure("payload offset overflow"))?;
        let payload = data
            .get(start..end)
            .ok_or(ZipError::InvalidStructure("payload out of bounds"))?;
        if crc32(payload) != crc {
            return Err(ZipError::CrcMismatch {
                name: name.to_owned(),
            });
        }
        views.push(ZipEntryView {
            name,
            data: payload,
        });
    }

    Ok(views)
}

/// Returns `Ok(())` when every named part is present in `entries`.
///
/// # Errors
///
/// Returns [`ZipError::MissingPart`] naming the first absent required part.
pub fn require_parts(entries: &[ZipEntryView<'_>], names: &[&str]) -> Result<(), ZipError> {
    for name in names {
        if !entries.iter().any(|entry| entry.name == *name) {
            return Err(ZipError::MissingPart((*name).to_owned()));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const PART: &[u8] = b"<root>hello</root>";

    #[test]
    fn crc32_matches_standard_vector() {
        assert_eq!(crc32(b""), 0);
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        let intermediate = crc32_continue(CRC_INIT, b"1234");
        assert_eq!(
            crc32_continue(intermediate, b"56789") ^ CRC_INIT,
            0xCBF4_3926
        );
    }

    #[test]
    fn write_read_round_trip_preserves_names_and_payloads() {
        let entries = [
            ZipEntry::new("[Content_Types].xml", b"<Types/>"),
            ZipEntry::new("word/document.xml", PART),
        ];
        let bytes = write_zip(&entries).expect("write");
        let views = read_zip(&bytes).expect("read");
        assert_eq!(views.len(), 2);
        assert_eq!(views[0].name, "[Content_Types].xml");
        assert_eq!(views[0].data, b"<Types/>");
        assert_eq!(views[1].name, "word/document.xml");
        assert_eq!(views[1].data, PART);
        require_parts(&views, &["word/document.xml"]).expect("part present");
    }

    #[test]
    fn output_is_deterministic() {
        let entries = [ZipEntry::new("a.xml", PART), ZipEntry::new("b.xml", b"b")];
        let a = write_zip(&entries).expect("write");
        let b = write_zip(&entries).expect("write");
        assert_eq!(a, b);
    }

    #[test]
    fn empty_archive_is_rejected() {
        assert_eq!(write_zip(&[]), Err(ZipError::EmptyArchive));
    }

    #[test]
    fn duplicate_names_are_rejected() {
        let entries = [ZipEntry::new("a", b"1"), ZipEntry::new("a", b"2")];
        assert_eq!(
            write_zip(&entries),
            Err(ZipError::DuplicateEntry("a".to_owned()))
        );
    }

    #[test]
    fn corrupt_payload_is_detected_by_crc() {
        let entries = [ZipEntry::new("doc.xml", PART)];
        let mut bytes = write_zip(&entries).expect("write");
        let payload_start = bytes
            .windows(PART.len())
            .position(|window| window == PART)
            .expect("payload present");
        bytes[payload_start] ^= 0xFF;
        assert_eq!(
            read_zip(&bytes),
            Err(ZipError::CrcMismatch {
                name: "doc.xml".to_owned()
            })
        );
    }

    #[test]
    fn truncated_or_garbage_bytes_are_rejected() {
        assert!(matches!(read_zip(b""), Err(ZipError::InvalidStructure(_))));
        assert!(matches!(
            read_zip(b"not a zip file at all"),
            Err(ZipError::InvalidStructure(_))
        ));
    }

    #[test]
    fn missing_required_part_is_reported() {
        let bytes = write_zip(&[ZipEntry::new("only.xml", PART)]).expect("write");
        let views = read_zip(&bytes).expect("read");
        assert_eq!(
            require_parts(&views, &["word/document.xml"]),
            Err(ZipError::MissingPart("word/document.xml".to_owned()))
        );
    }
}
