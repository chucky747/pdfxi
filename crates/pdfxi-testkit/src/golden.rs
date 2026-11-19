//! Small golden-image comparison helpers with deterministic dimensions.
//!
//! Rendering tests need a stable way to compare a rendered page against a
//! committed baseline. This module deliberately stores pixels as raw 8-bit
//! grayscale samples so the testkit does not depend on an image crate. A
//! [`GoldenImage`] always carries its own width and height, and every
//! comparison first checks that two images share the same dimensions.

use std::error::Error;
use std::fmt;

/// Errors produced when constructing or comparing [`GoldenImage`] values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GoldenError {
    /// `width * height` overflowed `usize`.
    DimensionsOverflow,
    /// The supplied pixel buffer length did not match `width * height`.
    PixelCountMismatch {
        /// The number of samples required by the dimensions.
        expected: usize,
        /// The number of samples actually supplied.
        actual: usize,
    },
    /// Two images with different dimensions were compared.
    DimensionMismatch {
        /// Dimensions of the first image `(width, height)`.
        left: (u32, u32),
        /// Dimensions of the second image `(width, height)`.
        right: (u32, u32),
    },
}

impl fmt::Display for GoldenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DimensionsOverflow => write!(f, "image dimensions overflow usize"),
            Self::PixelCountMismatch { expected, actual } => {
                write!(f, "expected {expected} pixels, got {actual}")
            }
            Self::DimensionMismatch { left, right } => {
                write!(f, "image dimensions differ: {left:?} vs {right:?}")
            }
        }
    }
}

impl Error for GoldenError {}

/// A grayscale golden image with fixed, explicit dimensions.
///
/// Pixels are stored row-major; the buffer length is always exactly
/// `width * height`. Construction validates that invariant so comparisons never
/// read out of bounds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoldenImage {
    width: u32,
    height: u32,
    pixels: Vec<u8>,
}

impl GoldenImage {
    /// Build a golden image from explicit dimensions and raw pixels.
    ///
    /// Returns [`GoldenError::DimensionsOverflow`] if `width * height` overflows
    /// and [`GoldenError::PixelCountMismatch`] if `pixels.len()` differs from the
    /// product.
    pub fn new(width: u32, height: u32, pixels: Vec<u8>) -> Result<Self, GoldenError> {
        let expected = (width as usize)
            .checked_mul(height as usize)
            .ok_or(GoldenError::DimensionsOverflow)?;
        if pixels.len() != expected {
            return Err(GoldenError::PixelCountMismatch {
                expected,
                actual: pixels.len(),
            });
        }
        Ok(Self {
            width,
            height,
            pixels,
        })
    }

    /// Build a solid-gray image of the given dimensions filled with `value`.
    pub fn solid(width: u32, height: u32, value: u8) -> Result<Self, GoldenError> {
        let len = (width as usize)
            .checked_mul(height as usize)
            .ok_or(GoldenError::DimensionsOverflow)?;
        Self::new(width, height, vec![value; len])
    }

    /// The image width in pixels.
    pub fn width(&self) -> u32 {
        self.width
    }

    /// The image height in pixels.
    pub fn height(&self) -> u32 {
        self.height
    }

    /// The raw row-major grayscale samples.
    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    /// Mean absolute per-pixel difference against `other`, in `0.0..=255.0`.
    ///
    /// Returns [`GoldenError::DimensionMismatch`] when dimensions differ. Two
    /// empty images of equal dimensions compare equal with a difference of zero.
    pub fn mean_absolute_difference(&self, other: &Self) -> Result<f64, GoldenError> {
        if self.width != other.width || self.height != other.height {
            return Err(GoldenError::DimensionMismatch {
                left: (self.width, self.height),
                right: (other.width, other.height),
            });
        }
        if self.pixels.is_empty() {
            return Ok(0.0);
        }
        let total: u64 = self
            .pixels
            .iter()
            .zip(other.pixels.iter())
            .map(|(a, b)| a.abs_diff(*b) as u64)
            .sum();
        Ok(total as f64 / self.pixels.len() as f64)
    }

    /// Whether the mean absolute difference is at most `tolerance`.
    pub fn matches(&self, other: &Self, tolerance: f64) -> Result<bool, GoldenError> {
        Ok(self.mean_absolute_difference(other)? <= tolerance)
    }

