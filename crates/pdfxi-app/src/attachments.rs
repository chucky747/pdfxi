//! Embedded-file attachment list model.
//!
//! This module implements the *model* behind the viewer's Attachments panel. It
//! is dependency-light: no `egui`, renderer, or PDF backend dependency. A caller
//! parses a document's embedded-file name tree into an [`AttachmentSummary`], and
//! [`AttachmentModel::from_summary`] validates it into a deterministically
//! ordered list. [`AttachmentState`] holds the per-document selection and status,
//! and [`AttachmentRegistry`] isolates that state per open tab.
//!
//! # Ordering and determinism
//!
//! Rows are sorted by a case-insensitive name key, then by the raw name and page
//! to break ties deterministically. No hash or locale ordering is used for the
//! displayed list, so the same summary always renders the same order.
//!
//! # Host open boundary
//!
//! This crate deliberately holds only attachment *metadata* (name, description,
//! size, and page association), never the embedded file bytes. Activating an
//! attachment produces an [`AttachmentOpenRequest`]; the host shell that owns a
//! stream backend is responsible for extracting the bytes and opening or saving
//! them. The model never performs filesystem or network I/O. See
//! [`AttachmentOpenRequest`] and [`crate::ui`] (with the `ui` feature) for the
//! shell-side boundary.

use std::collections::HashMap;

use crate::tabs::TabId;

/// Maximum number of attachments accepted in one summary.
pub const MAX_ATTACHMENTS: usize = 8192;

/// Maximum number of characters in an attachment name.
pub const MAX_ATTACHMENT_NAME_LEN: usize = 512;

/// Maximum number of characters in an attachment description.
pub const MAX_ATTACHMENT_DESCRIPTION_LEN: usize = 1024;

/// One caller-supplied attachment (embedded file) declaration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttachmentInfo {
    name: String,
    description: String,
    size: u64,
    page: Option<usize>,
}

impl AttachmentInfo {
    /// Describe one embedded file.
    ///
    /// `page` is the zero-based page the file is associated with, if the caller
    /// can determine one. `size` is the uncompressed stream length in bytes.
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        size: u64,
        page: Option<usize>,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            size,
            page,
        }
    }

    /// The attachment name (its file-spec `/UF` or `/F`).
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The human-readable description, which may be empty.
    pub fn description(&self) -> &str {
        &self.description
    }

    /// The uncompressed stream size in bytes.
    pub fn size(&self) -> u64 {
        self.size
    }

    /// The zero-based associated page, if known.
    pub fn page(&self) -> Option<usize> {
        self.page
    }
}

/// Caller-supplied embedded-file data.
#[derive(Clone, Debug, Default)]
pub struct AttachmentSummary {
    attachments: Vec<AttachmentInfo>,
}

impl AttachmentSummary {
    /// Create an empty summary.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add one attachment declaration.
    #[allow(clippy::should_implement_trait)] // builder method, not `std::ops::Add`
    pub fn add(mut self, attachment: AttachmentInfo) -> Self {
        self.attachments.push(attachment);
        self
    }

    /// The declared attachments.
    pub fn attachments(&self) -> &[AttachmentInfo] {
        &self.attachments
    }
}

/// Typed errors produced while parsing or manipulating attachments.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AttachmentError {
    /// More than [`MAX_ATTACHMENTS`] attachments were declared.
    #[error("too many attachments (maximum {MAX_ATTACHMENTS})")]
    TooManyAttachments,
    /// An attachment had an empty or over-long name.
    #[error("attachment name must be between 1 and {MAX_ATTACHMENT_NAME_LEN} characters")]
    InvalidName,
    /// An attachment description exceeded the length limit.
    #[error("attachment description exceeds {MAX_ATTACHMENT_DESCRIPTION_LEN} characters")]
    DescriptionTooLong(String),
    /// Two attachments shared a name.
    #[error("duplicate attachment name '{0}'")]
    DuplicateName(String),
    /// An attachment's associated page was outside the document.
    #[error("attachment '{0}' references page {1} outside the document")]
    PageOutOfBounds(String, usize),
    /// A runtime operation named an attachment that is not in the model.
    #[error("no attachment named '{0}'")]
    AttachmentNotFound(String),
    /// The tab has no installed attachment model.
    #[error("tab {0} has no attachments")]
    NoAttachmentsForTab(TabId),
}

/// A read-only view of one attachment in an [`AttachmentModel`].
#[derive(Clone, Copy, Debug)]
pub struct Attachment<'a> {
    info: &'a AttachmentInfo,
}

impl<'a> Attachment<'a> {
    fn new(info: &'a AttachmentInfo) -> Self {
        Self { info }
    }

