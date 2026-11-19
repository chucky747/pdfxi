//! Sanitization plans, actions, and removal verification.
//!
//! A [`Sanitizer`] selects which categories of hidden information to remove.
//! [`Sanitizer::plan`] expands that selection into a deterministic, ordered list
//! of [`SanitizationAction`]s. After the caller applies those actions to a
//! document, [`SanitizationReport::verify`] checks a caller-supplied inventory
//! to confirm the targeted categories are gone.
//!
//! This crate never opens a PDF and never logs document contents; the inventory
//! is supplied by the layer that owns the object graph. Category identifiers
//! are stable strings so callers can persist or compare plans.

use std::collections::BTreeSet;
use std::fmt;

use crate::error::{Result, SecurityError};

/// A category of hidden information that can be sanitized.
///
/// The declaration order is also the canonical ordering used by plans and
/// reports: metadata, embedded files, annotations, JavaScript, form values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SanitizationCategory {
    /// Document Info dictionary and XMP metadata stream.
    Metadata,
    /// Embedded-file name trees and their attachment streams.
    EmbeddedFiles,
    /// Page annotations (including their appearance streams).
    Annotations,
    /// Document- and action-level JavaScript.
    JavaScript,
    /// Interactive form field values.
    FormValues,
}

impl SanitizationCategory {
    /// Every category in canonical order.
    pub const ALL: [Self; 5] = [
        Self::Metadata,
        Self::EmbeddedFiles,
        Self::Annotations,
        Self::JavaScript,
        Self::FormValues,
    ];

    /// Stable string identifier suitable for persistence or comparison.
    pub const fn as_id(self) -> &'static str {
        match self {
            Self::Metadata => "metadata",
            Self::EmbeddedFiles => "embedded_files",
            Self::Annotations => "annotations",
            Self::JavaScript => "javascript",
            Self::FormValues => "form_values",
        }
    }

    /// Parse an identifier produced by [`Self::as_id`].
    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|category| category.as_id() == id)
    }
}

impl fmt::Display for SanitizationCategory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_id())
    }
}

/// How a sanitization action changes its target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SanitizationOperation {
    /// Delete the entry or stream entirely.
    Remove,
    /// Clear the stored values while keeping the containing structure.
    ClearValues,
}

impl SanitizationOperation {
    /// Stable string identifier for this operation.
    pub const fn as_id(self) -> &'static str {
        match self {
            Self::Remove => "remove",
            Self::ClearValues => "clear_values",
        }
    }
}

impl fmt::Display for SanitizationOperation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_id())
    }
}

/// One canonical step in a sanitization plan.
///
/// Actions are ordered by category, then target key, then operation, so a plan
/// is byte-for-byte reproducible across runs.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct SanitizationAction {
    category: SanitizationCategory,
    target: &'static str,
    operation: SanitizationOperation,
}

impl SanitizationAction {
    /// Build an action from its category, target key, and operation.
    pub const fn new(
        category: SanitizationCategory,
        target: &'static str,
        operation: SanitizationOperation,
    ) -> Self {
        Self {
            category,
            target,
            operation,
        }
    }

    /// Category this action belongs to.
    pub const fn category(&self) -> SanitizationCategory {
        self.category
    }

    /// Stable object-graph key this action operates on (for example `Info`).
    pub const fn target(&self) -> &'static str {
        self.target
    }

    /// What the action does to the target.
    pub const fn operation(&self) -> SanitizationOperation {
        self.operation
    }
}

/// A selection of hidden-information categories to remove.
///
/// Use [`Self::all`] for a full sanitization or [`Self::empty`] followed by
/// [`Self::include`] for a targeted one. [`Self::plan`] is deterministic.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Sanitizer {
    categories: BTreeSet<SanitizationCategory>,
}

impl Sanitizer {
    /// Select every known category.
    pub fn all() -> Self {
        Self {
            categories: SanitizationCategory::ALL.into_iter().collect(),
        }
    }

    /// Select no categories.
    pub fn empty() -> Self {
        Self::default()
    }

    /// Build a sanitizer from an explicit set of categories.
    pub fn new(categories: impl IntoIterator<Item = SanitizationCategory>) -> Self {
        Self {
            categories: categories.into_iter().collect(),
        }
    }

    /// Add a category to the selection.
    pub fn include(&mut self, category: SanitizationCategory) {
        self.categories.insert(category);
    }