    /// Fraction of pixels that are byte-identical, in `0.0..=1.0`.
    ///
    /// Returns [`GoldenError::DimensionMismatch`] when dimensions differ. Two
    /// empty images of equal dimensions score `1.0`.
    pub fn identical_fraction(&self, other: &Self) -> Result<f64, GoldenError> {
        if self.width != other.width || self.height != other.height {
            return Err(GoldenError::DimensionMismatch {
                left: (self.width, self.height),
                right: (other.width, other.height),
            });
        }
        if self.pixels.is_empty() {
            return Ok(1.0);
        }
        let same = self
            .pixels
            .iter()
            .zip(other.pixels.iter())
            .filter(|(a, b)| a == b)
            .count();
        Ok(same as f64 / self.pixels.len() as f64)
    }

    /// Deterministic FNV-1a 64-bit fingerprint over dimensions and pixels.
    ///
    /// Useful as a stable baseline key: identical images always produce the
    /// same value, and changing either a dimension or a sample changes it.
    pub fn fingerprint(&self) -> u64 {
        let mut hash = 0xcbf2_9ce4_8422_2325u64;
        let mut mix = |byte: u8| {
            hash ^= byte as u64;
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        };
        for byte in self.width.to_le_bytes() {
            mix(byte);
        }
        for byte in self.height.to_le_bytes() {
            mix(byte);
        }
        for &byte in &self.pixels {
            mix(byte);
        }
        hash
    }
}

/// Dictionary keys whose key/value pairs [`normalize_volatile_fields`] removes
/// before a page body is snapshotted (timestamps, file identifiers, and tool
/// provenance).
///
/// These are the *only* fields normalized: every other dictionary entry is
/// preserved verbatim. [`ContentSnapshot`] applies one separate, documented
/// transformation — it ignores trailing ASCII whitespace.
pub const VOLATILE_DICTIONARY_FIELDS: &[&str] = &["CreationDate", "ModDate", "ID", "Producer"];

/// Errors produced while building a [`PageTreeSnapshot`] from PDF bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotError {
    /// A `/Type /Page` object had none of the five recognised box arrays.
    MissingPageBox(u32),
    /// No `/Type /Page` object was found in the input.
    NoPages,
}

impl fmt::Display for SnapshotError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingPageBox(number) => {
                write!(f, "page object {number} has no recognised box array")
            }
            Self::NoPages => write!(f, "no /Type /Page objects found"),
        }
    }
}

impl Error for SnapshotError {}

