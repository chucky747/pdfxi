//! Minimal, dependency-free image encoders used by the `render` command.
//!
//! PNG output is a fully valid, non-interlaced, 8-bit RGBA image. To avoid
//! adding an image crate, its `IDAT` zlib stream uses *stored* (uncompressed)
//! deflate blocks: the file is larger than a compressed PNG but every standard
//! PNG decoder accepts it. PPM output uses the binary P6 subformat with the
//! alpha channel dropped.
//!
//! Both encoders validate their inputs with checked arithmetic so a hostile or
//! accidental size cannot overflow the buffer length computation.

use anyhow::{Result, anyhow, bail};

/// The fixed eight-byte signature at the start of every PNG file.
const PNG_SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

/// Largest number of raw bytes a single deflate stored block can carry.
// `usize` is at least 32 bits on every supported target, so widening a `u16`
// with `as` is lossless; `From::from` is not yet usable in a `const`.
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
fn push_chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) -> Result<()> {
    let length = u32::try_from(data.len()).map_err(|_| {
        anyhow!(
            "PNG chunk '{}' exceeds 2^32 bytes",
            String::from_utf8_lossy(kind)
        )
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

/// Compute the byte length of an RGBA raster, rejecting overflowing sizes.
fn pixel_len(width: u32, height: u32, channels: usize) -> Result<usize> {
    let width = usize::try_from(width).map_err(|_| anyhow!("image width does not fit in usize"))?;
    let height =
        usize::try_from(height).map_err(|_| anyhow!("image height does not fit in usize"))?;
    let area = width
        .checked_mul(height)
        .ok_or_else(|| anyhow!("image dimensions overflow the platform size"))?;
    area.checked_mul(channels)
        .ok_or_else(|| anyhow!("image pixel buffer size overflows the platform size"))
}

/// Encode tightly packed 8-bit RGBA pixels as a valid PNG file.
///
/// `width` and `height` are the raster dimensions and `rgba` must contain
/// exactly `width * height * 4` bytes in row-major order. Every scanline is
/// written with PNG filter type 0 (None), so a stored deflate block reproduces
/// the raw bytes verbatim. A mismatched buffer or an overflowing size returns
/// an error rather than panicking.
pub fn encode_png(width: u32, height: u32, rgba: &[u8]) -> Result<Vec<u8>> {
    let expected = pixel_len(width, height, 4)?;
    if rgba.len() != expected {
        bail!("RGBA buffer has {} bytes, expected {expected}", rgba.len());
    }

    let width_usize =
        usize::try_from(width).map_err(|_| anyhow!("image width does not fit in usize"))?;
    let height_usize =
        usize::try_from(height).map_err(|_| anyhow!("image height does not fit in usize"))?;
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
/// `width`, `height` and `rgba` are validated exactly as in [`encode_png`].
pub fn encode_ppm(width: u32, height: u32, rgba: &[u8]) -> Result<Vec<u8>> {
    let expected = pixel_len(width, height, 4)?;
    if rgba.len() != expected {
        bail!("RGBA buffer has {} bytes, expected {expected}", rgba.len());
    }

    let mut out = format!("P6\n{width} {height}\n255\n").into_bytes();
    // `pixel_len` already proved `width * height * 4` fits, and 3/4 of it fits.
    out.reserve(expected / 4 * 3);
    for pixel in rgba.as_chunks::<4>().0 {
        out.extend_from_slice(&pixel[..3]);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A two-pixel raster: opaque red then opaque green.
    fn red_green() -> Vec<u8> {
        vec![255, 0, 0, 255, 0, 255, 0, 255]
    }

    /// Split a PNG into its ordered `(type, data)` chunks.
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
            assert_eq!(stored_crc, crc32(&checked), "chunk checksum mismatch");
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
        // The 12-byte IEND chunk is [length=0][type][crc], so its type sits
        // eight bytes before the end of the file.
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
        assert!(encode_png(2, 2, &[0u8; 8]).is_err());
    }

    #[test]
    fn png_rejects_overflowing_dimensions() {
        assert!(encode_png(u32::MAX, u32::MAX, &[]).is_err());
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
        assert!(encode_ppm(1, 1, &[0u8; 3]).is_err());
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
}