    /// Remove a category from the selection.
    pub fn exclude(&mut self, category: SanitizationCategory) {
        self.categories.remove(&category);
    }

    /// Return `true` when `category` is selected.
    pub fn contains(&self, category: SanitizationCategory) -> bool {
        self.categories.contains(&category)
    }

    /// Selected categories in canonical order.
    pub fn categories(&self) -> impl Iterator<Item = SanitizationCategory> + '_ {
        self.categories.iter().copied()
    }

    /// Return `true` when no categories are selected.
    pub fn is_empty(&self) -> bool {
        self.categories.is_empty()
    }

    /// Number of selected categories.
    pub fn len(&self) -> usize {
        self.categories.len()
    }

    /// Expand the selection into a deterministic ordered action list.
    pub fn plan(&self) -> Vec<SanitizationAction> {
        let mut actions = Vec::new();
        for category in &self.categories {
            for (target, operation) in targets_for(*category) {
                actions.push(SanitizationAction::new(*category, target, *operation));
            }
        }
        actions.sort();
        actions
    }
}

/// Canonical target keys and operations for a category.
fn targets_for(category: SanitizationCategory) -> &'static [(&'static str, SanitizationOperation)] {
    match category {
        SanitizationCategory::Metadata => &[
            ("Info", SanitizationOperation::Remove),
            ("XMP", SanitizationOperation::Remove),
        ],
        SanitizationCategory::EmbeddedFiles => {
            &[("Names/EmbeddedFiles", SanitizationOperation::Remove)]
        }
        SanitizationCategory::Annotations => &[("Page/Annots", SanitizationOperation::Remove)],
        SanitizationCategory::JavaScript => &[
            ("Names/JavaScript", SanitizationOperation::Remove),
            ("OpenAction/JS", SanitizationOperation::Remove),
        ],
        SanitizationCategory::FormValues => {
            &[("AcroForm/FieldValues", SanitizationOperation::ClearValues)]
        }
    }
}

/// Caller-supplied inventory of categories still present in a document.
///
/// The layer that owns the PDF object graph builds this after applying a
/// [`Sanitizer::plan`], and passes it to [`SanitizationReport::verify`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SanitizationInventory {
    present: BTreeSet<SanitizationCategory>,
}

impl SanitizationInventory {
    /// Build an inventory containing `categories`.
    pub fn new(categories: impl IntoIterator<Item = SanitizationCategory>) -> Self {
        Self {
            present: categories.into_iter().collect(),
        }
    }

    /// Build an empty inventory (nothing present).
    pub fn empty() -> Self {
        Self::default()
    }

    /// Record that `category` is present.
    pub fn insert(&mut self, category: SanitizationCategory) {
        self.present.insert(category);
    }

    /// Record that `category` is no longer present.
    pub fn remove(&mut self, category: SanitizationCategory) {
        self.present.remove(&category);
    }

    /// Return `true` when `category` is present.
    pub fn contains(&self, category: SanitizationCategory) -> bool {
        self.present.contains(&category)
    }

    /// Present categories in canonical order.
    pub fn categories(&self) -> impl Iterator<Item = SanitizationCategory> + '_ {
        self.present.iter().copied()
    }

    /// Return `true` when the inventory is empty.
    pub fn is_empty(&self) -> bool {
        self.present.is_empty()
    }

    /// Number of present categories.
    pub fn len(&self) -> usize {
        self.present.len()
    }
}

/// Result of verifying that targeted categories are gone from an inventory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SanitizationReport {
    residuals: Vec<SanitizationCategory>,
    strict: bool,
}

impl SanitizationReport {
    /// Verify a sanitizer against a post-sanitization inventory.
    ///
    /// A category is a residual when it was both selected by `sanitizer` and
    /// still reported present by `inventory`. Unselected categories are never
    /// residuals. This method always produces a report; call [`Self::check`] to
    /// enforce strict mode.
    pub fn verify(sanitizer: &Sanitizer, inventory: &SanitizationInventory, strict: bool) -> Self {
        let residuals: Vec<SanitizationCategory> = sanitizer
            .categories()
            .filter(|category| inventory.contains(*category))
            .collect();
        Self { residuals, strict }
    }

    /// Residual categories in canonical order.
    pub fn residuals(&self) -> &[SanitizationCategory] {
        &self.residuals
    }

    /// Return `true` when no targeted category remains.
    pub fn is_clean(&self) -> bool {
        self.residuals.is_empty()
    }

