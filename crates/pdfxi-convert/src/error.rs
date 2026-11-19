//! Actionable errors produced by import and export adapters.
//!
//! [`ConversionError`] is a plain `std` error type: every variant names either
//! the unsupported source/target or the invalid input that caused the failure,
//! so callers can present a specific message without guessing. I/O failures
//! keep their [`std::io::Error`] as a source for diagnostics.

use crate::ExportKind;
use crate::SourceKind;
use std::fmt;
use std::io;

/// A failure encountered while importing or exporting a document.
///
/// Unsupported-format variants are deterministic and safe to match on, which
/// lets a UI hide an unavailable menu item after one failed probe. The
/// [`ConversionError::Io`] variant preserves the underlying I/O error.
#[derive(Debug)]
pub enum ConversionError {
    /// The source format has no import adapter.
    UnsupportedSource {
        /// The classified source that could not be imported.
        kind: SourceKind,
    },
    /// The export target has no output adapter.
    UnsupportedExport {
        /// The requested target that could not be produced.
        target: ExportKind,
    },
    /// The pair of source and target formats is not a supported conversion.
    UnsupportedConversion {
        /// The classified source format.
        source: SourceKind,
        /// The requested export target.
        target: ExportKind,
    },
    /// An option or input value is malformed.
    InvalidInput {
        /// A human-readable explanation of the invalid value.
        message: String,
    },
    /// An underlying I/O operation failed.
    Io(io::Error),
}

impl ConversionError {
    /// Returns `true` when the failure is caused by an unsupported format.
    #[must_use]
    pub fn is_unsupported(&self) -> bool {
        matches!(
            self,
            Self::UnsupportedSource { .. }
                | Self::UnsupportedExport { .. }
                | Self::UnsupportedConversion { .. }
        )
    }
}

impl fmt::Display for ConversionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedSource { kind } => {
                write!(f, "importing '{kind}' sources is not supported")
            }
            Self::UnsupportedExport { target } => {
                write!(f, "exporting to '{target}' is not supported")
            }
            Self::UnsupportedConversion { source, target } => {
                write!(f, "cannot convert '{source}' input to '{target}' output")
            }
            Self::InvalidInput { message } => {
                write!(f, "invalid conversion input: {message}")
            }
            Self::Io(error) => write!(f, "conversion I/O error: {error}"),
        }
    }
}

impl std::error::Error for ConversionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}

impl From<io::Error> for ConversionError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unsupported_variants_are_flagged_and_have_actionable_messages() {
        let source = ConversionError::UnsupportedSource {
            kind: SourceKind::Presentation,
        };
        assert!(source.is_unsupported());
        assert!(source.to_string().contains("presentation"));

        let target = ConversionError::UnsupportedExport {
            target: ExportKind::Spreadsheet,
        };
        assert!(target.is_unsupported());
        assert!(target.to_string().contains("spreadsheet"));

        let pair = ConversionError::UnsupportedConversion {
            source: SourceKind::Rtf,
            target: ExportKind::Image,
        };
        assert!(pair.is_unsupported());
        assert!(pair.to_string().contains("rtf"));
        assert!(pair.to_string().contains("image"));
    }

    #[test]
    fn invalid_input_and_io_errors_are_not_unsupported() {
        let invalid = ConversionError::InvalidInput {
            message: "page range is empty".to_owned(),
        };
        assert!(!invalid.is_unsupported());
        assert!(invalid.to_string().contains("page range is empty"));
        assert!(std::error::Error::source(&invalid).is_none());

        let io_error = ConversionError::from(io::Error::new(io::ErrorKind::NotFound, "missing"));
        assert!(!io_error.is_unsupported());
        assert!(io_error.to_string().contains("missing"));
        assert!(std::error::Error::source(&io_error).is_some());
    }
}
