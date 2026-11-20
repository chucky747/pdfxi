//! Dependency-free raster encoders for page-image export.
//!
//! This module turns the tightly packed 8-bit RGBA buffer produced by a
//! [`pdfxi_render::Renderer`](https://docs.rs/pdfxi-render) into a complete PNG
//! or PPM file without adding an image crate. Both encoders use checked
//! arithmetic so a hostile or accidental dimension cannot overflow a length
//! computation, and both report a mismatched pixel buffer as
//! [`ConversionError::InvalidInput`] rather than panicking.
//!
//! # PNG strategy
//!
//! The PNG output is a standard non-interlaced, 8-bit RGBA (colour type 6)
//! image. Its `IDAT` payload is a valid zlib stream built from *stored*
//! (uncompressed) deflate blocks: every scanline is written with filter type 0
//! (None), so a stored block reproduces the raw bytes verbatim. The file is
//! larger than a compressed PNG, but every conforming PNG decoder accepts it
//! and the implementation needs no compression dependency. CRC-32 protects
//! each chunk and Adler-32 terminates the zlib stream; large rasters are split
//! across several 65,535-byte stored blocks.
//!
//! # PPM strategy
//!
//! PPM output uses the binary P6 subformat with a `255` sample maximum. The
//! alpha channel is discarded, so four input channels become three output
//! channels.
//!
//! # BMP and TIFF strategy
//!
//! [`ImageFormat::Bmp`] writes a little-endian 32-bit `BITMAPINFOHEADER` file
//! with bottom-up BGRA rows and `BI_RGB` compression (row stride is `width *
//! 4`, always four-byte aligned). [`ImageFormat::Tiff`] writes a little-endian
//! classic TIFF with one uncompressed RGBA strip, a baseline 11-tag IFD in
//! ascending tag order, and an unassociated-alpha `ExtraSamples` tag. Both
//! formats carry no timestamp, so their bytes are deterministic.
//!
//! # Bounds and JPEG scope
//!
//! Every encoder rejects rasters whose dimensions, pixel count, or byte length
//! exceed documented constants before allocating. A full baseline JPEG
//! entropy encoder is intentionally *not* duplicated here: the already-verified
//! `pdfxi-render` export module (EXPORT-006) provides PNG/JPEG/TIFF from a real
//! renderer, while this dependency-free module covers the pure-RGBA encoders
//! that need no native or compression dependency.
//!
//! # Relationship to the CLI encoder
//!
//! `pdfxi-cli` contains an equivalent pair of encoders in
//! `crates/pdfxi-cli/src/image.rs`. They are intentionally *not* shared yet:
//! the CLI encoder is built on `anyhow`, moving it into this dependency-free
//! crate would change both crates' error contracts, and doing so is outside
//! this task's ownership. Deduplicating the two implementations into a shared
//! low-level crate is a documented follow-up.

use crate::ConversionError;

/// A raster file format supported by page-image export.
///
/// [`ImageFormat::Png`], [`ImageFormat::Bmp`], and [`ImageFormat::Tiff`] keep
/// the alpha channel; [`ImageFormat::Ppm`] drops it because the binary P6
/// subformat stores exactly three samples per pixel. A lossless baseline JPEG
/// encoder is intentionally not part of this dependency-free module; callers
/// needing JPEG can use the verified renderer-backed encoder in
/// `pdfxi-render`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ImageFormat {
    /// An 8-bit RGBA Portable Network Graphics file.
    Png,
    /// A binary P6 Portable Pixmap file (RGB, alpha discarded).
    Ppm,
    /// A 32-bit BGRA Windows bitmap with bottom-up rows.
    Bmp,
    /// A little-endian, uncompressed classic TIFF with one RGBA strip.
    Tiff,
}

impl ImageFormat {
    /// Every supported raster format, in a stable order.
    pub const ALL: &'static [Self] = &[Self::Png, Self::Ppm, Self::Bmp, Self::Tiff];

    /// Return every supported raster format in a stable order.
    #[must_use]
    pub fn all() -> &'static [Self] {
        Self::ALL
    }

    /// Classify a file-name extension, ignoring an optional leading dot and
    /// ASCII case.
    ///
    /// `png` selects [`ImageFormat::Png`], `ppm`/`pnm` select
    /// [`ImageFormat::Ppm`], `bmp` selects [`ImageFormat::Bmp`], and
    /// `tif`/`tiff` select [`ImageFormat::Tiff`]. Any other extension (including
    /// `jpg`/`jpeg`, which are served by the renderer-backed encoder) yields
    /// `None`.
    #[must_use]
    pub fn from_extension(extension: &str) -> Option<Self> {
        let extension = extension.strip_prefix('.').unwrap_or(extension);
        match extension.to_ascii_lowercase().as_str() {
            "png" => Some(Self::Png),
            "ppm" | "pnm" => Some(Self::Ppm),
            "bmp" => Some(Self::Bmp),
            "tif" | "tiff" => Some(Self::Tiff),
            _ => None,
        }
    }

    /// Return the canonical lower-case file extension for this format.
    #[must_use]
    pub const fn extension(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Ppm => "ppm",
            Self::Bmp => "bmp",
            Self::Tiff => "tiff",
        }
    }

    /// Return a short human-readable label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        self.extension()
    }
}

