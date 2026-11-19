//! Shared option primitives for the deterministic text-based exporters.
//!
//! Both the RTF writer ([`crate::rtf`]) and the tagged-text writer
//! ([`crate::tagged_text`]) need a page selection, a line-ending policy, and
//! validation with typed errors. This module defines those primitives once so
//! the two exporters behave consistently and can be tested against the same
//! contract. All types are dependency-free apart from `thiserror`, which is
//! already a workspace dependency.

use thiserror::Error;

/// The line separator written between structural lines of generated output.
///
/// Generated documents are assembled as a list of logical lines and joined with
/// the configured separator, so the byte output is stable across platforms.
/// [`LineEnding::normalize`] can also rewrite input text that contains mixed
/// line endings before it is processed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum LineEnding {
    /// Unix line feed (`U+000A`). The default.
    #[default]
    Lf,
    /// Windows carriage return followed by line feed (`U+000D U+000A`).
    CrLf,
    /// Classic Macintosh carriage return (`U+000D`).
    Cr,
}

impl LineEnding {
    /// Every supported line ending, in a stable order.
    pub const ALL: &'static [Self] = &[Self::Lf, Self::CrLf, Self::Cr];

    /// Returns every supported line ending in a stable order.
    #[must_use]
    pub fn all() -> &'static [Self] {
        Self::ALL
    }

    /// Returns the separator string emitted between structural lines.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Lf => "\n",
            Self::CrLf => "\r\n",
            Self::Cr => "\r",
        }
    }

    /// Joins logical lines with this separator.
    #[must_use]
    pub fn join<S: AsRef<str>>(self, lines: impl IntoIterator<Item = S>) -> String {
        let mut out = String::new();
        for (index, line) in lines.into_iter().enumerate() {
            if index > 0 {
                out.push_str(self.as_str());
            }
            out.push_str(line.as_ref());
        }
        out
    }

    /// Rewrites any of `\r\n`, `\n`, or `\r` in `text` to this separator.
    ///
    /// CRLF is recognized as a single break so it is not doubled. The function
    /// is allocation-light: it copies `text` unchanged when no rewrite is
    /// needed.
    #[must_use]
    pub fn normalize(self, text: &str) -> String {
        let separator = self.as_str();
        let bytes = text.as_bytes();
        if !bytes.iter().any(|&byte| byte == b'\n' || byte == b'\r') {
            return text.to_owned();
        }
        let mut out = String::with_capacity(text.len());
        let mut index = 0;
        while index < bytes.len() {
            match bytes[index] {
                b'\r' => {
                    out.push_str(separator);
                    index += if bytes.get(index + 1) == Some(&b'\n') {
                        2
                    } else {
                        1
                    };
                }
                b'\n' => {
                    out.push_str(separator);
                    index += 1;
                }
                _ => {
                    // Copy the next UTF-8 character verbatim. Indexing is safe
                    // because `index` always points at a character boundary:
                    // it starts at zero and ASCII break bytes cannot occur
                    // inside a multi-byte UTF-8 sequence.
                    let ch = text[index..].chars().next().unwrap_or('\u{fffd}');
                    out.push(ch);
                    index += ch.len_utf8();
                }
            }
        }
        out
    }

    /// Returns a short human-readable label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Lf => "lf",
            Self::CrLf => "crlf",
            Self::Cr => "cr",
        }
    }
}

impl std::fmt::Display for LineEnding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

/// Selects which zero-based pages an exporter writes.
///
/// Indices follow the rest of `pdfxi-convert`: page 0 is the first page.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum ExportPageSelection {
    /// Select every page present in the input. The default.
    #[default]
    All,
    /// Select the inclusive zero-based range `first..=last`, in ascending order.
    Range {
        /// First zero-based page to include.
        first: usize,
        /// Last zero-based page to include.
        last: usize,
    },
    /// Select an explicit list of zero-based pages.
    ///
    /// The list is used as a membership set; output order is always determined
    /// by the input model (ascending page order for RTF, reading order for
    /// tagged text), never by the order of this list.
    Pages(Vec<usize>),
}

