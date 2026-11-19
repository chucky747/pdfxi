//! Stream filters with a bounded, pure-Rust FlateDecode implementation.
//!
//! PDF content streams may carry a `/Filter` entry naming one or more decoding
//! steps. This module implements the one filter needed for headless text
//! extraction — `FlateDecode` (zlib-wrapped DEFLATE, RFC 1950/RFC 1951) — and
//! recognizes the other common filters only so that callers receive a clear
//! [`Error::UnsupportedFilter`] instead of undecoded bytes.
//!
//! The DEFLATE decoder is intentionally self-contained: `pdfxi-core` stays
//! UI-free and dependency-light, and untrusted input is handled with explicit
//! bounds checks and an output cap. Two input forms are accepted:
//!
//! - a **zlib stream** (CMF/FLG header with CM = 8, optional Adler-32 trailer);
//! - a **raw DEFLATE stream** (no header), used as a fallback when the first
//!   byte is not a plausible zlib CMF.
//!
//! All three DEFLATE block types are supported: stored, fixed Huffman and
//! dynamic Huffman. Output growth is checked against a limit before every
//! write, so a hostile stream cannot allocate more than the cap. The public
//! [`decode_stream`] uses [`DEFAULT_DECODE_CAP`]; [`decode_stream_with_limit`]
//! lets callers choose a smaller bound (and lets tests exercise the cap).
//!
//! PNG predictors 10–15 are applied after inflation when requested through
//! `/DecodeParms`. Predictor 1 (none) is the default; TIFF predictor 2 and
//! non-byte-aligned component sizes are reported as errors rather than decoded
//! incorrectly.

use crate::object::{Dictionary, Object};
use crate::{Error, Result};

/// Default upper bound on the bytes a single `decode_stream` call may produce.
///
/// The cap is 64 MiB, far above any ordinary content stream but low enough that
/// a decompression bomb cannot exhaust the process. [`decode_stream_with_limit`]
/// exposes the bound as a parameter for callers with tighter budgets.
pub const DEFAULT_DECODE_CAP: usize = 64 * 1024 * 1024;

/// Maximum code length in a DEFLATE Huffman tree (RFC 1951 section 3.2.2).
const MAX_CODE_LENGTH: u8 = 15;
/// Maximum number of literal/length codes in a dynamic block.
const MAX_LITERAL_CODES: usize = 288;
/// Maximum number of distance codes in a dynamic block.
const MAX_DISTANCE_CODES: usize = 32;
/// Number of distance codes defined by RFC 1951 (codes 30 and 31 are reserved).
const VALID_DISTANCE_CODES: u16 = 30;

/// The predictor declared by a stream's `/DecodeParms`.
///
/// The numeric values mirror the PDF `/Predictor` entry (ISO 32000-1,
/// Table 10). PNG predictors operate on rows of bytes; [`FlateParams`]
/// describes how those rows are laid out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i64)]
pub enum Predictor {
    /// Predictor 1: the inflated bytes are returned unchanged.
    None = 1,
    /// Predictor 2: TIFF predictor 2. Recognized but not decoded.
    Tiff = 2,
    /// Predictor 10: PNG `None` filter on every row.
    PngNone = 10,
    /// Predictor 11: PNG `Sub` filter on every row.
    PngSub = 11,
    /// Predictor 12: PNG `Up` filter on every row.
    PngUp = 12,
    /// Predictor 13: PNG `Average` filter on every row.
    PngAverage = 13,
    /// Predictor 14: PNG `Paeth` filter on every row.
    PngPaeth = 14,
    /// Predictor 15: each row is prefixed by its own filter type byte (0–4).
    PngOptimal = 15,
}

impl Predictor {
    /// Map a PDF `/Predictor` integer to a variant.
    ///
    /// Returns `None` for unknown predictor values so callers can produce a
    /// structured error instead of guessing.
    pub fn from_pdf(value: i64) -> Option<Self> {
        match value {
            1 => Some(Self::None),
            2 => Some(Self::Tiff),
            10 => Some(Self::PngNone),
            11 => Some(Self::PngSub),
            12 => Some(Self::PngUp),
            13 => Some(Self::PngAverage),
            14 => Some(Self::PngPaeth),
            15 => Some(Self::PngOptimal),
            _ => None,
        }
    }

    /// Return the per-row PNG filter type when this predictor fixes one.
    ///
    /// [`Predictor::PngOptimal`] returns `None` because each row carries its
    /// own filter byte; non-PNG predictors also return `None`.
    fn fixed_png_filter(self) -> Option<u8> {
        match self {
            Self::PngNone => Some(0),
            Self::PngSub => Some(1),
            Self::PngUp => Some(2),
            Self::PngAverage => Some(3),
            Self::PngPaeth => Some(4),
            Self::None | Self::Tiff | Self::PngOptimal => None,
        }
    }

    /// Return `true` when this predictor needs PNG row reconstruction.
    fn is_png(self) -> bool {
        matches!(
            self,
            Self::PngNone
                | Self::PngSub
                | Self::PngUp
                | Self::PngAverage
                | Self::PngPaeth
                | Self::PngOptimal
        )
    }
}

/// Parameters controlling a single `FlateDecode` step.
///
/// Defaults match the PDF specification: no prediction, one color component,
/// eight bits per component and one column. The predictor fields only affect
/// PNG predictors; other predictors ignore them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FlateParams {
    /// The predictor algorithm to apply after inflation.
    pub predictor: Predictor,
    /// Number of interleaved color components per sample.
    pub colors: u32,
    /// Bits per color component.
    pub bits_per_component: u32,
    /// Number of samples in each predictor row.
    pub columns: u32,
}

impl Default for FlateParams {
    fn default() -> Self {
        Self {
            predictor: Predictor::None,
            colors: 1,
            bits_per_component: 8,
            columns: 1,
        }
    }
}

impl FlateParams {
    /// Construct decode parameters explicitly.
    ///
    /// Use [`FlateParams::default`] for the PDF defaults. Invalid values are
    /// rejected when the stream is decoded, not here, so that a dictionary can
    /// be constructed without immediate validation.
    pub fn new(predictor: Predictor, colors: u32, bits_per_component: u32, columns: u32) -> Self {
        Self {
            predictor,
            colors,
            bits_per_component,
            columns,
        }
    }