    /// The attachment name.
    pub fn name(&self) -> &'a str {
        &self.info.name
    }

    /// The attachment description.
    pub fn description(&self) -> &'a str {
        &self.info.description
    }

    /// The uncompressed size in bytes.
    pub fn size(&self) -> u64 {
        self.info.size
    }

    /// The zero-based associated page, if known.
    pub fn page(&self) -> Option<usize> {
        self.info.page
    }
}

/// A validated, deterministically ordered attachment list.
///
/// Construct one with [`Self::from_summary`]. The model is immutable; mutable
/// selection and status live in [`AttachmentState`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttachmentModel {
    attachments: Vec<AttachmentInfo>,
    index: HashMap<String, usize>,
}

impl AttachmentModel {
    /// Validate `summary` into a deterministic attachment list.
    ///
    /// `page_count` bounds-checks associated pages; pass `0` to skip. Validation
    /// rejects empty or over-long names, over-long descriptions, duplicate names,
    /// and out-of-range associated pages. Resulting rows are sorted by a
    /// case-insensitive name key, then by raw name and page for stable ties.
    pub fn from_summary(
        summary: AttachmentSummary,
        page_count: usize,
    ) -> Result<Self, AttachmentError> {
        if summary.attachments.len() > MAX_ATTACHMENTS {
            return Err(AttachmentError::TooManyAttachments);
        }
        let mut attachments = summary.attachments;
        let mut index: HashMap<String, usize> = HashMap::new();
        for attachment in &attachments {
            let name_len = attachment.name.chars().count();
            if name_len == 0 || name_len > MAX_ATTACHMENT_NAME_LEN {
                return Err(AttachmentError::InvalidName);
            }
            if attachment.description.chars().count() > MAX_ATTACHMENT_DESCRIPTION_LEN {
                return Err(AttachmentError::DescriptionTooLong(attachment.name.clone()));
            }
            if let Some(page) = attachment.page
                && page_count > 0
                && page >= page_count
            {
                return Err(AttachmentError::PageOutOfBounds(
                    attachment.name.clone(),
                    page,
                ));
            }
            if index.insert(attachment.name.clone(), 0).is_some() {
                return Err(AttachmentError::DuplicateName(attachment.name.clone()));
            }
        }
        attachments.sort_by(|left, right| {
            let left_key = left.name.to_lowercase();
            let right_key = right.name.to_lowercase();
            left_key
                .cmp(&right_key)
                .then_with(|| left.name.cmp(&right.name))
                .then_with(|| left.page.cmp(&right.page))
        });
        for (position, attachment) in attachments.iter().enumerate() {
            index.insert(attachment.name.clone(), position);
        }
        Ok(Self { attachments, index })
    }

    /// The number of attachments in this model.
    pub fn len(&self) -> usize {
        self.attachments.len()
    }

    /// Whether this model has no attachments.
    pub fn is_empty(&self) -> bool {
        self.attachments.is_empty()
    }

    /// Iterate over attachments in deterministic display order.
    pub fn attachments(&self) -> impl ExactSizeIterator<Item = Attachment<'_>> + '_ {
        self.attachments.iter().map(Attachment::new)
    }

    /// Look up an attachment by exact name.
    pub fn get(&self, name: &str) -> Option<Attachment<'_>> {
        self.index
            .get(name)
            .map(|position| Attachment::new(&self.attachments[*position]))
    }

    /// Display position of an attachment by exact name.
    pub fn position(&self, name: &str) -> Option<usize> {
        self.index.get(name).copied()
    }

    /// Sum of all attachment sizes, saturating at `u64::MAX`.
    pub fn total_size(&self) -> u64 {
        self.attachments
            .iter()
            .fold(0u64, |sum, attachment| sum.saturating_add(attachment.size))
    }
}

/// A request produced when the user activates an attachment.
///
/// This is the host open boundary: the app crate records *what* the user wants
/// opened but never opens the bytes itself. A host with a stream backend
/// consumes the `name` and performs extraction/opening/saving. This type carries
/// no file contents, so it is safe to pass across module boundaries.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttachmentOpenRequest {
    name: String,
    page: Option<usize>,
}

impl AttachmentOpenRequest {
    /// Build an open request for the named attachment.
    pub fn new(name: impl Into<String>, page: Option<usize>) -> Self {
        Self {
            name: name.into(),
            page,
        }
    }

    /// The name of the attachment the host should open.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The zero-based associated page, if known.
    pub fn page(&self) -> Option<usize> {
        self.page
    }
}

/// Mutable per-document attachment selection and status.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AttachmentState {
    selected: Option<String>,
    status: String,
}

impl AttachmentState {
    /// Create an initial, empty selection state.
    pub fn new() -> Self {
        Self::default()
    }