    /// Whether this report enforces strict verification via [`Self::check`].
    pub const fn strict(&self) -> bool {
        self.strict
    }

    /// Enforce the verification policy.
    ///
    /// In strict mode, returns
    /// [`SecurityError::SanitizationVerificationFailed`] for the first residual
    /// category. In permissive mode this always succeeds; residuals remain
    /// available via [`Self::residuals`].
    ///
    /// # Errors
    ///
    /// See above.
    pub fn check(&self) -> Result<()> {
        if self.strict
            && let Some(&category) = self.residuals.first()
        {
            return Err(SecurityError::SanitizationVerificationFailed { category });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn action_tuples(
        sanitizer: &Sanitizer,
    ) -> Vec<(SanitizationCategory, &'static str, SanitizationOperation)> {
        sanitizer
            .plan()
            .into_iter()
            .map(|action| (action.category(), action.target(), action.operation()))
            .collect()
    }

    #[test]
    fn category_identifiers_round_trip() {
        for category in SanitizationCategory::ALL {
            assert_eq!(
                SanitizationCategory::from_id(category.as_id()),
                Some(category)
            );
        }
        assert_eq!(SanitizationCategory::from_id("unknown"), None);
        assert_eq!(SanitizationCategory::Metadata.to_string(), "metadata");
        assert_eq!(SanitizationOperation::Remove.as_id(), "remove");
        assert_eq!(
            SanitizationOperation::ClearValues.to_string(),
            "clear_values"
        );
    }

    #[test]
    fn full_plan_is_canonical_and_complete() {
        let sanitizer = Sanitizer::all();
        let actions = sanitizer.plan();
        assert_eq!(actions.len(), 7);
        assert_eq!(
            action_tuples(&sanitizer),
            vec![
                (
                    SanitizationCategory::Metadata,
                    "Info",
                    SanitizationOperation::Remove
                ),
                (
                    SanitizationCategory::Metadata,
                    "XMP",
                    SanitizationOperation::Remove
                ),
                (
                    SanitizationCategory::EmbeddedFiles,
                    "Names/EmbeddedFiles",
                    SanitizationOperation::Remove
                ),
                (
                    SanitizationCategory::Annotations,
                    "Page/Annots",
                    SanitizationOperation::Remove
                ),
                (
                    SanitizationCategory::JavaScript,
                    "Names/JavaScript",
                    SanitizationOperation::Remove
                ),
                (
                    SanitizationCategory::JavaScript,
                    "OpenAction/JS",
                    SanitizationOperation::Remove
                ),
                (
                    SanitizationCategory::FormValues,
                    "AcroForm/FieldValues",
                    SanitizationOperation::ClearValues
                ),
            ]
        );
        assert!(actions.windows(2).all(|pair| pair[0] <= pair[1]));
    }

    #[test]
    fn metadata_actions_remove_info_and_xmp() {
        let sanitizer = Sanitizer::new([SanitizationCategory::Metadata]);
        let targets: Vec<&str> = sanitizer
            .plan()
            .iter()
            .map(SanitizationAction::target)
            .collect();
        assert_eq!(targets, vec!["Info", "XMP"]);
        assert!(
            sanitizer
                .plan()
                .iter()
                .all(|action| action.operation() == SanitizationOperation::Remove)
        );
    }

    #[test]
    fn embedded_file_and_annotation_actions_are_planned() {
        let sanitizer = Sanitizer::new([
            SanitizationCategory::EmbeddedFiles,
            SanitizationCategory::Annotations,
        ]);
        let tuples = action_tuples(&sanitizer);
        assert_eq!(
            tuples,
            vec![
                (
                    SanitizationCategory::EmbeddedFiles,
                    "Names/EmbeddedFiles",
                    SanitizationOperation::Remove
                ),
                (
                    SanitizationCategory::Annotations,
                    "Page/Annots",
                    SanitizationOperation::Remove
                ),
            ]
        );
    }

    #[test]
    fn javascript_actions_are_planned() {
        let sanitizer = Sanitizer::new([SanitizationCategory::JavaScript]);
        let targets: Vec<&str> = sanitizer
            .plan()
            .iter()
            .map(SanitizationAction::target)
            .collect();
        assert_eq!(targets, vec!["Names/JavaScript", "OpenAction/JS"]);
    }

    #[test]
    fn form_value_action_clears_values() {
        let sanitizer = Sanitizer::new([SanitizationCategory::FormValues]);
        let actions = sanitizer.plan();
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].target(), "AcroForm/FieldValues");
        assert_eq!(actions[0].operation(), SanitizationOperation::ClearValues);
    }