    /// Validate the parameters and return the number of bytes per pixel.
    ///
    /// Non-PNG predictors need no layout information and use a one-byte
    /// fallback. PNG predictors require at least eight bits per component and
    /// a byte-aligned component size, plus positive colors and columns.
    fn bytes_per_pixel(&self) -> Result<usize> {
        if self.colors == 0 {
            return Err(Error::InvalidDecodeParameters(
                "Colors must be at least 1".to_owned(),
            ));
        }
        if self.columns == 0 {
            return Err(Error::InvalidDecodeParameters(
                "Columns must be at least 1".to_owned(),
            ));
        }
        let components = usize::try_from(self.colors).map_err(|_| {
            Error::InvalidDecodeParameters("Colors is too large to decode".to_owned())
        })?;
        if !self.predictor.is_png() {
            return Ok(components);
        }
        if self.bits_per_component == 0 || !self.bits_per_component.is_multiple_of(8) {
            return Err(Error::InvalidDecodeParameters(format!(
                "PNG predictors require a byte-aligned BitsPerComponent, got {}",
                self.bits_per_component
            )));
        }
        let bytes_per_component = usize::try_from(self.bits_per_component / 8).map_err(|_| {
            Error::InvalidDecodeParameters("BitsPerComponent is too large to decode".to_owned())
        })?;
        components.checked_mul(bytes_per_component).ok_or_else(|| {
            Error::InvalidDecodeParameters("color layout overflows usize".to_owned())
        })
    }
}

/// A PDF stream filter.
///
/// Only [`Filter::FlateDecode`] is decoded; the remaining variants record the
/// filter name so [`Error::UnsupportedFilter`] can name the exact filter that
/// blocked decoding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Filter {
    /// zlib/DEFLATE compression with optional predictor parameters.
    FlateDecode(FlateParams),
    /// LZW compression. Recognized but not decoded.
    LzwDecode,
    /// JPEG (DCT) compression. Recognized but not decoded.
    DctDecode,
    /// JPEG 2000 (JPX) compression. Recognized but not decoded.
    JpxDecode,
    /// CCITT fax compression. Recognized but not decoded.
    CcittFaxDecode,
    /// JBIG2 compression. Recognized but not decoded.
    Jbig2Decode,
    /// Run-length compression. Recognized but not decoded.
    RunLengthDecode,
    /// ASCII85 encoding. Recognized but not decoded.
    Ascii85Decode,
    /// ASCII hexadecimal encoding. Recognized but not decoded.
    AsciiHexDecode,
    /// The PDF `Crypt` filter. Recognized but not decoded.
    Crypt,
    /// A filter this build does not know by name.
    Unknown(String),
}

impl std::fmt::Display for Filter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            Self::FlateDecode(_) => "FlateDecode",
            Self::LzwDecode => "LZWDecode",
            Self::DctDecode => "DCTDecode",
            Self::JpxDecode => "JPXDecode",
            Self::CcittFaxDecode => "CCITTFaxDecode",
            Self::Jbig2Decode => "JBIG2Decode",
            Self::RunLengthDecode => "RunLengthDecode",
            Self::Ascii85Decode => "ASCII85Decode",
            Self::AsciiHexDecode => "ASCIIHexDecode",
            Self::Crypt => "Crypt",
            Self::Unknown(name) => return f.write_str(name),
        };
        f.write_str(name)
    }
}

/// Decode a chain of stream filters using the default output cap.
///
/// Filters are applied in the order given, matching the order of a PDF
/// `/Filter` array. An empty `filters` slice returns a copy of `data`.
///
/// # Errors
///
/// Returns [`Error::UnsupportedFilter`] (naming the filter) when a step other
/// than `FlateDecode` is requested, [`Error::InvalidZlibHeader`] or
/// [`Error::MalformedDeflate`] for corrupt input,
/// [`Error::DecodeLimitExceeded`] when the cap would be exceeded, and
/// [`Error::InvalidDecodeParameters`] for unusable predictor parameters.
pub fn decode_stream(data: &[u8], filters: &[Filter]) -> Result<Vec<u8>> {
    decode_stream_with_limit(data, filters, DEFAULT_DECODE_CAP)
}

/// Decode a chain of stream filters, refusing to produce more than `limit` bytes.
///
/// The limit is enforced inside the inflater before each write, including
/// back-reference copies, so a malicious stream cannot force a large
/// allocation. Predictor reconstruction never increases the byte count.
///
/// # Errors
///
/// Same failure modes as [`decode_stream`], with [`Error::DecodeLimitExceeded`]
/// reporting the caller-supplied `limit`.
pub fn decode_stream_with_limit(data: &[u8], filters: &[Filter], limit: usize) -> Result<Vec<u8>> {
    let mut current = data.to_vec();
    for filter in filters {
        current = match filter {
            Filter::FlateDecode(params) => {
                let inflated = inflate(&current, limit)?;
                apply_predictor(&inflated, params)?
            }
            unsupported => return Err(Error::UnsupportedFilter(unsupported.to_string())),
        };
        if current.len() > limit {
            return Err(Error::DecodeLimitExceeded { limit });
        }
    }
    Ok(current)
}

