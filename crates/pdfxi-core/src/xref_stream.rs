//! General, deterministic cross-reference stream writing.
//!
//! The object-stream writer emits an xref stream as part of object packing, and
//! `incremental::save_incremental_with` emits a small stream section for changed
//! objects. Neither path writes a *standalone* document whose single structural
//! table is a `/Type /XRef` stream built from an arbitrary set of objects. This
//! module fills that gap without changing either existing writer.
//!
//! The plan is pure: [`XrefStreamPlan::build`] derives entry order, `/Index`
//! ranges, `/W` widths, row width and payload size only from its inputs, so the
//! same object set always produces the same bytes regardless of insertion order.
//! Hostile sizes, duplicate or reserved object numbers and offsets that cannot
//! fit in a bounded field are rejected with [`XrefStreamError`] before any bytes
//! are written. Payload encoding is optionally wrapped in a stored-block zlib
//! stream, which the reader decodes through the normal filter path.
//!
//! # Behaviour decisions
//!
//! - `/W` field two is the *smallest* width that holds the largest in-use
//!   offset: 255 needs one byte, 256 needs two, 65_535 two and 65_536 three.
//!   An offset that would need the full eight-byte `u64` field (`>= 2^56`) is
//!   rejected as hostile instead of emitting a maximal-width field.
//! - The xref-stream object number must appear as an in-use entry in the input
//!   set, matching what [`write_xref_stream_document`] always emits. Callers
//!   building plans directly must include that entry.
//! - Free gaps up to `/Size` are inserted, so a generated stream has one
//!   contiguous `/Index` subsection. As an early hostile-input guard, an input
//!   set with more than [`MAX_XREF_INDEX_SUBSECTIONS`] disjoint object-number
//!   runs is rejected before gaps are materialised.
//! - [`XrefStreamPlan::decode_payload`] verifies every decoded row against the
//!   plan, not just the total length, so a flipped byte is detected in both
//!   compressed and uncompressed payloads.

use std::collections::BTreeMap;

use crate::filter::{Filter, FlateParams, decode_stream};
use crate::object::{Dictionary, Object, ObjectId};
use crate::{Document, Error, Result, writer};

/// Maximum number of entries a generated xref stream may declare.
///
/// This mirrors the reader's bound in `parser`, so a plan can never ask the
/// parser to materialise a larger table than the one it will accept.
pub const MAX_XREF_STREAM_ENTRIES: usize = 16_000_000;

/// Maximum number of bytes in one `/W` field.
pub const MAX_XREF_FIELD_BYTES: u8 = 8;

/// Maximum number of disjoint object-number runs accepted in a plan input.
///
/// Generated streams fill gaps and therefore emit one contiguous `/Index`
/// subsection, but this bound rejects an input set that is too fragmented to
/// plan safely before free placeholders are materialised. It also bounds the
/// defensive `/Index` check in [`XrefStreamPlan::validate`].
pub const MAX_XREF_INDEX_SUBSECTIONS: usize = 4096;

/// A typed reason an xref-stream write plan was rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum XrefStreamError {
    /// The caller supplied no entries.
    EmptyEntries,
    /// Object number 0 was supplied as an in-use entry; it is reserved.
    ReservedObjectZero,
    /// The same object number was supplied twice with conflicting values.
    DuplicateObject(u32),
    /// The catalog object was not among the in-use entries.
    MissingRoot(ObjectId),
    /// The generated xref-stream object number is reserved or already in use.
    InvalidXrefNumber(u32),
    /// An object number cannot be represented by the declared `/Size`.
    ObjectBeyondSize {
        /// The offending object number.
        object: u32,
        /// The declared size.
        size: u32,
    },
    /// The declared size or the number of emitted entries exceeds the bound.
    TooManyEntries {
        /// The requested count.
        count: usize,
        /// The maximum accepted count.
        limit: usize,
    },
    /// The sparse object set needs too many `/Index` subsections.
    TooManySubsections {
        /// The requested subsection count.
        count: usize,
        /// The maximum accepted count.
        limit: usize,
    },
    /// An object offset does not fit in a bounded xref field.
    OffsetTooLarge {
        /// The object whose offset was rejected.
        object: u32,
        /// The rejected offset.
        offset: u64,
    },
    /// A free entry carried a non-zero field value.
    InvalidFreeEntry(u32),
    /// The computed `/W` widths are outside the reader's accepted range.
    InvalidWidths {
        /// The three computed widths.
        widths: [u8; 3],
    },
    /// The stream header was not a PDF header.
    InvalidHeader,
    /// Non-zero generations cannot be represented by a type-1 xref entry here.
    UnsupportedGeneration(u32),
    /// An arithmetic bound was exceeded while planning or encoding.
    Overflow(&'static str),
    /// The encoded payload could not be decoded back with the declared filter.
    MalformedPayload(String),
}

