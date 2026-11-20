//! Deterministic PDF Portfolio creation and customization for CREATE-010
//! (ADR 0008).
//!
//! [`crate::portfolio::PortfolioManifest`] already validates entries and
//! serializes a stable XML descriptor. This module supplies the missing
//! *creation/customization* contract: a small builder that adds, removes, and
//! edits named entries before producing a validated manifest and its
//! deterministic serialization. Lookups follow the manifest's
//! case-insensitive name rule, and no operation touches the file system.

use crate::portfolio::{PortfolioEntry, PortfolioError, PortfolioManifest, serialize_portfolio};

/// A typed failure while customizing a portfolio builder.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PortfolioBuildError {
    /// No entry matched the requested case-insensitive name.
    #[error("no portfolio entry named '{name}'")]
    NotFound {
        /// The name that was requested.
        name: String,
    },
    /// The resulting manifest failed validation.
    #[error("invalid portfolio manifest: {0}")]
    Manifest(#[from] PortfolioError),
}

/// A mutable builder for a [`PortfolioManifest`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortfolioBuilder {
    title: String,
    entries: Vec<PortfolioEntry>,
}

impl PortfolioBuilder {
    /// Start a builder with a portfolio title.
    #[must_use]
    pub fn new(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            entries: Vec::new(),
        }
    }

    /// Return the current title.
    #[must_use]
    pub fn title(&self) -> &str {
        &self.title
    }

    /// Return entries in insertion order.
    #[must_use]
    pub fn entries(&self) -> &[PortfolioEntry] {
        &self.entries
    }

    /// Append a fully formed entry.
    pub fn add(&mut self, entry: PortfolioEntry) -> &mut Self {
        self.entries.push(entry);
        self
    }

    /// Append an entry from its constituent fields.
    pub fn add_entry(
        &mut self,
        name: impl Into<String>,
        description: impl Into<String>,
        order: usize,
    ) -> &mut Self {
        self.add(PortfolioEntry::new(name, description, order))
    }

    /// Find the zero-based position of a case-insensitive entry name.
    fn position_of(&self, name: &str) -> Option<usize> {
        let wanted = name.to_ascii_lowercase();
        self.entries
            .iter()
            .position(|entry| entry.name().to_ascii_lowercase() == wanted)
    }

    /// Remove and return an entry by case-insensitive name.
    ///
    /// # Errors
    ///
    /// Returns [`PortfolioBuildError::NotFound`] when no entry matches.
    pub fn remove(&mut self, name: &str) -> Result<PortfolioEntry, PortfolioBuildError> {
        let position = self
            .position_of(name)
            .ok_or_else(|| PortfolioBuildError::NotFound {
                name: name.to_owned(),
            })?;
        Ok(self.entries.remove(position))
    }

    /// Replace the description of an existing entry.
    ///
    /// # Errors
    ///
    /// Returns [`PortfolioBuildError::NotFound`] when no entry matches.
    pub fn set_description(
        &mut self,
        name: &str,
        description: impl Into<String>,
    ) -> Result<(), PortfolioBuildError> {
        let position = self
            .position_of(name)
            .ok_or_else(|| PortfolioBuildError::NotFound {
                name: name.to_owned(),
            })?;
        let entry = &mut self.entries[position];
        let replacement = PortfolioEntry::new(entry.name(), description, entry.order());
        self.entries[position] = replacement;
        Ok(())
    }

    /// Replace the sort key of an existing entry.
    ///
    /// # Errors
    ///
    /// Returns [`PortfolioBuildError::NotFound`] when no entry matches.
    pub fn set_order(&mut self, name: &str, order: usize) -> Result<(), PortfolioBuildError> {
        let position = self
            .position_of(name)
            .ok_or_else(|| PortfolioBuildError::NotFound {
                name: name.to_owned(),
            })?;
        let entry = &mut self.entries[position];
        let replacement = PortfolioEntry::new(entry.name(), entry.description(), order);
        self.entries[position] = replacement;
        Ok(())
    }

    /// Validate and finish the manifest.
    ///
    /// # Errors
    ///
    /// Returns [`PortfolioBuildError::Manifest`] when the assembled manifest
    /// fails [`PortfolioManifest::validate`].
    pub fn build(self) -> Result<PortfolioManifest, PortfolioBuildError> {
        let manifest = PortfolioManifest::new(self.title, self.entries);
        manifest.validate()?;
        Ok(manifest)
    }

    /// Validate, finish, and serialize the manifest deterministically.
    ///
    /// # Errors
    ///
    /// Returns the same errors as [`PortfolioBuilder::build`].
    pub fn build_serialized(self) -> Result<(PortfolioManifest, String), PortfolioBuildError> {
        let manifest = self.build()?;
        let descriptor = serialize_portfolio(&manifest)?;
        Ok((manifest, descriptor))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn portfolio_builder_creates_and_customizes_a_manifest() {
        let mut builder = PortfolioBuilder::new("Evidence pack");
        builder
            .add_entry("notes.txt", "Notes", 2)
            .add_entry("figures/chart.png", "Chart", 1);
        assert!(
            builder
                .set_description("NOTES.TXT", "Updated notes")
                .is_ok()
        );
        assert!(builder.set_order("notes.txt", 0).is_ok());
        let removed = builder.remove("figures/chart.png");
        assert!(removed.is_ok());
        assert_eq!(removed.expect("removed entry").name(), "figures/chart.png");
        let manifest = builder.build();
        assert!(manifest.is_ok());
        let manifest = manifest.expect("manifest");
        assert_eq!(manifest.entries().len(), 1);
        assert_eq!(manifest.entries()[0].description(), "Updated notes");
        assert_eq!(manifest.entries()[0].order(), 0);
    }

    #[test]
    fn portfolio_builder_rejects_unknown_and_duplicate_entries() {
        let mut builder = PortfolioBuilder::new("Pack");
        assert_eq!(
            builder.remove("missing.txt"),
            Err(PortfolioBuildError::NotFound {
                name: "missing.txt".to_owned()
            })
        );
        builder.add_entry("a.txt", "", 0).add_entry("A.TXT", "", 1);
        assert!(matches!(
            builder.build(),
            Err(PortfolioBuildError::Manifest(
                PortfolioError::DuplicateName { .. }
            ))
        ));
    }

    #[test]
    fn equivalent_builder_sequences_serialize_identically() {
        let mut first = PortfolioBuilder::new("Pack");
        first
            .add_entry("a.txt", "A", 0)
            .add_entry("b.txt", "B", 1)
            .add_entry("c.txt", "C", 2);
        let mut second = PortfolioBuilder::new("Pack");
        second.add_entry("c.txt", "C", 2).add_entry("b.txt", "B", 1);
        assert!(second.set_order("b.txt", 1).is_ok());
        second.add_entry("a.txt", "A", 0);
        let first_serialized = first.build_serialized();
        let second_serialized = second.build_serialized();
        assert!(first_serialized.is_ok() && second_serialized.is_ok());
        let (_, first_xml) = first_serialized.expect("first descriptor");
        let (_, second_xml) = second_serialized.expect("second descriptor");
        assert_eq!(first_xml, second_xml);
    }
}
