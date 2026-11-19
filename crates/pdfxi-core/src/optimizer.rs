//! Document optimization: object-graph compaction, stream compression and
//! deterministic reporting.
//!
//! This module turns a loaded [`Document`] into a fresh, standalone PDF byte
//! stream that is semantically equivalent to the source but smaller. The
//! pipeline is deliberately explicit and ordered so that every transform is
//! observable through [`OptimizationReport`]:
//!
//! 1. **Load** every in-use indirect object into the typed model, overlaying any
//!    copy-on-write changes staged with [`Document::set_object`].
//! 2. **Strip** optional entries (metadata, thumbnails, piece info) when the
//!    caller opts in. Unknown keys are preserved otherwise.
//! 3. **Compress** unfiltered streams with the bundled Flate encoder when the
//!    result is strictly smaller than the input. JPEG and other already-encoded
//!    streams are passed through untouched.
//! 4. **Prune** unreachable objects, starting from every reference in the
//!    trailer (which includes `/Root`).
//! 5. **Deduplicate** byte-identical objects, excluding structural page-tree
//!    nodes so that a repeated blank page is never collapsed into one kid.
//! 6. **Renumber** survivors densely from 1, rewriting every indirect reference
//!    (including the trailer's `/Root`) along the way.
//! 7. **Serialize** a single classic xref table, trailer, `startxref` and EOF
//!    with byte-accurate offsets.
//!
//! # Hostile input and cancellation
//!
//! Every graph walk is depth-bounded and cycle-safe. Encrypted documents and
//! xref-stream inputs are declined through the document's save gate before any
//! object is rewritten. A [`CancellationToken`] is checked between objects in
//! each pass; a cancelled run returns [`Error::Cancelled`] and no partial bytes
//! are produced. The module contains no `unsafe`, no panics on untrusted input,
//! and no external dependencies.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::object::{Dictionary, Object, ObjectId};
use crate::parser::load_object;
use crate::writer::{serialize_indirect, serialize_object, write_trailer, write_xref_section};
use crate::{Document, Error, Filter, Result};

/// Maximum depth permitted while walking or rewriting the object graph.
const MAX_GRAPH_DEPTH: usize = 64;

/// Extra bytes the `/Filter /FlateDecode` dictionary key costs.
///
/// A stream is only compressed when it saves at least this many bytes, which
/// enforces the "never grow a stream" policy including dictionary overhead.
const COMPRESSION_MIN_SAVINGS: usize = 24;

/// Dictionary `/Type` values whose identity must never be merged by dedup.
const STRUCTURAL_TYPES: [&str; 3] = ["Catalog", "Pages", "Page"];

/// A cooperatively checked cancellation flag.
///
/// Clone cheaply shares the same flag: one handle can call [`Self::cancel`]
/// while the worker periodically calls [`Self::is_cancelled`]. No threads are
/// spawned and no locks are taken beyond a single relaxed atomic load.
#[derive(Debug, Clone, Default)]
pub struct CancellationToken {
    flag: Arc<AtomicBool>,
}

impl CancellationToken {
    /// Create a fresh, uncancelled token.
    pub fn new() -> Self {
        Self::default()
    }

    /// Request cancellation of any operation holding a clone of this token.
    pub fn cancel(&self) {
        self.flag.store(true, Ordering::Relaxed);
    }

    /// Return `true` once [`Self::cancel`] has been called.
    pub fn is_cancelled(&self) -> bool {
        self.flag.load(Ordering::Relaxed)
    }

    /// Return `true` when no cancellation has been requested.
    pub fn is_active(&self) -> bool {
        !self.is_cancelled()
    }
}

/// Options controlling which optimizations are applied.
///
/// The default enables the lossless, semantics-preserving transforms only:
/// deduplication, unreachable removal, renumbering and beneficial stream
/// compression. Metadata and optional-entry removal are destructive and are
/// therefore opt-in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OptimizeOptions {
    /// Merge byte-identical non-structural objects.
    pub deduplicate_objects: bool,
    /// Drop objects that cannot be reached from the trailer.
    pub remove_unreachable: bool,
    /// Renumber surviving objects densely from 1 and rewrite references.
    pub renumber_objects: bool,
    /// Flate-compress unfiltered streams when strictly beneficial.
    pub compress_streams: bool,
    /// Remove the trailer `/Info` entry and the catalog `/Metadata` stream.
    pub remove_metadata: bool,
    /// Remove optional `/Thumb` and `/PieceInfo` dictionary entries.
    pub remove_optional_entries: bool,
}

impl Default for OptimizeOptions {
    fn default() -> Self {
        Self {
            deduplicate_objects: true,
            remove_unreachable: true,
            renumber_objects: true,
            compress_streams: true,
            remove_metadata: false,
            remove_optional_entries: false,
        }
    }
}

/// A deterministic account of one optimization run.
///
/// Every counter is derived from ordered maps and every warning is sorted and
/// deduplicated, so optimizing the same input twice yields an identical report.
/// This makes the report suitable for regression tests and UI progress panels.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OptimizationReport {
    /// Number of objects before any transform (including staged changes).
    pub objects_before: usize,
    /// Number of objects written to the output.
    pub objects_after: usize,
    /// Number of streams successfully Flate-compressed.
    pub streams_compressed: usize,
    /// Number of already-encoded image streams passed through unchanged.
    pub images_passed_through: usize,
    /// Number of objects merged into an identical counterpart.
    pub objects_deduplicated: usize,
    /// Number of unreachable objects removed.
    pub objects_removed: usize,
    /// Size of the source file in bytes.
    pub bytes_before: usize,
    /// Size of the optimized output in bytes.
    pub bytes_after: usize,
    /// Sorted, deduplicated notes about skipped or transformed items.
    pub warnings: Vec<String>,
}