/// Build a filter chain from a stream dictionary's `/Filter` and
/// `/DecodeParms` entries.
///
/// A single filter name or an array of names is accepted; `/DecodeParms` may
/// be a single dictionary (applied to the first filter) or a matching array.
/// A missing or null `/Filter` yields an empty chain.
pub(crate) fn filters_from_dictionary(dictionary: &Dictionary) -> Result<Vec<Filter>> {
    let Some(raw_filter) = dictionary.get("Filter") else {
        return Ok(Vec::new());
    };
    let names: Vec<&Object> = match raw_filter {
        Object::Null => return Ok(Vec::new()),
        Object::Name(_) => vec![raw_filter],
        Object::Array(items) => items.iter().collect(),
        _ => {
            return Err(Error::InvalidDecodeParameters(
                "Filter must be a name or an array of names".to_owned(),
            ));
        }
    };

    let parms: Vec<&Object> = match dictionary.get("DecodeParms") {
        None | Some(Object::Null) => Vec::new(),
        Some(Object::Array(items)) => items.iter().collect(),
        Some(other) => vec![other],
    };

    let mut filters = Vec::with_capacity(names.len());
    for (index, name_object) in names.iter().enumerate() {
        let name = name_object.as_name().ok_or_else(|| {
            Error::InvalidDecodeParameters("filter entries must be name objects".to_owned())
        })?;
        let params = parms.get(index).copied();
        let filter = match name {
            "FlateDecode" | "Fl" => Filter::FlateDecode(parse_flate_params(params)?),
            "LZWDecode" | "LZW" => Filter::LzwDecode,
            "DCTDecode" | "DCT" => Filter::DctDecode,
            "JPXDecode" => Filter::JpxDecode,
            "CCITTFaxDecode" | "CCF" => Filter::CcittFaxDecode,
            "JBIG2Decode" => Filter::Jbig2Decode,
            "RunLengthDecode" | "RL" => Filter::RunLengthDecode,
            "ASCII85Decode" | "A85" => Filter::Ascii85Decode,
            "ASCIIHexDecode" | "AHx" => Filter::AsciiHexDecode,
            "Crypt" => Filter::Crypt,
            other => Filter::Unknown(other.to_owned()),
        };
        filters.push(filter);
    }
    Ok(filters)
}

/// Parse the `/DecodeParms` dictionary for a `FlateDecode` step.
fn parse_flate_params(value: Option<&Object>) -> Result<FlateParams> {
    let Some(value) = value else {
        return Ok(FlateParams::default());
    };
    let dictionary = match value {
        Object::Null => return Ok(FlateParams::default()),
        Object::Dictionary(dictionary) => dictionary,
        _ => {
            return Err(Error::InvalidDecodeParameters(
                "DecodeParms entries must be dictionaries".to_owned(),
            ));
        }
    };

    let predictor = match dictionary.get("Predictor") {
        None | Some(Object::Null) => Predictor::None,
        Some(Object::Integer(number)) => Predictor::from_pdf(*number).ok_or_else(|| {
            Error::InvalidDecodeParameters(format!("unsupported Predictor value {number}"))
        })?,
        _ => {
            return Err(Error::InvalidDecodeParameters(
                "Predictor must be an integer".to_owned(),
            ));
        }
    };
    Ok(FlateParams::new(
        predictor,
        read_positive_u32(dictionary, "Colors", 1)?,
        read_positive_u32(dictionary, "BitsPerComponent", 8)?,
        read_positive_u32(dictionary, "Columns", 1)?,
    ))
}

/// Read a non-negative dictionary integer, falling back to `default`.
fn read_positive_u32(dictionary: &Dictionary, key: &str, default: u32) -> Result<u32> {
    match dictionary.get(key) {
        None | Some(Object::Null) => Ok(default),
        Some(Object::Integer(number)) if *number >= 0 => u32::try_from(*number)
            .map_err(|_| Error::InvalidDecodeParameters(format!("{key} is too large to decode"))),
        _ => Err(Error::InvalidDecodeParameters(format!(
            "{key} must be a non-negative integer"
        ))),
    }
}

// ---------------------------------------------------------------------------
// Bit-level reader
// ---------------------------------------------------------------------------

/// A cursor over a DEFLATE bit stream. Bits are consumed least-significant
/// byte first and least-significant bit first, as required by RFC 1951.
struct BitReader<'a> {
    data: &'a [u8],
    byte_pos: usize,
    bit_pos: u8,
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            byte_pos: 0,
            bit_pos: 0,
        }
    }

    fn read_bit(&mut self) -> Result<u32> {
        let byte = *self
            .data
            .get(self.byte_pos)
            .ok_or_else(|| Error::MalformedDeflate("unexpected end of DEFLATE data".to_owned()))?;
        let bit = u32::from((byte >> self.bit_pos) & 1);
        self.bit_pos += 1;
        if self.bit_pos == 8 {
            self.bit_pos = 0;
            self.byte_pos += 1;
        }
        Ok(bit)
    }

    fn read_bits(&mut self, count: u8) -> Result<u32> {
        let mut value = 0u32;
        for index in 0..count {
            value |= self.read_bit()? << index;
        }
        Ok(value)
    }

    /// Discard the remaining bits of the current byte (stored blocks align).
    fn align_to_byte(&mut self) {
        if self.bit_pos != 0 {
            self.bit_pos = 0;
            self.byte_pos += 1;
        }
    }

    /// Read a whole number of bytes once the reader is byte-aligned.
    fn read_bytes(&mut self, count: usize) -> Result<&'a [u8]> {
        debug_assert_eq!(self.bit_pos, 0, "read_bytes requires byte alignment");
        let end = self
            .byte_pos
            .checked_add(count)
            .ok_or_else(|| Error::MalformedDeflate("stored block length overflows".to_owned()))?;
        let slice = self
            .data
            .get(self.byte_pos..end)
            .ok_or_else(|| Error::MalformedDeflate("stored block is truncated".to_owned()))?;
        self.byte_pos = end;
        Ok(slice)
    }
}

// ---------------------------------------------------------------------------
// Huffman decoding
// ---------------------------------------------------------------------------

/// A canonical Huffman decoding table for DEFLATE codes.
struct Huffman {
    /// Number of symbols of each code length (index 1..=15).
    count: [u16; (MAX_CODE_LENGTH + 1) as usize],
    /// Symbols sorted by code length, then by symbol value.
    symbols: Vec<u16>,
}