    /// Select `name`, returning a typed error if it is not in `model`.
    pub fn select(&mut self, model: &AttachmentModel, name: &str) -> Result<(), AttachmentError> {
        if model.get(name).is_none() {
            return Err(AttachmentError::AttachmentNotFound(name.to_string()));
        }
        self.selected = Some(name.to_string());
        self.status = format!("Selected {name}");
        Ok(())
    }

    /// Clear the selection and status.
    pub fn clear(&mut self) {
        self.selected = None;
        self.status.clear();
    }

    /// The currently selected attachment name, if any.
    pub fn selected(&self) -> Option<&str> {
        self.selected.as_deref()
    }

    /// The most recent user-facing status line.
    pub fn status(&self) -> &str {
        &self.status
    }

    /// Set the status line directly (used by the host after an open request).
    pub fn set_status(&mut self, status: impl Into<String>) {
        self.status = status.into();
    }
}

/// One tab's installed attachment model and state.
#[derive(Clone, Debug)]
struct AttachmentDocument {
    model: AttachmentModel,
    state: AttachmentState,
}

/// Per-tab registry of attachment lists and their selection state.
///
/// Each tab gets its own [`AttachmentState`], so selecting an attachment in one
/// document never affects another. All operations are in-memory and never block.
#[derive(Debug, Default)]
pub struct AttachmentRegistry {
    documents: HashMap<TabId, AttachmentDocument>,
}

