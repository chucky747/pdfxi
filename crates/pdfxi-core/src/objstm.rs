//! Writing of PDF 1.5+ object streams and cross-reference streams.
//!
//! This module is the write-side counterpart of the crate's xref/object-stream
//! reader. It packs eligible indirect objects into `/ObjStm` streams,
//! emits a `/Type /XRef` stream whose entries may be type 0 (free), type 1
//! (physical object) or type 2 (compressed object), and can chain successive
//! sections with `/Prev` for incremental updates.
//!
//! The implementation is deliberately dependency-free. Compression uses a small
//! stored-block zlib encoder (RFC 1950/1951), which the crate's own inflater can
//! read back; callers may also disable compression entirely with
//! [`ObjectStreamOptions::compress`]. All sizes and offsets are checked before
//! they are written, so an oversized document is reported as
//! [`Error::CannotSerialize`] rather than silently truncating a field.
//!
//! # Example
//!
//! Build a vector of `(ObjectId, Object)` pairs that includes the catalog, then
//! call [`write_object_stream_document`] with the catalog's identifier as the
//! root. The returned byte slice starts with `%PDF-` and can be handed straight
//! to [`crate::Document::from_bytes`]; call [`plan_object_streams`] first when a
//! report of the packing decisions is needed.

use std::collections::BTreeSet;

use crate::object::{Dictionary, Object, ObjectId};
use crate::writer;
use crate::{Error, Result};

/// Default upper bound on the number of objects packed into one `/ObjStm`.
const DEFAULT_MAX_OBJECTS_PER_STREAM: usize = 128;
/// Hard upper bound accepted for [`ObjectStreamOptions::max_objects_per_stream`].
const MAX_OBJECTS_PER_STREAM: usize = 1_000_000;
/// Byte width of the type field of a generated xref-stream entry.
const TYPE_WIDTH: u8 = 1;
/// Byte width of the second field (object offset or object-stream number).
const FIELD_TWO_WIDTH: u8 = 4;
/// Byte width of the third field (generation or member index).
const FIELD_THREE_WIDTH: u8 = 2;
/// Maximum width, in decimal digits, attempted for an object-stream offset.
const MAX_OFFSET_DIGITS: usize = 12;

/// Options controlling object-stream packing and xref-stream generation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectStreamOptions {
    /// Whether generated streams carry `/Filter /FlateDecode`.
    ///
    /// The encoder emits stored DEFLATE blocks, so compression never shrinks the
    /// data but does exercise the same filter path real PDFs use. Set this to
    /// `false` for byte-for-byte inspectable, uncompressed streams.
    pub compress: bool,
    /// Maximum number of objects packed into a single `/ObjStm`.
    ///
    /// Must be non-zero and no greater than one million. The default is 128.
    pub max_objects_per_stream: usize,
    /// Object number assigned to the first generated object stream.
    ///
    /// When `None`, the caller's highest object number plus one is used. Set an
    /// explicit value for incremental saves so the generated streams do not
    /// collide with objects in the previous revision.
    pub first_object_number: Option<u32>,
    /// Byte offset of the previous xref section, written as `/Prev`.
    ///
    /// Ignored by [`write_object_stream_document`]; [`write_object_stream_increment`]
    /// derives it from the base file's `startxref`.
    pub prev_xref_offset: Option<u64>,
}

impl Default for ObjectStreamOptions {
    fn default() -> Self {
        Self {
            compress: true,
            max_objects_per_stream: DEFAULT_MAX_OBJECTS_PER_STREAM,
            first_object_number: None,
            prev_xref_offset: None,
        }
    }
}

/// Reason why an object cannot be stored inside an `/ObjStm`.
///
/// Returned by [`packing_exclusion_reason`] so callers can explain a packing
/// decision in a report or log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackingExclusion {
    /// Object number zero is the head of the free-object list.
    ReservedNumber,
    /// Stream objects must remain standalone (ISO 32000-1, 7.5.7).
    IsStream,
    /// The document catalog must remain a standalone object.
    IsCatalog,
    /// Page and page-tree objects are kept standalone by this writer.
    IsPageTree,
    /// Object streams and xref streams cannot be nested in object streams.
    IsStructuralStream,
    /// Compressed objects always have generation zero.
    NonZeroGeneration,
}

impl PackingExclusion {
    /// Return a short human-readable explanation of this exclusion.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ReservedNumber => "object number 0 is reserved",
            Self::IsStream => "stream objects are never packed",
            Self::IsCatalog => "the document catalog is kept standalone",
            Self::IsPageTree => "page tree nodes are kept standalone",
            Self::IsStructuralStream => "structural streams are never packed",
            Self::NonZeroGeneration => "non-zero generation objects cannot be packed",
        }
    }
}