impl Huffman {
    /// Build a table from one code length per symbol.
    fn new(lengths: &[u8]) -> Result<Self> {
        let mut count = [0u16; (MAX_CODE_LENGTH + 1) as usize];
        for &length in lengths {
            if length > MAX_CODE_LENGTH {
                return Err(Error::MalformedDeflate(
                    "Huffman code length exceeds 15 bits".to_owned(),
                ));
            }
            count[usize::from(length)] += 1;
        }

        // Kraft check: the code space must not be over-subscribed. An
        // incomplete tree (space left over) is tolerated, matching zlib.
        let mut space: i32 = 1;
        for &codes in count.iter().skip(1) {
            space <<= 1;
            space -= i32::from(codes);
            if space < 0 {
                return Err(Error::MalformedDeflate(
                    "over-subscribed Huffman tree".to_owned(),
                ));
            }
        }

        let mut offsets = [0usize; (MAX_CODE_LENGTH + 1) as usize];
        let mut next_offset = 0usize;
        for length in 1..=usize::from(MAX_CODE_LENGTH) {
            offsets[length] = next_offset;
            next_offset += usize::from(count[length]);
        }

        let mut symbols = vec![0u16; lengths.len()];
        for (symbol, &length) in lengths.iter().enumerate() {
            if length != 0 {
                let slot = offsets[usize::from(length)];
                symbols[slot] = u16::try_from(symbol)
                    .map_err(|_| Error::MalformedDeflate("too many Huffman symbols".to_owned()))?;
                offsets[usize::from(length)] += 1;
            }
        }
        Ok(Self { count, symbols })
    }

    /// Decode the next symbol by walking canonical code lengths.
    fn decode(&self, reader: &mut BitReader<'_>) -> Result<u16> {
        let mut code = 0u32;
        let mut first = 0u32;
        let mut index = 0usize;
        for length in 1..=u32::from(MAX_CODE_LENGTH) {
            code |= reader.read_bit()?;
            let count = u32::from(self.count[length as usize]);
            // At this point code >= first is invariant: any shorter valid
            // code would already have been returned in an earlier iteration.
            if code - first < count {
                return self
                    .symbols
                    .get(index + (code - first) as usize)
                    .copied()
                    .ok_or_else(|| {
                        Error::MalformedDeflate("Huffman symbol index out of range".to_owned())
                    });
            }
            index += count as usize;
            first = (first + count) << 1;
            code <<= 1;
        }
        Err(Error::MalformedDeflate(
            "invalid Huffman code in DEFLATE stream".to_owned(),
        ))
    }
}

// ---------------------------------------------------------------------------
// DEFLATE block decoding
// ---------------------------------------------------------------------------

/// Base lengths for length codes 257..=285 (RFC 1951 Table 4).
const LENGTH_BASE: [u16; 29] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131,
    163, 195, 227, 258,
];
/// Extra bits for length codes 257..=285 (RFC 1951 Table 4).
const LENGTH_EXTRA: [u8; 29] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
];
/// Base distances for distance codes 0..=29 (RFC 1951 Table 5).
const DISTANCE_BASE: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
    2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
];
/// Extra bits for distance codes 0..=29 (RFC 1951 Table 5).
const DISTANCE_EXTRA: [u8; 30] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13,
    13,
];
/// Order in which dynamic code lengths are transmitted (RFC 1951 section 3.2.7).
const CODE_LENGTH_ORDER: [usize; 19] = [
    16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
];

/// Inflate a zlib or raw DEFLATE stream without applying any predictor.
fn inflate(data: &[u8], cap: usize) -> Result<Vec<u8>> {
    if data.len() < 2 {
        return Err(Error::MalformedDeflate(
            "stream is shorter than a DEFLATE header".to_owned(),
        ));
    }
    let cmf = data[0];
    let flg = data[1];
    let looks_like_zlib = cmf & 0x0f == 8 && cmf >> 4 <= 7 && zlib_header_check(cmf, flg);
    if looks_like_zlib {
        if flg & 0x20 != 0 {
            return Err(Error::InvalidZlibHeader(
                "preset dictionaries are not supported".to_owned(),
            ));
        }
        if data.len() < 6 {
            return Err(Error::MalformedDeflate(
                "zlib stream is missing its Adler-32 trailer".to_owned(),
            ));
        }
        let split = data.len() - 4;
        let expected = u32::from_be_bytes([
            data[split],
            data[split + 1],
            data[split + 2],
            data[split + 3],
        ]);
        let output = inflate_raw(&data[2..split], cap)?;
        let actual = adler32(&output);
        if actual != expected {
            return Err(Error::InvalidZlibHeader(format!(
                "Adler-32 mismatch: expected {expected:08x}, computed {actual:08x}"
            )));
        }
        Ok(output)
    } else if cmf == 0x78 {
        Err(Error::InvalidZlibHeader(format!(
            "invalid zlib header bytes {cmf:#04x} {flg:#04x}"
        )))
    } else {
        inflate_raw(data, cap)
    }
}

/// Verify the RFC 1950 header check bits: `(CMF << 8 | FLG) % 31 == 0`.
fn zlib_header_check(cmf: u8, flg: u8) -> bool {
    ((u16::from(cmf) << 8) | u16::from(flg)) % 31 == 0
}

/// Compute the Adler-32 checksum of `data` (RFC 1950 section 2).
fn adler32(data: &[u8]) -> u32 {
    const MOD: u32 = 65521;
    let mut a = 1u32;
    let mut b = 0u32;
    for &byte in data {
        a = (a + u32::from(byte)) % MOD;
        b = (b + a) % MOD;
    }
    (b << 16) | a
}

/// Inflate a raw (headerless) DEFLATE stream.
fn inflate_raw(data: &[u8], cap: usize) -> Result<Vec<u8>> {
    let mut reader = BitReader::new(data);
    let mut output = Vec::new();
    loop {
        let is_final = reader.read_bit()? == 1;
        let block_type = reader.read_bits(2)?;
        match block_type {
            0 => inflate_stored(&mut reader, &mut output, cap)?,
            1 => inflate_compressed(&mut reader, &mut output, cap, false)?,
            2 => inflate_compressed(&mut reader, &mut output, cap, true)?,
            _ => {
                return Err(Error::MalformedDeflate(
                    "reserved DEFLATE block type 3".to_owned(),
                ));
            }
        }
        if is_final {
            break;
        }
    }
    Ok(output)
}

