//! On-disk identity used for external-change detection.
//!
//! The identity is deliberately cheap and lossy in the same way a version
//! control "stat" cache is: length, last-write time, and a SHA-256 of the first
//! and last [`IDENTITY_SAMPLE_BYTES`] bytes. That catches truncation, appends
//! and typical rewrites without hashing a large file on every save.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::time::SystemTime;

use pdfxi_security::crypto::sha::sha256;

/// Number of leading and trailing bytes summarized by the identity hash.
pub(crate) const IDENTITY_SAMPLE_BYTES: usize = 4096;

/// A stable-enough fingerprint of a file captured at load or save time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FileIdentity {
    /// File length in bytes.
    pub(crate) len: u64,
    /// Last-write time, when the platform reports one.
    pub(crate) modified: Option<SystemTime>,
    /// SHA-256 of the first [`IDENTITY_SAMPLE_BYTES`] bytes.
    pub(crate) head: [u8; 32],
    /// SHA-256 of the last [`IDENTITY_SAMPLE_BYTES`] bytes.
    pub(crate) tail: [u8; 32],
}

/// Capture the identity of the file at `path`.
///
/// Only the two bounded samples are read, never the whole file, so the check
/// stays cheap for large documents.
pub(crate) fn capture(path: &Path) -> std::io::Result<FileIdentity> {
    let metadata = std::fs::metadata(path)?;
    let len = metadata.len();
    let mut file = File::open(path)?;
    let head_sample = sample(&mut file, len, 0)?;
    let tail_offset = len.saturating_sub(IDENTITY_SAMPLE_BYTES as u64);
    let tail_sample = if tail_offset == 0 {
        head_sample.clone()
    } else {
        sample(&mut file, len, tail_offset)?
    };
    Ok(FileIdentity {
        len,
        modified: metadata.modified().ok(),
        head: sha256(&head_sample),
        tail: sha256(&tail_sample),
    })
}

/// Read up to [`IDENTITY_SAMPLE_BYTES`] bytes starting at `offset`.
fn sample(file: &mut File, len: u64, offset: u64) -> std::io::Result<Vec<u8>> {
    let available = len.saturating_sub(offset);
    let size = available.min(IDENTITY_SAMPLE_BYTES as u64) as usize;
    file.seek(SeekFrom::Start(offset))?;
    let mut buffer = vec![0u8; size];
    file.read_exact(&mut buffer)?;
    Ok(buffer)
}
