//! Severity levels, preflight findings, and their canonical ordering.
//!
//! Every check produces [`PreflightFinding`] values with a stable machine code,
//! an actionable message, and an optional location. Ordering is defined once
//! here so every profile and report presents findings identically regardless
//! of the order in which checks discovered them.

use std::cmp::Ordering;
use std::fmt;

/// How serious a [`PreflightFinding`] is.
///
/// The derived `Ord` places `Info` before `Warning` before `Error`, which makes
/// `finding.severity() >= Severity::Warning` a natural "at least a warning"
/// filter. Canonical finding order reverses this so the most severe problems
/// appear first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Severity {
    /// An advisory note that does not affect conformance.
    Info,
    /// A problem that should be fixed but does not by itself fail conformance.
    Warning,
    /// A problem that makes the document non-conforming.
    Error,
}

impl Severity {
    /// Every severity from least to most severe.
    pub const ALL: &'static [Self] = &[Self::Info, Self::Warning, Self::Error];

    /// Returns every severity from least to most severe.
    #[must_use]
    pub fn all() -> &'static [Self] {
        Self::ALL
    }

    /// Returns the lowercase report label.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Warning => "warning",
            Self::Error => "error",
        }
    }

    /// Returns `true` for [`Severity::Error`].
    #[must_use]
    pub fn is_error(self) -> bool {
        self == Self::Error
    }
}

impl fmt::Display for Severity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// A single actionable result produced by a preflight check.
///
/// The `code` is a stable identifier (for example `STD-002`) intended for
/// filtering and documentation links. `message` explains how to fix the
/// problem, and `location` is an optional human-readable position such as
/// `page 3` or `Catalog /AcroForm`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreflightFinding {
    severity: Severity,
    code: String,
    message: String,
    location: Option<String>,
}

impl PreflightFinding {
    /// Creates a finding without a location.
    #[must_use]
    pub fn new(severity: Severity, code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            severity,
            code: code.into(),
            message: message.into(),
            location: None,
        }
    }

    /// Attaches a human-readable location to this finding.
    #[must_use]
    pub fn with_location(mut self, location: impl Into<String>) -> Self {
        self.location = Some(location.into());
        self
    }

    /// Returns the severity.
    #[must_use]
    pub fn severity(&self) -> Severity {
        self.severity
    }

    /// Returns the stable machine code.
    #[must_use]
    pub fn code(&self) -> &str {
        &self.code
    }

    /// Returns the actionable message.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }

    /// Returns the location, if one was attached.
    #[must_use]
    pub fn location(&self) -> Option<&str> {
        self.location.as_deref()
    }
}

impl Ord for PreflightFinding {
    fn cmp(&self, other: &Self) -> Ordering {
        other
            .severity
            .cmp(&self.severity)
            .then_with(|| self.code.cmp(&other.code))
            .then_with(|| self.message.cmp(&other.message))
            .then_with(|| self.location.cmp(&other.location))
    }
}

impl PartialOrd for PreflightFinding {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl fmt::Display for PreflightFinding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {}: {}", self.severity, self.code, self.message)?;
        match &self.location {
            Some(location) => write!(f, " ({location})"),
            None => Ok(()),
        }
    }
}

/// Sorts findings into [`PreflightFinding`]'s canonical order in place.
///
/// The order is severity descending (`Error` first), then code, message, and
/// location ascending. Sorting is stable, so equal findings retain their
/// discovery order.
pub fn sort_findings(findings: &mut [PreflightFinding]) {
    findings.sort();
}

/// Returns findings at or above `min_severity` in canonical order.
///
/// For example, passing [`Severity::Warning`] keeps warnings and errors and
/// drops informational notes. The input slice is never modified.
#[must_use]
pub fn filter_by_severity(
    findings: &[PreflightFinding],
    min_severity: Severity,
) -> Vec<PreflightFinding> {
    let mut selected: Vec<PreflightFinding> = findings
        .iter()
        .filter(|finding| finding.severity() >= min_severity)
        .cloned()
        .collect();
    sort_findings(&mut selected);
    selected
}

#[cfg(test)]
mod tests {
    use super::*;

    fn finding(severity: Severity, code: &str, message: &str) -> PreflightFinding {
        PreflightFinding::new(severity, code, message)
    }

    #[test]
    fn severity_ordering_and_labels_are_stable() {
        assert!(Severity::Info < Severity::Warning);
        assert!(Severity::Warning < Severity::Error);
        assert_eq!(Severity::Info.label(), "info");
        assert_eq!(Severity::Warning.label(), "warning");
        assert_eq!(Severity::Error.label(), "error");
        assert!(Severity::Error.is_error());
        assert!(!Severity::Warning.is_error());
        assert_eq!(Severity::all(), Severity::ALL);
    }

    #[test]
    fn findings_sort_by_severity_then_code_then_message_then_location() {
        let mut findings = vec![
            finding(Severity::Info, "STD-001", "alpha"),
            finding(Severity::Error, "STD-009", "zeta"),
            finding(Severity::Warning, "STD-005", "beta"),
            finding(Severity::Error, "STD-002", "beta"),
            finding(Severity::Error, "STD-002", "alpha").with_location("page 2"),
            finding(Severity::Error, "STD-002", "alpha").with_location("page 1"),
        ];
        sort_findings(&mut findings);

        let observed: Vec<(Severity, &str, &str, Option<&str>)> = findings
            .iter()
            .map(|item| {
                (
                    item.severity(),
                    item.code(),
                    item.message(),
                    item.location(),
                )
            })
            .collect();
        assert_eq!(
            observed,
            vec![
                (Severity::Error, "STD-002", "alpha", Some("page 1")),
                (Severity::Error, "STD-002", "alpha", Some("page 2")),
                (Severity::Error, "STD-002", "beta", None),
                (Severity::Error, "STD-009", "zeta", None),
                (Severity::Warning, "STD-005", "beta", None),
                (Severity::Info, "STD-001", "alpha", None),
            ]
        );
    }

    #[test]
    fn filtering_by_severity_keeps_warnings_and_errors_in_order() {
        let findings = vec![
            finding(Severity::Info, "STD-100", "advisory"),
            finding(Severity::Warning, "STD-200", "warning"),
            finding(Severity::Error, "STD-300", "error"),
        ];
        let filtered = filter_by_severity(&findings, Severity::Warning);
        assert_eq!(filtered.len(), 2);
        assert_eq!(filtered[0].severity(), Severity::Error);
        assert_eq!(filtered[0].code(), "STD-300");
        assert_eq!(filtered[1].severity(), Severity::Warning);
        assert_eq!(filtered[1].code(), "STD-200");

        assert_eq!(
            filter_by_severity(&findings, Severity::Info).len(),
            findings.len()
        );
        assert!(filter_by_severity(&findings, Severity::Error).len() == 1);
        assert_eq!(findings.len(), 3, "filtering must not mutate the input");
    }

    #[test]
    fn finding_display_includes_location_when_present() {
        let plain = finding(Severity::Error, "STD-001", "bad header");
        assert_eq!(plain.to_string(), "error STD-001: bad header");
        let located = plain.with_location("offset 0");
        assert_eq!(located.to_string(), "error STD-001: bad header (offset 0)");
        assert_eq!(located.location(), Some("offset 0"));
        assert_eq!(located.code(), "STD-001");
        assert_eq!(located.message(), "bad header");
    }
}