/// Decode a stored (uncompressed) DEFLATE block.
fn inflate_stored(reader: &mut BitReader<'_>, output: &mut Vec<u8>, cap: usize) -> Result<()> {
    reader.align_to_byte();
    let length = reader.read_bits(16)? as usize;
    let inverse = reader.read_bits(16)? as usize;
    if (length ^ 0xffff) != inverse {
        return Err(Error::MalformedDeflate(
            "stored block length complement mismatch".to_owned(),
        ));
    }
    reserve_output(output, length, cap)?;
    let bytes = reader.read_bytes(length)?;
    output.extend_from_slice(bytes);
    Ok(())
}

/// Decode a fixed or dynamic Huffman DEFLATE block.
fn inflate_compressed(
    reader: &mut BitReader<'_>,
    output: &mut Vec<u8>,
    cap: usize,
    dynamic: bool,
) -> Result<()> {
    let (literals, distances) = if dynamic {
        dynamic_trees(reader)?
    } else {
        fixed_trees()?
    };

    loop {
        let symbol = literals.decode(reader)?;
        match symbol {
            0..=255 => {
                reserve_output(output, 1, cap)?;
                output.push(symbol as u8);
            }
            256 => return Ok(()),
            257..=285 => {
                let index = usize::from(symbol - 257);
                let length = usize::from(LENGTH_BASE[index])
                    + reader.read_bits(LENGTH_EXTRA[index])? as usize;
                let distance_symbol = distances.decode(reader)?;
                if distance_symbol >= VALID_DISTANCE_CODES {
                    return Err(Error::MalformedDeflate(format!(
                        "reserved distance code {distance_symbol}"
                    )));
                }
                let distance_index = usize::from(distance_symbol);
                let distance = usize::from(DISTANCE_BASE[distance_index])
                    + reader.read_bits(DISTANCE_EXTRA[distance_index])? as usize;
                if distance == 0 || distance > output.len() {
                    return Err(Error::MalformedDeflate(format!(
                        "back-reference distance {distance} exceeds {} decoded bytes",
                        output.len()
                    )));
                }
                reserve_output(output, length, cap)?;
                for _ in 0..length {
                    let byte = output[output.len() - distance];
                    output.push(byte);
                }
            }
            _ => {
                return Err(Error::MalformedDeflate(format!(
                    "invalid literal/length code {symbol}"
                )));
            }
        }
    }
}

/// The fixed literal/length and distance trees defined by RFC 1951 3.2.6.
fn fixed_trees() -> Result<(Huffman, Huffman)> {
    let mut lengths = [0u8; MAX_LITERAL_CODES];
    for (symbol, length) in lengths.iter_mut().enumerate() {
        *length = match symbol {
            0..=143 => 8,
            144..=255 => 9,
            256..=279 => 7,
            280..=287 => 8,
            _ => 0,
        };
    }
    let distances = [5u8; MAX_DISTANCE_CODES];
    Ok((Huffman::new(&lengths)?, Huffman::new(&distances)?))
}

/// Read the dynamic code-length trees from the start of a block.
fn dynamic_trees(reader: &mut BitReader<'_>) -> Result<(Huffman, Huffman)> {
    let literal_count = reader.read_bits(5)? as usize + 257;
    let distance_count = reader.read_bits(5)? as usize + 1;
    let code_length_count = reader.read_bits(4)? as usize + 4;
    if literal_count > MAX_LITERAL_CODES || distance_count > MAX_DISTANCE_CODES {
        return Err(Error::MalformedDeflate(
            "dynamic block declares too many codes".to_owned(),
        ));
    }

    let mut code_lengths = [0u8; 19];
    for &slot in CODE_LENGTH_ORDER.iter().take(code_length_count) {
        code_lengths[slot] = reader.read_bits(3)? as u8;
    }
    let code_tree = Huffman::new(&code_lengths)?;

    let total = literal_count + distance_count;
    let mut lengths: Vec<u8> = Vec::with_capacity(total);
    while lengths.len() < total {
        let symbol = code_tree.decode(reader)?;
        match symbol {
            0..=15 => lengths.push(symbol as u8),
            16 => {
                if lengths.is_empty() {
                    return Err(Error::MalformedDeflate(
                        "repeat code before any code length".to_owned(),
                    ));
                }
                let previous = lengths[lengths.len() - 1];
                let repeat = 3 + reader.read_bits(2)? as usize;
                push_lengths(&mut lengths, previous, repeat, total)?;
            }
            17 => {
                let repeat = 3 + reader.read_bits(3)? as usize;
                push_lengths(&mut lengths, 0, repeat, total)?;
            }
            18 => {
                let repeat = 11 + reader.read_bits(7)? as usize;
                push_lengths(&mut lengths, 0, repeat, total)?;
            }
            _ => {
                return Err(Error::MalformedDeflate(format!(
                    "invalid code-length symbol {symbol}"
                )));
            }
        }
    }

    let literals = Huffman::new(&lengths[..literal_count])?;
    let distances = Huffman::new(&lengths[literal_count..])?;
    Ok((literals, distances))
}

/// Append `repeat` copies of `value`, never exceeding `total` lengths.
fn push_lengths(lengths: &mut Vec<u8>, value: u8, repeat: usize, total: usize) -> Result<()> {
    match lengths.len().checked_add(repeat) {
        Some(end) if end <= total => {}
        _ => {
            return Err(Error::MalformedDeflate(
                "code-length repeat exceeds the declared tree size".to_owned(),
            ));
        }
    }
    lengths.resize(lengths.len() + repeat, value);
    Ok(())
}

/// Check an output write against the cap before it is performed.
fn reserve_output(output: &[u8], additional: usize, cap: usize) -> Result<()> {
    match output.len().checked_add(additional) {
        Some(end) if end <= cap => Ok(()),
        _ => Err(Error::DecodeLimitExceeded { limit: cap }),
    }
}

// ---------------------------------------------------------------------------
// PNG predictors
// ---------------------------------------------------------------------------