impl std::fmt::Display for ImageFormat {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.label())
    }
}

/// The fixed eight-byte signature at the start of every PNG file.
const PNG_SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

/// Largest accepted width or height for a caller-supplied raster.
pub const MAX_IMAGE_DIMENSION: u32 = 16_384;
/// Largest accepted number of pixels in a caller-supplied raster.
pub const MAX_IMAGE_PIXELS: usize = 268_435_456;
/// Largest accepted RGBA byte length of a caller-supplied raster (1 GiB).
pub const MAX_IMAGE_BYTES: usize = 1_073_741_824;

/// Largest number of raw bytes a single deflate stored block can carry.
const MAX_STORED_BLOCK: usize = u16::MAX as usize;

/// Starting value of the CRC-32 register used by PNG chunk checksums.
const CRC_INIT: u32 = 0xFFFF_FFFF;

/// Adler-32 modulus (the largest prime below 2^16).
const ADLER_MODULUS: u32 = 65_521;

/// Continue a CRC-32 (IEEE 802.3) register over `data`.
///
/// Splitting the update lets a PNG chunk checksum cover its type and payload
/// without building a temporary concatenation of the two.
fn crc32_continue(mut crc: u32, data: &[u8]) -> u32 {
    for &byte in data {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            // Select the polynomial for a set low bit, otherwise zero.
            let mask = 0u32.wrapping_sub(crc & 1);
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    crc
}

/// Return the standard CRC-32 checksum of `data`.
#[cfg(test)]
fn crc32(data: &[u8]) -> u32 {
    crc32_continue(CRC_INIT, data) ^ CRC_INIT
}

/// Return the standard Adler-32 checksum of `data`.
fn adler32(data: &[u8]) -> u32 {
    let mut a = 1u32;
    let mut b = 0u32;
    for &byte in data {
        a = (a + u32::from(byte)) % ADLER_MODULUS;
        b = (b + a) % ADLER_MODULUS;
    }
    (b << 16) | a
}

/// Append one PNG chunk (big-endian length, type, payload, CRC-32) to `out`.
fn push_chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) -> Result<(), ConversionError> {
    let length = u32::try_from(data.len()).map_err(|_| ConversionError::InvalidInput {
        message: format!(
            "PNG chunk '{}' exceeds 2^32 bytes",
            String::from_utf8_lossy(kind)
        ),
    })?;
    out.extend_from_slice(&length.to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let crc = crc32_continue(crc32_continue(CRC_INIT, kind), data) ^ CRC_INIT;
    out.extend_from_slice(&crc.to_be_bytes());
    Ok(())
}

/// Wrap `raw` in a zlib stream built from uncompressed deflate stored blocks.
///
/// The two-byte header selects a 32 KiB window with no preset dictionary and
/// the fastest (in fact uncompressed) algorithm. Each stored block carries up
/// to 65,535 bytes; the final block sets `BFINAL`. An empty input still gets a
/// single empty final block so the zlib stream is well formed.
fn zlib_stored(raw: &[u8]) -> Vec<u8> {
    let block_count = (raw.len() / MAX_STORED_BLOCK) + 1;
    let mut out = Vec::with_capacity(raw.len() + block_count * 5 + 6);
    out.extend_from_slice(&[0x78, 0x01]);

    if raw.is_empty() {
        out.extend_from_slice(&[0x01, 0x00, 0x00, 0xFF, 0xFF]);
    } else {
        let mut remaining = raw.len();
        for chunk in raw.chunks(MAX_STORED_BLOCK) {
            remaining -= chunk.len();
            // `chunks(MAX_STORED_BLOCK)` guarantees this cast is lossless.
            let len = chunk.len() as u16;
            out.push(u8::from(remaining == 0)); // BFINAL=1/0, BTYPE=00 (stored)
            out.extend_from_slice(&len.to_le_bytes());
            out.extend_from_slice(&(!len).to_le_bytes());
            out.extend_from_slice(chunk);
        }
    }

    out.extend_from_slice(&adler32(raw).to_be_bytes());
    out
}

/// Compute the byte length of an RGBA raster, rejecting oversized or
/// overflowing sizes before any allocation happens.
fn pixel_len(width: u32, height: u32) -> Result<usize, ConversionError> {
    if width > MAX_IMAGE_DIMENSION || height > MAX_IMAGE_DIMENSION {
        return Err(ConversionError::InvalidInput {
            message: format!(
                "image dimension {width}x{height} exceeds the {MAX_IMAGE_DIMENSION} pixel limit"
            ),
        });
    }
    let width = usize::try_from(width).map_err(|_| ConversionError::InvalidInput {
        message: "image width does not fit in usize".to_owned(),
    })?;
    let height = usize::try_from(height).map_err(|_| ConversionError::InvalidInput {
        message: "image height does not fit in usize".to_owned(),
    })?;
    let area = width
        .checked_mul(height)
        .ok_or_else(|| ConversionError::InvalidInput {
            message: "image dimensions overflow the platform size".to_owned(),
        })?;
    if area > MAX_IMAGE_PIXELS {
        return Err(ConversionError::InvalidInput {
            message: format!(
                "image has {area} pixels, exceeding the {MAX_IMAGE_PIXELS} pixel limit"
            ),
        });
    }
    let bytes = area
        .checked_mul(4)
        .ok_or_else(|| ConversionError::InvalidInput {
            message: "image pixel buffer size overflows the platform size".to_owned(),
        })?;
    if bytes > MAX_IMAGE_BYTES {
        return Err(ConversionError::InvalidInput {
            message: format!(
                "image pixel buffer is {bytes} bytes, exceeding the {MAX_IMAGE_BYTES} byte limit"
            ),
        });
    }
    Ok(bytes)
}

/// Encode tightly packed 8-bit RGBA pixels as a valid PNG file.
///
/// `width` and `height` are the raster dimensions and `rgba` must contain
/// exactly `width * height * 4` bytes in row-major order, top-left first. Every
/// scanline is written with PNG filter type 0 (None), so a stored deflate block
/// reproduces the raw bytes verbatim. A mismatched buffer or an overflowing
/// size returns [`ConversionError::InvalidInput`] rather than panicking.
///
/// # Errors
///
/// Returns [`ConversionError::InvalidInput`] when the dimensions overflow or
/// the buffer length does not equal `width * height * 4`.
pub fn encode_png(width: u32, height: u32, rgba: &[u8]) -> Result<Vec<u8>, ConversionError> {
    let expected = pixel_len(width, height)?;
    if rgba.len() != expected {
        return Err(ConversionError::InvalidInput {
            message: format!("RGBA buffer has {} bytes, expected {expected}", rgba.len()),
        });
    }

    let width_usize = usize::try_from(width).map_err(|_| ConversionError::InvalidInput {
        message: "image width does not fit in usize".to_owned(),
    })?;
    let height_usize = usize::try_from(height).map_err(|_| ConversionError::InvalidInput {
        message: "image height does not fit in usize".to_owned(),
    })?;
    let row_len = width_usize * 4;
    let mut raw = Vec::with_capacity(height_usize.saturating_mul(row_len.saturating_add(1)));
    if width == 0 {
        // One zero filter byte per (empty) row keeps the scanline count honest.
        raw.resize(height_usize, 0);
    } else {
        for row in rgba.chunks(row_len) {
            raw.push(0); // PNG filter type: None
            raw.extend_from_slice(row);
        }
    }

    let mut header = Vec::with_capacity(13);
    header.extend_from_slice(&width.to_be_bytes());
    header.extend_from_slice(&height.to_be_bytes());
    header.push(8); // bit depth
    header.push(6); // colour type 6 = truecolour with alpha
    header.push(0); // compression method: deflate
    header.push(0); // filter method: adaptive
    header.push(0); // interlace method: none

    let mut out = Vec::with_capacity(PNG_SIGNATURE.len() + 13 + 12 + raw.len() + 24);
    out.extend_from_slice(&PNG_SIGNATURE);
    push_chunk(&mut out, b"IHDR", &header)?;
    push_chunk(&mut out, b"IDAT", &zlib_stored(&raw))?;
    push_chunk(&mut out, b"IEND", &[])?;
    Ok(out)
}

/// Encode tightly packed 8-bit RGBA pixels as a binary P6 PPM file.
///
/// The four-channel input is reduced to three channels by discarding alpha.
/// `width`, `height`, and `rgba` are validated exactly as in [`encode_png`].
///
/// # Errors
///
/// Returns [`ConversionError::InvalidInput`] when the dimensions overflow or
/// the buffer length does not equal `width * height * 4`.
pub fn encode_ppm(width: u32, height: u32, rgba: &[u8]) -> Result<Vec<u8>, ConversionError> {
    let expected = pixel_len(width, height)?;
    if rgba.len() != expected {
        return Err(ConversionError::InvalidInput {
            message: format!("RGBA buffer has {} bytes, expected {expected}", rgba.len()),
        });
    }

    let mut out = format!("P6\n{width} {height}\n255\n").into_bytes();
    // `pixel_len` already proved `width * height * 4` fits, and 3/4 of it fits.
    out.reserve(expected / 4 * 3);
    for pixel in rgba.as_chunks::<4>().0 {
        out.extend_from_slice(&pixel[..3]);
    }
    Ok(out)
}

/// Append one little-endian TIFF IFD entry.
///
/// `value` is the four-byte value field: for a `SHORT` with count one the value
/// occupies the low two bytes, and for a `LONG` it occupies all four. Multi-value
/// fields store an offset in this slot instead.
fn push_tiff_entry(out: &mut Vec<u8>, tag: u16, kind: u16, count: u32, value: u32) {
    out.extend_from_slice(&tag.to_le_bytes());
    out.extend_from_slice(&kind.to_le_bytes());
    out.extend_from_slice(&count.to_le_bytes());
    out.extend_from_slice(&value.to_le_bytes());
}

/// Encode tightly packed 8-bit RGBA pixels as a 32-bit bottom-up BMP file.
///
/// The file has a 14-byte `BITMAPFILEHEADER` followed by a 40-byte
/// `BITMAPINFOHEADER` with `BI_RGB` compression and 32 bits per pixel. Rows are
/// stored bottom-up and each pixel is written as BGRA, so the alpha byte is
/// preserved. No timestamp or palette is emitted, making the bytes
/// deterministic. Input validation matches [`encode_png`].
///
/// # Errors
///
/// Returns [`ConversionError::InvalidInput`] when the dimensions exceed the
/// documented bounds or the buffer length is not `width * height * 4`.
pub fn encode_bmp(width: u32, height: u32, rgba: &[u8]) -> Result<Vec<u8>, ConversionError> {
    let expected = pixel_len(width, height)?;
    if rgba.len() != expected {
        return Err(ConversionError::InvalidInput {
            message: format!("RGBA buffer has {} bytes, expected {expected}", rgba.len()),
        });
    }
    let pixel_bytes = u32::try_from(expected).map_err(|_| ConversionError::InvalidInput {
        message: "BMP pixel data exceeds the 4 GiB header limit".to_owned(),
    })?;
    let file_size =
        54u32
            .checked_add(pixel_bytes)
            .ok_or_else(|| ConversionError::InvalidInput {
                message: "BMP file size exceeds the 4 GiB header limit".to_owned(),
            })?;
    let width_usize = usize::try_from(width).map_err(|_| ConversionError::InvalidInput {
        message: "image width does not fit in usize".to_owned(),
    })?;
    let row_len = width_usize
        .checked_mul(4)
        .ok_or_else(|| ConversionError::InvalidInput {
            message: "BMP row length overflows".to_owned(),
        })?;

    let mut out = Vec::with_capacity(54usize.saturating_add(expected));
    out.extend_from_slice(b"BM");
    out.extend_from_slice(&file_size.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes()); // bfReserved1
    out.extend_from_slice(&0u16.to_le_bytes()); // bfReserved2
    out.extend_from_slice(&54u32.to_le_bytes()); // pixel-data offset

    out.extend_from_slice(&40u32.to_le_bytes()); // biSize
    out.extend_from_slice(&(width as i32).to_le_bytes()); // biWidth
    out.extend_from_slice(&(height as i32).to_le_bytes()); // biHeight (positive = bottom-up)
    out.extend_from_slice(&1u16.to_le_bytes()); // biPlanes
    out.extend_from_slice(&32u16.to_le_bytes()); // biBitCount
    out.extend_from_slice(&0u32.to_le_bytes()); // biCompression = BI_RGB
    out.extend_from_slice(&pixel_bytes.to_le_bytes()); // biSizeImage
    out.extend_from_slice(&0i32.to_le_bytes()); // biXPelsPerMeter
    out.extend_from_slice(&0i32.to_le_bytes()); // biYPelsPerMeter
    out.extend_from_slice(&0u32.to_le_bytes()); // biClrUsed
    out.extend_from_slice(&0u32.to_le_bytes()); // biClrImportant

    if row_len > 0 {
        for row in rgba.chunks_exact(row_len).rev() {
            for pixel in row.as_chunks::<4>().0 {
                out.extend_from_slice(&[pixel[2], pixel[1], pixel[0], pixel[3]]);
            }
        }
    }
    Ok(out)
}

/// Encode tightly packed 8-bit RGBA pixels as an uncompressed classic TIFF.
///
/// The output is little-endian (`II`, magic 42) with the IFD at byte 8. The IFD
/// carries the eleven baseline tags in ascending order, `BitsPerSample` points
/// at four 16-bit `8` values, and `StripOffsets`/`StripByteCounts` describe one
/// RGBA strip. `ExtraSamples = 2` marks the fourth channel as unassociated
/// alpha. There are no timestamps, so repeated calls are byte-identical. Input
/// validation matches [`encode_png`].
///
/// # Errors
///
/// Returns [`ConversionError::InvalidInput`] when the dimensions exceed the
/// documented bounds or the buffer length is not `width * height * 4`.
pub fn encode_tiff(width: u32, height: u32, rgba: &[u8]) -> Result<Vec<u8>, ConversionError> {
    let expected = pixel_len(width, height)?;
    if rgba.len() != expected {
        return Err(ConversionError::InvalidInput {
            message: format!("RGBA buffer has {} bytes, expected {expected}", rgba.len()),
        });
    }
    let pixel_bytes = u32::try_from(expected).map_err(|_| ConversionError::InvalidInput {
        message: "TIFF strip exceeds the 4 GiB field limit".to_owned(),
    })?;

    // 8-byte header + 2-byte count + 11 * 12-byte entries + 4-byte next-IFD.
    const ENTRY_COUNT: u16 = 11;
    let ifd_end = 8u32
        .checked_add(2)
        .and_then(|value| value.checked_add(u32::from(ENTRY_COUNT) * 12))
        .and_then(|value| value.checked_add(4))
        .ok_or_else(|| ConversionError::InvalidInput {
            message: "TIFF IFD size overflows".to_owned(),
        })?;
    let bits_offset = ifd_end;
    let strip_offset = bits_offset
        .checked_add(8)
        .ok_or_else(|| ConversionError::InvalidInput {
            message: "TIFF strip offset overflows".to_owned(),
        })?;
    let total =
        strip_offset
            .checked_add(pixel_bytes)
            .ok_or_else(|| ConversionError::InvalidInput {
                message: "TIFF total size overflows the 4 GiB field limit".to_owned(),
            })?;

    let mut out = Vec::with_capacity(usize::try_from(total).unwrap_or(expected + 154));
    out.extend_from_slice(b"II");
    out.extend_from_slice(&42u16.to_le_bytes());
    out.extend_from_slice(&8u32.to_le_bytes());
    out.extend_from_slice(&ENTRY_COUNT.to_le_bytes());

    push_tiff_entry(&mut out, 256, 4, 1, width); // ImageWidth
    push_tiff_entry(&mut out, 257, 4, 1, height); // ImageLength
    push_tiff_entry(&mut out, 258, 3, 4, bits_offset); // BitsPerSample -> offset
    push_tiff_entry(&mut out, 259, 3, 1, 1); // Compression = none
    push_tiff_entry(&mut out, 262, 3, 1, 2); // PhotometricInterpretation = RGB
    push_tiff_entry(&mut out, 273, 4, 1, strip_offset); // StripOffsets
    push_tiff_entry(&mut out, 277, 3, 1, 4); // SamplesPerPixel
    push_tiff_entry(&mut out, 278, 4, 1, height); // RowsPerStrip
    push_tiff_entry(&mut out, 279, 4, 1, pixel_bytes); // StripByteCounts
    push_tiff_entry(&mut out, 284, 3, 1, 1); // PlanarConfiguration = chunky
    push_tiff_entry(&mut out, 338, 3, 1, 2); // ExtraSamples = unassociated alpha
    out.extend_from_slice(&0u32.to_le_bytes()); // no next IFD

    for _ in 0..4 {
        out.extend_from_slice(&8u16.to_le_bytes());
    }
    out.extend_from_slice(rgba);
    Ok(out)
}

/// Encode an RGBA raster in the requested `format`.
///
/// This is the single dispatch point used by page-image export. It forwards to
/// the matching encoder with identical input validation.
///
/// # Errors
///
/// Returns [`ConversionError::InvalidInput`] from the selected encoder when the
/// dimensions are out of bounds or the pixel buffer length is wrong.
pub fn encode(
    width: u32,
    height: u32,
    rgba: &[u8],
    format: ImageFormat,
) -> Result<Vec<u8>, ConversionError> {
    match format {
        ImageFormat::Png => encode_png(width, height, rgba),
        ImageFormat::Ppm => encode_ppm(width, height, rgba),
        ImageFormat::Bmp => encode_bmp(width, height, rgba),
        ImageFormat::Tiff => encode_tiff(width, height, rgba),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A two-pixel raster: opaque red then opaque green.
    fn red_green() -> Vec<u8> {
        vec![255, 0, 0, 255, 0, 255, 0, 255]
    }

    /// Split a PNG into its ordered `(type, data)` chunks, verifying every
    /// chunk's CRC-32 along the way.
    fn png_chunks(bytes: &[u8]) -> Vec<([u8; 4], Vec<u8>)> {
        assert_eq!(&bytes[..8], &PNG_SIGNATURE[..], "bad PNG signature");
        let mut pos = 8;
        let mut chunks = Vec::new();
        while pos < bytes.len() {
            let len =
                u32::from_be_bytes([bytes[pos], bytes[pos + 1], bytes[pos + 2], bytes[pos + 3]])
                    as usize;
            let mut kind = [0u8; 4];
            kind.copy_from_slice(&bytes[pos + 4..pos + 8]);
            let data = bytes[pos + 8..pos + 8 + len].to_vec();
            let stored_crc = u32::from_be_bytes([
                bytes[pos + 8 + len],
                bytes[pos + 9 + len],
                bytes[pos + 10 + len],
                bytes[pos + 11 + len],
            ]);
            let mut checked = Vec::with_capacity(4 + len);
            checked.extend_from_slice(&kind);
            checked.extend_from_slice(&data);
            assert_eq!(stored_crc, crc32(&checked), "chunk CRC-32 mismatch");
            chunks.push((kind, data));
            pos += 12 + len;
        }
        assert_eq!(pos, bytes.len(), "chunk lengths must cover the whole file");
        chunks
    }

    /// Decode the stored deflate blocks of an IDAT payload into raw scanlines.
    fn stored_payload(idat: &[u8]) -> (Vec<u8>, usize) {
        assert_eq!(&idat[..2], &[0x78u8, 0x01], "expected zlib stored header");
        let mut pos = 2;
        let mut raw = Vec::new();
        let mut blocks = 0;
        loop {
            let header = idat[pos];
            assert_eq!(header & 0x06, 0, "deflate block is not stored");
            let len = u16::from_le_bytes([idat[pos + 1], idat[pos + 2]]);
            let nlen = u16::from_le_bytes([idat[pos + 3], idat[pos + 4]]);
            assert_eq!(len ^ 0xFFFF, nlen, "stored block length complement");
            raw.extend_from_slice(&idat[pos + 5..pos + 5 + len as usize]);
            pos += 5 + len as usize;
            blocks += 1;
            if header & 1 == 1 {
                break;
            }
        }
        let adler = u32::from_be_bytes([idat[pos], idat[pos + 1], idat[pos + 2], idat[pos + 3]]);
        assert_eq!(adler, adler32(&raw), "zlib Adler-32 trailer");
        assert_eq!(pos + 4, idat.len(), "zlib trailer must end the stream");
        (raw, blocks)
    }

    #[test]
    fn crc32_matches_standard_vector() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32(b""), 0);
    }

    #[test]
    fn adler32_matches_standard_vector() {
        assert_eq!(adler32(b"Wikipedia"), 0x11E6_0398);
        assert_eq!(adler32(b""), 1);
    }

    #[test]
    fn png_has_signature_and_ordered_chunks() {
        let bytes = encode_png(2, 1, &red_green()).expect("encode PNG");
        assert_eq!(&bytes[..8], &PNG_SIGNATURE[..]);
        let chunks = png_chunks(&bytes);
        let kinds: Vec<&[u8; 4]> = chunks.iter().map(|(kind, _)| kind).collect();
        assert_eq!(kinds, vec![b"IHDR", b"IDAT", b"IEND"]);
    }

    #[test]
    fn ihdr_declares_8bit_rgba_without_interlace() {
        let bytes = encode_png(2, 3, &[0u8; 24]).expect("encode PNG");
        let chunks = png_chunks(&bytes);
        let header = &chunks[0].1;
        assert_eq!(header.len(), 13);
        assert_eq!(&header[..4], &2u32.to_be_bytes());
        assert_eq!(&header[4..8], &3u32.to_be_bytes());
        assert_eq!(header[8], 8, "bit depth");
        assert_eq!(header[9], 6, "RGBA colour type");
        assert_eq!(
            &header[10..13],
            &[0u8, 0, 0],
            "compression/filter/interlace"
        );
    }

    #[test]
    fn idat_is_a_valid_stored_zlib_stream() {
        let pixels = red_green();
        let bytes = encode_png(2, 1, &pixels).expect("encode PNG");
        let chunks = png_chunks(&bytes);
        let (raw, blocks) = stored_payload(&chunks[1].1);
        assert_eq!(blocks, 1);
        let mut expected = Vec::with_capacity(1 + pixels.len());
        expected.push(0); // filter type None
        expected.extend_from_slice(&pixels);
        assert_eq!(raw, expected);
    }

    #[test]
    fn idat_ends_with_empty_iend_and_length_is_consistent() {
        let bytes = encode_png(2, 1, &red_green()).expect("encode PNG");
        let chunks = png_chunks(&bytes);
        assert_eq!(&chunks[2].0, b"IEND");
        assert!(chunks[2].1.is_empty());
        let idat_len = chunks[1].1.len();
        assert_eq!(bytes.len(), 8 + 12 + 13 + 12 + idat_len + 12);
        assert_eq!(&bytes[bytes.len() - 8..bytes.len() - 4], &b"IEND"[..]);
        assert_eq!(&bytes[bytes.len() - 4..], &crc32(b"IEND").to_be_bytes()[..]);
    }

    #[test]
    fn large_raster_is_split_across_multiple_stored_blocks() {
        // 200 * 100 * 4 = 80,000 raw pixel bytes plus 100 filter bytes.
        let pixels = vec![0x7Fu8; 200 * 100 * 4];
        let bytes = encode_png(200, 100, &pixels).expect("encode PNG");
        let chunks = png_chunks(&bytes);
        let (raw, blocks) = stored_payload(&chunks[1].1);
        assert!(blocks >= 2, "80 KiB must span more than one stored block");
        assert_eq!(raw.len(), 100 + pixels.len());
    }

    #[test]
    fn png_rejects_mismatched_pixel_buffer() {
        assert!(matches!(
            encode_png(2, 2, &[0u8; 8]),
            Err(ConversionError::InvalidInput { .. })
        ));
    }

    #[test]
    fn png_rejects_overflowing_dimensions() {
        assert!(matches!(
            encode_png(u32::MAX, u32::MAX, &[]),
            Err(ConversionError::InvalidInput { .. })
        ));
    }

    #[test]
    fn ppm_has_p6_header_and_dropped_alpha() {
        let bytes = encode_ppm(2, 1, &red_green()).expect("encode PPM");
        let header = b"P6\n2 1\n255\n";
        assert_eq!(header.len(), 11);
        assert_eq!(&bytes[..11], &header[..]);
        assert_eq!(bytes.len(), 11 + 6);
        assert_eq!(&bytes[11..], &[255u8, 0, 0, 0, 255, 0]);
    }

    #[test]
    fn ppm_rejects_mismatched_pixel_buffer() {
        assert!(matches!(
            encode_ppm(1, 1, &[0u8; 3]),
            Err(ConversionError::InvalidInput { .. })
        ));
    }

    #[test]
    fn empty_rasters_still_encode_valid_files() {
        let png = encode_png(0, 0, &[]).expect("empty PNG");
        let chunks = png_chunks(&png);
        assert_eq!(chunks.len(), 3);
        assert_eq!(&chunks[0].1[..8], &[0u8; 8], "zero dimensions");
        let ppm = encode_ppm(0, 0, &[]).expect("empty PPM");
        assert_eq!(ppm.as_slice(), &b"P6\n0 0\n255\n"[..]);
    }

    #[test]
    fn image_format_classifies_extensions_and_labels_stably() {
        assert_eq!(ImageFormat::from_extension(".PNG"), Some(ImageFormat::Png));
        assert_eq!(ImageFormat::from_extension("ppm"), Some(ImageFormat::Ppm));
        assert_eq!(ImageFormat::from_extension("pnm"), Some(ImageFormat::Ppm));
        assert_eq!(ImageFormat::from_extension("BMP"), Some(ImageFormat::Bmp));
        assert_eq!(ImageFormat::from_extension(".tif"), Some(ImageFormat::Tiff));
        assert_eq!(ImageFormat::from_extension("TIFF"), Some(ImageFormat::Tiff));
        // JPEG is served by the verified renderer-backed encoder, not here.
        assert_eq!(ImageFormat::from_extension("jpg"), None);
        assert_eq!(ImageFormat::Png.extension(), "png");
        assert_eq!(ImageFormat::Ppm.label(), "ppm");
        assert_eq!(ImageFormat::Bmp.extension(), "bmp");
        assert_eq!(ImageFormat::Tiff.extension(), "tiff");
        assert_eq!(ImageFormat::Png.to_string(), "png");
        assert_eq!(ImageFormat::all().len(), 4);
    }

    #[test]
    fn encode_dispatch_matches_each_format() {
        let pixels = red_green();
        for &format in ImageFormat::all() {
            let dispatched = encode(2, 1, &pixels, format).expect("format dispatch");
            assert_eq!(
                dispatched,
                match format {
                    ImageFormat::Png => encode_png(2, 1, &pixels),
                    ImageFormat::Ppm => encode_ppm(2, 1, &pixels),
                    ImageFormat::Bmp => encode_bmp(2, 1, &pixels),
                    ImageFormat::Tiff => encode_tiff(2, 1, &pixels),
                }
                .expect("direct encoder")
            );
            assert!(matches!(
                encode(2, 1, &[0u8; 3], format),
                Err(ConversionError::InvalidInput { .. })
            ));
        }
    }

    /// Read a little-endian `u16` at `offset` with explicit bounds checking.
    fn le_u16(bytes: &[u8], offset: usize) -> u16 {
        let slice = bytes.get(offset..offset + 2).expect("u16 in range");
        u16::from_le_bytes([slice[0], slice[1]])
    }

    /// Read a little-endian `u32` at `offset` with explicit bounds checking.
    fn le_u32(bytes: &[u8], offset: usize) -> u32 {
        let slice = bytes.get(offset..offset + 4).expect("u32 in range");
        u32::from_le_bytes([slice[0], slice[1], slice[2], slice[3]])
    }

    #[test]
    fn png_header_and_crc_round_trip_parse() {
        let pixels = vec![255u8, 0, 0, 255, 0, 255, 0, 255];
        let bytes = encode_png(2, 1, &pixels).expect("encode PNG");
        // `png_chunks` verifies each chunk's CRC-32 as it walks the file.
        let chunks = png_chunks(&bytes);
        assert_eq!(chunks.len(), 3);
        let ihdr = &chunks[0].1;
        assert_eq!(&ihdr[..4], &2u32.to_be_bytes());
        assert_eq!(&ihdr[4..8], &1u32.to_be_bytes());
        assert_eq!(&ihdr[8..10], &[8, 6]);
        let (raw, _) = stored_payload(&chunks[1].1);
        assert_eq!(raw, [&[0u8][..], pixels.as_slice()].concat());
        // Re-encoding the same pixels must reproduce every byte, CRC included.
        assert_eq!(bytes, encode_png(2, 1, &pixels).expect("re-encode PNG"));
    }

    #[test]
    fn raster_encoders_enforce_dimension_and_size_bounds() {
        for &format in ImageFormat::all() {
            // A dimension above the per-axis limit is rejected before allocation.
            assert!(
                matches!(
                    encode(MAX_IMAGE_DIMENSION + 1, 1, &[], format),
                    Err(ConversionError::InvalidInput { .. })
                ),
                "dimension bound for {format}"
            );
            // An area above the pixel limit is rejected even though each axis fits.
            assert!(
                matches!(
                    encode(200_000, 200_000, &[], format),
                    Err(ConversionError::InvalidInput { .. })
                ),
                "pixel-count bound for {format}"
            );
        }
        // A hostile u32::MAX pair is rejected, never multiplied into a buffer.
        assert!(matches!(
            encode(u32::MAX, u32::MAX, &[], ImageFormat::Png),
            Err(ConversionError::InvalidInput { .. })
        ));
    }

    #[test]
    fn every_encoder_rejects_a_short_or_overlong_pixel_buffer() {
        for &format in ImageFormat::all() {
            assert!(matches!(
                encode(2, 2, &[0u8; 15], format),
                Err(ConversionError::InvalidInput { .. })
            ));
            assert!(matches!(
                encode(1, 1, &[0u8; 5], format),
                Err(ConversionError::InvalidInput { .. })
            ));
        }
    }

    #[test]
    fn all_image_formats_are_byte_deterministic() {
        let pixels: Vec<u8> = (0..(4 * 4 * 4)).map(|index| index as u8).collect();
        for &format in ImageFormat::all() {
            let first = encode(4, 4, &pixels, format).expect("first encoding");
            let second = encode(4, 4, &pixels, format).expect("second encoding");
            assert_eq!(first, second, "determinism for {format}");
            assert!(!first.is_empty());
        }
    }

    #[test]
    fn bmp_header_fields_and_bottom_up_bgra_rows() {
        // Two rows: top = opaque red then green, bottom = opaque blue then white.
        let pixels = vec![
            255, 0, 0, 255, 0, 255, 0, 255, // top row
            0, 0, 255, 255, 255, 255, 255, 255, // bottom row
        ];
        let bytes = encode_bmp(2, 2, &pixels).expect("encode BMP");
        assert_eq!(&bytes[..2], b"BM");
        assert_eq!(le_u32(&bytes, 2) as usize, bytes.len());
        assert_eq!(le_u32(&bytes, 10), 54, "pixel data offset");
        assert_eq!(le_u32(&bytes, 14), 40, "BITMAPINFOHEADER size");
        assert_eq!(le_u32(&bytes, 18), 2, "width");
        assert_eq!(le_u32(&bytes, 22), 2, "height (top-down declaration)");
        assert_eq!(le_u16(&bytes, 26), 1, "planes");
        assert_eq!(le_u16(&bytes, 28), 32, "bits per pixel");
        assert_eq!(le_u32(&bytes, 30), 0, "BI_RGB compression");
        assert_eq!(le_u32(&bytes, 34), 16, "pixel byte count");
        // Rows are stored bottom-up; within a row pixels are B,G,R,A. The
        // bottom row (blue, white) therefore precedes the top row (red, green).
        assert_eq!(
            &bytes[54..],
            &[
                255, 0, 0, 255, 255, 255, 255, 255, 0, 0, 255, 255, 0, 255, 0, 255
            ]
        );
    }

    #[test]
    fn tiff_header_ifd_tags_and_rgba_payload() {
        let pixels = red_green();
        let bytes = encode_tiff(2, 1, &pixels).expect("encode TIFF");
        assert_eq!(&bytes[..2], b"II", "little-endian marker");
        assert_eq!(le_u16(&bytes, 2), 42, "TIFF magic");
        assert_eq!(le_u32(&bytes, 4), 8, "IFD offset");
        assert_eq!(le_u16(&bytes, 8), 11, "baseline tag count");

        // Entries are 12 bytes each starting at 10; collect (tag, type, value).
        let mut tags = Vec::new();
        for index in 0..11 {
            let base = 10 + index * 12;
            tags.push((
                le_u16(&bytes, base),
                le_u16(&bytes, base + 2),
                le_u32(&bytes, base + 8),
            ));
        }
        let ordered: Vec<u16> = tags.iter().map(|entry| entry.0).collect();
        assert_eq!(
            ordered,
            vec![256, 257, 258, 259, 262, 273, 277, 278, 279, 284, 338]
        );
        let entry = |tag: u16| tags.iter().find(|item| item.0 == tag).expect("tag exists");
        assert_eq!(entry(256).2, 2, "ImageWidth");
        assert_eq!(entry(257).2, 1, "ImageLength");
        assert_eq!(entry(259).2, 1, "Compression = none");
        assert_eq!(entry(262).2, 2, "Photometric = RGB");
        assert_eq!(entry(277).2, 4, "SamplesPerPixel");
        assert_eq!(entry(338).2, 2, "ExtraSamples = unassociated alpha");
        assert_eq!(entry(278).2, 1, "RowsPerStrip");
        assert_eq!(entry(279).2, 8, "StripByteCounts");

        // BitsPerSample is an out-of-line array of four 16-bit 8s at byte 146.
        let bits_offset = entry(258).2 as usize;
        assert_eq!(bits_offset, 146);
        for index in 0..4 {
            assert_eq!(le_u16(&bytes, bits_offset + index * 2), 8);
        }
        let strip_offset = entry(273).2 as usize;
        assert_eq!(strip_offset, 154);
        assert_eq!(&bytes[strip_offset..], &pixels[..]);
    }
}