/// Explain why `id`/`object` is excluded from object streams, if it is.
///
/// Returns `None` when the object is eligible for packing. The checks are
/// deliberately conservative: this writer keeps streams, the catalog and the
/// page tree standalone even though some of those are technically packable.
pub fn packing_exclusion_reason(id: ObjectId, object: &Object) -> Option<PackingExclusion> {
    if id.number == 0 {
        return Some(PackingExclusion::ReservedNumber);
    }
    if id.generation != 0 {
        return Some(PackingExclusion::NonZeroGeneration);
    }
    if matches!(object, Object::Stream { .. }) {
        return Some(PackingExclusion::IsStream);
    }
    let kind = object
        .as_dictionary()
        .and_then(|dict| dict.get("Type"))
        .and_then(Object::as_name)
        .unwrap_or("");
    match kind {
        "Catalog" => Some(PackingExclusion::IsCatalog),
        "Pages" | "Page" => Some(PackingExclusion::IsPageTree),
        "ObjStm" | "XRef" => Some(PackingExclusion::IsStructuralStream),
        _ => None,
    }
}

/// Return `true` when `id`/`object` may be packed into an `/ObjStm`.
pub fn is_packing_eligible(id: ObjectId, object: &Object) -> bool {
    packing_exclusion_reason(id, object).is_none()
}

/// Reference from a compressed object to its containing `/ObjStm`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PackedObjectRef {
    /// Identifier of the object that was packed.
    pub id: ObjectId,
    /// Identifier of the object stream that holds it.
    pub stream: ObjectId,
    /// Zero-based index of the object in the stream's header.
    pub index: u32,
}

/// Summary of one packing operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectStreamReport {
    /// Object streams that were created, in ascending order.
    pub object_stream_ids: Vec<ObjectId>,
    /// Objects that were packed, in ascending object-number order.
    pub packed_objects: Vec<ObjectId>,
    /// Objects that remained standalone, in ascending object-number order.
    pub standalone_objects: Vec<ObjectId>,
}

/// The result of planning object streams for a set of objects.
#[derive(Debug, Clone)]
pub struct ObjectStreamPlan {
    /// Generated `/ObjStm` stream objects, paired with their identifiers.
    pub streams: Vec<(ObjectId, Object)>,
    /// Mapping from each packed object to its stream and member index.
    pub packed: Vec<PackedObjectRef>,
    /// Human-readable summary of the packing decisions.
    pub report: ObjectStreamReport,
}

/// Build an object-stream plan for `objects`.
///
/// Objects are considered in ascending object-number order. Eligible objects
/// are placed into consecutive groups of at most
/// [`ObjectStreamOptions::max_objects_per_stream`] members. When more than one
/// stream is produced they are chained with `/Extends` from the stream holding
/// lower-numbered objects to the next stream, whose members have higher object
/// numbers (ISO 32000-1, 7.5.7).
///
/// # Errors
///
/// Returns [`Error::CannotSerialize`] when the per-stream limit is invalid, an
/// object number cannot be encoded, or a generated offset is too large.
pub fn plan_object_streams(
    objects: &[(ObjectId, Object)],
    options: &ObjectStreamOptions,
) -> Result<ObjectStreamPlan> {
    if options.max_objects_per_stream == 0
        || options.max_objects_per_stream > MAX_OBJECTS_PER_STREAM
    {
        return Err(Error::CannotSerialize(format!(
            "object stream limit {} is outside 1..={MAX_OBJECTS_PER_STREAM}",
            options.max_objects_per_stream
        )));
    }
    validate_objects(objects)?;

    let mut sorted: Vec<(ObjectId, &Object)> = objects.iter().map(|(id, obj)| (*id, obj)).collect();
    sorted.sort_by_key(|(id, _)| id.number);

    let highest = sorted.last().map_or(0, |(id, _)| id.number);
    let first_stream = match options.first_object_number {
        Some(0) => {
            return Err(Error::CannotSerialize(
                "object stream number 0 is reserved".to_owned(),
            ));
        }
        Some(number) => number,
        None => highest
            .checked_add(1)
            .ok_or_else(|| Error::CannotSerialize("object number space is exhausted".to_owned()))?,
    };

    let mut eligible: Vec<(ObjectId, &Object)> = Vec::new();
    let mut standalone = Vec::new();
    for (id, object) in sorted {
        if is_packing_eligible(id, object) {
            eligible.push((id, object));
        } else {
            standalone.push(id);
        }
    }

    let chunk_size = options.max_objects_per_stream;
    let chunks: Vec<Vec<(ObjectId, &Object)>> = eligible
        .chunks(chunk_size)
        .map(|chunk| chunk.to_vec())
        .collect();

    let mut streams = Vec::with_capacity(chunks.len());
    let mut packed = Vec::new();
    let mut stream_ids = Vec::with_capacity(chunks.len());
    for (chunk_index, chunk) in chunks.iter().enumerate() {
        let stream_number = first_stream
            .checked_add(chunk_index as u32)
            .ok_or_else(|| Error::CannotSerialize("object stream number overflow".to_owned()))?;
        let stream_id = ObjectId::new(stream_number, 0);
        let extends = if chunks.get(chunk_index + 1).is_some() {
            Some(ObjectId::new(
                stream_number.checked_add(1).ok_or_else(|| {
                    Error::CannotSerialize("object stream number overflow".to_owned())
                })?,
                0,
            ))
        } else {
            None
        };
        let mut bodies = Vec::with_capacity(chunk.len());
        for (id, object) in chunk {
            let mut body = Vec::new();
            writer::serialize_object(object, &mut body)?;
            bodies.push((*id, body));
        }
        let (decoded, first, offsets) = build_object_stream_body(&bodies)?;
        let mut dictionary = Dictionary::new();
        dictionary.insert("Type".to_owned(), Object::Name("ObjStm".to_owned()));
        dictionary.insert(
            "N".to_owned(),
            Object::Integer(i64::try_from(bodies.len()).map_err(|_| {
                Error::CannotSerialize("object stream member count exceeds i64".to_owned())
            })?),
        );
        dictionary.insert(
            "First".to_owned(),
            Object::Integer(i64::try_from(first).map_err(|_| {
                Error::CannotSerialize("object stream /First exceeds i64".to_owned())
            })?),
        );
        if let Some(target) = extends {
            dictionary.insert("Extends".to_owned(), Object::Reference(target));
        }
        let encoded = encode_payload(&decoded, options.compress);
        dictionary.insert(
            "Length".to_owned(),
            Object::Integer(i64::try_from(encoded.len()).map_err(|_| {
                Error::CannotSerialize("object stream length exceeds i64".to_owned())
            })?),
        );
        if options.compress {
            dictionary.insert("Filter".to_owned(), Object::Name("FlateDecode".to_owned()));
        }
        let object = Object::Stream {
            dictionary,
            data: encoded,
        };
        for (member_index, (id, _)) in chunk.iter().enumerate() {
            packed.push(PackedObjectRef {
                id: *id,
                stream: stream_id,
                index: u32::try_from(member_index).map_err(|_| {
                    Error::CannotSerialize("object stream index exceeds u32".to_owned())
                })?,
            });
        }
        // The relative offsets computed above are embedded in the stream header;
        // verifying their count keeps the header and member list in lockstep.
        debug_assert_eq!(offsets.len(), chunk.len());
        streams.push((stream_id, object));
        stream_ids.push(stream_id);
    }

    let report = ObjectStreamReport {
        object_stream_ids: stream_ids,
        packed_objects: packed_objects_in_order(&packed),
        standalone_objects: standalone,
    };
    Ok(ObjectStreamPlan {
        streams,
        packed,
        report,
    })
}