impl std::fmt::Display for XrefStreamError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyEntries => f.write_str("xref stream needs at least one entry"),
            Self::ReservedObjectZero => {
                f.write_str("object number 0 is reserved for the free list")
            }
            Self::DuplicateObject(number) => write!(f, "duplicate xref entry for object {number}"),
            Self::MissingRoot(root) => write!(f, "catalog object {root} is not in the xref plan"),
            Self::InvalidXrefNumber(number) => {
                write!(
                    f,
                    "xref stream object number {number} is reserved or in use"
                )
            }
            Self::ObjectBeyondSize { object, size } => {
                write!(f, "object {object} lies outside /Size {size}")
            }
            Self::TooManyEntries { count, limit } => {
                write!(
                    f,
                    "xref stream declares {count} entries, above the {limit} limit"
                )
            }
            Self::TooManySubsections { count, limit } => {
                write!(
                    f,
                    "xref stream needs {count} /Index subsections, above the {limit} limit"
                )
            }
            Self::OffsetTooLarge { object, offset } => {
                write!(
                    f,
                    "object {object} offset {offset} does not fit in an xref field"
                )
            }
            Self::InvalidFreeEntry(number) => {
                write!(
                    f,
                    "free xref entry for object {number} has a non-zero offset"
                )
            }
            Self::InvalidWidths { widths } => {
                write!(f, "invalid xref /W field widths {widths:?}")
            }
            Self::InvalidHeader => f.write_str("xref stream output needs a %PDF- header"),
            Self::UnsupportedGeneration(number) => {
                write!(
                    f,
                    "object {number} has a non-zero generation, which this writer cannot emit"
                )
            }
            Self::Overflow(what) => write!(f, "{what} overflowed while writing an xref stream"),
            Self::MalformedPayload(reason) => {
                write!(f, "xref stream payload failed to decode: {reason}")
            }
        }
    }
}

impl std::error::Error for XrefStreamError {}

/// The type byte of one generated xref-stream entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum XrefEntryKind {
    /// Type 0: a free object.
    Free,
    /// Type 1: an in-use object at a byte offset.
    InUse,
}

/// One entry of a generated xref stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct XrefStreamEntry {
    /// The object number addressed by this entry.
    pub number: u32,
    /// Whether the object is free or in use.
    pub kind: XrefEntryKind,
    /// Byte offset of an in-use object; always zero for a free entry.
    pub offset: u64,
}

impl XrefStreamEntry {
    /// Construct an in-use entry at `offset`.
    #[must_use]
    pub const fn in_use(number: u32, offset: u64) -> Self {
        Self {
            number,
            kind: XrefEntryKind::InUse,
            offset,
        }
    }

    /// Construct a free entry.
    #[must_use]
    pub const fn free(number: u32) -> Self {
        Self {
            number,
            kind: XrefEntryKind::Free,
            offset: 0,
        }
    }

    /// Return the type byte written into the first `/W` field.
    #[must_use]
    pub const fn type_byte(self) -> u8 {
        match self.kind {
            XrefEntryKind::Free => 0,
            XrefEntryKind::InUse => 1,
        }
    }
}

/// Options for a standalone xref-stream save.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct XrefStreamOptions {
    /// Wrap the entry table in a stored-block zlib stream (deterministic).
    ///
    /// This is real Flate compression at the transport level; stored blocks do
    /// not shrink the payload, but the reader exercises the same filter path as
    /// a fully compressed stream and no compressor dependency is required.
    pub compress: bool,
}

impl Default for XrefStreamOptions {
    fn default() -> Self {
        Self { compress: true }
    }
}

impl XrefStreamOptions {
    /// Select or deselect the Flate wrapper.
    #[must_use]
    pub const fn with_compress(mut self, compress: bool) -> Self {
        self.compress = compress;
        self
    }
}

/// A validated, deterministic xref-stream write plan.
///
/// Entries are sorted by object number and include free placeholders for every
/// gap up to `/Size`, so `/Index` subsections are contiguous and decode exactly
/// the entry rows the writer emits. No field is wider than eight bytes and the
/// row width never exceeds the reader's 16-byte bound.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XrefStreamPlan {
    entries: Vec<XrefStreamEntry>,
    ranges: Vec<(u32, u32)>,
    widths: [u8; 3],
    size: u32,
    root: ObjectId,
    xref_number: u32,
    compress: bool,
}

