//! Local PDF Portfolio manifest model (CREATE-010).
//!
//! ADR 0008 requires a local, file-based equivalent. This module replaces the
//! retired "customize PDF Portfolios" workflow with a caller-consumable
//! [`PortfolioManifest`]: named, described, ordered file entries that validate
//! to a traversal-free duplicate-free set and serialize deterministically to an
//! XML descriptor. It models and validates the portfolio structure only; it
//! does not package file bytes into an actual PDF package, which remains a host
//! adapter responsibility.
//!
//! Serialization reuses the existing Office writers' `xml::declaration` and
//! `xml::escape` helpers so the descriptor is well-formed XML and hostile
//! entry text can never break it. Ordering is by `order` then name, giving a
//! byte-identical descriptor for equivalent manifests.

use crate::office_export::xml::{declaration, escape};
use std::collections::HashSet;

/// Maximum number of entries in one portfolio.
pub const PORTFOLIO_MAX_ENTRIES: usize = 10_000;

/// Maximum length of an entry name, in bytes.
pub const PORTFOLIO_MAX_NAME_BYTES: usize = 1_024;

/// Maximum length of an entry description, in bytes.
pub const PORTFOLIO_MAX_DESCRIPTION_BYTES: usize = 4_096;

/// Maximum length of a portfolio title, in bytes.
pub const PORTFOLIO_MAX_TITLE_BYTES: usize = 512;

/// One named, described, ordered entry in a portfolio.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortfolioEntry {
    name: String,
    description: String,
    order: usize,
}

impl PortfolioEntry {
    /// Create a portfolio entry.
    ///
    /// `name` is a relative, forward-slash-separated path inside the portfolio.
    /// `description` is an optional human-readable label. `order` is the
    /// caller's sort key; ties are broken by name for deterministic output.
    #[must_use]
    pub fn new(name: impl Into<String>, description: impl Into<String>, order: usize) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            order,
        }
    }

    /// Return the relative entry name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Return the entry description.
    #[must_use]
    pub fn description(&self) -> &str {
        &self.description
    }

    /// Return the caller's sort key.
    #[must_use]
    pub const fn order(&self) -> usize {
        self.order
    }
}

/// A typed failure while validating a portfolio manifest.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PortfolioError {
    /// The portfolio title was empty.
    #[error("portfolio title must not be empty")]
    EmptyTitle,
    /// The portfolio title exceeded the documented byte bound.
    #[error("portfolio title exceeds {limit} bytes")]
    TitleTooLong {
        /// Enforced byte limit.
        limit: usize,
    },
    /// The manifest carried too many entries.
    #[error("portfolio has {count} entries (limit {limit})")]
    TooManyEntries {
        /// Number of entries supplied.
        count: usize,
        /// Enforced entry budget.
        limit: usize,
    },
    /// An entry name or description was invalid.
    #[error("portfolio entry {index}: {reason}")]
    InvalidEntry {
        /// Zero-based index of the offending entry in input order.
        index: usize,
        /// Stable explanation.
        reason: &'static str,
    },
    /// Two entries resolved to the same case-insensitive name.
    #[error("duplicate portfolio entry name '{name}'")]
    DuplicateName {
        /// The duplicated name as first supplied.
        name: String,
    },
}

/// An ordered, validated portfolio manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortfolioManifest {
    title: String,
    entries: Vec<PortfolioEntry>,
}

impl PortfolioManifest {
    /// Create a manifest from a title and a list of entries.
    #[must_use]
    pub fn new(title: impl Into<String>, entries: Vec<PortfolioEntry>) -> Self {
        Self {
            title: title.into(),
            entries,
        }
    }

    /// Return the portfolio title.
    #[must_use]
    pub fn title(&self) -> &str {
        &self.title
    }

    /// Return the entries in caller-supplied order.
    #[must_use]
    pub fn entries(&self) -> &[PortfolioEntry] {
        &self.entries
    }

    /// Return the entries in deterministic display order (`order`, then name).
    #[must_use]
    pub fn sorted_entries(&self) -> Vec<&PortfolioEntry> {
        let mut ordered: Vec<&PortfolioEntry> = self.entries.iter().collect();
        ordered.sort_by(|left, right| {
            left.order
                .cmp(&right.order)
                .then_with(|| left.name.cmp(&right.name))
        });
        ordered
    }