    #[test]
    fn include_exclude_change_the_plan() {
        let mut sanitizer = Sanitizer::empty();
        assert!(sanitizer.is_empty());
        sanitizer.include(SanitizationCategory::Metadata);
        assert!(sanitizer.contains(SanitizationCategory::Metadata));
        assert_eq!(sanitizer.len(), 1);
        sanitizer.exclude(SanitizationCategory::Metadata);
        assert!(sanitizer.is_empty());
        assert!(sanitizer.plan().is_empty());
    }

    #[test]
    fn verification_confirms_categories_are_removed() {
        let sanitizer = Sanitizer::all();
        let inventory = SanitizationInventory::empty();
        let report = SanitizationReport::verify(&sanitizer, &inventory, true);
        assert!(report.is_clean());
        assert!(report.residuals().is_empty());
        assert!(report.check().is_ok());
    }

    #[test]
    fn verification_detects_residual_categories_in_canonical_order() {
        let sanitizer = Sanitizer::all();
        let inventory = SanitizationInventory::new([
            SanitizationCategory::JavaScript,
            SanitizationCategory::Metadata,
        ]);
        assert_eq!(inventory.len(), 2);
        assert_eq!(
            inventory.categories().collect::<Vec<_>>(),
            vec![
                SanitizationCategory::Metadata,
                SanitizationCategory::JavaScript
            ]
        );
        let report = SanitizationReport::verify(&sanitizer, &inventory, false);
        assert!(!report.is_clean());
        assert_eq!(
            report.residuals(),
            &[
                SanitizationCategory::Metadata,
                SanitizationCategory::JavaScript
            ]
        );
        assert!(report.check().is_ok());
    }

    #[test]
    fn strict_mode_fails_when_a_category_remains() {
        let sanitizer = Sanitizer::new([SanitizationCategory::EmbeddedFiles]);
        let mut inventory = SanitizationInventory::empty();
        inventory.insert(SanitizationCategory::EmbeddedFiles);
        let report = SanitizationReport::verify(&sanitizer, &inventory, true);
        assert!(report.strict());
        assert_eq!(
            report.check(),
            Err(SecurityError::SanitizationVerificationFailed {
                category: SanitizationCategory::EmbeddedFiles
            })
        );
        inventory.remove(SanitizationCategory::EmbeddedFiles);
        assert!(inventory.is_empty());
        let clean = SanitizationReport::verify(&sanitizer, &inventory, true);
        assert!(clean.check().is_ok());
    }

    #[test]
    fn permissive_mode_reports_without_failing() {
        let sanitizer = Sanitizer::new([SanitizationCategory::Annotations]);
        let inventory = SanitizationInventory::new([SanitizationCategory::Annotations]);
        let report = SanitizationReport::verify(&sanitizer, &inventory, false);
        assert!(!report.is_clean());
        assert!(!report.strict());
        assert!(report.check().is_ok());
    }

    #[test]
    fn unselected_categories_are_never_residuals() {
        let sanitizer = Sanitizer::new([SanitizationCategory::Metadata]);
        let inventory = SanitizationInventory::new([
            SanitizationCategory::Annotations,
            SanitizationCategory::FormValues,
        ]);
        let report = SanitizationReport::verify(&sanitizer, &inventory, true);
        assert!(report.is_clean());
        assert!(report.check().is_ok());
    }

    #[test]
    fn empty_plan_produces_no_actions_and_verifies_clean() {
        let sanitizer = Sanitizer::empty();
        assert!(sanitizer.plan().is_empty());
        let report = SanitizationReport::verify(
            &sanitizer,
            &SanitizationInventory::new(SanitizationCategory::ALL),
            true,
        );
        assert!(report.is_clean());
        assert!(report.check().is_ok());
    }

    #[test]
    fn plan_is_deterministic_for_equivalent_selections() {
        let forward = Sanitizer::new([
            SanitizationCategory::FormValues,
            SanitizationCategory::Metadata,
            SanitizationCategory::JavaScript,
        ]);
        let reverse = Sanitizer::new([
            SanitizationCategory::JavaScript,
            SanitizationCategory::Metadata,
            SanitizationCategory::FormValues,
        ]);
        assert_eq!(forward, reverse);
        assert_eq!(forward.plan(), reverse.plan());
        assert_eq!(forward.plan(), forward.plan());
    }
}