impl XrefStreamPlan {
    /// Build and validate a plan from a caller-supplied entry set.
    ///
    /// Identical entries may be supplied in any order; conflicting duplicates
    /// are rejected. Entry 0 is inserted as free when absent, and every gap up
    /// to `size` is inserted as a free entry so `/Index` stays contiguous.
    ///
    /// # Errors
    ///
    /// Returns a typed [`XrefStreamError`] for empty or malformed entry sets,
    /// hostile sizes, duplicate object numbers, an absent catalog, offsets that
    /// cannot fit in eight bytes, or a row wider than the reader accepts.
    pub fn build(
        entries: &[XrefStreamEntry],
        size: u32,
        root: ObjectId,
        xref_number: u32,
        options: &XrefStreamOptions,
    ) -> std::result::Result<Self, XrefStreamError> {
        if entries.is_empty() {
            return Err(XrefStreamError::EmptyEntries);
        }
        if xref_number == 0 {
            return Err(XrefStreamError::InvalidXrefNumber(xref_number));
        }
        let mut by_number: BTreeMap<u32, XrefStreamEntry> = BTreeMap::new();
        for &entry in entries {
            if entry.number == 0 && entry.kind == XrefEntryKind::InUse {
                return Err(XrefStreamError::ReservedObjectZero);
            }
            if entry.kind == XrefEntryKind::Free && entry.offset != 0 {
                return Err(XrefStreamError::InvalidFreeEntry(entry.number));
            }
            match by_number.get(&entry.number) {
                Some(existing) if *existing != entry => {
                    return Err(XrefStreamError::DuplicateObject(entry.number));
                }
                Some(_) => {}
                None => {
                    by_number.insert(entry.number, entry);
                }
            }
        }
        by_number
            .entry(0)
            .or_insert_with(|| XrefStreamEntry::free(0));

        let highest = by_number
            .keys()
            .next_back()
            .copied()
            .ok_or(XrefStreamError::EmptyEntries)?;
        let size = size.max(highest.saturating_add(1));
        let size_usize = usize::try_from(size).map_err(|_| XrefStreamError::Overflow("/Size"))?;
        if size_usize > MAX_XREF_STREAM_ENTRIES {
            return Err(XrefStreamError::TooManyEntries {
                count: size_usize,
                limit: MAX_XREF_STREAM_ENTRIES,
            });
        }
        for &number in by_number.keys() {
            if number >= size {
                return Err(XrefStreamError::ObjectBeyondSize {
                    object: number,
                    size,
                });
            }
        }
        if !by_number
            .get(&xref_number)
            .is_some_and(|entry| entry.kind == XrefEntryKind::InUse && entry.number == xref_number)
        {
            return Err(XrefStreamError::DuplicateObject(xref_number));
        }
        match by_number.get(&root.number) {
            Some(entry) if entry.kind == XrefEntryKind::InUse && root.generation == 0 => {}
            _ => return Err(XrefStreamError::MissingRoot(root)),
        }

        // Reject fragmented inputs before materialising free gaps. A caller that
        // supplies thousands of disjoint object numbers would otherwise force a
        // large allocation and /Index walk even for a small in-use set.
        let input_runs = count_runs(&by_number);
        if input_runs > MAX_XREF_INDEX_SUBSECTIONS {
            return Err(XrefStreamError::TooManySubsections {
                count: input_runs,
                limit: MAX_XREF_INDEX_SUBSECTIONS,
            });
        }

        let widest = by_number
            .values()
            .filter(|entry| entry.kind == XrefEntryKind::InUse)
            .max_by_key(|entry| entry.offset);
        let max_offset = widest.map_or(0, |entry| entry.offset);
        let max_object = widest.map_or(0, |entry| entry.number);
        let width_two = width_for(max_offset, max_object)?;
        let widths = [1, width_two, 0];
        let row_width = usize::from(widths[0])
            .checked_add(usize::from(widths[1]))
            .and_then(|sum| sum.checked_add(usize::from(widths[2])))
            .ok_or(XrefStreamError::Overflow("/W row width"))?;
        if row_width > usize::from(MAX_XREF_FIELD_BYTES) * 2 {
            return Err(XrefStreamError::InvalidWidths { widths });
        }

        // Insert free placeholders for gaps so /Index can describe contiguous
        // runs. The bound is checked before allocating the output vector.
        if size as usize > by_number.len() + MAX_XREF_STREAM_ENTRIES {
            return Err(XrefStreamError::TooManyEntries {
                count: size as usize,
                limit: MAX_XREF_STREAM_ENTRIES,
            });
        }
        let mut entries = Vec::with_capacity(size as usize);
        for number in 0..size {
            entries.push(
                by_number
                    .get(&number)
                    .copied()
                    .unwrap_or_else(|| XrefStreamEntry::free(number)),
            );
        }
        let mut ranges: Vec<(u32, u32)> = Vec::new();
        for entry in &entries {
            match ranges.last_mut() {
                Some((first, count)) if first.saturating_add(*count) == entry.number => {
                    *count += 1;
                }
                _ => ranges.push((entry.number, 1)),
            }
        }
        if ranges.len() > MAX_XREF_INDEX_SUBSECTIONS {
            return Err(XrefStreamError::TooManySubsections {
                count: ranges.len(),
                limit: MAX_XREF_INDEX_SUBSECTIONS,
            });
        }

        let plan = Self {
            entries,
            ranges,
            widths,
            size,
            root,
            xref_number,
            compress: options.compress,
        };
        plan.validate()?;
        Ok(plan)
    }