    /// Validate the manifest without serializing it.
    ///
    /// Names must be non-empty, bounded, and relative traversal-free paths that
    /// use forward slashes. Two names that differ only by ASCII case are treated
    /// as duplicates. Descriptions are optional but length-bounded.
    ///
    /// # Errors
    ///
    /// Returns the typed [`PortfolioError`] variants for an invalid title, an
    /// oversized manifest, an invalid entry, or a duplicate name.
    pub fn validate(&self) -> Result<(), PortfolioError> {
        if self.title.is_empty() {
            return Err(PortfolioError::EmptyTitle);
        }
        if self.title.len() > PORTFOLIO_MAX_TITLE_BYTES {
            return Err(PortfolioError::TitleTooLong {
                limit: PORTFOLIO_MAX_TITLE_BYTES,
            });
        }
        if self.entries.len() > PORTFOLIO_MAX_ENTRIES {
            return Err(PortfolioError::TooManyEntries {
                count: self.entries.len(),
                limit: PORTFOLIO_MAX_ENTRIES,
            });
        }
        let mut seen: HashSet<String> = HashSet::with_capacity(self.entries.len());
        for (index, entry) in self.entries.iter().enumerate() {
            validate_entry_name(&entry.name)
                .map_err(|reason| PortfolioError::InvalidEntry { index, reason })?;
            if entry.description.len() > PORTFOLIO_MAX_DESCRIPTION_BYTES {
                return Err(PortfolioError::InvalidEntry {
                    index,
                    reason: "description exceeds the byte bound",
                });
            }
            let key = entry.name.to_ascii_lowercase();
            if !seen.insert(key) {
                return Err(PortfolioError::DuplicateName {
                    name: entry.name.clone(),
                });
            }
        }
        Ok(())
    }

    /// Serialize the validated manifest to a deterministic XML descriptor.
    ///
    /// The output is a well-formed XML document built from the shared Office
    /// XML declaration and escaping helpers. Entries appear in
    /// [`PortfolioManifest::sorted_entries`] order, so equivalent manifests
    /// produce byte-identical descriptors. An empty (but otherwise valid)
    /// manifest with no entries is allowed and serializes to a document with no
    /// `entry` elements.
    ///
    /// # Errors
    ///
    /// Returns the same errors as [`PortfolioManifest::validate`].
    pub fn serialize(&self) -> Result<String, PortfolioError> {
        self.validate()?;
        let mut out = String::new();
        out.push_str(declaration());
        out.push('\n');
        out.push_str(&format!(
            "<pdfxi-portfolio version=\"1\" title=\"{}\">",
            escape(&self.title)
        ));
        out.push('\n');
        for entry in self.sorted_entries() {
            out.push_str(&format!(
                "<entry order=\"{}\" name=\"{}\" description=\"{}\"/>",
                entry.order,
                escape(&entry.name),
                escape(&entry.description)
            ));
            out.push('\n');
        }
        out.push_str("</pdfxi-portfolio>\n");
        Ok(out)
    }
}

/// Validate that an entry name is a bounded, relative, traversal-free path.
fn validate_entry_name(name: &str) -> Result<(), &'static str> {
    if name.is_empty() {
        return Err("name must not be empty");
    }
    if name.len() > PORTFOLIO_MAX_NAME_BYTES {
        return Err("name exceeds the byte bound");
    }
    if name.chars().any(char::is_control) {
        return Err("name contains a control character");
    }
    if name.starts_with('/') || name.starts_with('\\') {
        return Err("name must be relative");
    }
    let bytes = name.as_bytes();
    if bytes.len() >= 2 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic() {
        return Err("name must not carry a drive prefix");
    }
    if name.contains('\\') {
        return Err("name must use forward slashes");
    }
    for component in name.split('/') {
        if component.is_empty() || component == "." || component == ".." {
            return Err("name contains an empty, current, or parent component");
        }
    }
    Ok(())
}

