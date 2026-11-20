//! Atomic file replacement for saves.
//!
//! A save never writes the destination in place. It writes a sibling temporary
//! file, flushes it to the device, and renames it over the destination. A
//! failure at any point leaves the original file untouched, and the temporary
//! file is removed on every error path.

use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

/// Distinguishes concurrent saves in one process.
static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Write `bytes` to `path` through a sibling temporary file and a rename.
///
/// The temporary file is created next to the destination so the rename stays
/// on one filesystem and is therefore atomic on the platforms this project
/// targets. The file is flushed with `sync_all` before the rename.
///
/// # Errors
///
/// Returns the I/O error from creating, writing, syncing, or renaming. On
/// error the destination is unchanged and the temporary file is removed.
pub(crate) fn write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };
    let name = path.file_name().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "atomic save destination has no file name",
        )
    })?;

    let mut last_error = None;
    for _ in 0..16 {
        let sequence = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let temp = parent.join(temp_name(name, sequence));
        match write_temp(&temp, bytes) {
            Ok(()) => {
                return match fs::rename(&temp, path) {
                    Ok(()) => Ok(()),
                    Err(error) => {
                        let _ = fs::remove_file(&temp);
                        Err(error)
                    }
                };
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                last_error = Some(error);
            }
            Err(error) => {
                let _ = fs::remove_file(&temp);
                return Err(error);
            }
        }
    }
    Err(last_error.unwrap_or_else(|| {
        io::Error::new(
            io::ErrorKind::AlreadyExists,
            "could not create a unique temporary file",
        )
    }))
}

/// Build a hidden, unique sibling name for the temporary file.
fn temp_name(name: &std::ffi::OsStr, sequence: u64) -> OsString {
    let mut temp = OsString::from(".");
    temp.push(name);
    temp.push(format!(".{}.{sequence}.pdfxi-tmp", std::process::id()));
    temp
}

/// Create the temporary file exclusively, write it, and flush it to disk.
fn write_temp(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}