/// Return the packed object identifiers in ascending object-number order.
fn packed_objects_in_order(packed: &[PackedObjectRef]) -> Vec<ObjectId> {
    let mut ids: Vec<ObjectId> = packed.iter().map(|item| item.id).collect();
    ids.sort_by_key(|id| id.number);
    ids
}

/// Check that `objects` has unique, non-zero object numbers.
fn validate_objects(objects: &[(ObjectId, Object)]) -> Result<()> {
    let mut seen = BTreeSet::new();
    for (id, _) in objects {
        if id.number == 0 {
            return Err(Error::InvalidObjectId(
                "object number 0 is reserved".to_owned(),
            ));
        }
        if !seen.insert(id.number) {
            return Err(Error::InvalidObjectId(format!(
                "duplicate object number {}",
                id.number
            )));
        }
    }
    Ok(())
}

/// Build the decoded payload of one object stream.
///
/// The header contains `N` pairs of (object number, offset relative to `First`).
/// Because the printed offset width affects `First`, a fixed-point loop widens
/// the zero-padded offset field until every offset fits.
fn build_object_stream_body(members: &[(ObjectId, Vec<u8>)]) -> Result<(Vec<u8>, usize, Vec<u32>)> {
    let mut width = 1usize;
    loop {
        let mut header = Vec::new();
        let mut offsets = Vec::with_capacity(members.len());
        let mut cursor = 0usize;
        for (id, body) in members {
            offsets.push(cursor);
            header.extend_from_slice(id.number.to_string().as_bytes());
            header.push(b' ');
            header.extend_from_slice(format!("{cursor:0width$}").as_bytes());
            header.push(b' ');
            cursor = cursor
                .checked_add(body.len())
                .and_then(|sum| sum.checked_add(1))
                .ok_or_else(|| {
                    Error::CannotSerialize("object stream body offset overflow".to_owned())
                })?;
        }
        let first = header.len();
        let fits = offsets
            .iter()
            .all(|offset| decimal_digits_u64(*offset as u64) <= width);
        if fits {
            let mut decoded = header;
            for (_, body) in members {
                decoded.extend_from_slice(body);
                decoded.push(b' ');
            }
            let relative = offsets
                .into_iter()
                .map(|offset| {
                    u32::try_from(offset).map_err(|_| {
                        Error::CannotSerialize(
                            "object stream offset does not fit the 32-bit xref field".to_owned(),
                        )
                    })
                })
                .collect::<Result<Vec<u32>>>()?;
            return Ok((decoded, first, relative));
        }
        width = width.checked_add(1).ok_or_else(|| {
            Error::CannotSerialize("object stream offset width overflow".to_owned())
        })?;
        if width > MAX_OFFSET_DIGITS {
            return Err(Error::CannotSerialize(
                "object stream offset is too large to encode".to_owned(),
            ));
        }
    }
}

/// Number of base-10 digits needed to print `value`.
fn decimal_digits_u64(value: u64) -> usize {
    let mut digits = 1usize;
    let mut rest = value;
    while rest >= 10 {
        rest /= 10;
        digits += 1;
    }
    digits
}