    /// Re-check every constructor invariant before emitting bytes.
    ///
    /// # Errors
    ///
    /// Returns a typed error if the plan no longer has sorted unique entries,
    /// contiguous ranges, valid widths or an in-use catalog and xref object.
    pub fn validate(&self) -> std::result::Result<(), XrefStreamError> {
        if self.entries.is_empty() || self.entries.first().is_none_or(|e| e.number != 0) {
            return Err(XrefStreamError::EmptyEntries);
        }
        let mut expected = 0u32;
        for entry in &self.entries {
            if entry.number != expected {
                return Err(XrefStreamError::DuplicateObject(entry.number));
            }
            if entry.kind == XrefEntryKind::Free && entry.offset != 0 {
                return Err(XrefStreamError::InvalidFreeEntry(entry.number));
            }
            expected = expected
                .checked_add(1)
                .ok_or(XrefStreamError::Overflow("object number"))?;
        }
        if expected != self.size {
            return Err(XrefStreamError::ObjectBeyondSize {
                object: expected,
                size: self.size,
            });
        }
        if self.entries.len() > MAX_XREF_STREAM_ENTRIES {
            return Err(XrefStreamError::TooManyEntries {
                count: self.entries.len(),
                limit: MAX_XREF_STREAM_ENTRIES,
            });
        }
        if self.ranges.len() > MAX_XREF_INDEX_SUBSECTIONS {
            return Err(XrefStreamError::TooManySubsections {
                count: self.ranges.len(),
                limit: MAX_XREF_INDEX_SUBSECTIONS,
            });
        }
        let mut covered = 0usize;
        for &(first, count) in &self.ranges {
            let start = usize::try_from(first).map_err(|_| XrefStreamError::Overflow("/Index"))?;
            if start != covered {
                return Err(XrefStreamError::Overflow("/Index"));
            }
            covered = covered
                .checked_add(count as usize)
                .ok_or(XrefStreamError::Overflow("/Index"))?;
        }
        if covered != self.entries.len() {
            return Err(XrefStreamError::Overflow("/Index"));
        }
        let widths = self.widths;
        let row = usize::from(widths[0])
            .checked_add(usize::from(widths[1]))
            .and_then(|sum| sum.checked_add(usize::from(widths[2])))
            .ok_or(XrefStreamError::Overflow("/W row width"))?;
        if widths[0] != 1
            || widths[1] == 0
            || widths[1] > MAX_XREF_FIELD_BYTES
            || widths[2] != 0
            || row > usize::from(MAX_XREF_FIELD_BYTES) * 2
        {
            return Err(XrefStreamError::InvalidWidths { widths });
        }
        for entry in &self.entries {
            if fits_in(entry.offset, widths[1]) {
                continue;
            }
            return Err(XrefStreamError::OffsetTooLarge {
                object: entry.number,
                offset: entry.offset,
            });
        }
        match self
            .entries
            .get(usize::try_from(self.root.number).unwrap_or(usize::MAX))
        {
            Some(entry)
                if entry.kind == XrefEntryKind::InUse
                    && entry.number == self.root.number
                    && self.root.generation == 0 => {}
            _ => return Err(XrefStreamError::MissingRoot(self.root)),
        }
        match self
            .entries
            .get(usize::try_from(self.xref_number).unwrap_or(usize::MAX))
        {
            Some(entry)
                if entry.kind == XrefEntryKind::InUse && entry.number == self.xref_number => {}
            _ => return Err(XrefStreamError::InvalidXrefNumber(self.xref_number)),
        }
        Ok(())
    }

    /// Entries in ascending object-number order, including free gaps.
    #[must_use]
    pub fn entries(&self) -> &[XrefStreamEntry] {
        &self.entries
    }

    /// Contiguous `(first, count)` pairs written to `/Index`.
    #[must_use]
    pub fn ranges(&self) -> &[(u32, u32)] {
        &self.ranges
    }

    /// The three `/W` field widths.
    #[must_use]
    pub const fn widths(&self) -> [u8; 3] {
        self.widths
    }

    /// The declared `/Size`, always at least one greater than the highest entry.
    #[must_use]
    pub const fn size(&self) -> u32 {
        self.size
    }

    /// Bytes per entry row.
    #[must_use]
    pub fn row_width(&self) -> usize {
        usize::from(self.widths[0]) + usize::from(self.widths[1]) + usize::from(self.widths[2])
    }

    /// Number of decoded entry bytes before the optional Flate wrapper.
    #[must_use]
    pub fn decoded_payload_len(&self) -> usize {
        self.entries.len() * self.row_width()
    }

    /// Encode the entry rows, wrapping them in Flate when configured.
    ///
    /// # Errors
    ///
    /// Returns a typed error when the plan is invalid or an offset cannot be
    /// written into its computed field width.
    pub fn encode(&self) -> std::result::Result<Vec<u8>, XrefStreamError> {
        self.validate()?;
        let mut decoded = Vec::with_capacity(self.decoded_payload_len());
        for entry in &self.entries {
            push_field(&mut decoded, self.widths[0], u64::from(entry.type_byte()));
            push_field(&mut decoded, self.widths[1], entry.offset);
        }
        Ok(crate::objstm::encode_payload(&decoded, self.compress))
    }

    /// Decode a previously encoded payload and check it against this plan.
    ///
    /// This is the library-level round-trip counterpart of [`Self::encode`].
    /// Every decoded row is compared with the plan's expected type byte and
    /// offset, so corruption is detected even when no checksum is carried
    /// (that is, when compression is disabled).
    ///
    /// # Errors
    ///
    /// Returns a typed error when the payload is malformed or its decoded rows
    /// do not match the plan's entries.
    pub fn decode_payload(&self, encoded: &[u8]) -> std::result::Result<Vec<u8>, XrefStreamError> {
        let decoded = if self.compress {
            decode_stream(encoded, &[Filter::FlateDecode(FlateParams::default())])
                .map_err(|error| XrefStreamError::MalformedPayload(error.to_string()))?
        } else {
            encoded.to_vec()
        };
        let mut expected = Vec::with_capacity(self.decoded_payload_len());
        for entry in &self.entries {
            push_field(&mut expected, self.widths[0], u64::from(entry.type_byte()));
            push_field(&mut expected, self.widths[1], entry.offset);
        }
        if decoded != expected {
            return Err(XrefStreamError::MalformedPayload(format!(
                "decoded {} bytes do not match the planned {} rows",
                decoded.len(),
                self.entries.len()
            )));
        }
        Ok(decoded)
    }