/// Undo a PNG predictor after inflation.
fn apply_predictor(data: &[u8], params: &FlateParams) -> Result<Vec<u8>> {
    match params.predictor {
        Predictor::None => return Ok(data.to_vec()),
        Predictor::Tiff => {
            return Err(Error::InvalidDecodeParameters(
                "TIFF predictor 2 is not supported".to_owned(),
            ));
        }
        _ => {}
    }

    let bytes_per_pixel = params.bytes_per_pixel()?;
    let columns = usize::try_from(params.columns)
        .map_err(|_| Error::InvalidDecodeParameters("Columns is too large to decode".to_owned()))?;
    let row_length = columns.checked_mul(bytes_per_pixel).ok_or_else(|| {
        Error::InvalidDecodeParameters("predictor row length overflows".to_owned())
    })?;
    if row_length == 0 {
        return Err(Error::InvalidDecodeParameters(
            "predictor row length must be positive".to_owned(),
        ));
    }
    // A well-formed predictor stream must contain at least one full row.
    // Reject an oversized row *before* allocating `previous_row`: `row_length`
    // is derived from attacker-controlled `/DecodeParms`, and `data.len()` is
    // already bounded by the decode cap. Without this check a tiny zlib stream
    // with a huge `/Columns` could force a multi-gigabyte allocation here.
    if row_length > data.len() {
        return Err(Error::MalformedDeflate(
            "truncated predictor row in Flate stream".to_owned(),
        ));
    }

    let fixed_filter = params.predictor.fixed_png_filter();
    let mut output = Vec::with_capacity(data.len());
    let mut previous_row = vec![0u8; row_length];
    let mut position = 0usize;

    while position < data.len() {
        let (filter_type, row_start, row_end) = match fixed_filter {
            Some(filter) => {
                let end = position.checked_add(row_length).ok_or_else(|| {
                    Error::MalformedDeflate("predictor row offset overflows".to_owned())
                })?;
                if end > data.len() {
                    return Err(Error::MalformedDeflate(
                        "truncated predictor row in Flate stream".to_owned(),
                    ));
                }
                (filter, position, end)
            }
            None => {
                let filter = data[position];
                if filter > 4 {
                    return Err(Error::InvalidDecodeParameters(format!(
                        "invalid PNG predictor row filter {filter}"
                    )));
                }
                let start = position + 1;
                let end = start.checked_add(row_length).ok_or_else(|| {
                    Error::MalformedDeflate("predictor row offset overflows".to_owned())
                })?;
                if end > data.len() {
                    return Err(Error::MalformedDeflate(
                        "truncated predictor row in Flate stream".to_owned(),
                    ));
                }
                (filter, start, end)
            }
        };

        let mut row = data[row_start..row_end].to_vec();
        for index in 0..row_length {
            let left = if index >= bytes_per_pixel {
                row[index - bytes_per_pixel]
            } else {
                0
            };
            let up = previous_row[index];
            let up_left = if index >= bytes_per_pixel {
                previous_row[index - bytes_per_pixel]
            } else {
                0
            };
            let estimate = match filter_type {
                0 => 0,
                1 => left,
                2 => up,
                3 => ((u16::from(left) + u16::from(up)) / 2) as u8,
                4 => paeth_predictor(left, up, up_left),
                _ => {
                    return Err(Error::InvalidDecodeParameters(format!(
                        "invalid PNG predictor row filter {filter_type}"
                    )));
                }
            };
            row[index] = row[index].wrapping_add(estimate);
        }
        output.extend_from_slice(&row);
        previous_row = row;
        position = row_end;
    }
    Ok(output)
}