/// Deterministic FNV-1a 64-bit hash over `bytes`.
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for byte in bytes {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// Remove documented volatile key/value pairs from a PDF object body.
///
/// For every key in [`VOLATILE_DICTIONARY_FIELDS`], the key token and exactly
/// one following PDF value are dropped. Supported value forms are literal
/// strings `(...)`, hex strings `<...>`, dictionaries `<<...>>`, arrays `[...]`,
/// names, numbers, and indirect references `N 0 R`. All other bytes are copied
/// verbatim, so this cannot mask a change to a structural field such as a page
/// box or `/Rotate`. The input is treated as raw bytes; the result is decoded
/// lossily as UTF-8 for convenience.
pub fn normalize_volatile_fields(body: &str) -> String {
    let bytes = body.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut cursor = 0usize;
    while cursor < bytes.len() {
        if bytes[cursor] == b'/' && cursor + 1 < bytes.len() {
            let name_start = cursor + 1;
            let mut name_end = name_start;
            while name_end < bytes.len() && is_name_byte(bytes[name_end]) {
                name_end += 1;
            }
            let name = &body[name_start..name_end];
            if VOLATILE_DICTIONARY_FIELDS.contains(&name) {
                cursor = name_end;
                cursor = skip_whitespace(bytes, cursor);
                cursor = skip_value(bytes, cursor);
                continue;
            }
        }
        out.push(bytes[cursor]);
        cursor += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Whether `byte` may appear inside a PDF name token.
fn is_name_byte(byte: u8) -> bool {
    !byte.is_ascii_whitespace()
        && !matches!(
            byte,
            b'/' | b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'%'
        )
}

/// Advance past ASCII PDF white space.
fn skip_whitespace(bytes: &[u8], mut at: usize) -> usize {
    while at < bytes.len() && bytes[at].is_ascii_whitespace() {
        at += 1;
    }
    at
}

/// Advance past one PDF value starting at `at`.
fn skip_value(bytes: &[u8], at: usize) -> usize {
    if at >= bytes.len() {
        return at;
    }
    match bytes[at] {
        b'(' => skip_delimited(bytes, at, b'(', b')', true),
        b'[' => skip_delimited(bytes, at, b'[', b']', false),
        b'<' if bytes.get(at + 1) == Some(&b'<') => skip_double_angle(bytes, at),
        b'<' => skip_until(bytes, at + 1, b'>'),
        _ => {
            let mut end = at;
            while end < bytes.len()
                && !bytes[end].is_ascii_whitespace()
                && !matches!(bytes[end], b'/' | b'(' | b')' | b'<' | b'>' | b'[' | b']')
            {
                end += 1;
            }
            // An integer token may be the object number of an indirect
            // reference; consume the generation and `R` as part of the value.
            if bytes[at..end].iter().all(u8::is_ascii_digit) && end > at {
                let generation = skip_whitespace(bytes, end);
                let mut after_generation = generation;
                while after_generation < bytes.len() && bytes[after_generation].is_ascii_digit() {
                    after_generation += 1;
                }
                if after_generation > generation {
                    let reference_end = skip_whitespace(bytes, after_generation);
                    if bytes.get(reference_end) == Some(&b'R') {
                        end = reference_end + 1;
                    }
                }
            }
            end
        }
    }
}

/// Advance past a balanced, delimiter-enclosed value.
fn skip_delimited(bytes: &[u8], at: usize, open: u8, close: u8, escapes: bool) -> usize {
    let mut depth = 0usize;
    let mut index = at;
    while index < bytes.len() {
        match bytes[index] {
            b'\\' if escapes => {
                index = index.saturating_add(2);
                continue;
            }
            byte if byte == open => depth = depth.saturating_add(1),
            byte if byte == close => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return index + 1;
                }
            }
            _ => {}
        }
        index += 1;
    }
    bytes.len()
}

/// Advance past a `<< ... >>` dictionary value.
fn skip_double_angle(bytes: &[u8], at: usize) -> usize {
    let mut index = at + 2;
    while index + 1 < bytes.len() {
        if bytes[index] == b'>' && bytes[index + 1] == b'>' {
            return index + 2;
        }
        index += 1;
    }
    bytes.len()
}

/// Advance past a value terminated by `terminator`, including that byte.
fn skip_until(bytes: &[u8], mut at: usize, terminator: u8) -> usize {
    while at < bytes.len() && bytes[at] != terminator {
        at += 1;
    }
    if at < bytes.len() { at + 1 } else { at }
}

/// A stable snapshot of a decoded content stream.
///
/// The snapshot preserves every byte of the stream except trailing ASCII
/// whitespace, which is the single documented volatile transformation: different
/// PDF writers append a different number of newlines after the final operator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContentSnapshot {
    text: String,
}

impl ContentSnapshot {
    /// Build a snapshot from raw content-stream bytes.
    ///
    /// The bytes are interpreted lossily as UTF-8 (PDF operator syntax is ASCII)
    /// and trailing ASCII whitespace is removed.
    pub fn new(bytes: &[u8]) -> Self {
        let mut text = String::from_utf8_lossy(bytes).into_owned();
        while text.ends_with(|ch: char| ch.is_ascii_whitespace()) {
            text.pop();
        }
        Self { text }
    }

    /// The normalized snapshot text.
    pub fn as_str(&self) -> &str {
        &self.text
    }

    /// A stable fingerprint of the normalized content.
    pub fn fingerprint(&self) -> u64 {
        fnv1a(self.text.as_bytes())
    }

    /// Whether two snapshots carry identical normalized content.
    pub fn matches(&self, other: &Self) -> bool {
        self.text == other.text
    }
}

/// One box array found on a page object, keyed by its PDF name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoxSnapshot {
    /// Box name without the leading slash, for example `MediaBox`.
    pub name: String,
    /// The four raw coordinate tokens, in PDF order.
    pub values: [String; 4],
}

/// Structural snapshot of a single page object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageSnapshot {
    /// Indirect object number of the `/Page` object.
    pub object_number: u32,
    /// Box arrays found on the page, in canonical order.
    pub boxes: Vec<BoxSnapshot>,
    /// The `/Rotate` value in degrees, or zero when absent.
    pub rotation: i64,
}