impl AttachmentRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Install a validated attachment list for `tab`, resetting its state.
    pub fn install(
        &mut self,
        tab: TabId,
        summary: AttachmentSummary,
        page_count: usize,
    ) -> Result<(), AttachmentError> {
        let model = AttachmentModel::from_summary(summary, page_count)?;
        self.documents.insert(
            tab,
            AttachmentDocument {
                model,
                state: AttachmentState::new(),
            },
        );
        Ok(())
    }

    /// Remove a tab's attachments, returning `true` if any were installed.
    pub fn remove(&mut self, tab: TabId) -> bool {
        self.documents.remove(&tab).is_some()
    }

    /// Whether `tab` has attachments installed.
    pub fn contains(&self, tab: TabId) -> bool {
        self.documents.contains_key(&tab)
    }

    /// The attachment model installed for `tab`, if any.
    pub fn model(&self, tab: TabId) -> Option<&AttachmentModel> {
        self.documents.get(&tab).map(|document| &document.model)
    }

    /// The selection state installed for `tab`, if any.
    pub fn state(&self, tab: TabId) -> Option<&AttachmentState> {
        self.documents.get(&tab).map(|document| &document.state)
    }

    /// Mutable selection state for `tab`, if any.
    pub fn state_mut(&mut self, tab: TabId) -> Option<&mut AttachmentState> {
        self.documents
            .get_mut(&tab)
            .map(|document| &mut document.state)
    }

    /// Select an attachment for `tab`.
    pub fn select(&mut self, tab: TabId, name: &str) -> Result<(), AttachmentError> {
        let document = self
            .documents
            .get_mut(&tab)
            .ok_or(AttachmentError::NoAttachmentsForTab(tab))?;
        document.state.select(&document.model, name)
    }

    /// Build an open request for an attachment on `tab` without doing any I/O.
    pub fn open_request(
        &self,
        tab: TabId,
        name: &str,
    ) -> Result<AttachmentOpenRequest, AttachmentError> {
        let document = self
            .documents
            .get(&tab)
            .ok_or(AttachmentError::NoAttachmentsForTab(tab))?;
        let attachment = document
            .model
            .get(name)
            .ok_or_else(|| AttachmentError::AttachmentNotFound(name.to_string()))?;
        Ok(AttachmentOpenRequest::new(
            attachment.name(),
            attachment.page(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tab(id: u64) -> TabId {
        TabId::from_u64(id)
    }

    fn summary() -> AttachmentSummary {
        AttachmentSummary::new()
            .add(AttachmentInfo::new("zeta.txt", "Last", 30, Some(2)))
            .add(AttachmentInfo::new("Alpha.pdf", "First", 1024, Some(0)))
            .add(AttachmentInfo::new("beta.bin", "", 7, None))
    }

    #[test]
    fn attachment_order_is_deterministic_and_case_insensitive() {
        let model = AttachmentModel::from_summary(summary(), 5).expect("valid");
        let names: Vec<&str> = model.attachments().map(|item| item.name()).collect();
        assert_eq!(names, vec!["Alpha.pdf", "beta.bin", "zeta.txt"]);
        assert_eq!(model.position("zeta.txt"), Some(2));
        // Building twice yields the same order.
        let again = AttachmentModel::from_summary(summary(), 5).expect("valid");
        assert_eq!(model, again);
    }

    #[test]
    fn attachment_size_and_page_metadata_are_preserved() {
        let model = AttachmentModel::from_summary(summary(), 5).expect("valid");
        let alpha = model.get("Alpha.pdf").expect("present");
        assert_eq!(alpha.size(), 1024);
        assert_eq!(alpha.page(), Some(0));
        assert_eq!(alpha.description(), "First");
        assert_eq!(model.get("beta.bin").expect("present").page(), None);
        assert_eq!(model.total_size(), 30 + 1024 + 7);
    }

    #[test]
    fn empty_attachment_summary_has_no_rows() {
        let model = AttachmentModel::from_summary(AttachmentSummary::new(), 3).expect("empty");
        assert!(model.is_empty());
        assert_eq!(model.len(), 0);
        assert_eq!(model.total_size(), 0);
        assert!(model.get("anything").is_none());
    }

    #[test]
    fn selection_and_status_work_and_reject_unknown_names() {
        let model = AttachmentModel::from_summary(summary(), 5).expect("valid");
        let mut state = AttachmentState::new();
        assert_eq!(
            state.select(&model, "ghost"),
            Err(AttachmentError::AttachmentNotFound("ghost".into()))
        );
        state.select(&model, "Alpha.pdf").expect("known");
        assert_eq!(state.selected(), Some("Alpha.pdf"));
        assert_eq!(state.status(), "Selected Alpha.pdf");
        state.set_status("Opened by host");
        assert_eq!(state.status(), "Opened by host");
        state.clear();
        assert_eq!(state.selected(), None);
        assert_eq!(state.status(), "");
    }

    #[test]
    fn invalid_attachments_are_rejected() {
        let base = || AttachmentSummary::new().add(AttachmentInfo::new("ok.txt", "", 1, None));
        assert_eq!(
            AttachmentModel::from_summary(
                AttachmentSummary::new().add(AttachmentInfo::new("", "", 1, None)),
                5
            ),
            Err(AttachmentError::InvalidName)
        );
        assert_eq!(
            AttachmentModel::from_summary(
                base().add(AttachmentInfo::new("ok.txt", "dup", 2, None)),
                5
            ),
            Err(AttachmentError::DuplicateName("ok.txt".into()))
        );
        assert_eq!(
            AttachmentModel::from_summary(
                base().add(AttachmentInfo::new("bad.txt", "", 2, Some(9))),
                5
            ),
            Err(AttachmentError::PageOutOfBounds("bad.txt".into(), 9))
        );
        let long = "x".repeat(MAX_ATTACHMENT_DESCRIPTION_LEN + 1);
        assert!(matches!(
            AttachmentModel::from_summary(
                AttachmentSummary::new().add(AttachmentInfo::new("a.txt", long, 1, None)),
                5
            ),
            Err(AttachmentError::DescriptionTooLong(_))
        ));
    }

    #[test]
    fn per_tab_attachment_state_is_isolated() {
        let mut registry = AttachmentRegistry::new();
        let a = tab(1);
        let b = tab(2);
        registry.install(a, summary(), 5).expect("a");
        registry.install(b, summary(), 5).expect("b");
        registry.select(a, "Alpha.pdf").expect("known");
        assert_eq!(registry.state(a).expect("a").selected(), Some("Alpha.pdf"));
        assert_eq!(registry.state(b).expect("b").selected(), None);
        assert!(registry.remove(a));
        assert!(!registry.contains(a));
        assert!(registry.contains(b));
        assert!(registry.model(a).is_none());
    }

    #[test]
    fn open_request_is_a_metadata_boundary_without_io() {
        let mut registry = AttachmentRegistry::new();
        let a = tab(1);
        assert_eq!(
            registry.open_request(a, "Alpha.pdf"),
            Err(AttachmentError::NoAttachmentsForTab(a))
        );
        registry.install(a, summary(), 5).expect("a");
        let request = registry.open_request(a, "Alpha.pdf").expect("request");
        assert_eq!(request.name(), "Alpha.pdf");
        assert_eq!(request.page(), Some(0));
        assert_eq!(
            registry.open_request(a, "ghost"),
            Err(AttachmentError::AttachmentNotFound("ghost".into()))
        );
    }

    #[test]
    fn failed_install_leaves_previous_state_untouched() {
        let mut registry = AttachmentRegistry::new();
        let a = tab(1);
        registry.install(a, summary(), 5).expect("a");
        registry.select(a, "Alpha.pdf").expect("known");
        let bad = AttachmentSummary::new().add(AttachmentInfo::new("", "", 1, None));
        assert!(registry.install(a, bad, 5).is_err());
        assert_eq!(
            registry.state(a).expect("kept").selected(),
            Some("Alpha.pdf")
        );
        assert_eq!(
            registry.select(a, "ghost"),
            Err(AttachmentError::AttachmentNotFound("ghost".into()))
        );
    }
}