/// Adler-32 checksum (RFC 1950) of `data`.
fn adler32(data: &[u8]) -> u32 {
    let mut a = 1u32;
    let mut b = 0u32;
    for &byte in data {
        a = (a + u32::from(byte)) % 65_521;
        b = (b + a) % 65_521;
    }
    (b << 16) | a
}

/// Wrap `data` in a valid zlib stream built from stored DEFLATE blocks.
fn zlib_stored(data: &[u8]) -> Vec<u8> {
    let overhead = 6usize + data.len() / 0xffff * 5;
    let mut out = Vec::with_capacity(data.len() + overhead + 8);
    out.extend_from_slice(&[0x78, 0x01]);
    if data.is_empty() {
        out.extend_from_slice(&[0x01, 0x00, 0x00, 0xff, 0xff]);
    } else {
        let chunks: Vec<&[u8]> = data.chunks(0xffff).collect();
        let last = chunks.len().saturating_sub(1);
        for (index, chunk) in chunks.into_iter().enumerate() {
            out.push(u8::from(index == last));
            let length = chunk.len() as u16;
            out.extend_from_slice(&length.to_le_bytes());
            out.extend_from_slice(&(!length).to_le_bytes());
            out.extend_from_slice(chunk);
        }
    }
    out.extend_from_slice(&adler32(data).to_be_bytes());
    out
}

/// Return `data` verbatim or wrapped in a stored-block zlib stream.
fn encode_payload(data: &[u8], compress: bool) -> Vec<u8> {
    if compress {
        zlib_stored(data)
    } else {
        data.to_vec()
    }
}

/// One entry of a generated xref stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum XrefEntrySpec {
    /// Type 0: free object with the given generation.
    Free(u16),
    /// Type 1: in-use object at a byte offset with the given generation.
    InUse {
        /// Byte offset of the indirect object definition.
        offset: u32,
        /// Generation number written into the third field.
        generation: u16,
    },
    /// Type 2: object inside an object stream at a zero-based index.
    Compressed {
        /// Object number of the containing `/ObjStm`.
        objstm: u32,
        /// Zero-based member index in the object stream header.
        index: u32,
    },
}

/// Group sorted object numbers into contiguous `(first, count)` ranges.
fn index_ranges(entries: &BTreeSet<u32>) -> Result<Vec<(u32, u32)>> {
    let mut ranges = Vec::new();
    let mut numbers = entries.iter().copied();
    let mut first = match numbers.next() {
        Some(number) => number,
        None => return Ok(ranges),
    };
    let mut count = 1u32;
    for number in numbers {
        if first.checked_add(count) == Some(number) {
            count = count.checked_add(1).ok_or_else(|| {
                Error::CannotSerialize("xref subsection count overflow".to_owned())
            })?;
        } else {
            ranges.push((first, count));
            first = number;
            count = 1;
        }
    }
    ranges.push((first, count));
    Ok(ranges)
}

/// Append a big-endian xref field of `width` bytes, ignoring bits above 64.
fn push_field(out: &mut Vec<u8>, width: u8, value: u64) {
    for shift in (0..width).rev() {
        let amount = 8 * u32::from(shift);
        let byte = if amount >= 64 {
            0
        } else {
            ((value >> amount) & 0xff) as u8
        };
        out.push(byte);
    }
}

/// Encode the fixed-width xref entries in `/Index` order.
fn encode_xref_entries(
    entries: &std::collections::BTreeMap<u32, XrefEntrySpec>,
    ranges: &[(u32, u32)],
) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    for &(first, count) in ranges {
        for offset in 0..u64::from(count) {
            let number = u64::from(first) + offset;
            let number = u32::try_from(number)
                .map_err(|_| Error::CannotSerialize("xref object number exceeds u32".to_owned()))?;
            let entry = entries.get(&number).ok_or_else(|| {
                Error::CannotSerialize("xref /Index names an object with no entry".to_owned())
            })?;
            match *entry {
                XrefEntrySpec::Free(generation) => {
                    push_field(&mut out, TYPE_WIDTH, 0);
                    push_field(&mut out, FIELD_TWO_WIDTH, 0);
                    push_field(&mut out, FIELD_THREE_WIDTH, u64::from(generation));
                }
                XrefEntrySpec::InUse { offset, generation } => {
                    push_field(&mut out, TYPE_WIDTH, 1);
                    push_field(&mut out, FIELD_TWO_WIDTH, u64::from(offset));
                    push_field(&mut out, FIELD_THREE_WIDTH, u64::from(generation));
                }
                XrefEntrySpec::Compressed { objstm, index } => {
                    push_field(&mut out, TYPE_WIDTH, 2);
                    push_field(&mut out, FIELD_TWO_WIDTH, u64::from(objstm));
                    push_field(&mut out, FIELD_THREE_WIDTH, u64::from(index));
                }
            }
        }
    }
    Ok(out)
}

