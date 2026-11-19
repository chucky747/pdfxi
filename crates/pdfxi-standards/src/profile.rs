//! Preflight profiles and deterministic reports.
//!
//! A [`PreflightProfile`] checks one kind of input against one [`Standard`] and
//! returns [`PreflightFinding`] values. The associated [`PreflightProfile::Input`]
//! type lets the same trait serve both byte-oriented checks
//! ([`HeaderProfile`] and [`StaticProfile`] use `[u8]`) and structured document
//! checks (the profiles in [`crate::document_profile`] use
//! [`crate::PreflightInput`]). The trait's provided methods guarantee that every
//! caller sees findings in the same canonical order, whether it wants all
//! findings, a filtered subset, or a [`PreflightReport`].
//!
//! [`StaticProfile`] is a configurable test double. [`HeaderProfile`] is a
//! small real profile that validates the `%PDF-x.y` header; it is deliberately
//! shallow and is not a substitute for a full conformance checker such as
//! veraPDF.

use crate::Severity;
use crate::Standard;
use crate::finding::{PreflightFinding, filter_by_severity, sort_findings};

/// A deterministic conformance checker for one PDF standard and input kind.
///
/// Implementors provide [`PreflightProfile::Input`],
/// [`PreflightProfile::standard`], and [`PreflightProfile::check`]. The provided
/// methods add canonical ordering and severity filtering without allowing
/// implementations to reorder results.
pub trait PreflightProfile {
    /// The kind of value this profile inspects.
    ///
    /// Byte-oriented profiles use `[u8]`; document profiles use
    /// [`crate::PreflightInput`]. The input is always borrowed, so unsized
    /// types such as `[u8]` are supported.
    type Input: ?Sized;

    /// Returns the standard this profile checks against.
    fn standard(&self) -> Standard;

    /// Runs the checks and returns findings in discovery order.
    ///
    /// Implementations must be deterministic for a given input. Results may
    /// be unsorted; the provided methods sort them.
    fn check(&self, input: &Self::Input) -> Vec<PreflightFinding>;

    /// Runs the checks and returns findings in canonical order.
    fn check_sorted(&self, input: &Self::Input) -> Vec<PreflightFinding> {
        let mut findings = self.check(input);
        sort_findings(&mut findings);
        findings
    }

    /// Returns findings at or above `min_severity` in canonical order.
    fn check_filtered(&self, input: &Self::Input, min_severity: Severity) -> Vec<PreflightFinding> {
        filter_by_severity(&self.check(input), min_severity)
    }

    /// Runs the checks and builds a canonically ordered report.
    fn report(&self, input: &Self::Input) -> PreflightReport {
        PreflightReport::new(self.standard(), self.check(input))
    }
}

/// The result of running one [`PreflightProfile`].
///
/// Findings are stored in canonical order and the report exposes a simple
/// [`PreflightReport::is_conforming`] decision based on error-severity
/// findings. Warnings and informational notes do not block conformance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreflightReport {
    standard: Standard,
    findings: Vec<PreflightFinding>,
}

impl PreflightReport {
    /// Builds a report, sorting the supplied findings into canonical order.
    #[must_use]
    pub fn new(standard: Standard, mut findings: Vec<PreflightFinding>) -> Self {
        sort_findings(&mut findings);
        Self { standard, findings }
    }

    /// Returns the checked standard.
    #[must_use]
    pub fn standard(&self) -> Standard {
        self.standard
    }

    /// Returns the findings in canonical order.
    #[must_use]
    pub fn findings(&self) -> &[PreflightFinding] {
        &self.findings
    }

    /// Returns `true` when no error-severity findings were reported.
    #[must_use]
    pub fn is_conforming(&self) -> bool {
        !self
            .findings
            .iter()
            .any(|finding| finding.severity().is_error())
    }

    /// Returns the number of error-severity findings.
    #[must_use]
    pub fn error_count(&self) -> usize {
        self.findings
            .iter()
            .filter(|finding| finding.severity().is_error())
            .count()
    }

    /// Returns the number of warning-severity findings.
    #[must_use]
    pub fn warning_count(&self) -> usize {
        self.findings
            .iter()
            .filter(|finding| finding.severity() == Severity::Warning)
            .count()
    }

    /// Returns the number of informational findings.
    #[must_use]
    pub fn info_count(&self) -> usize {
        self.findings
            .iter()
            .filter(|finding| finding.severity() == Severity::Info)
            .count()
    }

