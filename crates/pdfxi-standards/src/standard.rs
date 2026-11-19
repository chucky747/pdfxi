//! PDF preservation-standard identifiers and their public labels.
//!
//! [`Standard`] enumerates the conformance targets supported by the standards
//! lane. It is purely descriptive: checking a document against one of these
//! targets is the job of a [`crate::PreflightProfile`]. The minimum PDF version
//! reported here is the base version a producer must request for that target.

use std::fmt;

/// A PDF preservation or production standard.
///
/// Labels follow the ISO names used in preflight reports (for example
/// `PDF/A-1b`). The enum is `Copy`, `Eq`, and `Ord` so it can be used as a map
/// key and sorted deterministically in reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Standard {
    /// ISO 19005-1, Level B (PDF 1.4).
    PdfA1b,
    /// ISO 19005-2, Level B (PDF 1.7).
    PdfA2b,
    /// ISO 19005-3, Level B (PDF 1.7).
    PdfA3b,
    /// ISO 15930-1 (PDF 1.3), colour-managed print exchange.
    PdfX1a,
    /// ISO 15930-7 (PDF 1.6), PDF/X with transparency.
    PdfX4,
    /// ISO 14739-1 (PDF 1.6), engineering document exchange.
    PdfE1,
    /// ISO 14289-1 (PDF 1.7), universal accessibility.
    PdfUa1,
}

impl Standard {
    /// Every supported standard, in a stable order.
    pub const ALL: &'static [Self] = &[
        Self::PdfA1b,
        Self::PdfA2b,
        Self::PdfA3b,
        Self::PdfX1a,
        Self::PdfX4,
        Self::PdfE1,
        Self::PdfUa1,
    ];

    /// Returns every supported standard in a stable order.
    #[must_use]
    pub fn all() -> &'static [Self] {
        Self::ALL
    }

    /// Returns the canonical report label, such as `PDF/A-2b`.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::PdfA1b => "PDF/A-1b",
            Self::PdfA2b => "PDF/A-2b",
            Self::PdfA3b => "PDF/A-3b",
            Self::PdfX1a => "PDF/X-1a",
            Self::PdfX4 => "PDF/X-4",
            Self::PdfE1 => "PDF/E-1",
            Self::PdfUa1 => "PDF/UA-1",
        }
    }

    /// Parses a canonical label, ignoring ASCII case.
    ///
    /// Unknown labels return `None` instead of panicking, so user input from a
    /// command line or configuration file can be handled directly.
    #[must_use]
    pub fn from_label(label: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .copied()
            .find(|standard| standard.label().eq_ignore_ascii_case(label))
    }

    /// Returns the family this standard belongs to.
    #[must_use]
    pub fn family(self) -> StandardFamily {
        match self {
            Self::PdfA1b | Self::PdfA2b | Self::PdfA3b => StandardFamily::PdfA,
            Self::PdfX1a | Self::PdfX4 => StandardFamily::PdfX,
            Self::PdfE1 => StandardFamily::PdfE,
            Self::PdfUa1 => StandardFamily::PdfUa,
        }
    }

    /// Returns the minimum PDF version required by the standard.
    ///
    /// The value is a version string such as `"1.4"`, suitable for display and
    /// for the deterministic header check in [`crate::HeaderProfile`].
    #[must_use]
    pub fn minimum_pdf_version(self) -> &'static str {
        match self {
            Self::PdfA1b => "1.4",
            Self::PdfA2b => "1.7",
            Self::PdfA3b => "1.7",
            Self::PdfX1a => "1.3",
            Self::PdfX4 => "1.6",
            Self::PdfE1 => "1.6",
            Self::PdfUa1 => "1.7",
        }
    }
}

impl fmt::Display for Standard {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// The family of a [`Standard`].
///
/// Families group related conformance targets for UI filtering; they do not
/// carry checking rules themselves.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum StandardFamily {
    /// PDF/A, long-term archival.
    PdfA,
    /// PDF/X, print exchange.
    PdfX,
    /// PDF/E, engineering workflows.
    PdfE,
    /// PDF/UA, universal accessibility.
    PdfUa,
}

impl StandardFamily {
    /// Every family, in a stable order.
    pub const ALL: &'static [Self] = &[Self::PdfA, Self::PdfX, Self::PdfE, Self::PdfUa];

    /// Returns every family in a stable order.
    #[must_use]
    pub fn all() -> &'static [Self] {
        Self::ALL
    }

    /// Returns the canonical family label.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::PdfA => "PDF/A",
            Self::PdfX => "PDF/X",
            Self::PdfE => "PDF/E",
            Self::PdfUa => "PDF/UA",
        }
    }
}

impl fmt::Display for StandardFamily {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standard_labels_are_canonical() {
        assert_eq!(Standard::PdfA1b.label(), "PDF/A-1b");
        assert_eq!(Standard::PdfA2b.label(), "PDF/A-2b");
        assert_eq!(Standard::PdfA3b.label(), "PDF/A-3b");
        assert_eq!(Standard::PdfX1a.label(), "PDF/X-1a");
        assert_eq!(Standard::PdfX4.label(), "PDF/X-4");
        assert_eq!(Standard::PdfE1.label(), "PDF/E-1");
        assert_eq!(Standard::PdfUa1.label(), "PDF/UA-1");
        for standard in Standard::all() {
            assert_eq!(standard.to_string(), standard.label());
        }
    }

    #[test]
    fn from_label_round_trips_and_ignores_ascii_case() {
        for standard in Standard::all() {
            assert_eq!(Standard::from_label(standard.label()), Some(*standard));
            assert_eq!(
                Standard::from_label(&standard.label().to_ascii_lowercase()),
                Some(*standard)
            );
        }
        assert_eq!(Standard::from_label("PDF/A-9c"), None);
        assert_eq!(Standard::from_label(""), None);
    }

    #[test]
    fn families_and_minimum_versions_match_expectations() {
        assert_eq!(Standard::PdfA1b.family(), StandardFamily::PdfA);
        assert_eq!(Standard::PdfA2b.family(), StandardFamily::PdfA);
        assert_eq!(Standard::PdfA3b.family(), StandardFamily::PdfA);
        assert_eq!(Standard::PdfX1a.family(), StandardFamily::PdfX);
        assert_eq!(Standard::PdfX4.family(), StandardFamily::PdfX);
        assert_eq!(Standard::PdfE1.family(), StandardFamily::PdfE);
        assert_eq!(Standard::PdfUa1.family(), StandardFamily::PdfUa);

        assert_eq!(Standard::PdfA1b.minimum_pdf_version(), "1.4");
        assert_eq!(Standard::PdfA2b.minimum_pdf_version(), "1.7");
        assert_eq!(Standard::PdfA3b.minimum_pdf_version(), "1.7");
        assert_eq!(Standard::PdfX1a.minimum_pdf_version(), "1.3");
        assert_eq!(Standard::PdfX4.minimum_pdf_version(), "1.6");
        assert_eq!(Standard::PdfE1.minimum_pdf_version(), "1.6");
        assert_eq!(Standard::PdfUa1.minimum_pdf_version(), "1.7");
    }

    #[test]
    fn all_lists_are_unique_and_stable() {
        assert_eq!(Standard::all().len(), 7);
        assert_eq!(StandardFamily::all().len(), 4);
        assert_eq!(Standard::all(), Standard::ALL);
        assert_eq!(StandardFamily::all(), StandardFamily::ALL);
        assert_eq!(StandardFamily::PdfA.to_string(), "PDF/A");
    }
}