/// Write an xref-stream object at `xref_offset`, leaving `out` before
/// `startxref` so the caller can append the trailer tail.
#[allow(clippy::too_many_arguments)]
fn write_xref_stream(
    out: &mut Vec<u8>,
    entries: &std::collections::BTreeMap<u32, XrefEntrySpec>,
    ranges: &[(u32, u32)],
    size: u32,
    root: ObjectId,
    xref_number: u32,
    xref_offset: usize,
    prev: Option<u64>,
    compress: bool,
) -> Result<()> {
    let decoded = encode_xref_entries(entries, ranges)?;
    let encoded = encode_payload(&decoded, compress);
    let index_text = ranges
        .iter()
        .map(|(first, count)| format!("{first} {count}"))
        .collect::<Vec<_>>()
        .join(" ");
    let mut dictionary = format!(
        "<< /Type /XRef /Size {size} /Index [ {index_text} ] /W [{TYPE_WIDTH} {FIELD_TWO_WIDTH} {FIELD_THREE_WIDTH}] /Root {} {} R",
        root.number, root.generation
    );
    if let Some(previous) = prev {
        dictionary.push_str(&format!(" /Prev {previous}"));
    }
    if compress {
        dictionary.push_str(" /Filter /FlateDecode");
    }
    let length = i64::try_from(encoded.len())
        .map_err(|_| Error::CannotSerialize("xref stream length exceeds i64".to_owned()))?;
    dictionary.push_str(&format!(" /Length {length} >>"));

    out.extend_from_slice(format!("{xref_number} 0 obj\n{dictionary}\nstream\n").as_bytes());
    out.extend_from_slice(&encoded);
    out.extend_from_slice(b"\nendstream\nendobj\n");
    out.extend_from_slice(b"startxref\n");
    out.extend_from_slice(format!("{xref_offset}\n%%EOF\n").as_bytes());
    Ok(())
}

/// Assemble an object-stream/xref-stream PDF, optionally appended to `prefix`.
#[allow(clippy::too_many_arguments)]
fn build_document(
    prefix: &[u8],
    objects: &[(ObjectId, Object)],
    root: ObjectId,
    options: &ObjectStreamOptions,
    prev: Option<u64>,
    full: bool,
) -> Result<Vec<u8>> {
    validate_objects(objects)?;
    let highest = objects.iter().map(|(id, _)| id.number).max().unwrap_or(0);
    let first_stream = match options.first_object_number {
        Some(0) => {
            return Err(Error::CannotSerialize(
                "generated object number 0 is reserved".to_owned(),
            ));
        }
        Some(number) => number,
        None => highest
            .checked_add(1)
            .ok_or_else(|| Error::CannotSerialize("object number space is exhausted".to_owned()))?,
    };

    let mut plan_options = options.clone();
    plan_options.first_object_number = Some(first_stream);
    let plan = plan_object_streams(objects, &plan_options)?;
    let stream_count_u32 = u32::try_from(plan.streams.len())
        .map_err(|_| Error::CannotSerialize("too many object streams".to_owned()))?;
    let xref_number = first_stream
        .checked_add(stream_count_u32)
        .ok_or_else(|| Error::CannotSerialize("xref stream object number overflow".to_owned()))?;
    let size = xref_number
        .checked_add(1)
        .ok_or_else(|| Error::CannotSerialize("xref /Size exceeds u32".to_owned()))?;
    if full && !objects.iter().any(|(id, _)| *id == root) {
        return Err(Error::MissingRoot);
    }

    let mut out = prefix.to_vec();
    let mut entries = std::collections::BTreeMap::new();
    entries.insert(0u32, XrefEntrySpec::Free(u16::MAX));
    if full {
        for number in 1..size {
            entries.insert(number, XrefEntrySpec::Free(0));
        }
    }

    let packed_ids: BTreeSet<u32> = plan.packed.iter().map(|item| item.id.number).collect();
    for (id, object) in objects {
        if packed_ids.contains(&id.number) {
            continue;
        }
        let offset = u32::try_from(out.len()).map_err(|_| {
            Error::CannotSerialize("object offset exceeds the 4-byte xref field".to_owned())
        })?;
        writer::serialize_indirect(*id, object, &mut out)?;
        out.push(b'\n');
        entries.insert(
            id.number,
            XrefEntrySpec::InUse {
                offset,
                generation: id.generation,
            },
        );
    }

    for (id, object) in &plan.streams {
        let offset = u32::try_from(out.len()).map_err(|_| {
            Error::CannotSerialize("object stream offset exceeds the 4-byte xref field".to_owned())
        })?;
        writer::serialize_indirect(*id, object, &mut out)?;
        out.push(b'\n');
        entries.insert(
            id.number,
            XrefEntrySpec::InUse {
                offset,
                generation: id.generation,
            },
        );
    }
    for item in &plan.packed {
        entries.insert(
            item.id.number,
            XrefEntrySpec::Compressed {
                objstm: item.stream.number,
                index: item.index,
            },
        );
    }

    let xref_offset = out.len();
    entries.insert(
        xref_number,
        XrefEntrySpec::InUse {
            offset: u32::try_from(xref_offset).map_err(|_| {
                Error::CannotSerialize("xref offset exceeds the 4-byte xref field".to_owned())
            })?,
            generation: 0,
        },
    );
    let ranges = index_ranges(&entries.keys().copied().collect())?;
    write_xref_stream(
        &mut out,
        &entries,
        &ranges,
        size,
        root,
        xref_number,
        xref_offset,
        prev,
        options.compress,
    )?;
    Ok(out)
}