/// Serialize a manifest to its deterministic descriptor.
///
/// Convenience free function equivalent to [`PortfolioManifest::serialize`].
///
/// # Errors
///
/// Returns the same errors as [`PortfolioManifest::validate`].
pub fn serialize_portfolio(manifest: &PortfolioManifest) -> Result<String, PortfolioError> {
    manifest.serialize()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(entries: Vec<PortfolioEntry>) -> PortfolioManifest {
        PortfolioManifest::new("My Portfolio", entries)
    }

    #[test]
    fn portfolio_entries_validate_when_names_are_safe() {
        let entries = vec![
            PortfolioEntry::new("report.pdf", "Final report", 1),
            PortfolioEntry::new("assets/chart.png", "", 2),
        ];
        assert!(manifest(entries).validate().is_ok());
    }

    #[test]
    fn empty_names_and_oversized_descriptions_are_rejected() {
        let empty = manifest(vec![PortfolioEntry::new("", "desc", 0)]);
        assert!(matches!(
            empty.validate(),
            Err(PortfolioError::InvalidEntry { .. })
        ));
        let long = manifest(vec![PortfolioEntry::new(
            "a.pdf",
            "x".repeat(PORTFOLIO_MAX_DESCRIPTION_BYTES + 1),
            0,
        )]);
        assert!(matches!(
            long.validate(),
            Err(PortfolioError::InvalidEntry { .. })
        ));
    }

    #[test]
    fn duplicate_names_are_rejected_case_insensitively() {
        let duplicates = manifest(vec![
            PortfolioEntry::new("Report.pdf", "one", 0),
            PortfolioEntry::new("report.PDF", "two", 1),
        ]);
        assert!(matches!(
            duplicates.validate(),
            Err(PortfolioError::DuplicateName { .. })
        ));
    }

    #[test]
    fn traversal_and_absolute_names_are_rejected() {
        for bad in [
            "../secret.txt",
            "assets/../../secret.txt",
            "/etc/passwd",
            r"C:\secret.txt",
            r"assets\file.pdf",
            "folder/./file.pdf",
            "folder//file.pdf",
        ] {
            let result = manifest(vec![PortfolioEntry::new(bad, "", 0)]);
            assert!(
                matches!(result.validate(), Err(PortfolioError::InvalidEntry { .. })),
                "expected rejection for '{bad}'"
            );
        }
    }

    #[test]
    fn duplicate_paths_in_different_folders_are_allowed() {
        let entries = vec![
            PortfolioEntry::new("a/file.pdf", "", 0),
            PortfolioEntry::new("b/file.pdf", "", 1),
        ];
        assert!(manifest(entries).validate().is_ok());
    }

    #[test]
    fn manifest_output_is_byte_for_byte_deterministic_and_escaped() {
        let entries = vec![
            PortfolioEntry::new("b.pdf", "B & <final>", 2),
            PortfolioEntry::new("a.pdf", "A \"quoted\"", 1),
        ];
        let first = manifest(entries.clone()).serialize().expect("descriptor");
        let second = manifest(entries).serialize().expect("descriptor");
        assert_eq!(first, second);
        assert!(first.starts_with(declaration()));
        assert!(first.contains("&amp;"));
        assert!(first.contains("&lt;final&gt;"));
        assert!(first.contains("&quot;quoted&quot;"));
        let a = first.find("name=\"a.pdf\"").expect("a entry");
        let b = first.find("name=\"b.pdf\"").expect("b entry");
        assert!(a < b, "entries must be sorted by order");
    }

    #[test]
    fn empty_manifest_is_valid_and_serializes_to_an_empty_document() {
        let empty = manifest(Vec::new());
        assert!(empty.validate().is_ok());
        let descriptor = empty.serialize().expect("descriptor");
        assert!(descriptor.contains("<pdfxi-portfolio"));
        assert!(!descriptor.contains("<entry"));
        assert!(descriptor.ends_with("</pdfxi-portfolio>\n"));
    }

    #[test]
    fn empty_title_and_too_many_entries_are_rejected() {
        let empty_title = PortfolioManifest::new("", Vec::new());
        assert_eq!(empty_title.validate(), Err(PortfolioError::EmptyTitle));

        let entries: Vec<PortfolioEntry> = (0..PORTFOLIO_MAX_ENTRIES + 1)
            .map(|index| PortfolioEntry::new(format!("f{index}.pdf"), "", index))
            .collect();
        assert!(matches!(
            manifest(entries).validate(),
            Err(PortfolioError::TooManyEntries { .. })
        ));
    }

    #[test]
    fn free_function_matches_method_serialization() {
        let entries = vec![PortfolioEntry::new("x.pdf", "desc", 0)];
        let portfolio = manifest(entries);
        assert_eq!(
            serialize_portfolio(&portfolio).expect("descriptor"),
            portfolio.serialize().expect("descriptor")
        );
    }
}