/// The bytes and report produced by [`Document::optimize`].
#[derive(Debug, Clone)]
pub struct OptimizationResult {
    /// The optimized, standalone PDF byte stream.
    pub bytes: Vec<u8>,
    /// The deterministic report for this run.
    pub report: OptimizationReport,
}

/// Optimize `document` according to `options`, honoring `cancel`.
///
/// This is the implementation behind [`Document::optimize`]. It never mutates
/// the source document; the returned bytes form a fresh classic-table PDF whose
/// references, page order and unknown keys are preserved.
///
/// # Errors
///
/// Returns [`Error::UnsupportedState`] for encrypted or xref-stream inputs,
/// [`Error::Cancelled`] when the token is signalled, and structural or
/// serialization errors for malformed documents.
pub(crate) fn optimize(
    document: &Document,
    options: &OptimizeOptions,
    cancel: &CancellationToken,
) -> Result<OptimizationResult> {
    let structure = document.supported_structure()?;
    let root_number = structure.root_id.number;

    // 1. Load every in-use object, then overlay staged changes.
    let mut objects: BTreeMap<u32, Object> = BTreeMap::new();
    for (number, _offset) in structure.xref.in_use_entries() {
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let object = load_object(document.bytes(), &structure.xref, ObjectId::new(number, 0))?;
        objects.insert(number, object);
    }
    for (id, object) in &document.changes {
        if id.generation == 0 {
            objects.insert(id.number, object.clone());
        }
    }
    let objects_before = objects.len();
    let bytes_before = document.len();
    let mut report = OptimizationReport {
        objects_before,
        objects_after: objects_before,
        streams_compressed: 0,
        images_passed_through: 0,
        objects_deduplicated: 0,
        objects_removed: 0,
        bytes_before,
        bytes_after: bytes_before,
        warnings: Vec::new(),
    };

    let mut trailer = structure.xref.trailer();
    for (key, value) in &document.trailer_entries {
        trailer.insert(key.clone(), value.clone());
    }

    // 2. Destructive, opt-in entry removal.
    if options.remove_metadata {
        if trailer.remove("Info").is_some() {
            report
                .warnings
                .push("trailer /Info removed (metadata option)".to_owned());
        }
        if let Some(Object::Dictionary(catalog)) = objects.get_mut(&root_number)
            && catalog.remove("Metadata").is_some()
        {
            report
                .warnings
                .push("catalog /Metadata removed (metadata option)".to_owned());
        }
    }
    if options.remove_optional_entries {
        let mut removed = 0usize;
        for object in objects.values_mut() {
            if cancel.is_cancelled() {
                return Err(Error::Cancelled);
            }
            removed += strip_entries(object, &["Thumb", "PieceInfo"], 0)?;
        }
        if removed > 0 {
            report.warnings.push(format!(
                "removed {removed} optional /Thumb or /PieceInfo entries"
            ));
        }
    }

    // 3. Stream compression, with strict no-growth and image passthrough.
    for (number, object) in objects.iter_mut() {
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let Object::Stream { dictionary, data } = object else {
            continue;
        };
        let filters = crate::filter::filters_from_dictionary(dictionary)?;
        if filters.is_empty() {
            if options.compress_streams {
                let compressed = flate::encode(data);
                if compressed.len().saturating_add(COMPRESSION_MIN_SAVINGS) < data.len() {
                    *data = compressed;
                    dictionary.insert("Filter".to_owned(), Object::Name("FlateDecode".to_owned()));
                    report.streams_compressed += 1;
                } else {
                    report.warnings.push(format!(
                        "object {number}: stream not compressed (no-growth policy)"
                    ));
                }
            }
            continue;
        }
        let is_image = filters.iter().any(|filter| {
            matches!(
                filter,
                Filter::DctDecode
                    | Filter::JpxDecode
                    | Filter::Jbig2Decode
                    | Filter::CcittFaxDecode
            )
        });
        if is_image {
            report.images_passed_through += 1;
            report.warnings.push(format!(
                "object {number}: image stream passed through unchanged ({} filter)",
                filters[0]
            ));
        } else if let Some(Filter::FlateDecode(_)) = filters.first() {
            report.warnings.push(format!(
                "object {number}: stream already Flate-compressed; left unchanged"
            ));
        } else {
            report.warnings.push(format!(
                "object {number}: stream uses {} filter; left unchanged",
                filters[0]
            ));
        }
    }

    // 4. Reachability from the trailer (which includes /Root and /Info).
    if options.remove_unreachable {
        let reachable = reachable_objects(&objects, &trailer, root_number, cancel)?;
        let removed: Vec<u32> = objects
            .keys()
            .copied()
            .filter(|number| !reachable.contains(number))
            .collect();
        for number in &removed {
            objects.remove(number);
            if cancel.is_cancelled() {
                return Err(Error::Cancelled);
            }
        }
        report.objects_removed = removed.len();
        if !removed.is_empty() {
            report
                .warnings
                .push(format!("removed {} unreachable object(s)", removed.len()));
        }
    }

    // 5. Deduplicate byte-identical objects, then apply the canonical map.
    if options.deduplicate_objects {
        let dedup_map = deduplicate(&mut objects, root_number, &mut report, cancel)?;
        if !dedup_map.is_empty() {
            rewrite_dictionary(&mut trailer, &dedup_map);
        }
    }

    // 6. Renumber densely and rewrite all references again.
    let mut renumber_map = BTreeMap::new();
    if options.renumber_objects {
        renumber_map = renumber(&mut objects, &mut trailer, cancel)?;
    }

    // The root follows both maps; structural objects are never deduplicated so
    // the dedup step leaves it unchanged.
    let final_root = renumber_map
        .get(&root_number)
        .copied()
        .unwrap_or(root_number);
    sanitize_trailer(&mut trailer);
    trailer.insert(
        "Root".to_owned(),
        Object::Reference(ObjectId::new(final_root, 0)),
    );
    let highest =
        objects.keys().copied().max().ok_or_else(|| {
            Error::OptimizationFailed("optimization produced no objects".to_owned())
        })?;
    let size = highest
        .checked_add(1)
        .ok_or_else(|| Error::OptimizationFailed("trailer /Size overflow".to_owned()))?;
    trailer.insert("Size".to_owned(), Object::Integer(i64::from(size)));

    // 7. Serialize one classic xref section with byte-accurate offsets.
    let mut out = Vec::with_capacity(bytes_before);
    out.extend_from_slice(format!("%PDF-{}\n%PDFXI\n", document.version()).as_bytes());
    let mut offsets = BTreeMap::new();
    for (number, object) in &objects {
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        offsets.insert(*number, out.len());
        serialize_indirect(ObjectId::new(*number, 0), object, &mut out)?;
        out.push(b'\n');
    }
    let xref_offset = out.len();
    write_xref_section(&mut out, &offsets, true);
    write_trailer(&mut out, &trailer, xref_offset)?;

    report.objects_after = objects.len();
    report.bytes_after = out.len();
    report.warnings.sort();
    report.warnings.dedup();
    Ok(OptimizationResult { bytes: out, report })
}