    /// Returns a stable, human-readable verdict and severity tally.
    ///
    /// The exact format is
    /// `<standard>: conforming|non-conforming (<errors> errors, <warnings>
    /// warnings, <info> info)`. It is suitable for logs and CLI output and does
    /// not depend on finding discovery order.
    #[must_use]
    pub fn summary(&self) -> String {
        let verdict = if self.is_conforming() {
            "conforming"
        } else {
            "non-conforming"
        };
        format!(
            "{}: {} ({} errors, {} warnings, {} info)",
            self.standard,
            verdict,
            self.error_count(),
            self.warning_count(),
            self.info_count()
        )
    }

    /// Consumes the report and returns its canonically ordered findings.
    #[must_use]
    pub fn into_findings(self) -> Vec<PreflightFinding> {
        self.findings
    }
}

/// A profile that returns a fixed list of findings.
///
/// Useful for exercising trait defaults, UI ordering, and severity filtering
/// without constructing a PDF. The list is cloned on every check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StaticProfile {
    standard: Standard,
    findings: Vec<PreflightFinding>,
}

impl StaticProfile {
    /// Creates a profile that always returns `findings` for `standard`.
    #[must_use]
    pub fn new(standard: Standard, findings: Vec<PreflightFinding>) -> Self {
        Self { standard, findings }
    }
}

impl PreflightProfile for StaticProfile {
    type Input = [u8];

    fn standard(&self) -> Standard {
        self.standard
    }

    fn check(&self, _pdf: &[u8]) -> Vec<PreflightFinding> {
        self.findings.clone()
    }
}

/// A minimal real profile that validates the PDF header.
///
/// It reports an error for empty input, an error for a missing `%PDF-` prefix,
/// and a warning when the declared version is older than the minimum required
/// by the selected standard. It never panics on short, truncated, or non-UTF-8
/// input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeaderProfile {
    standard: Standard,
}

impl HeaderProfile {
    /// Creates a header checker for `standard`.
    #[must_use]
    pub fn new(standard: Standard) -> Self {
        Self { standard }
    }
}

impl PreflightProfile for HeaderProfile {
    type Input = [u8];

    fn standard(&self) -> Standard {
        self.standard
    }

    fn check(&self, pdf: &[u8]) -> Vec<PreflightFinding> {
        const PREFIX: &[u8] = b"%PDF-";
        if pdf.is_empty() {
            return vec![PreflightFinding::new(
                Severity::Error,
                "STD-001",
                "input is empty; a PDF header is required",
            )];
        }
        if !pdf.starts_with(PREFIX) {
            return vec![
                PreflightFinding::new(
                    Severity::Error,
                    "STD-002",
                    "missing %PDF- header at the start of the file",
                )
                .with_location("offset 0"),
            ];
        }

        let version_end = PREFIX.len().saturating_add(3).min(pdf.len());
        let version_bytes = &pdf[PREFIX.len()..version_end];
        let actual = match std::str::from_utf8(version_bytes)
            .ok()
            .and_then(parse_version)
        {
            Some(version) => version,
            None => {
                return vec![
                    PreflightFinding::new(
                        Severity::Warning,
                        "STD-003",
                        "PDF header version is missing or malformed",
                    )
                    .with_location("offset 5"),
                ];
            }
        };

        // The bundled minimum-version strings are known-valid constants. An
        // unexpected value yields no finding rather than a panic.
        let required = match parse_version(self.standard.minimum_pdf_version()) {
            Some(version) => version,
            None => return Vec::new(),
        };
        if actual < required {
            return vec![
                PreflightFinding::new(
                    Severity::Warning,
                    "STD-004",
                    format!(
                        "PDF header declares version {}.{}, but {} requires at least {}.{}",
                        actual.0, actual.1, self.standard, required.0, required.1
                    ),
                )
                .with_location("offset 5"),
            ];
        }
        Vec::new()
    }
}