/// The PNG Paeth predictor: pick the neighbor closest to `a + b - c`.
fn paeth_predictor(a: u8, b: u8, c: u8) -> u8 {
    let (a, b, c) = (i32::from(a), i32::from(b), i32::from(c));
    let estimate = a + b - c;
    let distance_a = (estimate - a).abs();
    let distance_b = (estimate - b).abs();
    let distance_c = (estimate - c).abs();
    let chosen = if distance_a <= distance_b && distance_a <= distance_c {
        a
    } else if distance_b <= distance_c {
        b
    } else {
        c
    };
    chosen as u8
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::object::Dictionary;

    /// Wrap bytes in a valid zlib stream made of stored DEFLATE blocks.
    ///
    /// This is independent of the inflate implementation and lets predictor
    /// tests supply arbitrary pre-compressed bytes without a codec dependency.
    fn zlib_stored(data: &[u8]) -> Vec<u8> {
        let mut out = vec![0x78, 0x01]; // CM=8, CINFO=7, FCHECK valid
        let mut chunks = data.chunks(0xffff).peekable();
        while let Some(chunk) = chunks.next() {
            let last = chunks.peek().is_none();
            out.push(if last { 0x01 } else { 0x00 }); // BFINAL, BTYPE=00
            let length = chunk.len() as u16;
            out.extend_from_slice(&length.to_le_bytes());
            out.extend_from_slice(&(!length).to_le_bytes());
            out.extend_from_slice(chunk);
        }
        out.extend_from_slice(&adler32(data).to_be_bytes());
        out
    }

    /// Decode one Flate filter with explicit parameters.
    fn flate(data: &[u8], params: FlateParams) -> Result<Vec<u8>> {
        decode_stream(data, &[Filter::FlateDecode(params)])
    }

    /// A zlib stream produced by .NET DeflateStream (fixed Huffman) for
    /// "Hello, PDFXI!".
    const HELLO_ZLIB: &[u8] = &[
        0x78, 0x9C, 0xF3, 0x48, 0xCD, 0xC9, 0xC9, 0xD7, 0x51, 0x08, 0x70, 0x71, 0x8B, 0xF0, 0x54,
        0x04, 0x00, 0x1D, 0x7B, 0x03, 0xDD,
    ];

    /// The raw DEFLATE payload of `HELLO_ZLIB` without the wrapper.
    const HELLO_RAW: &[u8] = &[
        0xF3, 0x48, 0xCD, 0xC9, 0xC9, 0xD7, 0x51, 0x08, 0x70, 0x71, 0x8B, 0xF0, 0x54, 0x04, 0x00,
    ];

    /// A zlib stream produced by .NET DeflateStream using dynamic Huffman
    /// codes. Its plain text is reproduced by `dynamic_plain_text`.
    const DYNAMIC_ZLIB: &[u8] = &[
        0x78, 0x9C, 0x9D, 0xD2, 0xCB, 0x09, 0xC3, 0x30, 0x10, 0x45, 0xD1, 0x56, 0xA6, 0x02, 0x93,
        0xE7, 0x7C, 0xED, 0x1A, 0xD4, 0xC4, 0x98, 0x88, 0xC8, 0x20, 0xCB, 0x02, 0x6B, 0x95, 0xEA,
        0x63, 0x95, 0x90, 0xBB, 0xD0, 0x20, 0x18, 0xEE, 0x6A, 0x4E, 0x58, 0x4B, 0xB4, 0xCB, 0x6C,
        0x9E, 0x6B, 0x72, 0x5B, 0x62, 0x73, 0xFB, 0xF8, 0xB6, 0xB9, 0xBD, 0x63, 0x3E, 0xFF, 0xB1,
        0x1E, 0x6B, 0xDE, 0x8B, 0x7D, 0xFB, 0xA2, 0xBF, 0x96, 0xCE, 0x39, 0x58, 0xE8, 0x99, 0x58,
        0x36, 0xB2, 0xEC, 0xCA, 0xB2, 0x1B, 0xCB, 0xEE, 0x2C, 0x7B, 0xB0, 0xEC, 0xC9, 0xB2, 0x17,
        0xCB, 0x26, 0x78, 0x6E, 0xCA, 0x04, 0x3A, 0x11, 0x84, 0x22, 0x28, 0x45, 0x90, 0x8A, 0xA0,
        0x15, 0x41, 0x2C, 0x82, 0x5A, 0x04, 0xB9, 0xE8, 0x7F, 0x2F, 0x3F, 0xCB, 0x73, 0x7F, 0x58,
    ];

    fn dynamic_plain_text() -> String {
        let mut text = String::new();
        for index in 0..20 {
            text.push_str(&format!(
                "Line {index}: alpha beta gamma delta epsilon zeta eta theta. "
            ));
        }
        text
    }

    #[test]
    fn decodes_known_zlib_vector() {
        let decoded = flate(HELLO_ZLIB, FlateParams::default()).expect("zlib decode");
        assert_eq!(decoded, b"Hello, PDFXI!");
    }

    #[test]
    fn decodes_fixed_huffman_back_references() {
        // HELLO_ZLIB contains back-references; decoding it verifies length and
        // distance handling as well as the fixed literal/length tree.
        let decoded = flate(HELLO_ZLIB, FlateParams::default()).expect("decode");
        assert_eq!(decoded, b"Hello, PDFXI!");
    }

    #[test]
    fn decodes_dynamic_huffman_vector() {
        let decoded = flate(DYNAMIC_ZLIB, FlateParams::default()).expect("dynamic decode");
        assert_eq!(decoded, dynamic_plain_text().as_bytes());
    }

    #[test]
    fn decodes_raw_deflate_without_zlib_header() {
        let decoded = flate(HELLO_RAW, FlateParams::default()).expect("raw deflate decode");
        assert_eq!(decoded, b"Hello, PDFXI!");
    }

    #[test]
    fn decodes_stored_deflate_blocks() {
        let compressed = zlib_stored(b"Stored block test");
        let decoded = flate(&compressed, FlateParams::default()).expect("stored decode");
        assert_eq!(decoded, b"Stored block test");
    }

    #[test]
    fn empty_filter_chain_is_a_copy() {
        let decoded = decode_stream(b"raw bytes", &[]).expect("copy");
        assert_eq!(decoded, b"raw bytes");
    }

    #[test]
    fn png_up_predictor_reconstructs_rows() {
        let encoded = [10, 20, 30, 40, 5, 5, 5, 5];
        let compressed = zlib_stored(&encoded);
        let params = FlateParams::new(Predictor::PngUp, 1, 8, 4);
        let decoded = flate(&compressed, params).expect("up predictor");
        assert_eq!(decoded, [10, 20, 30, 40, 15, 25, 35, 45]);
    }

    #[test]
    fn png_paeth_predictor_reconstructs_rows() {
        // Row 0 uses Sub-like Paeth values; row 1's first byte (50) would be
        // 50 for Up but the remaining bytes (1) prove Paeth was applied.
        let encoded = [100, 1, 1, 1, 50, 1, 1, 1];
        let compressed = zlib_stored(&encoded);
        let params = FlateParams::new(Predictor::PngPaeth, 1, 8, 4);
        let decoded = flate(&compressed, params).expect("paeth predictor");
        assert_eq!(decoded, [100, 101, 102, 103, 150, 151, 152, 153]);
    }

    #[test]
    fn png_sub_average_none_and_optimal_predictors() {
        let params = FlateParams::new(Predictor::PngSub, 1, 8, 4);
        let decoded = flate(&zlib_stored(&[5, 2, 2, 2]), params).expect("sub");
        assert_eq!(decoded, [5, 7, 9, 11]);

        let params = FlateParams::new(Predictor::PngAverage, 1, 8, 4);
        let decoded = flate(&zlib_stored(&[4, 6, 8, 10]), params).expect("average");
        assert_eq!(decoded, [4, 8, 12, 16]);

        let params = FlateParams::new(Predictor::PngNone, 1, 8, 4);
        let decoded = flate(&zlib_stored(&[1, 2, 3, 4]), params).expect("none");
        assert_eq!(decoded, [1, 2, 3, 4]);

        // Predictor 15 prefixes each row with its filter type (2 = Up).
        let params = FlateParams::new(Predictor::PngOptimal, 1, 8, 4);
        let encoded = [2, 10, 20, 30, 40, 2, 5, 5, 5, 5];
        let decoded = flate(&zlib_stored(&encoded), params).expect("optimal");
        assert_eq!(decoded, [10, 20, 30, 40, 15, 25, 35, 45]);
    }

    #[test]
    fn predictor_1_is_a_passthrough() {
        let params = FlateParams::default();
        let decoded = flate(&zlib_stored(b"plain content"), params).expect("passthrough");
        assert_eq!(decoded, b"plain content");
    }

    #[test]
    fn predictor_2_is_rejected() {
        let params = FlateParams::new(Predictor::Tiff, 1, 8, 4);
        assert!(matches!(
            flate(&zlib_stored(&[1, 2, 3, 4]), params),
            Err(Error::InvalidDecodeParameters(_))
        ));
    }

    #[test]
    fn truncated_stream_is_an_error() {
        let compressed = zlib_stored(b"Hello, PDFXI!");
        let truncated = &compressed[..compressed.len() - 2];
        assert!(matches!(
            flate(truncated, FlateParams::default()),
            Err(Error::MalformedDeflate(_)) | Err(Error::InvalidZlibHeader(_))
        ));
    }

    #[test]
    fn wrong_zlib_header_is_an_error() {
        let mut bad = HELLO_ZLIB.to_vec();
        // 0x78 0x00 fails the (CMF<<8|FLG) % 31 check.
        bad[1] = 0x00;
        assert!(matches!(
            flate(&bad, FlateParams::default()),
            Err(Error::InvalidZlibHeader(_))
        ));
    }

    #[test]
    fn bad_adler32_is_an_error() {
        let mut bad = HELLO_ZLIB.to_vec();
        let last = bad.len() - 1;
        bad[last] ^= 0xFF;
        assert!(matches!(
            flate(&bad, FlateParams::default()),
            Err(Error::InvalidZlibHeader(message)) if message.contains("Adler-32")
        ));
    }

    #[test]
    fn unsupported_filter_error_names_the_filter() {
        let error =
            decode_stream(b"anything", &[Filter::LzwDecode]).expect_err("LZW is unsupported");
        match error {
            Error::UnsupportedFilter(name) => assert_eq!(name, "LZWDecode"),
            other => panic!("unexpected error: {other}"),
        }
    }

    #[test]
    fn filter_chain_reports_the_first_unsupported_step() {
        let chain = [
            Filter::FlateDecode(FlateParams::default()),
            Filter::DctDecode,
        ];
        let error = decode_stream(HELLO_ZLIB, &chain).expect_err("DCT is unsupported");
        assert!(matches!(error, Error::UnsupportedFilter(name) if name == "DCTDecode"));
    }

    #[test]
    fn decode_cap_is_enforced() {
        let compressed = zlib_stored(b"Hello, PDFXI!");
        let error = decode_stream_with_limit(
            &compressed,
            &[Filter::FlateDecode(FlateParams::default())],
            4,
        )
        .expect_err("output exceeds the four byte cap");
        assert!(matches!(error, Error::DecodeLimitExceeded { limit: 4 }));
    }

    #[test]
    fn truncated_predictor_row_is_an_error() {
        let compressed = zlib_stored(&[10, 20, 30]);
        let params = FlateParams::new(Predictor::PngUp, 1, 8, 4);
        assert!(matches!(
            flate(&compressed, params),
            Err(Error::MalformedDeflate(_))
        ));
    }

    #[test]
    fn hostile_columns_is_rejected_before_allocating_predictor_row() {
        // A two-byte valid zlib stream with an enormous /Columns must be
        // rejected as a truncated row before `previous_row` is allocated.
        // This is a regression test for the hostile-/DecodeParms allocation
        // blocker: before the up-front `row_length > data.len()` check this
        // call tried to allocate a multi-gigabyte buffer.
        let compressed = zlib_stored(&[1, 2]);
        let params = FlateParams::new(Predictor::PngOptimal, 1, 8, u32::MAX);
        assert!(matches!(
            flate(&compressed, params),
            Err(Error::MalformedDeflate(message))
                if message.contains("truncated predictor row")
        ));

        let params = FlateParams::new(Predictor::PngUp, 1, 8, u32::MAX);
        assert!(matches!(
            flate(&zlib_stored(&[1, 2]), params),
            Err(Error::MalformedDeflate(_))
        ));
    }

    #[test]
    fn invalid_predictor_filter_type_is_an_error() {
        let compressed = zlib_stored(&[9, 10, 20, 30, 40]);
        let params = FlateParams::new(Predictor::PngOptimal, 1, 8, 4);
        assert!(matches!(
            flate(&compressed, params),
            Err(Error::InvalidDecodeParameters(_))
        ));
    }

    #[test]
    fn invalid_component_size_is_an_error() {
        let params = FlateParams::new(Predictor::PngUp, 1, 4, 4);
        assert!(matches!(
            flate(&zlib_stored(&[1, 2, 3, 4]), params),
            Err(Error::InvalidDecodeParameters(_))
        ));
    }

    #[test]
    fn dictionary_filter_chain_is_parsed() {
        let mut dictionary = Dictionary::new();
        dictionary.insert("Filter".to_owned(), Object::Name("FlateDecode".to_owned()));
        let mut parms = Dictionary::new();
        parms.insert("Predictor".to_owned(), Object::Integer(12));
        parms.insert("Columns".to_owned(), Object::Integer(4));
        dictionary.insert("DecodeParms".to_owned(), Object::Dictionary(parms));
        let filters = filters_from_dictionary(&dictionary).expect("parse filters");
        assert_eq!(
            filters,
            vec![Filter::FlateDecode(FlateParams::new(
                Predictor::PngUp,
                1,
                8,
                4
            ))]
        );
    }

    #[test]
    fn dictionary_filter_array_is_parsed_in_order() {
        let mut dictionary = Dictionary::new();
        dictionary.insert(
            "Filter".to_owned(),
            Object::Array(vec![
                Object::Name("FlateDecode".to_owned()),
                Object::Name("DCTDecode".to_owned()),
            ]),
        );
        let filters = filters_from_dictionary(&dictionary).expect("parse filters");
        assert_eq!(
            filters,
            vec![
                Filter::FlateDecode(FlateParams::default()),
                Filter::DctDecode
            ]
        );
        assert_eq!(filters[1].to_string(), "DCTDecode");
    }

    #[test]
    fn missing_filter_yields_empty_chain() {
        let dictionary = Dictionary::new();
        assert!(
            filters_from_dictionary(&dictionary)
                .expect("empty chain")
                .is_empty()
        );
    }
}