    /// The catalog this plan validates.
    #[must_use]
    pub const fn root(&self) -> ObjectId {
        self.root
    }

    /// The object number used for the xref stream itself.
    #[must_use]
    pub const fn xref_number(&self) -> u32 {
        self.xref_number
    }

    /// Whether the payload carries a Flate wrapper.
    #[must_use]
    pub const fn compress(&self) -> bool {
        self.compress
    }
}

/// Return the fewest bytes (one through seven) that can hold `value`.
///
/// A `u64` always fits in eight bytes, so admitting the full-width field would
/// make an absurd offset (one needing `>= 2^56` bytes of file) look valid. The
/// writer rejects such values as hostile. `object` is only used for the error.
fn width_for(value: u64, object: u32) -> std::result::Result<u8, XrefStreamError> {
    for width in 1..MAX_XREF_FIELD_BYTES {
        if value < (1u64 << (8 * u32::from(width))) {
            return Ok(width);
        }
    }
    Err(XrefStreamError::OffsetTooLarge {
        object,
        offset: value,
    })
}

/// Count contiguous object-number runs in a validated entry map.
fn count_runs(by_number: &BTreeMap<u32, XrefStreamEntry>) -> usize {
    let mut runs = 0usize;
    let mut previous: Option<u32> = None;
    for &number in by_number.keys() {
        if !previous.is_some_and(|last| last.saturating_add(1) == number) {
            runs += 1;
        }
        previous = Some(number);
    }
    runs
}

/// Return `true` when `value` fits in `width` big-endian bytes.
fn fits_in(value: u64, width: u8) -> bool {
    if width == 0 {
        return value == 0;
    }
    if width >= 8 {
        return true;
    }
    value < (1u64 << (8 * u32::from(width)))
}

/// Append a big-endian field of `width` bytes.
fn push_field(out: &mut Vec<u8>, width: u8, value: u64) {
    for shift in (0..width).rev() {
        let amount = 8 * u32::from(shift);
        out.push(if amount >= 64 {
            0
        } else {
            ((value >> amount) & 0xff) as u8
        });
    }
}

/// A standalone xref-stream document with its validated plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XrefStreamSaveResult {
    /// The complete PDF byte stream.
    pub bytes: Vec<u8>,
    /// The plan that produced the `/W`, `/Size` and `/Index` entries.
    pub plan: XrefStreamPlan,
}