/// Parses a three-byte `x.y` version string into `(major, minor)`.
fn parse_version(text: &str) -> Option<(u8, u8)> {
    let bytes = text.as_bytes();
    if bytes.len() != 3 || bytes[1] != b'.' {
        return None;
    }
    if !bytes[0].is_ascii_digit() || !bytes[2].is_ascii_digit() {
        return None;
    }
    Some((bytes[0] - b'0', bytes[2] - b'0'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PreflightFinding;
    use crate::finding::sort_findings;

    fn unsorted_findings() -> Vec<PreflightFinding> {
        vec![
            PreflightFinding::new(Severity::Info, "STD-100", "advisory"),
            PreflightFinding::new(Severity::Warning, "STD-200", "warning"),
            PreflightFinding::new(Severity::Error, "STD-300", "error"),
        ]
    }

    #[test]
    fn static_profile_check_sorted_is_canonical() {
        let profile = StaticProfile::new(Standard::PdfA2b, unsorted_findings());
        let sorted = profile.check_sorted(b"anything");
        let severities: Vec<Severity> = sorted.iter().map(PreflightFinding::severity).collect();
        assert_eq!(
            severities,
            vec![Severity::Error, Severity::Warning, Severity::Info]
        );
        assert_eq!(profile.standard(), Standard::PdfA2b);
    }

    #[test]
    fn static_profile_check_filtered_applies_severity_threshold() {
        let profile = StaticProfile::new(Standard::PdfX4, unsorted_findings());
        let filtered = profile.check_filtered(b"anything", Severity::Warning);
        assert_eq!(filtered.len(), 2);
        assert_eq!(filtered[0].code(), "STD-300");
        assert_eq!(filtered[1].code(), "STD-200");
        assert_eq!(
            profile.check_filtered(b"anything", Severity::Error).len(),
            1
        );
    }

    #[test]
    fn report_sorts_findings_and_decides_conformance() {
        let failing = StaticProfile::new(Standard::PdfA1b, unsorted_findings()).report(b"x");
        assert_eq!(failing.standard(), Standard::PdfA1b);
        assert!(!failing.is_conforming());
        assert_eq!(failing.error_count(), 1);
        assert_eq!(failing.warning_count(), 1);
        assert_eq!(failing.findings()[0].code(), "STD-300");

        let passing = StaticProfile::new(
            Standard::PdfE1,
            vec![PreflightFinding::new(Severity::Info, "STD-001", "note")],
        )
        .report(b"x");
        assert!(passing.is_conforming());
        assert_eq!(passing.into_findings().len(), 1);
    }

    #[test]
    fn header_profile_reports_empty_input_and_bad_header() {
        let profile = HeaderProfile::new(Standard::PdfA2b);
        let empty = profile.check(b"");
        assert_eq!(empty.len(), 1);
        assert_eq!(empty[0].code(), "STD-001");
        assert_eq!(empty[0].severity(), Severity::Error);

        let bad = profile.check(b"not a pdf at all");
        assert_eq!(bad.len(), 1);
        assert_eq!(bad[0].code(), "STD-002");
        assert_eq!(bad[0].location(), Some("offset 0"));
    }

    #[test]
    fn header_profile_warns_on_malformed_or_old_version() {
        let profile = HeaderProfile::new(Standard::PdfA2b);
        let malformed = profile.check(b"%PDF-x");
        assert_eq!(malformed[0].code(), "STD-003");
        assert_eq!(malformed[0].severity(), Severity::Warning);

        let old = profile.check(b"%PDF-1.4 content");
        assert_eq!(old[0].code(), "STD-004");
        assert!(old[0].message().contains("PDF/A-2b"));
        assert!(old[0].message().contains("1.7"));
    }

    #[test]
    fn header_profile_accepts_conforming_header() {
        assert!(
            HeaderProfile::new(Standard::PdfA1b)
                .check(b"%PDF-1.4")
                .is_empty()
        );
        assert!(
            HeaderProfile::new(Standard::PdfUa1)
                .check(b"%PDF-1.7")
                .is_empty()
        );
        assert!(
            HeaderProfile::new(Standard::PdfX1a)
                .check(b"%PDF-1.3")
                .is_empty()
        );
        assert!(
            HeaderProfile::new(Standard::PdfX4)
                .check(b"%PDF-1.6")
                .is_empty()
        );
        assert!(
            HeaderProfile::new(Standard::PdfE1)
                .check(b"%PDF-2.0")
                .is_empty()
        );
        assert!(
            !HeaderProfile::new(Standard::PdfA2b)
                .check(b"%PDF-")
                .is_empty()
        );
    }

    #[test]
    fn preflight_report_find_order_is_independent_of_discovery_order() {
        let a = StaticProfile::new(Standard::PdfUa1, unsorted_findings()).report(b"x");
        let mut reversed = unsorted_findings();
        reversed.reverse();
        let b = StaticProfile::new(Standard::PdfUa1, reversed).report(b"x");
        assert_eq!(a, b);

        let mut raw = unsorted_findings();
        sort_findings(&mut raw);
        assert_eq!(a.into_findings(), raw);
    }
}