/// Remove `keys` from every dictionary reachable inside `object`.
///
/// Stream payload bytes are never inspected; only the dictionary values and
/// nested arrays/dictionaries are rewritten.
fn strip_entries(object: &mut Object, keys: &[&str], depth: usize) -> Result<usize> {
    if depth > MAX_GRAPH_DEPTH {
        return Err(Error::NestingTooDeep(MAX_GRAPH_DEPTH));
    }
    let mut removed = 0usize;
    match object {
        Object::Dictionary(dictionary) | Object::Stream { dictionary, .. } => {
            for key in keys {
                if dictionary.remove(*key).is_some() {
                    removed += 1;
                }
            }
            for value in dictionary.values_mut() {
                removed += strip_entries(value, keys, depth + 1)?;
            }
        }
        Object::Array(items) => {
            for item in items.iter_mut() {
                removed += strip_entries(item, keys, depth + 1)?;
            }
        }
        _ => {}
    }
    Ok(removed)
}

/// Collect all indirect references contained in `object`.
fn collect_references(object: &Object, out: &mut BTreeSet<u32>, depth: usize) -> Result<()> {
    if depth > MAX_GRAPH_DEPTH {
        return Err(Error::NestingTooDeep(MAX_GRAPH_DEPTH));
    }
    match object {
        Object::Reference(id) => {
            out.insert(id.number);
        }
        Object::Array(items) => {
            for item in items {
                collect_references(item, out, depth + 1)?;
            }
        }
        Object::Dictionary(dictionary) | Object::Stream { dictionary, .. } => {
            for value in dictionary.values() {
                collect_references(value, out, depth + 1)?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// Return the set of object numbers reachable from the trailer and catalog.
fn reachable_objects(
    objects: &BTreeMap<u32, Object>,
    trailer: &Dictionary,
    root_number: u32,
    cancel: &CancellationToken,
) -> Result<BTreeSet<u32>> {
    let mut seeds = BTreeSet::new();
    for value in trailer.values() {
        collect_references(value, &mut seeds, 0)?;
    }
    seeds.insert(root_number);

    let mut reachable = BTreeSet::new();
    let mut stack: Vec<u32> = seeds.into_iter().collect();
    while let Some(number) = stack.pop() {
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        if !reachable.insert(number) {
            continue;
        }
        let Some(object) = objects.get(&number) else {
            continue;
        };
        let mut references = BTreeSet::new();
        collect_references(object, &mut references, 0)?;
        for reference in references {
            if objects.contains_key(&reference) {
                stack.push(reference);
            }
        }
    }
    Ok(reachable)
}

/// Return `true` when an object must keep its identity for page-tree integrity.
fn is_structural(object: &Object) -> bool {
    object
        .as_dictionary()
        .and_then(|dictionary| dictionary.get("Type"))
        .and_then(Object::as_name)
        .is_some_and(|name| STRUCTURAL_TYPES.contains(&name))
}

/// Replace each indirect reference's object number using `map`.
///
/// Numbers absent from `map` are left unchanged so originally dangling
/// references keep their value instead of silently becoming `null`.
fn rewrite_references(object: &mut Object, map: &BTreeMap<u32, u32>, depth: usize) -> Result<()> {
    if depth > MAX_GRAPH_DEPTH {
        return Err(Error::NestingTooDeep(MAX_GRAPH_DEPTH));
    }
    match object {
        Object::Reference(id) => {
            if let Some(number) = map.get(&id.number) {
                id.number = *number;
                id.generation = 0;
            }
        }
        Object::Array(items) => {
            for item in items.iter_mut() {
                rewrite_references(item, map, depth + 1)?;
            }
        }
        Object::Dictionary(dictionary) | Object::Stream { dictionary, .. } => {
            rewrite_dictionary(dictionary, map)
        }
        _ => {}
    }
    Ok(())
}

/// Apply `map` to every value of `dictionary` (one level of indirection).
fn rewrite_dictionary(dictionary: &mut Dictionary, map: &BTreeMap<u32, u32>) {
    for value in dictionary.values_mut() {
        apply_reference_map(value, map);
    }
}

/// Shallow reference rewrite used for dictionaries and the trailer.
fn apply_reference_map(object: &mut Object, map: &BTreeMap<u32, u32>) {
    match object {
        Object::Reference(id) => {
            if let Some(number) = map.get(&id.number) {
                id.number = *number;
                id.generation = 0;
            }
        }
        Object::Array(items) => {
            for item in items {
                apply_reference_map(item, map);
            }
        }
        Object::Dictionary(dictionary) => {
            for value in dictionary.values_mut() {
                apply_reference_map(value, map);
            }
        }
        _ => {}
    }
}

/// Merge byte-identical objects and return the old-to-canonical number map.
fn deduplicate(
    objects: &mut BTreeMap<u32, Object>,
    root_number: u32,
    report: &mut OptimizationReport,
    cancel: &CancellationToken,
) -> Result<BTreeMap<u32, u32>> {
    let mut canonical: BTreeMap<Vec<u8>, u32> = BTreeMap::new();
    let mut map = BTreeMap::new();
    let numbers: Vec<u32> = objects.keys().copied().collect();
    for number in numbers {
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let Some(object) = objects.get(&number) else {
            continue;
        };
        if number == root_number || is_structural(object) {
            map.insert(number, number);
            continue;
        }
        let mut key = Vec::new();
        serialize_object(object, &mut key)?;
        match canonical.entry(key) {
            std::collections::btree_map::Entry::Vacant(slot) => {
                slot.insert(number);
                map.insert(number, number);
            }
            std::collections::btree_map::Entry::Occupied(slot) => {
                let target = *slot.get();
                map.insert(number, target);
                report.objects_deduplicated += 1;
                report.warnings.push(format!(
                    "object {number}: deduplicated into object {target}"
                ));
            }
        }
    }

    for object in objects.values_mut() {
        rewrite_references(object, &map, 0)?;
    }
    let duplicates: Vec<u32> = map
        .iter()
        .filter(|(old, canonical_number)| *old != *canonical_number)
        .map(|(old, _)| *old)
        .collect();
    for number in duplicates {
        objects.remove(&number);
    }
    Ok(map)
}

/// Renumber survivors densely from 1 and rewrite all references.
fn renumber(
    objects: &mut BTreeMap<u32, Object>,
    trailer: &mut Dictionary,
    cancel: &CancellationToken,
) -> Result<BTreeMap<u32, u32>> {
    let mut map = BTreeMap::new();
    let mut next = 1u32;
    for number in objects.keys().copied() {
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        map.insert(number, next);
        next = next
            .checked_add(1)
            .ok_or_else(|| Error::OptimizationFailed("too many objects to renumber".to_owned()))?;
    }

    for object in objects.values_mut() {
        rewrite_references(object, &map, 0)?;
    }
    for value in trailer.values_mut() {
        apply_reference_map(value, &map);
    }
    let mut renamed = BTreeMap::new();
    for (old_number, object) in std::mem::take(objects) {
        let new_number = map.get(&old_number).copied().ok_or_else(|| {
            Error::OptimizationFailed("object renumber map is inconsistent".to_owned())
        })?;
        renamed.insert(new_number, object);
    }
    *objects = renamed;
    Ok(map)
}

/// Remove revision-specific and xref-stream-only keys from the output trailer.
fn sanitize_trailer(trailer: &mut Dictionary) {
    for key in [
        "Size", "Root", "Prev", "Encrypt", "XRefStm", "Length", "Filter", "W", "Index", "Type",
    ] {
        trailer.remove(key);
    }
}

/// A small, self-contained DEFLATE/zlib encoder used by stream compression.
///
/// The crate already ships a decoder ([`crate::filter::decode_stream`]) but no
/// encoder, so this module emits RFC 1951 fixed-Huffman blocks with greedy LZ77
/// matching and an RFC 1950 zlib wrapper. It is intentionally modest: it only
/// needs to compress well enough to make repetitive streams smaller, and the
/// caller applies a strict no-growth check to every result.
mod flate {
    /// Fixed-tree length codes (RFC 1951 table 4).
    const LENGTH_BASE: [u16; 29] = [
        3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115,
        131, 163, 195, 227, 258,
    ];
    /// Extra bits for the fixed-tree length codes.
    const LENGTH_EXTRA: [u8; 29] = [
        0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
    ];
    /// Fixed-tree distance codes (RFC 1951 table 5).
    const DISTANCE_BASE: [u16; 30] = [
        1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
        2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
    ];
    /// Extra bits for the fixed-tree distance codes.
    const DISTANCE_EXTRA: [u8; 30] = [
        0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12,
        13, 13,
    ];

    const MIN_MATCH: usize = 3;
    const MAX_MATCH: usize = 258;
    const MAX_DISTANCE: usize = 32_768;
    const HASH_BITS: u8 = 15;
    const HASH_SIZE: usize = 1 << HASH_BITS;
    const MAX_CHAIN: usize = 128;

    /// One LZ77 token: a literal byte or a back-reference.
    enum Token {
        /// A single literal byte.
        Literal(u8),
        /// A match of `len` bytes starting `dist` bytes earlier.
        Match {
            /// Match length in bytes (3..=258).
            len: usize,
            /// Back-reference distance in bytes (1..=32768).
            dist: usize,
        },
    }

    /// Accumulate bits least-significant-bit first into bytes, as DEFLATE
    /// requires for the byte-level packing of both codes and extra bits.
    struct BitWriter {
        out: Vec<u8>,
        current: u8,
        bits: u8,
    }

    impl BitWriter {
        fn new() -> Self {
            Self {
                out: Vec::new(),
                current: 0,
                bits: 0,
            }
        }

        fn write_bit(&mut self, bit: u8) {
            self.current |= (bit & 1) << self.bits;
            self.bits += 1;
            if self.bits == 8 {
                self.out.push(self.current);
                self.current = 0;
                self.bits = 0;
            }
        }

        /// Write the low `count` bits of `value`, least-significant bit first.
        fn write_extra(&mut self, value: u32, count: u8) {
            for index in 0..count {
                self.write_bit(((value >> index) & 1) as u8);
            }
        }

        /// Write a canonical Huffman code most-significant bit first.
        fn write_code(&mut self, code: u16, len: u8) {
            for index in (0..len).rev() {
                self.write_bit(((code >> index) & 1) as u8);
            }
        }

        fn finish(mut self) -> Vec<u8> {
            if self.bits > 0 {
                self.out.push(self.current);
            }
            self.out
        }
    }

    /// Derive canonical Huffman codes from one code length per symbol.
    fn canonical<const N: usize>(lengths: [u8; N]) -> ([u16; N], [u8; N]) {
        let mut count = [0u16; 16];
        for &length in &lengths {
            if length > 0 {
                count[usize::from(length)] += 1;
            }
        }
        let mut next = [0u16; 16];
        let mut code = 0u16;
        for bits in 1..=15 {
            code = (code + count[bits - 1]) << 1;
            next[bits] = code;
        }
        let mut codes = [0u16; N];
        for symbol in 0..N {
            let length = lengths[symbol];
            if length > 0 {
                codes[symbol] = next[usize::from(length)];
                next[usize::from(length)] += 1;
            }
        }
        (codes, lengths)
    }

    fn fixed_literal_table() -> ([u16; 288], [u8; 288]) {
        let mut lengths = [0u8; 288];
        for symbol in &mut lengths[..=143] {
            *symbol = 8;
        }
        for symbol in &mut lengths[144..=255] {
            *symbol = 9;
        }
        for symbol in &mut lengths[256..=279] {
            *symbol = 7;
        }
        for symbol in &mut lengths[280..=287] {
            *symbol = 8;
        }
        canonical(lengths)
    }

    fn fixed_distance_table() -> ([u16; 30], [u8; 30]) {
        canonical([5u8; 30])
    }

    /// Hash the three-byte sequence starting at `index`.
    fn hash3(data: &[u8], index: usize) -> usize {
        let value = (u32::from(data[index]) << 16)
            | (u32::from(data[index + 1]) << 8)
            | u32::from(data[index + 2]);
        value.wrapping_mul(2_654_435_761) as usize & (HASH_SIZE - 1)
    }

    /// Greedily parse `data` into literals and back-references.
    fn tokenize(data: &[u8]) -> Vec<Token> {
        let len = data.len();
        let mut tokens = Vec::new();
        if len < MIN_MATCH {
            tokens.extend(data.iter().copied().map(Token::Literal));
            return tokens;
        }
        let mut head = vec![0u32; HASH_SIZE];
        let mut previous = vec![0u32; len];
        let mut index = 0usize;
        while index < len {
            let mut best_len = 0usize;
            let mut best_dist = 0usize;
            if index + MIN_MATCH <= len {
                let hash = hash3(data, index);
                let mut candidate = head[hash];
                let mut chain = 0usize;
                while candidate != 0 && chain < MAX_CHAIN {
                    let position = (candidate - 1) as usize;
                    let distance = index - position;
                    if distance > MAX_DISTANCE {
                        break;
                    }
                    let max_len = (len - index).min(MAX_MATCH);
                    let mut matched = 0usize;
                    while matched < max_len && data[position + matched] == data[index + matched] {
                        matched += 1;
                    }
                    if matched > best_len {
                        best_len = matched;
                        best_dist = distance;
                        if matched >= max_len {
                            break;
                        }
                    }
                    candidate = previous[position];
                    chain += 1;
                }
            }

            if best_len >= MIN_MATCH {
                tokens.push(Token::Match {
                    len: best_len,
                    dist: best_dist,
                });
                let end = index + best_len;
                let mut position = index;
                while position < end && position + MIN_MATCH <= len {
                    let hash = hash3(data, position);
                    previous[position] = head[hash];
                    head[hash] = (position + 1) as u32;
                    position += 1;
                }
                index = end;
            } else {
                tokens.push(Token::Literal(data[index]));
                if index + MIN_MATCH <= len {
                    let hash = hash3(data, index);
                    previous[index] = head[hash];
                    head[hash] = (index + 1) as u32;
                }
                index += 1;
            }
        }
        tokens
    }

    /// Map a match length to its RFC 1951 length code and extra bits.
    fn length_code(length: usize) -> (usize, u8, u32) {
        let mut index = 0usize;
        for (candidate, base) in LENGTH_BASE.iter().enumerate() {
            if usize::from(*base) <= length {
                index = candidate;
            } else {
                break;
            }
        }
        (
            index,
            LENGTH_EXTRA[index],
            (length - usize::from(LENGTH_BASE[index])) as u32,
        )
    }

    /// Map a distance to its RFC 1951 distance code and extra bits.
    fn distance_code(distance: usize) -> (usize, u8, u32) {
        let mut index = 0usize;
        for (candidate, base) in DISTANCE_BASE.iter().enumerate() {
            if usize::from(*base) <= distance {
                index = candidate;
            } else {
                break;
            }
        }
        (
            index,
            DISTANCE_EXTRA[index],
            (distance - usize::from(DISTANCE_BASE[index])) as u32,
        )
    }

    /// Adler-32 checksum required by the zlib wrapper (RFC 1950 section 2).
    fn adler32(data: &[u8]) -> u32 {
        const MOD: u32 = 65_521;
        let mut a = 1u32;
        let mut b = 0u32;
        for &byte in data {
            a = (a + u32::from(byte)) % MOD;
            b = (b + a) % MOD;
        }
        (b << 16) | a
    }

    /// Encode `data` as a zlib stream using one fixed-Huffman DEFLATE block.
    ///
    /// The result always round-trips through
    /// [`crate::filter::decode_stream`] with default Flate parameters. It may
    /// be larger than `data` for tiny or incompressible inputs; callers enforce
    /// the no-growth policy before using it.
    pub(super) fn encode(data: &[u8]) -> Vec<u8> {
        let (literal_codes, literal_lengths) = fixed_literal_table();
        let (distance_codes, distance_lengths) = fixed_distance_table();
        let mut writer = BitWriter::new();

        // BFINAL = 1, BTYPE = 01 (fixed Huffman), BTYPE bits written LSB first.
        writer.write_bit(1);
        writer.write_bit(1);
        writer.write_bit(0);

        for token in tokenize(data) {
            match token {
                Token::Literal(byte) => {
                    let symbol = usize::from(byte);
                    writer.write_code(literal_codes[symbol], literal_lengths[symbol]);
                }
                Token::Match { len, dist } => {
                    let (code, extra_bits, extra) = length_code(len);
                    let symbol = 257 + code;
                    writer.write_code(literal_codes[symbol], literal_lengths[symbol]);
                    writer.write_extra(extra, extra_bits);

                    let (distance, distance_extra_bits, distance_extra) = distance_code(dist);
                    writer.write_code(distance_codes[distance], distance_lengths[distance]);
                    writer.write_extra(distance_extra, distance_extra_bits);
                }
            }
        }
        writer.write_code(literal_codes[256], literal_lengths[256]);

        let mut out = Vec::with_capacity(data.len() / 2 + 16);
        out.extend_from_slice(&[0x78, 0x9C]);
        out.extend_from_slice(&writer.finish());
        out.extend_from_slice(&adler32(data).to_be_bytes());
        out
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use crate::filter::{Filter, FlateParams, decode_stream};

        fn round_trip(data: &[u8]) -> Vec<u8> {
            let encoded = encode(data);
            let decoded = decode_stream(&encoded, &[Filter::FlateDecode(FlateParams::default())])
                .expect("encoded stream must decode with the crate decoder");
            assert_eq!(decoded, data);
            encoded
        }

        #[test]
        fn encodes_empty_and_short_inputs() {
            let empty = round_trip(b"");
            assert!(empty.len() >= 6);
            round_trip(b"hi");
            round_trip(&[0u8, 1, 2]);
        }

        #[test]
        fn compresses_repetitive_data_below_original_size() {
            let data = b"ABABABAB".repeat(256);
            let encoded = round_trip(&data);
            assert!(
                encoded.len() < data.len(),
                "encoder should shrink repetitive data: {} vs {}",
                encoded.len(),
                data.len()
            );
        }

        #[test]
        fn exercises_long_matches_and_large_distances() {
            let mut data = Vec::new();
            for value in 0..=255u8 {
                data.push(value);
            }
            data.extend_from_slice(&data.clone());
            data.extend_from_slice(&data.clone());
            let encoded = round_trip(&data);
            assert!(
                encoded.len() < data.len(),
                "long matches should compress: {} vs {}",
                encoded.len(),
                data.len()
            );
        }

        #[test]
        fn incompressible_data_still_round_trips() {
            let data: Vec<u8> = (0..64)
                .map(|index| ((index * 37 + 11) % 251) as u8)
                .collect();
            round_trip(&data);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filter::{Filter, FlateParams, decode_stream};

    /// Wrap a dictionary or stream body as an indirect object body.
    fn obj(body: &str) -> Vec<u8> {
        body.as_bytes().to_vec()
    }

    /// Build a stream object body with an exact `/Length`.
    fn stream(data: &[u8]) -> Vec<u8> {
        let mut body = format!("<< /Length {} >>\nstream\n", data.len()).into_bytes();
        body.extend_from_slice(data);
        body.extend_from_slice(b"\nendstream");
        body
    }

    /// Assemble a one-page document whose content stream is `content`.
    fn base_objects(content: &[u8]) -> Vec<(u32, Vec<u8>)> {
        vec![
            (1, obj("<< /Type /Catalog /Pages 2 0 R >>")),
            (2, obj("<< /Type /Pages /Kids [3 0 R] /Count 1 >>")),
            (
                3,
                obj("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R >>"),
            ),
            (4, stream(content)),
        ]
    }

    /// Build a structurally valid PDF from explicit object bodies.
    fn pdf(objects: &[(u32, Vec<u8>)], trailer_extra: &str) -> Vec<u8> {
        let mut out = b"%PDF-1.7\n".to_vec();
        let mut offsets = BTreeMap::new();
        for (number, body) in objects {
            offsets.insert(*number, out.len());
            out.extend_from_slice(format!("{number} 0 obj\n").as_bytes());
            out.extend_from_slice(body);
            out.extend_from_slice(b"\nendobj\n");
        }
        let highest = objects.iter().map(|(number, _)| *number).max().unwrap_or(0);
        let xref = out.len();
        out.extend_from_slice(format!("xref\n0 {}\n", highest + 1).as_bytes());
        out.extend_from_slice(b"0000000000 65535 f \n");
        for number in 1..=highest {
            if let Some(offset) = offsets.get(&number) {
                out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
            } else {
                out.extend_from_slice(b"0000000000 00000 f \n");
            }
        }
        out.extend_from_slice(
            format!(
                "trailer\n<< /Size {} {trailer_extra} >>\nstartxref\n{xref}\n%%EOF\n",
                highest + 1
            )
            .as_bytes(),
        );
        out
    }

    fn optimize(source: Vec<u8>) -> OptimizationResult {
        let document = Document::from_bytes(source).expect("fixture parses");
        document
            .optimize(&OptimizeOptions::default(), &CancellationToken::new())
            .expect("optimization succeeds")
    }

    fn contains(haystack: &[u8], needle: &[u8]) -> bool {
        !needle.is_empty() && haystack.windows(needle.len()).any(|w| w == needle)
    }

    #[test]
    fn removes_unreachable_objects() {
        let mut objects = base_objects(b"BT (Hi) Tj ET");
        objects.push((9, obj("<< /Type /Unused /Payload 12345 >>")));
        let source = pdf(&objects, "/Root 1 0 R");
        let result = optimize(source);
        assert_eq!(result.report.objects_before, 5);
        assert_eq!(result.report.objects_removed, 1);
        assert_eq!(result.report.objects_after, 4);
        assert!(!contains(&result.bytes, b"/Payload 12345"));
        let reparsed = Document::from_bytes(result.bytes).expect("optimized PDF parses");
        assert_eq!(reparsed.page_count().expect("page count"), 1);
        assert_eq!(reparsed.page_text(0).expect("page text"), "Hi");
    }

    #[test]
    fn deduplicates_identical_objects() {
        let mut objects = base_objects(b"BT (Hi) Tj ET");
        objects[0].1 = obj("<< /Type /Catalog /Pages 2 0 R /Foo 5 0 R /Bar 6 0 R >>");
        objects.push((5, stream(b"abc")));
        objects.push((6, stream(b"abc")));
        let source = pdf(&objects, "/Root 1 0 R");
        let result = optimize(source);
        assert_eq!(result.report.objects_deduplicated, 1);
        assert_eq!(result.report.objects_after, 5);
        let reparsed = Document::from_bytes(result.bytes).expect("optimized PDF parses");
        assert_eq!(reparsed.page_count().expect("page count"), 1);
    }

    #[test]
    fn renumbers_sparse_objects_and_rewrites_references() {
        let objects = vec![
            (10, obj("<< /Type /Catalog /Pages 20 0 R >>")),
            (20, obj("<< /Type /Pages /Kids [30 0 R] /Count 1 >>")),
            (
                30,
                obj("<< /Type /Page /Parent 20 0 R /MediaBox [0 0 612 792] /Contents 40 0 R >>"),
            ),
            (40, stream(b"BT (Renumbered) Tj ET")),
        ];
        let source = pdf(&objects, "/Root 10 0 R");
        let result = optimize(source);
        assert_eq!(result.report.objects_after, 4);
        for number in 1..=4 {
            assert!(
                contains(&result.bytes, format!("{number} 0 obj").as_bytes()),
                "object {number} missing"
            );
        }
        assert!(!contains(&result.bytes, b"10 0 obj"));
        assert!(contains(&result.bytes, b"/Root 1 0 R"));
        let reparsed = Document::from_bytes(result.bytes).expect("optimized PDF parses");
        assert_eq!(reparsed.root().expect("root"), ObjectId::new(1, 0));
        assert_eq!(reparsed.page_count().expect("page count"), 1);
        assert_eq!(reparsed.page_text(0).expect("page text"), "Renumbered");
    }

    #[test]
    fn writes_correct_xref_and_startxref() {
        let source = pdfxi_testkit::multipage_pdf(3);
        let result = optimize(source.clone());
        let original = Document::from_bytes(source).expect("source parses");
        let reparsed = Document::from_bytes(result.bytes.clone()).expect("output parses");
        assert_eq!(reparsed.page_count().expect("page count"), 3);
        assert_eq!(
            reparsed.root().expect("root"),
            original.root().expect("root")
        );
        assert!(contains(&result.bytes, b"startxref\n"));
        for index in 0..3 {
            assert_eq!(
                reparsed.page_text(index).expect("page text"),
                original.page_text(index).expect("source page text")
            );
        }
    }

    #[test]
    fn shrinks_compressible_streams() {
        let content = b"BT (Hello) Tj ET\n".repeat(400);
        let source = pdf(&base_objects(&content), "/Root 1 0 R");
        let before = source.len();
        let result = optimize(source);
        assert_eq!(result.report.streams_compressed, 1);
        assert!(
            result.report.bytes_after < before,
            "expected byte reduction: {} -> {}",
            before,
            result.report.bytes_after
        );
        let reparsed = Document::from_bytes(result.bytes).expect("optimized PDF parses");
        assert_eq!(
            reparsed.page_content(0).expect("decoded content"),
            content,
            "Flate stream must decode back to the original bytes"
        );
    }

    #[test]
    fn respects_no_growth_policy() {
        let source = pdf(&base_objects(b"abc"), "/Root 1 0 R");
        let result = optimize(source);
        assert_eq!(result.report.streams_compressed, 0);
        assert!(
            result
                .report
                .warnings
                .iter()
                .any(|w| w.contains("no-growth")),
            "warnings: {:?}",
            result.report.warnings
        );
        assert!(contains(&result.bytes, b"abc"));
    }

    #[test]
    fn passes_jpeg_streams_through() {
        let mut objects = base_objects(b"BT (Hi) Tj ET");
        let jpeg = [
            0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10, 0x4A, 0x46, 0x49, 0x46, 0xFF, 0xD9,
        ];
        objects[3].1 = {
            let mut body = format!(
                "<< /Length {} /Filter /DCTDecode /ColorSpace /DeviceRGB /Width 2 /Height 2 >>\nstream\n",
                jpeg.len()
            )
            .into_bytes();
            body.extend_from_slice(&jpeg);
            body.extend_from_slice(b"\nendstream");
            body
        };
        let source = pdf(&objects, "/Root 1 0 R");
        let result = optimize(source);
        assert_eq!(result.report.images_passed_through, 1);
        assert_eq!(result.report.streams_compressed, 0);
        assert!(
            result
                .report
                .warnings
                .iter()
                .any(|w| w.contains("DCTDecode")),
            "warnings: {:?}",
            result.report.warnings
        );
        assert!(contains(&result.bytes, &jpeg));
        Document::from_bytes(result.bytes).expect("optimized PDF parses");
    }

    #[test]
    fn report_is_deterministic() {
        let mut objects = base_objects(b"abc");
        objects[0].1 = obj("<< /Type /Catalog /Pages 2 0 R /Foo 5 0 R >>");
        objects.push((5, stream(b"shared")));
        objects.push((6, stream(b"shared")));
        objects.push((7, obj("<< /Type /Orphan >>")));
        let source = pdf(&objects, "/Root 1 0 R");
        let first = optimize(source.clone()).report;
        let second = optimize(source).report;
        assert_eq!(first, second);
        assert!(first.warnings.windows(2).all(|pair| pair[0] <= pair[1]));
    }

    #[test]
    fn preserves_document_semantics_and_unknown_keys() {
        let mut objects = base_objects(b"BT (Keep me) Tj ET");
        objects[0].1 = obj("<< /Type /Catalog /Pages 2 0 R /CustomKey /CustomValue >>");
        objects[2].1 = obj(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Custom 42 >>",
        );
        let source = pdf(&objects, "/Root 1 0 R");
        let original = Document::from_bytes(source.clone()).expect("source parses");
        let result = optimize(source);
        assert!(contains(&result.bytes, b"/CustomKey /CustomValue"));
        assert!(contains(&result.bytes, b"/Custom 42"));
        let reparsed = Document::from_bytes(result.bytes).expect("optimized PDF parses");
        assert_eq!(reparsed.page_count().expect("page count"), 1);
        assert_eq!(
            reparsed.page_text(0).expect("page text"),
            original.page_text(0).expect("source page text")
        );
    }

    #[test]
    fn removes_metadata_when_requested() {
        let mut objects = base_objects(b"BT (Meta) Tj ET");
        objects[0].1 = obj("<< /Type /Catalog /Pages 2 0 R /Metadata 7 0 R >>");
        objects.push((6, obj("<< /Producer (TestSuite) /Title (Secret Report) >>")));
        objects.push((7, stream(b"<x:xmpmeta />")));
        let source = pdf(&objects, "/Root 1 0 R /Info 6 0 R");
        let document = Document::from_bytes(source).expect("fixture parses");
        let options = OptimizeOptions {
            remove_metadata: true,
            ..OptimizeOptions::default()
        };
        let result = document
            .optimize(&options, &CancellationToken::new())
            .expect("optimization succeeds");
        assert!(!contains(&result.bytes, b"/Info"));
        assert!(!contains(&result.bytes, b"/Metadata"));
        assert!(!contains(&result.bytes, b"Secret Report"));
        assert!(
            result
                .report
                .warnings
                .iter()
                .any(|w| w.contains("metadata")),
            "warnings: {:?}",
            result.report.warnings
        );
        let reparsed = Document::from_bytes(result.bytes).expect("optimized PDF parses");
        assert_eq!(reparsed.page_count().expect("page count"), 1);
    }

    #[test]
    fn removes_optional_entries_when_requested() {
        let mut objects = base_objects(b"BT (Optional) Tj ET");
        objects[2].1 = obj(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R \
             /Thumb 8 0 R /PieceInfo << /Private 1 >> >>",
        );
        objects.push((8, stream(b"thumbnail-bytes")));
        let source = pdf(&objects, "/Root 1 0 R");
        let document = Document::from_bytes(source).expect("fixture parses");
        let options = OptimizeOptions {
            remove_optional_entries: true,
            ..OptimizeOptions::default()
        };
        let result = document
            .optimize(&options, &CancellationToken::new())
            .expect("optimization succeeds");
        assert!(!contains(&result.bytes, b"/Thumb"));
        assert!(!contains(&result.bytes, b"/PieceInfo"));
        assert!(!contains(&result.bytes, b"thumbnail-bytes"));
        Document::from_bytes(result.bytes).expect("optimized PDF parses");
    }

    #[test]
    fn cancellation_is_honored() {
        let source = pdf(&base_objects(b"BT (Cancel) Tj ET"), "/Root 1 0 R");
        let document = Document::from_bytes(source).expect("fixture parses");
        let token = CancellationToken::new();
        token.cancel();
        assert!(matches!(
            document.optimize(&OptimizeOptions::default(), &token),
            Err(Error::Cancelled)
        ));
    }

    #[test]
    fn incremental_save_input_remains_readable() {
        let source = pdfxi_testkit::minimal_pdf();
        let mut document = Document::from_bytes(source).expect("fixture parses");
        let mut dictionary = Dictionary::new();
        dictionary.insert("Type".to_owned(), Object::Name("Marker".to_owned()));
        document
            .set_object(ObjectId::new(9, 0), Object::Dictionary(dictionary))
            .expect("valid object id");
        let incremental = document.save_incremental().expect("incremental save");
        let reloaded = Document::from_bytes(incremental).expect("incremental PDF parses");
        let result = reloaded
            .optimize(&OptimizeOptions::default(), &CancellationToken::new())
            .expect("optimization succeeds");
        let reparsed = Document::from_bytes(result.bytes).expect("optimized PDF parses");
        assert_eq!(reparsed.page_count().expect("page count"), 1);
    }

    #[test]
    fn already_flate_stream_is_left_unchanged() {
        let encoded = flate::encode(&b"BT (Preset) Tj ET".repeat(20));
        let mut objects = base_objects(&encoded);
        objects[3].1 = {
            let mut body = format!(
                "<< /Length {} /Filter /FlateDecode >>\nstream\n",
                encoded.len()
            )
            .into_bytes();
            body.extend_from_slice(&encoded);
            body.extend_from_slice(b"\nendstream");
            body
        };
        let source = pdf(&objects, "/Root 1 0 R");
        let result = optimize(source);
        assert_eq!(result.report.streams_compressed, 0);
        assert!(
            result
                .report
                .warnings
                .iter()
                .any(|w| w.contains("already Flate")),
            "warnings: {:?}",
            result.report.warnings
        );
        let filters = [Filter::FlateDecode(FlateParams::default())];
        let decoded = decode_stream(&encoded, &filters).expect("fixture decodes");
        assert_eq!(decoded, b"BT (Preset) Tj ET".repeat(20));
    }
}