/// Assemble a standalone PDF with one xref stream as its structural table.
///
/// Every entry is type 0 (free) or type 1 (in use at a byte offset): the writer
/// deliberately does not pack objects into `/ObjStm`, so it can serialise any
/// object set without changing object numbers. Xref-stream-specific trailer
/// keys (`/Type`, `/W`, `/Index`, `/Prev`, `/XRefStm`, filters and lengths) are
/// removed before the new stream dictionary is built; all other trailer keys,
/// including `/Info` and `/ID`, are preserved.
///
/// # Errors
///
/// Returns a typed error when `header` is not a PDF header, when an object has
/// number zero or a non-zero generation, when object numbers are duplicated,
/// when the catalog or xref-stream object number is invalid, or when the plan's
/// size, offsets or subsection count exceed the reader's bounds.
#[allow(clippy::too_many_arguments)]
pub fn write_xref_stream_document(
    objects: &[(ObjectId, Object)],
    root: ObjectId,
    trailer: &Dictionary,
    xref_number: u32,
    header: &[u8],
    options: &XrefStreamOptions,
) -> Result<XrefStreamSaveResult> {
    if !header.starts_with(b"%PDF-") {
        return Err(Error::from(XrefStreamError::InvalidHeader));
    }
    let mut bodies: BTreeMap<u32, Vec<u8>> = BTreeMap::new();
    for (id, object) in objects {
        if id.number == 0 {
            return Err(Error::from(XrefStreamError::ReservedObjectZero));
        }
        if id.generation != 0 {
            return Err(Error::from(XrefStreamError::UnsupportedGeneration(
                id.number,
            )));
        }
        if bodies.insert(id.number, Vec::new()).is_some() {
            return Err(Error::from(XrefStreamError::DuplicateObject(id.number)));
        }
        let mut body = Vec::new();
        writer::serialize_indirect(*id, object, &mut body)?;
        bodies.insert(id.number, body);
    }
    if xref_number == 0 || bodies.contains_key(&xref_number) {
        return Err(Error::from(XrefStreamError::InvalidXrefNumber(xref_number)));
    }
    if !bodies.contains_key(&root.number) || root.generation != 0 {
        return Err(Error::from(XrefStreamError::MissingRoot(root)));
    }

    let declared = trailer
        .get("Size")
        .and_then(Object::as_i64)
        .filter(|value| *value >= 0)
        .and_then(|value| u32::try_from(value).ok())
        .unwrap_or(0);
    let highest = bodies
        .keys()
        .next_back()
        .copied()
        .unwrap_or(0)
        .max(xref_number);
    let size = declared.max(highest.saturating_add(1)).max(1);

    let mut out = header.to_vec();
    if out.last() != Some(&b'\n') {
        out.push(b'\n');
    }
    let mut entries = vec![XrefStreamEntry::free(0)];
    for (number, body) in &bodies {
        let offset = u64::try_from(out.len()).map_err(|_| XrefStreamError::Overflow("offset"))?;
        entries.push(XrefStreamEntry::in_use(*number, offset));
        out.extend_from_slice(body);
        out.push(b'\n');
    }
    let xref_offset = u64::try_from(out.len()).map_err(|_| XrefStreamError::Overflow("offset"))?;
    for number in 1..size {
        if !bodies.contains_key(&number) && number != xref_number {
            entries.push(XrefStreamEntry::free(number));
        }
    }
    entries.push(XrefStreamEntry::in_use(xref_number, xref_offset));

    let plan = XrefStreamPlan::build(&entries, size, root, xref_number, options)?;
    let payload = plan.encode()?;

    let mut dictionary = trailer.clone();
    for key in [
        "Type",
        "W",
        "Index",
        "Length",
        "Filter",
        "DecodeParms",
        "Prev",
        "XRefStm",
        "Size",
        "Root",
    ] {
        dictionary.remove(key);
    }
    dictionary.insert("Type".to_owned(), Object::Name("XRef".to_owned()));
    dictionary.insert("Size".to_owned(), Object::Integer(i64::from(size)));
    dictionary.insert("Root".to_owned(), Object::Reference(root));
    let w = plan.widths().map(|width| Object::Integer(i64::from(width)));
    dictionary.insert("W".to_owned(), Object::Array(w.to_vec()));
    let index = plan
        .ranges()
        .iter()
        .flat_map(|&(first, count)| {
            [
                Object::Integer(i64::from(first)),
                Object::Integer(i64::from(count)),
            ]
        })
        .collect();
    dictionary.insert("Index".to_owned(), Object::Array(index));
    if options.compress {
        dictionary.insert("Filter".to_owned(), Object::Name("FlateDecode".to_owned()));
    }
    writer::serialize_indirect(
        ObjectId::new(xref_number, 0),
        &Object::Stream {
            dictionary,
            data: payload,
        },
        &mut out,
    )?;
    out.extend_from_slice(b"\nstartxref\n");
    out.extend_from_slice(format!("{xref_offset}\n%%EOF\n").as_bytes());
    Ok(XrefStreamSaveResult { bytes: out, plan })
}