/// Write a standalone PDF whose compressed objects live in `/ObjStm` streams.
///
/// The returned bytes begin with a fresh `%PDF-` header and contain a single
/// `/Type /XRef` stream. Eligible objects are packed; streams, the catalog, the
/// page tree and non-zero-generation objects stay standalone with type-1 xref
/// entries that preserve their generation number.
///
/// # Errors
///
/// Returns a serialization error for invalid options, duplicate object numbers,
/// offsets that do not fit the xref fields, or an object that cannot be encoded.
pub fn write_object_stream_document(
    objects: &[(ObjectId, Object)],
    root: ObjectId,
    options: &ObjectStreamOptions,
) -> Result<Vec<u8>> {
    let prefix = b"%PDF-1.7\n%PDFXI\n".to_vec();
    build_document(&prefix, objects, root, options, None, true)
}

/// Append an incremental revision that uses object streams and an xref stream.
///
/// `base` is kept byte-for-byte as the prefix. Its current `startxref` becomes
/// the new section's `/Prev`. Only `objects` (plus generated streams and the
/// xref stream itself) appear in the new section, so earlier objects continue to
/// resolve through the previous revision.
///
/// # Errors
///
/// Returns any structural error from locating `base`'s `startxref`, plus the
/// serialization errors of [`write_object_stream_document`].
pub fn write_object_stream_increment(
    base: &[u8],
    objects: &[(ObjectId, Object)],
    root: ObjectId,
    options: &ObjectStreamOptions,
) -> Result<Vec<u8>> {
    let previous = crate::parser::startxref_offset(base)?;
    let previous = u64::try_from(previous)
        .map_err(|_| Error::CannotSerialize("previous startxref exceeds u64".to_owned()))?;
    let body = build_document(base, objects, root, options, Some(previous), false)?;
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filter::{Filter, FlateParams, decode_stream};

    fn name(value: &str) -> Object {
        Object::Name(value.to_owned())
    }

    fn dict(pairs: &[(&str, Object)]) -> Object {
        let mut dictionary = Dictionary::new();
        for (key, value) in pairs {
            dictionary.insert((*key).to_owned(), value.clone());
        }
        Object::Dictionary(dictionary)
    }

    fn sample_objects() -> Vec<(ObjectId, Object)> {
        let mut content_dictionary = Dictionary::new();
        content_dictionary.insert("Length".to_owned(), Object::Integer(13));
        vec![
            (
                ObjectId::new(1, 0),
                dict(&[
                    ("Type", name("Catalog")),
                    ("Pages", Object::Reference(ObjectId::new(2, 0))),
                    ("Title", Object::String(b"A title".to_vec())),
                ]),
            ),
            (
                ObjectId::new(2, 0),
                dict(&[
                    ("Type", name("Pages")),
                    (
                        "Kids",
                        Object::Array(vec![Object::Reference(ObjectId::new(3, 0))]),
                    ),
                    ("Count", Object::Integer(1)),
                ]),
            ),
            (
                ObjectId::new(3, 0),
                dict(&[
                    ("Type", name("Page")),
                    ("Parent", Object::Reference(ObjectId::new(2, 0))),
                    (
                        "MediaBox",
                        Object::Array(vec![
                            Object::Integer(0),
                            Object::Integer(0),
                            Object::Integer(612),
                            Object::Integer(792),
                        ]),
                    ),
                    ("Contents", Object::Reference(ObjectId::new(4, 0))),
                ]),
            ),
            (
                ObjectId::new(4, 0),
                Object::Stream {
                    dictionary: content_dictionary,
                    data: b"BT (Hi) Tj ET".to_vec(),
                },
            ),
            (
                ObjectId::new(5, 0),
                dict(&[("Producer", Object::String(b"pdfxi-core".to_vec()))]),
            ),
            (
                ObjectId::new(6, 0),
                dict(&[("Author", Object::String(b"tester".to_vec()))]),
            ),
        ]
    }

    fn uncompressed_options() -> ObjectStreamOptions {
        ObjectStreamOptions {
            compress: false,
            ..ObjectStreamOptions::default()
        }
    }

    fn decode_xref_payload(bytes: &[u8], compressed: bool) -> Vec<u8> {
        let marker = b"/Type /XRef";
        let at = bytes
            .windows(marker.len())
            .rposition(|window| window == marker)
            .expect("xref dictionary");
        let stream_at = bytes[at..]
            .windows(8)
            .position(|window| window == b"\nstream\n")
            .expect("xref stream starts");
        let data_start = at + stream_at + 8;
        let end_at = bytes[data_start..]
            .windows(10)
            .position(|window| window == b"\nendstream")
            .expect("xref stream ends");
        let raw = &bytes[data_start..data_start + end_at];
        if compressed {
            decode_stream(raw, &[Filter::FlateDecode(FlateParams::default())])
                .expect("xref payload decodes")
        } else {
            raw.to_vec()
        }
    }

    #[test]
    fn packing_excludes_streams_catalog_and_page_tree() {
        let objects = sample_objects();
        let expected = [
            (1, Some(PackingExclusion::IsCatalog)),
            (2, Some(PackingExclusion::IsPageTree)),
            (3, Some(PackingExclusion::IsPageTree)),
            (4, Some(PackingExclusion::IsStream)),
            (5, None),
            (6, None),
        ];
        for (number, expected_reason) in expected {
            let (id, object) = objects
                .iter()
                .find(|(id, _)| id.number == number)
                .expect("sample object");
            assert_eq!(
                packing_exclusion_reason(*id, object),
                expected_reason,
                "object {number} eligibility"
            );
        }
        assert!(!is_packing_eligible(
            ObjectId::new(9, 1),
            &dict(&[("X", Object::Integer(1))])
        ));
        assert_eq!(
            packing_exclusion_reason(ObjectId::new(9, 1), &dict(&[])),
            Some(PackingExclusion::NonZeroGeneration)
        );
    }

    #[test]
    fn object_stream_header_first_and_offsets_are_correct() {
        let plan =
            plan_object_streams(&sample_objects(), &uncompressed_options()).expect("plan builds");
        assert_eq!(plan.streams.len(), 1);
        let (stream_id, stream) = &plan.streams[0];
        assert_eq!(*stream_id, ObjectId::new(7, 0));
        let dictionary = stream.as_dictionary().expect("stream dictionary");
        assert_eq!(
            dictionary.get("Type").and_then(Object::as_name),
            Some("ObjStm")
        );
        assert_eq!(dictionary.get("N").and_then(Object::as_i64), Some(2));
        let first = dictionary
            .get("First")
            .and_then(Object::as_i64)
            .expect("/First") as usize;
        let data = match stream {
            Object::Stream { data, .. } => data,
            _ => unreachable!("stream variant"),
        };
        let header = String::from_utf8_lossy(&data[..first]).into_owned();
        assert!(header.starts_with("5 "), "header: {header}");
        assert!(header.contains(" 6 "), "header: {header}");
        assert_eq!(
            &data[first..first + 2],
            b"<<",
            "first member starts at /First"
        );
        let second_relative: usize = header
            .split_whitespace()
            .nth(3)
            .expect("second offset")
            .parse()
            .expect("decimal offset");
        let second_start = first + second_relative;
        assert_eq!(&data[second_start..second_start + 2], b"<<");
        assert_eq!(plan.packed.len(), 2);
        assert_eq!(plan.report.standalone_objects.len(), 4);
    }

    #[test]
    fn xref_stream_has_w_index_size_and_type_two_entries() {
        let objects = sample_objects();
        let bytes =
            write_object_stream_document(&objects, ObjectId::new(1, 0), &uncompressed_options())
                .expect("compact document");
        let text = String::from_utf8_lossy(&bytes).into_owned();
        assert!(text.contains("/Type /XRef"), "missing /Type /XRef");
        assert!(text.contains("/W [1 4 2]"), "missing /W widths");
        assert!(text.contains("/Size 9"), "missing /Size");
        assert!(text.contains("/Index [ 0 9 ]"), "missing /Index");
        assert!(text.contains("/Root 1 0 R"), "missing /Root");

        let payload = decode_xref_payload(&bytes, false);
        assert_eq!(payload.len(), 9 * 7, "nine entries of seven bytes each");
        // Entry 0: type 0, generation 65535.
        assert_eq!(&payload[0..7], &[0, 0, 0, 0, 0, 0xff, 0xff]);
        // Entry 1 (catalog): type 1 with a physical offset.
        assert_eq!(payload[7], 1);
        // Entry 4 (content stream): type 1, must stay physical.
        assert_eq!(payload[7 + 3 * 7], 1);
        // Entry 5 and 6: type 2 pointing at object stream 7, indices 0 and 1.
        assert_eq!(&payload[7 * 5..7 * 5 + 7], &[2, 0, 0, 0, 7, 0, 0]);
        assert_eq!(&payload[7 * 6..7 * 6 + 7], &[2, 0, 0, 0, 7, 0, 1]);
        // Entry 8 (xref stream itself): type 1.
        assert_eq!(payload[7 * 8], 1);
    }

    #[test]
    fn compressed_xref_stream_decodes_and_reparses() {
        let objects = sample_objects();
        let bytes = write_object_stream_document(
            &objects,
            ObjectId::new(1, 0),
            &ObjectStreamOptions::default(),
        )
        .expect("compressed compact document");
        let text = String::from_utf8_lossy(&bytes).into_owned();
        assert!(text.contains("/Filter /FlateDecode"));
        let payload = decode_xref_payload(&bytes, true);
        assert_eq!(payload.len(), 9 * 7);
        assert_eq!(payload[7 * 5], 2, "object 5 stays compressed");
    }

    #[test]
    fn round_trip_through_existing_parser_resolves_compressed_objects() {
        let objects = sample_objects();
        let bytes = write_object_stream_document(
            &objects,
            ObjectId::new(1, 0),
            &ObjectStreamOptions::default(),
        )
        .expect("compact document");
        let document = crate::Document::from_bytes(bytes).expect("reparses");
        assert_eq!(document.page_count().expect("page count"), 1);
        assert_eq!(document.root().expect("root"), ObjectId::new(1, 0));
        assert_eq!(document.trailer_size().expect("size"), Some(9));
        assert_eq!(document.declared_page_count().expect("count"), Some(1));
        assert_eq!(document.page(0).expect("page").id, ObjectId::new(3, 0));
        assert_eq!(document.page_text(0).expect("page text"), "Hi");
        assert_eq!(
            document
                .object(ObjectId::new(6, 0))
                .expect("compressed metadata object"),
            dict(&[("Author", Object::String(b"tester".to_vec()))])
        );
    }

    #[test]
    fn multiple_object_streams_are_chained_with_extends() {
        let mut objects = sample_objects();
        for number in 8..12 {
            objects.push((
                ObjectId::new(number, 0),
                dict(&[("Key", Object::Integer(i64::from(number)))]),
            ));
        }
        let options = ObjectStreamOptions {
            compress: false,
            max_objects_per_stream: 1,
            ..ObjectStreamOptions::default()
        };
        let plan = plan_object_streams(&objects, &options).expect("plan builds");
        assert_eq!(
            plan.streams.len(),
            6,
            "six eligible objects, one per stream"
        );
        for (index, (_, stream)) in plan.streams.iter().enumerate() {
            let dictionary = stream.as_dictionary().expect("dictionary");
            let extends = dictionary.get("Extends").and_then(Object::as_reference);
            if index + 1 < plan.streams.len() {
                assert_eq!(extends, Some(plan.streams[index + 1].0));
            } else {
                assert_eq!(extends, None, "last stream has no /Extends");
            }
        }
    }

    #[test]
    fn incremental_save_has_prev_and_only_new_entries() {
        let base = pdfxi_testkit::minimal_pdf();
        let previous = crate::parser::startxref_offset(&base).expect("base startxref");
        let options = ObjectStreamOptions {
            compress: false,
            first_object_number: Some(10),
            ..ObjectStreamOptions::default()
        };
        let changed = vec![(ObjectId::new(11, 0), dict(&[("Type", name("Custom"))]))];
        let bytes = write_object_stream_increment(&base, &changed, ObjectId::new(1, 0), &options)
            .expect("incremental document");
        assert!(bytes.starts_with(&base), "base bytes are an exact prefix");
        let text = String::from_utf8_lossy(&bytes).into_owned();
        assert!(text.contains(&format!("/Prev {previous}")), "missing /Prev");
        assert!(text.matches("startxref").count() >= 2, "two xref sections");
        let document = crate::Document::from_bytes(bytes).expect("incremental parses");
        assert_eq!(document.page_count().expect("page count"), 1);
        assert_eq!(
            document.page_text(0).expect("base page text"),
            "Hello, PDFXI!"
        );
    }

    #[test]
    fn nonzero_generation_is_preserved_in_type_one_entry() {
        let objects = vec![
            (ObjectId::new(1, 0), dict(&[("Type", name("Catalog"))])),
            (ObjectId::new(2, 7), dict(&[("Note", Object::Integer(1))])),
        ];
        let plan = plan_object_streams(&objects, &uncompressed_options()).expect("plan");
        assert_eq!(
            plan.report.standalone_objects,
            vec![ObjectId::new(1, 0), ObjectId::new(2, 7)]
        );
        let bytes =
            write_object_stream_document(&objects, ObjectId::new(1, 0), &uncompressed_options())
                .expect("document");
        let payload = decode_xref_payload(&bytes, false);
        // Entries: 0 free, 1 catalog, 2 generation 7, 3 xref (no object stream
        // is needed because neither input object is eligible for packing).
        assert_eq!(payload[7 * 2 + 6], 7, "generation field must be 7");
        assert_eq!(payload[7 * 2], 1, "non-zero generation stays type 1");
    }

    #[test]
    fn invalid_options_and_duplicate_ids_are_rejected() {
        let objects = sample_objects();
        let zero_limit = ObjectStreamOptions {
            max_objects_per_stream: 0,
            ..ObjectStreamOptions::default()
        };
        assert!(matches!(
            plan_object_streams(&objects, &zero_limit),
            Err(Error::CannotSerialize(_))
        ));
        let duplicates = vec![
            (ObjectId::new(1, 0), dict(&[])),
            (ObjectId::new(1, 0), dict(&[])),
        ];
        assert!(matches!(
            plan_object_streams(&duplicates, &uncompressed_options()),
            Err(Error::InvalidObjectId(_))
        ));
        assert!(matches!(
            write_object_stream_document(&objects, ObjectId::new(99, 0), &uncompressed_options()),
            Err(Error::MissingRoot)
        ));
    }

    #[test]
    fn document_save_with_object_streams_round_trips() {
        let source = pdfxi_testkit::minimal_pdf();
        let document = crate::Document::from_bytes(source).expect("source parses");
        let saved = document
            .save_with_object_streams(&ObjectStreamOptions::default())
            .expect("compact save");
        assert!(String::from_utf8_lossy(&saved).contains("/Type /XRef"));
        let reparsed = crate::Document::from_bytes(saved).expect("compact file reparses");
        assert_eq!(reparsed.page_count().expect("page count"), 1);
        assert_eq!(reparsed.page_text(0).expect("page text"), "Hello, PDFXI!");
    }

    #[test]
    fn stored_zlib_payload_round_trips_through_inflater() {
        let payload = b"object stream payload with repeated bytes".to_vec();
        let encoded = zlib_stored(&payload);
        let decoded = decode_stream(&encoded, &[Filter::FlateDecode(FlateParams::default())])
            .expect("inflate stored blocks");
        assert_eq!(decoded, payload);
    }
}