/// A stable, ordered snapshot of a document's page tree.
///
/// The snapshot records exactly three structural facts per page: its object
/// number, the five page-box arrays, and its `/Rotate` value. Other dictionary
/// entries are out of scope for this snapshot rather than normalized. Before
/// extraction, [`normalize_volatile_fields`] removes the key/value pairs named
/// in [`VOLATILE_DICTIONARY_FIELDS`], so a changed `/ModDate` or `/ID` cannot
/// perturb the structural fields that *are* recorded. Any change to a recorded
/// field (a box coordinate, rotation, or object number) changes the
/// [`Self::fingerprint`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageTreeSnapshot {
    pages: Vec<PageSnapshot>,
}

impl PageTreeSnapshot {
    /// Extract a page-tree snapshot from the bytes of a classic-xref PDF.
    ///
    /// This helper targets the simple object layout emitted by `pdfxi-testkit`:
    /// one `N 0 obj ... endobj` block per object with ASCII dictionary syntax.
    /// It performs no network or GUI access.
    ///
    /// # Errors
    ///
    /// Returns [`SnapshotError::NoPages`] when no `/Type /Page` object exists,
    /// or [`SnapshotError::MissingPageBox`] when a page carries none of the
    /// five recognised box arrays.
    pub fn from_pdf(bytes: &[u8]) -> Result<Self, SnapshotError> {
        let text = String::from_utf8_lossy(bytes);
        let mut pages = Vec::new();
        for (number, raw_body) in object_bodies(&text) {
            if !raw_body.contains("/Type /Page ") {
                continue;
            }
            let body = normalize_volatile_fields(raw_body);
            let mut boxes = Vec::new();
            for key in ["MediaBox", "CropBox", "BleedBox", "TrimBox", "ArtBox"] {
                if let Some(values) = extract_box(&body, key) {
                    boxes.push(BoxSnapshot {
                        name: key.to_string(),
                        values,
                    });
                }
            }
            if boxes.is_empty() {
                return Err(SnapshotError::MissingPageBox(number));
            }
            pages.push(PageSnapshot {
                object_number: number,
                boxes,
                rotation: extract_rotation(&body),
            });
        }
        if pages.is_empty() {
            return Err(SnapshotError::NoPages);
        }
        Ok(Self { pages })
    }

    /// The page snapshots in document order.
    pub fn pages(&self) -> &[PageSnapshot] {
        &self.pages
    }

    /// A stable fingerprint over object numbers, boxes, and rotations.
    pub fn fingerprint(&self) -> u64 {
        let mut encoded = String::new();
        for page in &self.pages {
            encoded.push_str(&format!("p{}:", page.object_number));
            for snapshot in &page.boxes {
                encoded.push_str(&format!(
                    "{}=[{}];",
                    snapshot.name,
                    snapshot.values.join(",")
                ));
            }
            encoded.push_str(&format!("r{}|", page.rotation));
        }
        fnv1a(encoded.as_bytes())
    }

    /// Whether two page-tree snapshots are structurally identical.
    pub fn matches(&self, other: &Self) -> bool {
        self.pages == other.pages
    }
}

/// Yield `(object number, body)` pairs for every `N 0 obj ... endobj` block.
fn object_bodies(text: &str) -> Vec<(u32, &str)> {
    let mut result = Vec::new();
    let marker = " 0 obj\n";
    for chunk in text.split("\nendobj\n") {
        let Some(at) = chunk.find(marker) else {
            continue;
        };
        let mut start = at;
        while start > 0 && chunk.as_bytes()[start - 1].is_ascii_digit() {
            start -= 1;
        }
        if start == at {
            continue;
        }
        let Ok(number) = chunk[start..at].parse::<u32>() else {
            continue;
        };
        result.push((number, &chunk[at + marker.len()..]));
    }
    result
}

/// Extract the four coordinate tokens of a named box array from a page body.
fn extract_box(body: &str, key: &str) -> Option<[String; 4]> {
    let marker = format!("/{key} [");
    let at = body.find(&marker)? + marker.len();
    let rest = &body[at..];
    let end = rest.find(']')?;
    let tokens: Vec<&str> = rest[..end].split_whitespace().collect();
    if tokens.len() != 4 {
        return None;
    }
    let mut values = [String::new(), String::new(), String::new(), String::new()];
    for (slot, token) in values.iter_mut().zip(tokens) {
        *slot = token.to_string();
    }
    Some(values)
}