impl Document {
    /// Save the document with a single, general xref stream.
    ///
    /// Eligible objects are serialised at their existing object numbers (stream
    /// bytes are not re-compressed), staged changes from
    /// [`Document::set_object`] are applied, and a fresh `/Type /XRef` stream is
    /// appended as the only structural table. Unlike [`Document::save`] this
    /// path accepts sources that already use xref streams or object streams,
    /// because compressed objects are resolved by the parser. Encrypted input
    /// is declined because the writer does not encrypt its output.
    ///
    /// The returned plan and bytes are deterministic for a given document and
    /// option set, and the output can be read back with
    /// [`Document::from_bytes`].
    ///
    /// # Errors
    ///
    /// Returns the stored structural error, [`Error::UnsupportedState`] for
    /// encrypted input, [`Error::XrefStreamPlan`] for a hostile object set, and
    /// any serialization error from the staged objects.
    pub fn save_with_xref_stream(
        &self,
        options: &XrefStreamOptions,
    ) -> Result<XrefStreamSaveResult> {
        let structure = self.structure.as_ref().map_err(Clone::clone)?;
        if structure.xref.trailer().contains_key("Encrypt") {
            return Err(Error::UnsupportedState(
                "encrypted documents cannot be saved",
            ));
        }
        let mut objects = Vec::new();
        for number in structure.xref.all_object_numbers() {
            if number == 0 {
                continue;
            }
            let generation =
                crate::parser::object_generation(&self.bytes, &structure.xref, number)?;
            let id = ObjectId::new(number, generation);
            let object = match self.changes.get(&id) {
                Some(changed) => changed.clone(),
                None => crate::parser::load_object(&self.bytes, &structure.xref, id)?,
            };
            objects.push((id, object));
        }
        for (id, object) in &self.changes {
            if !objects.iter().any(|(existing, _)| existing == id) {
                objects.push((*id, object.clone()));
            }
        }
        objects.sort_by_key(|(id, _)| (id.number, id.generation));

        let xref_number = self.allocate_object_number()?;
        let mut trailer = structure.xref.trailer();
        for (key, value) in &self.trailer_entries {
            trailer.insert(key.clone(), value.clone());
        }
        let header = format!("%PDF-{}\n%PDFXI\n", self.version);
        write_xref_stream_document(
            &objects,
            structure.root_id,
            &trailer,
            xref_number,
            header.as_bytes(),
            options,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::XrefSectionKind;

    fn entries(values: &[(u32, u64)]) -> Vec<XrefStreamEntry> {
        values
            .iter()
            .map(|&(number, offset)| XrefStreamEntry::in_use(number, offset))
            .collect()
    }

    fn fixture() -> Vec<u8> {
        pdfxi_testkit::minimal_pdf()
    }

    #[test]
    fn plan_is_deterministic_and_insertion_order_independent() {
        // The xref-stream object (7) is an in-use entry, distinct from the
        // catalog (1), as the writer always emits.
        let forward = entries(&[(1, 12), (2, 48), (3, 96), (6, 180), (7, 216)]);
        let mut shuffled = forward.clone();
        shuffled.reverse();
        shuffled.push(forward[0]);
        let options = XrefStreamOptions::default();
        let a = XrefStreamPlan::build(&forward, 8, ObjectId::new(1, 0), 7, &options)
            .expect("forward plan");
        let b = XrefStreamPlan::build(&shuffled, 8, ObjectId::new(1, 0), 7, &options)
            .expect("shuffled plan");
        assert_eq!(a, b);
        assert_eq!(
            a.encode().expect("payload a"),
            b.encode().expect("payload b")
        );
        assert_eq!(a.ranges(), &[(0, 8)]);
        assert_eq!(a.widths(), [1, 1, 0]);
    }

    #[test]
    fn plan_validation_rejects_reserved_duplicate_and_missing_root() {
        let options = XrefStreamOptions::default();
        let reserved = entries(&[(0, 4), (1, 10), (2, 20)]);
        assert_eq!(
            XrefStreamPlan::build(&reserved, 3, ObjectId::new(1, 0), 3, &options)
                .expect_err("object zero is reserved"),
            XrefStreamError::ReservedObjectZero
        );
        let mut conflicting = entries(&[(1, 10), (2, 20), (2, 30)]);
        conflicting.push(XrefStreamEntry::in_use(3, 40));
        assert_eq!(
            XrefStreamPlan::build(&conflicting, 4, ObjectId::new(1, 0), 4, &options)
                .expect_err("conflicting duplicate"),
            XrefStreamError::DuplicateObject(2)
        );
        let rootless = entries(&[(1, 10), (2, 20), (3, 30)]);
        // Object 3 is the xref-stream entry; the catalog id 99 is absent.
        assert_eq!(
            XrefStreamPlan::build(&rootless, 4, ObjectId::new(99, 0), 3, &options)
                .expect_err("catalog not present"),
            XrefStreamError::MissingRoot(ObjectId::new(99, 0))
        );
    }

    #[test]
    fn plan_rejects_hostile_size_and_index_counts() {
        let options = XrefStreamOptions::default();
        let small = entries(&[(1, 10), (2, 20), (3, 30)]);
        let huge = (MAX_XREF_STREAM_ENTRIES + 1) as u32;
        assert!(matches!(
            XrefStreamPlan::build(&small, huge, ObjectId::new(1, 0), huge, &options),
            Err(XrefStreamError::TooManyEntries { .. })
        ));
        let xref = (MAX_XREF_INDEX_SUBSECTIONS * 2 + 2) as u32;
        // More disjoint runs than the bound, with the xref-stream object present
        // so the plan reaches the fragmentation check.
        let sparse: Vec<_> = (0..(MAX_XREF_INDEX_SUBSECTIONS + 1))
            .map(|index| XrefStreamEntry::in_use((index * 2 + 1) as u32, 16))
            .chain(std::iter::once(XrefStreamEntry::in_use(xref, 32)))
            .collect();
        assert!(matches!(
            XrefStreamPlan::build(&sparse, xref + 1, ObjectId::new(1, 0), xref, &options),
            Err(XrefStreamError::TooManySubsections { .. })
        ));
    }

    #[test]
    fn field_widths_follow_offset_boundaries() {
        let options = XrefStreamOptions::default();
        // Every entry sits at the tested boundary offset so the maximal width is
        // determined by that offset; object 3 is the xref-stream entry.
        for (offset, expected) in [(255, 1), (256, 2), (65_535, 2), (65_536, 3)] {
            let input = entries(&[(1, offset), (2, offset), (3, offset)]);
            let plan = XrefStreamPlan::build(&input, 4, ObjectId::new(1, 0), 3, &options)
                .expect("bounded plan");
            assert_eq!(plan.widths(), [1, expected, 0], "offset {offset}");
            assert_eq!(plan.row_width(), 1 + usize::from(expected));
            assert!(plan.validate().is_ok());
        }
        // An offset that would need the full eight-byte field is hostile.
        let huge = entries(&[(1, u64::MAX), (2, 4), (3, 4)]);
        assert!(matches!(
            XrefStreamPlan::build(&huge, 4, ObjectId::new(1, 0), 3, &options),
            Err(XrefStreamError::OffsetTooLarge { .. })
                | Err(XrefStreamError::InvalidWidths { .. })
        ));
    }

    #[test]
    fn payload_round_trips_with_and_without_flate() {
        for compress in [false, true] {
            let options = XrefStreamOptions::default().with_compress(compress);
            let input = entries(&[(1, 9), (3, 100), (4, 250), (5, 251)]);
            let plan =
                XrefStreamPlan::build(&input, 6, ObjectId::new(1, 0), 5, &options).expect("plan");
            let encoded = plan.encode().expect("encoded payload");
            let decoded = plan.decode_payload(&encoded).expect("decoded payload");
            assert_eq!(decoded.len(), plan.decoded_payload_len());
            assert_eq!(decoded[0], 0);
            assert_eq!(decoded[plan.row_width()], 1);
            let mut corrupt = encoded.clone();
            if let Some(last) = corrupt.last_mut() {
                *last ^= 0xff;
            }
            assert!(plan.decode_payload(&corrupt).is_err());
        }
    }

    #[test]
    fn document_save_with_xref_stream_round_trips_structure_and_text() {
        let doc = Document::from_bytes(fixture()).expect("valid fixture");
        let saved = doc
            .save_with_xref_stream(&XrefStreamOptions::default())
            .expect("xref-stream save");
        let reparsed = Document::from_bytes(saved.bytes.clone()).expect("saved bytes parse");
        assert_eq!(reparsed.page_count().expect("page count"), 1);
        assert_eq!(reparsed.page_text(0).expect("page text"), "Hello, PDFXI!");
        let report = reparsed.xref_report().expect("xref report");
        assert_eq!(report.sections.len(), 1);
        assert_eq!(report.sections[0].kind, XrefSectionKind::Stream);
        assert!(report.uses_xref_stream);
        assert_eq!(saved.plan.size(), report.sections[0].entries as u32);
        assert!(saved.plan.validate().is_ok());
    }

    #[test]
    fn save_is_byte_deterministic_and_uncompressed_output_parses() {
        let doc = Document::from_bytes(fixture()).expect("valid fixture");
        let options = XrefStreamOptions::default().with_compress(false);
        let first = doc.save_with_xref_stream(&options).expect("first save");
        let second = doc.save_with_xref_stream(&options).expect("second save");
        assert_eq!(first.bytes, second.bytes);
        assert!(!first.plan.compress());
        let reparsed = Document::from_bytes(first.bytes).expect("uncompressed parses");
        assert_eq!(reparsed.page_text(0).expect("text"), "Hello, PDFXI!");
    }

    #[test]
    fn sparse_objects_are_written_with_free_gaps_and_valid_index() {
        let mut doc = Document::from_bytes(fixture()).expect("valid fixture");
        let id = ObjectId::new(42, 0);
        doc.set_object(id, Object::Integer(7))
            .expect("stage object");
        let saved = doc
            .save_with_xref_stream(&XrefStreamOptions::default())
            .expect("sparse save");
        assert!(
            saved
                .plan
                .entries()
                .iter()
                .any(|entry| entry.number == 42 && entry.kind == XrefEntryKind::InUse)
        );
        assert!(
            saved
                .plan
                .entries()
                .iter()
                .any(|entry| (5..42).contains(&entry.number) && entry.kind == XrefEntryKind::Free)
        );
        let reparsed = Document::from_bytes(saved.bytes).expect("sparse parses");
        assert_eq!(reparsed.page_text(0).expect("text"), "Hello, PDFXI!");
        assert_eq!(
            reparsed.object(id).expect("staged object"),
            Object::Integer(7)
        );
    }

    #[test]
    fn xref_stream_source_can_be_rewritten_as_a_general_stream() {
        let doc = Document::from_bytes(fixture()).expect("valid fixture");
        let packed = doc
            .save_with_object_streams(&crate::ObjectStreamOptions::default())
            .expect("object-stream source");
        let source = Document::from_bytes(packed).expect("packed source parses");
        let saved = source
            .save_with_xref_stream(&XrefStreamOptions::default())
            .expect("rewrite source");
        let reparsed = Document::from_bytes(saved.bytes).expect("rewrite parses");
        assert_eq!(reparsed.page_text(0).expect("text"), "Hello, PDFXI!");
        assert_eq!(
            reparsed.xref_report().expect("report").sections[0].kind,
            XrefSectionKind::Stream
        );
    }

    #[test]
    fn encrypted_input_is_declined_with_a_typed_error() {
        let mut pdf = fixture();
        let marker = b"/Root 1 0 R";
        if let Some(position) = pdf.windows(marker.len()).position(|w| w == marker) {
            let replacement = b"/Root 1 0 R /Encrypt 9 0 R";
            pdf.splice(
                position..position + marker.len(),
                replacement.iter().copied(),
            );
        }
        let doc = Document::from_bytes(pdf).expect("encrypted trailer fixture");
        assert!(matches!(
            doc.save_with_xref_stream(&XrefStreamOptions::default()),
            Err(Error::UnsupportedState(_))
        ));
    }

    #[test]
    fn malformed_header_is_a_typed_error() {
        let dict = Dictionary::new();
        let objects = [(ObjectId::new(1, 0), Object::Dictionary(Dictionary::new()))];
        let error = write_xref_stream_document(
            &objects,
            ObjectId::new(1, 0),
            &dict,
            2,
            b"not a pdf header",
            &XrefStreamOptions::default(),
        )
        .expect_err("bad header");
        assert!(matches!(
            error,
            Error::XrefStreamPlan(XrefStreamError::InvalidHeader)
        ));
    }
}