impl ExportPageSelection {
    /// Returns `true` when `page` belongs to this selection.
    #[must_use]
    pub fn contains(&self, page: usize) -> bool {
        match self {
            Self::All => true,
            Self::Range { first, last } => page >= *first && page <= *last,
            Self::Pages(pages) => pages.contains(&page),
        }
    }

    /// Validates structural constraints that do not depend on the input.
    ///
    /// # Errors
    ///
    /// Returns [`ExportOptionError::InvalidPageRange`] for an inverted range or
    /// [`ExportOptionError::EmptyExportPageSelection`] for an empty explicit list.
    pub fn validate(&self) -> Result<(), ExportOptionError> {
        match self {
            Self::All => Ok(()),
            Self::Range { first, last } if last < first => {
                Err(ExportOptionError::InvalidPageRange {
                    first: *first,
                    last: *last,
                })
            }
            Self::Range { .. } => Ok(()),
            Self::Pages(pages) if pages.is_empty() => {
                Err(ExportOptionError::EmptyExportPageSelection)
            }
            Self::Pages(_) => Ok(()),
        }
    }
}

/// A typed, actionable failure produced while validating export options.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ExportOptionError {
    /// A page range ended before it started.
    #[error("invalid page range {first}..={last}: last page precedes first page")]
    InvalidPageRange {
        /// First requested page (zero-based).
        first: usize,
        /// Last requested page (zero-based).
        last: usize,
    },
    /// An explicit page list contained no pages.
    #[error("page selection is empty")]
    EmptyExportPageSelection,
    /// A heading-level limit fell outside the supported `1..=6` span.
    #[error("invalid heading level limit {levels}: expected a value in 1..=6")]
    InvalidHeadingLevels {
        /// The rejected heading-level limit.
        levels: u8,
    },
}

/// Validates a heading-level limit, returning a typed error when out of range.
pub fn validate_heading_levels(levels: u8) -> Result<(), ExportOptionError> {
    if (1..=6).contains(&levels) {
        Ok(())
    } else {
        Err(ExportOptionError::InvalidHeadingLevels { levels })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_ending_join_and_normalize_are_deterministic() {
        let lines = ["a", "b", "c"];
        assert_eq!(LineEnding::Lf.join(lines), "a\nb\nc");
        assert_eq!(LineEnding::CrLf.join(lines), "a\r\nb\r\nc");
        assert_eq!(LineEnding::Cr.join(lines), "a\rb\rc");
        assert_eq!(
            LineEnding::CrLf.normalize("a\r\nb\nc\rd"),
            "a\r\nb\r\nc\r\nd"
        );
        assert_eq!(LineEnding::Lf.normalize("plain"), "plain");
        assert_eq!(LineEnding::default(), LineEnding::Lf);
        assert_eq!(LineEnding::CrLf.to_string(), "crlf");
        assert_eq!(LineEnding::all().len(), 3);
    }

    #[test]
    fn page_selection_contains_and_validates() {
        assert!(ExportPageSelection::All.contains(usize::MAX));
        let range = ExportPageSelection::Range { first: 1, last: 3 };
        assert!(!range.contains(0));
        assert!(range.contains(2));
        assert!(!range.contains(4));
        assert!(range.validate().is_ok());
        assert_eq!(
            ExportPageSelection::Range { first: 3, last: 1 }.validate(),
            Err(ExportOptionError::InvalidPageRange { first: 3, last: 1 })
        );
        assert_eq!(
            ExportPageSelection::Pages(Vec::new()).validate(),
            Err(ExportOptionError::EmptyExportPageSelection)
        );
        assert!(ExportPageSelection::Pages(vec![2, 0]).validate().is_ok());
    }

    #[test]
    fn heading_level_validation_accepts_one_through_six() {
        for level in 1..=6 {
            assert!(validate_heading_levels(level).is_ok());
        }
        assert_eq!(
            validate_heading_levels(0),
            Err(ExportOptionError::InvalidHeadingLevels { levels: 0 })
        );
        assert_eq!(
            validate_heading_levels(7),
            Err(ExportOptionError::InvalidHeadingLevels { levels: 7 })
        );
    }
}