/// Read a page's `/Rotate` value, returning zero when absent or malformed.
fn extract_rotation(body: &str) -> i64 {
    const KEY: &str = "/Rotate ";
    let Some(at) = body.find(KEY) else {
        return 0;
    };
    let rest = &body[at + KEY.len()..];
    let token: String = rest
        .chars()
        .take_while(|ch| ch.is_ascii_digit() || *ch == '-' || *ch == '+')
        .collect();
    token.parse::<i64>().unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn solid_image_has_expected_dimensions_and_pixels() {
        let image = GoldenImage::solid(3, 2, 7).expect("valid image");
        assert_eq!(image.width(), 3);
        assert_eq!(image.height(), 2);
        assert_eq!(image.pixels(), &[7, 7, 7, 7, 7, 7]);
    }

    #[test]
    fn new_rejects_wrong_pixel_count() {
        let err = GoldenImage::new(2, 2, vec![0; 3]).expect_err("bad length");
        assert_eq!(
            err,
            GoldenError::PixelCountMismatch {
                expected: 4,
                actual: 3
            }
        );
    }

    #[test]
    fn identical_images_have_zero_difference() {
        let a = GoldenImage::solid(4, 4, 128).expect("valid image");
        let b = GoldenImage::solid(4, 4, 128).expect("valid image");
        assert_eq!(a.mean_absolute_difference(&b).expect("same dims"), 0.0);
        assert_eq!(a.identical_fraction(&b).expect("same dims"), 1.0);
        assert!(a.matches(&b, 0.0).expect("same dims"));
    }

    #[test]
    fn difference_is_mean_absolute_sample_delta() {
        let a = GoldenImage::solid(2, 1, 0).expect("valid image");
        let b = GoldenImage::new(2, 1, vec![10, 20]).expect("valid image");
        assert_eq!(a.mean_absolute_difference(&b).expect("same dims"), 15.0);
        assert_eq!(a.identical_fraction(&b).expect("same dims"), 0.0);
        assert!(a.matches(&b, 15.0).expect("same dims"));
        assert!(!a.matches(&b, 14.9).expect("same dims"));
    }

    #[test]
    fn comparing_different_dimensions_is_an_error() {
        let a = GoldenImage::solid(2, 2, 0).expect("valid image");
        let b = GoldenImage::solid(3, 2, 0).expect("valid image");
        assert_eq!(
            a.mean_absolute_difference(&b),
            Err(GoldenError::DimensionMismatch {
                left: (2, 2),
                right: (3, 2),
            })
        );
        assert!(a.fingerprint() != b.fingerprint());
    }

    #[test]
    fn fingerprint_is_deterministic_and_content_sensitive() {
        let a = GoldenImage::solid(2, 2, 5).expect("valid image");
        let b = GoldenImage::solid(2, 2, 5).expect("valid image");
        let c = GoldenImage::solid(2, 2, 6).expect("valid image");
        assert_eq!(a.fingerprint(), b.fingerprint());
        assert_ne!(a.fingerprint(), c.fingerprint());
    }

    #[test]
    fn empty_images_compare_cleanly() {
        let a = GoldenImage::new(0, 0, vec![]).expect("valid image");
        let b = GoldenImage::new(0, 0, vec![]).expect("valid image");
        assert_eq!(a.mean_absolute_difference(&b).expect("same dims"), 0.0);
        assert_eq!(a.identical_fraction(&b).expect("same dims"), 1.0);
    }

    #[test]
    fn error_messages_are_descriptive() {
        let mismatch = GoldenError::PixelCountMismatch {
            expected: 4,
            actual: 3,
        };
        assert!(mismatch.to_string().contains("expected 4 pixels, got 3"));
        let dims = GoldenError::DimensionMismatch {
            left: (1, 2),
            right: (3, 4),
        };
        assert!(dims.to_string().contains("dimensions differ"));
    }

    #[test]
    fn content_snapshot_ignores_only_trailing_whitespace() {
        let a = ContentSnapshot::new(b"BT (Hi) Tj ET");
        let b = ContentSnapshot::new(b"BT (Hi) Tj ET\n\n  ");
        assert!(a.matches(&b));
        assert_eq!(a.fingerprint(), b.fingerprint());
        let c = ContentSnapshot::new(b"BT (Hi) Tj ET\n");
        assert_eq!(a.as_str(), "BT (Hi) Tj ET");
        assert!(c.matches(&a));
        let changed = ContentSnapshot::new(b"BT (Bye) Tj ET");
        assert!(!a.matches(&changed));
        assert_ne!(a.fingerprint(), changed.fingerprint());
    }

    #[test]
    fn page_tree_snapshot_records_boxes_and_rotation() {
        let pdf = crate::authoring::build(
            &crate::authoring::by_name("authoring-rotation.pdf").expect("rotation fixture"),
        );
        let snapshot = PageTreeSnapshot::from_pdf(&pdf).expect("page tree");
        assert_eq!(snapshot.pages().len(), 3);
        let rotations: Vec<i64> = snapshot.pages().iter().map(|p| p.rotation).collect();
        assert_eq!(rotations, vec![90, 180, 270]);
        for page in snapshot.pages() {
            assert_eq!(page.boxes.len(), 1);
            assert_eq!(page.boxes[0].name, "MediaBox");
            assert_eq!(
                page.boxes[0].values,
                [
                    "0".to_string(),
                    "0".to_string(),
                    "612".to_string(),
                    "792".to_string()
                ]
            );
        }
    }

    #[test]
    fn normalize_volatile_fields_removes_only_documented_keys() {
        let body = "<< /Type /Page /MediaBox [0 0 612 792] /Rotate 90 \
                    /CreationDate (D:20260101000000Z) /ModDate <0102> \
                    /ID [<00> <01>] /Producer (QA 1.0) /Parent 2 0 R >>";
        let normalized = normalize_volatile_fields(body);
        assert!(!normalized.contains("CreationDate"));
        assert!(!normalized.contains("ModDate"));
        assert!(!normalized.contains("/ID"));
        assert!(!normalized.contains("Producer"));
        assert!(normalized.contains("/MediaBox [0 0 612 792]"));
        assert!(normalized.contains("/Rotate 90"));
        assert!(normalized.contains("/Parent 2 0 R"));
        // A non-volatile key with a similar value form is preserved.
        assert!(normalized.contains("/Type /Page"));
        assert!(VOLATILE_DICTIONARY_FIELDS.contains(&"ModDate"));
    }

    #[test]
    fn page_tree_snapshot_ignores_documented_volatile_fields() {
        let fixture =
            crate::authoring::by_name("authoring-page-boxes.pdf").expect("page-boxes fixture");
        let base = fixture.build();
        let baseline = PageTreeSnapshot::from_pdf(&base).expect("base tree");
        assert_eq!(baseline.pages()[0].boxes.len(), 5);

        // Adding documented volatile keys to the page dictionary must not
        // change the fingerprint.
        let base_text = String::from_utf8(base).expect("ASCII fixture");
        let with_volatile = base_text.replace(
            "/Type /Page ",
            "/Type /Page /ModDate (D:20260101000000Z) /Producer (QA) /ID <00> ",
        );
        let altered = PageTreeSnapshot::from_pdf(with_volatile.as_bytes()).expect("altered tree");
        assert_eq!(baseline.fingerprint(), altered.fingerprint());
    }

    #[test]
    fn page_tree_snapshot_detects_non_volatile_changes() {
        let fixture =
            crate::authoring::by_name("authoring-page-boxes.pdf").expect("page-boxes fixture");
        let base = fixture.build();
        let baseline = PageTreeSnapshot::from_pdf(&base).expect("base tree");

        // A changed box coordinate is a recorded, non-volatile field.
        let changed_box = String::from_utf8(fixture.build())
            .expect("ASCII fixture")
            .replace("/CropBox [10 10 602 782]", "/CropBox [11 10 602 782]");
        let box_snapshot = PageTreeSnapshot::from_pdf(changed_box.as_bytes()).expect("changed box");
        assert_ne!(baseline.fingerprint(), box_snapshot.fingerprint());
        assert!(!baseline.matches(&box_snapshot));

        // A changed rotation is also recorded and therefore detected.
        let changed_rotation = String::from_utf8(fixture.build())
            .expect("ASCII fixture")
            .replace("/Type /Page ", "/Type /Page /Rotate 270 ");
        let rotation_snapshot =
            PageTreeSnapshot::from_pdf(changed_rotation.as_bytes()).expect("changed rotation");
        assert_ne!(baseline.fingerprint(), rotation_snapshot.fingerprint());
        assert!(!baseline.matches(&rotation_snapshot));
    }

    #[test]
    fn page_tree_snapshot_reports_missing_pages() {
        let err = PageTreeSnapshot::from_pdf(b"%PDF-1.7\n%%EOF\n").expect_err("no pages");
        assert_eq!(err, SnapshotError::NoPages);
        assert!(err.to_string().contains("no /Type /Page"));
    }
}
