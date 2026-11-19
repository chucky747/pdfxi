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
}
