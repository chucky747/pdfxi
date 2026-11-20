//! Window-free document-open pipeline.
//!
//! This module records the *outcome* of opening a document rather than holding
//! a live renderer handle, so the whole workflow is deterministic and testable
//! without a GUI or a native PDF library. A [`DocumentOpener`] backend performs
//! the actual load: [`MockDocumentOpener`] is available in every build and is
//! fully deterministic, while `PdfiumDocumentOpener` (behind the `pdfium`
//! feature) validates a real PDF with PDFium and reads its page count, title,
//! and encryption state.
//!
//! [`DocumentOpenPipeline`] drives a backend, checks cancellation before and
//! after the load, maintains a bounded [`RecentFiles`] list on success, and
//! records the [`OpenResult`] against a stable [`TabId`] in [`OpenTabStates`].
//! Every failure is a typed [`OpenError`]; there is no I/O on the UI thread and
//! no panic path in non-test code.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use pdfxi_render::CancellationToken;

use crate::tabs::{TabId, display_name_from_path, normalize_path_key};

/// Typed errors produced by the document-open pipeline.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum OpenError {
    /// The source bytes or path could not be read.
    #[error("cannot read document: {0}")]
    Io(String),
    /// The source is not a supported document format.
    #[error("unsupported document format")]
    UnsupportedFormat,
    /// The document is encrypted or password protected.
    #[error("document is encrypted or password protected")]
    Encrypted,
    /// The document opened but contains no pages.
    #[error("document has no pages")]
    EmptyDocument,
    /// The configured renderer does not support this operation.
    #[error("the renderer does not support this operation")]
    UnsupportedRenderer,
    /// The open was cancelled through its [`CancellationToken`].
    #[error("open was cancelled")]
    Cancelled,
    /// A backend-specific failure that does not fit a typed variant.
    #[error("document backend error: {0}")]
    Backend(String),
}

/// The encryption state recorded for an opened document.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum EncryptionState {
    /// The document has no security handler.
    #[default]
    Unencrypted,
    /// The document has an active security handler at the given revision.
    Encrypted {
        /// Security handler revision reported by the backend (positive).
        revision: i32,
    },
}

impl EncryptionState {
    /// Build a state from a raw security revision (`-1` means unprotected).
    pub fn from_revision(revision: i32) -> Self {
        if revision >= 0 {
            Self::Encrypted { revision }
        } else {
            Self::Unencrypted
        }
    }

    /// Return `true` when the document reports encryption.
    pub fn is_encrypted(self) -> bool {
        matches!(self, Self::Encrypted { .. })
    }

    /// Return the security revision, or `None` for an unencrypted document.
    pub fn revision(self) -> Option<i32> {
        match self {
            Self::Encrypted { revision } => Some(revision),
            Self::Unencrypted => None,
        }
    }
}

/// Which backend served an open request.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum OpenBackendKind {
    /// The deterministic mock backend.
    #[default]
    Mock,
    /// The native PDFium backend.
    Pdfium,
}

/// Metadata recorded after a successful document open.
///
/// This value holds no renderer handle and takes no lock; it is a plain data
/// record suitable for storing in UI state or asserting in tests.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenedDocument {
    /// Human-readable label (file name or caller-supplied bytes label).
    pub label: String,
    /// Source path when the document was opened from a file.
    pub path: Option<PathBuf>,
    /// Number of pages reported by the backend (always positive).
    pub page_count: usize,
    /// Information-dictionary title, when present and non-empty.
    pub title: Option<String>,
    /// Recorded encryption state.
    pub encryption: EncryptionState,
    /// Backend that produced this record.
    pub backend: OpenBackendKind,
}

impl OpenedDocument {
    /// Return `true` when the recorded document is encrypted.
    pub fn is_encrypted(&self) -> bool {
        self.encryption.is_encrypted()
    }

    /// Return the title when present, otherwise the display label.
    pub fn effective_title(&self) -> &str {
        self.title.as_deref().unwrap_or(&self.label)
    }
}

/// The recorded result of one document-open attempt.
#[derive(Clone, Debug, PartialEq)]
pub enum OpenResult {
    /// The document opened successfully.
    Success(OpenedDocument),
    /// The open failed with a typed error.
    Failure(OpenError),
}

impl OpenResult {
    /// Return `true` for a successful open.
    pub fn is_success(&self) -> bool {
        matches!(self, Self::Success(_))
    }

    /// Return `true` for a failed open.
    pub fn is_failure(&self) -> bool {
        matches!(self, Self::Failure(_))
    }

    /// Borrow the opened document, if this is a success.
    pub fn document(&self) -> Option<&OpenedDocument> {
        match self {
            Self::Success(document) => Some(document),
            Self::Failure(_) => None,
        }
    }

    /// Borrow the error, if this is a failure.
    pub fn error(&self) -> Option<&OpenError> {
        match self {
            Self::Success(_) => None,
            Self::Failure(error) => Some(error),
        }
    }
}

/// The lifecycle phase of one tab's document-open request.
///
/// A request is `Loading` from the moment it is dispatched until its typed
/// result arrives, then becomes `Ready` or `Failed`. Keeping the intermediate
/// phase explicit lets a host show progress while I/O runs on a worker without
/// ever blocking the UI thread.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum OpenPhase {
    /// The load has been dispatched and no result has been recorded yet.
    #[default]
    Loading,
    /// The document opened successfully and is ready for viewing.
    Ready,
    /// The load failed with a typed [`OpenError`].
    Failed,
}

impl OpenPhase {
    /// Stable human-readable label for this phase.
    pub fn label(self) -> &'static str {
        match self {
            Self::Loading => "loading",
            Self::Ready => "ready",
            Self::Failed => "failed",
        }
    }
}

/// Recorded open-request state for one tab.
#[derive(Clone, Debug, PartialEq)]
pub struct OpenPhaseState {
    /// The tab this state belongs to.
    pub tab: TabId,
    /// Current lifecycle phase.
    pub phase: OpenPhase,
    /// Number of open attempts started for this tab.
    pub attempts: usize,
    /// Typed outcome of the most recent attempt, if it has completed.
    pub last_result: Option<OpenResult>,
}

/// Per-tab open lifecycle tracker; phases move `Loading -> Ready | Failed`.
///
/// Tabs are stored in a `BTreeMap`, so reports and iteration are deterministic
/// regardless of the order in which requests are started.
#[derive(Clone, Debug, Default)]
pub struct OpenPhases {
    states: BTreeMap<TabId, OpenPhaseState>,
}

impl OpenPhases {
    /// Create an empty tracker.
    pub fn new() -> Self {
        Self::default()
    }

    /// Begin an attempt for `tab`, setting it to [`OpenPhase::Loading`].
    ///
    /// A repeat call while a load is in flight counts as a new attempt and
    /// clears the previous result, so the newest request always wins.
    pub fn begin(&mut self, tab: TabId) -> OpenPhase {
        let state = self.states.entry(tab).or_insert_with(|| OpenPhaseState {
            tab,
            phase: OpenPhase::Loading,
            attempts: 0,
            last_result: None,
        });
        state.attempts += 1;
        state.phase = OpenPhase::Loading;
        state.last_result = None;
        OpenPhase::Loading
    }

    /// Complete `tab`'s in-flight attempt with a typed `result`.
    ///
    /// Returns the resulting [`OpenPhase`]. Beginning a tab that was never
    /// started still records the outcome, so hosts may skip [`Self::begin`]
    /// for synchronous opens.
    pub fn finish(&mut self, tab: TabId, result: OpenResult) -> OpenPhase {
        let phase = if result.is_success() {
            OpenPhase::Ready
        } else {
            OpenPhase::Failed
        };
        let state = self.states.entry(tab).or_insert_with(|| OpenPhaseState {
            tab,
            phase,
            attempts: 1,
            last_result: None,
        });
        state.phase = phase;
        state.last_result = Some(result);
        phase
    }

    /// Borrow the recorded state for `tab`, if any.
    pub fn state(&self, tab: TabId) -> Option<&OpenPhaseState> {
        self.states.get(&tab)
    }

    /// Current phase for `tab`, if any attempt has been started.
    pub fn phase(&self, tab: TabId) -> Option<OpenPhase> {
        self.state(tab).map(|state| state.phase)
    }

    /// Number of tabs with recorded phase state.
    pub fn len(&self) -> usize {
        self.states.len()
    }

    /// Return `true` when no tab has recorded phase state.
    pub fn is_empty(&self) -> bool {
        self.states.is_empty()
    }
}

/// One line in a deterministic [`OpenReport`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenReportEntry {
    /// Tab whose open request this line describes.
    pub tab: TabId,
    /// Current lifecycle phase of the request.
    pub phase: OpenPhase,
    /// Stable label of the source (document title/label or an error marker).
    pub label: String,
    /// Deterministic description of the recorded outcome.
    pub outcome: String,
}

/// A deterministic report of all open activity tracked by a pipeline.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenReport {
    /// Entries ordered by ascending [`TabId`].
    pub entries: Vec<OpenReportEntry>,
    /// Stable summary of the whole report.
    pub summary: String,
}

/// A backend capable of loading a document and reporting its metadata.
///
/// Implementations must check `cancellation` before doing work, never panic on
/// hostile input, and return a typed [`OpenError`] for every failure.
pub trait DocumentOpener {
    /// Open a document from a filesystem path.
    ///
    /// # Errors
    ///
    /// Returns a typed [`OpenError`] when the path is unusable, the format is
    /// unsupported, the document is invalid, or cancellation fires.
    fn open_path(
        &self,
        path: &Path,
        cancellation: &CancellationToken,
    ) -> Result<OpenedDocument, OpenError>;

    /// Open a document from an in-memory byte buffer with a display `label`.
    ///
    /// # Errors
    ///
    /// Returns a typed [`OpenError`] for empty input, an unsupported format, a
    /// backend failure, or cancellation.
    fn open_bytes(
        &self,
        label: &str,
        bytes: &[u8],
        cancellation: &CancellationToken,
    ) -> Result<OpenedDocument, OpenError>;
}

/// A canned response returned by [`MockDocumentOpener`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MockResponse {
    /// A successful open with the supplied metadata.
    Document {
        /// Page count to report (must be positive).
        page_count: usize,
        /// Optional metadata title.
        title: Option<String>,
        /// Encryption state to report.
        encryption: EncryptionState,
    },
    /// A failed open with a typed error.
    Error(OpenError),
}

impl MockResponse {
    /// Build a successful, unencrypted, untitled response with `page_count`.
    pub fn simple(page_count: usize) -> Self {
        Self::Document {
            page_count,
            title: None,
            encryption: EncryptionState::Unencrypted,
        }
    }
}

/// A deterministic [`DocumentOpener`] for tests and default (non-PDFium) builds.
///
/// Responses are selected by a normalized path key or a lowercased bytes
/// label; anything unmatched falls back to the configured default response.
#[derive(Clone, Debug)]
pub struct MockDocumentOpener {
    default: MockResponse,
    by_key: BTreeMap<String, MockResponse>,
}

impl Default for MockDocumentOpener {
    fn default() -> Self {
        Self::new(MockResponse::simple(1))
    }
}

impl MockDocumentOpener {
    /// Create an opener returning `default` for every unmatched source.
    pub fn new(default: MockResponse) -> Self {
        Self {
            default,
            by_key: BTreeMap::new(),
        }
    }

    /// Register a canned `response` for a path key or bytes label.
    ///
    /// Path keys are normalized by [`normalize_path_key`]; bytes labels are
    /// lowercased. The caller may pass either spelling directly.
    pub fn with(mut self, key: impl Into<String>, response: MockResponse) -> Self {
        self.by_key.insert(key.into(), response);
        self
    }

    fn respond(
        &self,
        key: &str,
        label: &str,
        path: Option<PathBuf>,
        bytes_len: usize,
        cancellation: &CancellationToken,
    ) -> Result<OpenedDocument, OpenError> {
        cancellation.check().map_err(|_| OpenError::Cancelled)?;
        if bytes_len == 0 {
            return Err(OpenError::Io("empty document bytes".to_owned()));
        }
        let response = self.by_key.get(key).unwrap_or(&self.default);
        match response {
            MockResponse::Error(error) => Err(error.clone()),
            MockResponse::Document {
                page_count,
                title,
                encryption,
            } => {
                if *page_count == 0 {
                    return Err(OpenError::EmptyDocument);
                }
                Ok(OpenedDocument {
                    label: label.to_owned(),
                    path,
                    page_count: *page_count,
                    title: title.clone(),
                    encryption: *encryption,
                    backend: OpenBackendKind::Mock,
                })
            }
        }
    }
}

impl DocumentOpener for MockDocumentOpener {
    fn open_path(
        &self,
        path: &Path,
        cancellation: &CancellationToken,
    ) -> Result<OpenedDocument, OpenError> {
        let key = normalize_path_key(path);
        if key.is_empty() {
            return Err(OpenError::Io("empty document path".to_owned()));
        }
        let label = display_name_from_path(path);
        self.respond(&key, &label, Some(path.to_path_buf()), 1, cancellation)
    }

    fn open_bytes(
        &self,
        label: &str,
        bytes: &[u8],
        cancellation: &CancellationToken,
    ) -> Result<OpenedDocument, OpenError> {
        self.respond(
            &label.to_lowercase(),
            label,
            None,
            bytes.len(),
            cancellation,
        )
    }
}

/// A native [`DocumentOpener`] backed by PDFium.
///
/// Only compiled with the `pdfium` feature. The renderer validates that the
/// source parses; metadata is then read under the renderer's process-wide lock.
#[cfg(feature = "pdfium")]
#[derive(Clone, Copy, Debug, Default)]
pub struct PdfiumDocumentOpener;

#[cfg(feature = "pdfium")]
fn map_render_error(error: pdfxi_render::RenderError) -> OpenError {
    match error {
        pdfxi_render::RenderError::Cancelled => OpenError::Cancelled,
        other => OpenError::Backend(other.to_string()),
    }
}

#[cfg(feature = "pdfium")]
impl DocumentOpener for PdfiumDocumentOpener {
    fn open_path(
        &self,
        path: &Path,
        cancellation: &CancellationToken,
    ) -> Result<OpenedDocument, OpenError> {
        cancellation.check().map_err(|_| OpenError::Cancelled)?;
        let renderer = pdfxi_render::PdfiumRenderer::from_file(path).map_err(map_render_error)?;
        cancellation.check().map_err(|_| OpenError::Cancelled)?;
        let meta = renderer.document_meta().map_err(map_render_error)?;
        Ok(OpenedDocument {
            label: display_name_from_path(path),
            path: Some(path.to_path_buf()),
            page_count: meta.page_count,
            title: meta.title,
            encryption: EncryptionState::from_revision(meta.security_revision),
            backend: OpenBackendKind::Pdfium,
        })
    }

    fn open_bytes(
        &self,
        label: &str,
        bytes: &[u8],
        cancellation: &CancellationToken,
    ) -> Result<OpenedDocument, OpenError> {
        cancellation.check().map_err(|_| OpenError::Cancelled)?;
        let renderer =
            pdfxi_render::PdfiumRenderer::from_bytes(bytes.to_vec()).map_err(map_render_error)?;
        cancellation.check().map_err(|_| OpenError::Cancelled)?;
        let meta = renderer.document_meta().map_err(map_render_error)?;
        Ok(OpenedDocument {
            label: label.to_owned(),
            path: None,
            page_count: meta.page_count,
            title: meta.title,
            encryption: EncryptionState::from_revision(meta.security_revision),
            backend: OpenBackendKind::Pdfium,
        })
    }
}

/// A bounded, most-recent-first list of successfully opened file paths.
#[derive(Clone, Debug)]
pub struct RecentFiles {
    entries: Vec<PathBuf>,
    capacity: usize,
}

impl RecentFiles {
    /// Create a list that keeps at most `capacity` distinct paths.
    pub fn new(capacity: usize) -> Self {
        Self {
            entries: Vec::with_capacity(capacity.min(64)),
            capacity,
        }
    }

    /// Maximum number of retained paths.
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Record `path`, promoting a previous entry to the front.
    ///
    /// Empty or root-only paths and a zero capacity are ignored. The list is
    /// truncated to [`Self::capacity`] afterwards, so memory stays bounded.
    pub fn record(&mut self, path: &Path) {
        if self.capacity == 0 {
            return;
        }
        let key = normalize_path_key(path);
        if key.is_empty() {
            return;
        }
        if let Some(index) = self
            .entries
            .iter()
            .position(|entry| normalize_path_key(entry) == key)
        {
            let existing = self.entries.remove(index);
            self.entries.insert(0, existing);
        } else {
            self.entries.insert(0, path.to_path_buf());
        }
        self.entries.truncate(self.capacity);
    }

    /// Retained paths, most recently recorded first.
    pub fn entries(&self) -> &[PathBuf] {
        &self.entries
    }

    /// Number of retained paths.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Return `true` when no paths are retained.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Return `true` when `path` is in the recent list.
    pub fn contains(&self, path: &Path) -> bool {
        let key = normalize_path_key(path);
        !key.is_empty()
            && self
                .entries
                .iter()
                .any(|entry| normalize_path_key(entry) == key)
    }

    /// Drop every retained path.
    pub fn clear(&mut self) {
        self.entries.clear();
    }
}

/// The most recent open outcome recorded for one tab.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TabOpenState {
    /// Number of open attempts recorded for this tab.
    pub attempts: usize,
    /// The most recent open result, if any.
    pub last_result: Option<OpenResult>,
}

/// Per-tab open outcomes, isolated by stable [`TabId`].
#[derive(Clone, Debug, Default)]
pub struct OpenTabStates {
    states: BTreeMap<TabId, TabOpenState>,
}

impl OpenTabStates {
    /// Record `result` against `id`, incrementing that tab's attempt counter.
    pub fn record(&mut self, id: TabId, result: OpenResult) {
        let state = self.states.entry(id).or_default();
        state.attempts += 1;
        state.last_result = Some(result);
    }

    /// Borrow the open state recorded for `id`.
    pub fn state(&self, id: TabId) -> Option<&TabOpenState> {
        self.states.get(&id)
    }

    /// Borrow the most recent result recorded for `id`.
    pub fn last_result(&self, id: TabId) -> Option<&OpenResult> {
        self.state(id).and_then(|state| state.last_result.as_ref())
    }

    /// Forget the recorded state for `id`.
    pub fn clear(&mut self, id: TabId) {
        self.states.remove(&id);
    }

    /// Number of tabs with recorded state.
    pub fn len(&self) -> usize {
        self.states.len()
    }

    /// Return `true` when no tab has recorded state.
    pub fn is_empty(&self) -> bool {
        self.states.is_empty()
    }
}

/// Drives a [`DocumentOpener`] and records its outcome, recent files, and
/// per-tab state without any GUI or blocking I/O on the caller's thread.
#[derive(Debug)]
pub struct DocumentOpenPipeline<B: DocumentOpener> {
    backend: B,
    recent: RecentFiles,
    tab_states: OpenTabStates,
    phases: OpenPhases,
}

impl<B: DocumentOpener> DocumentOpenPipeline<B> {
    /// Create a pipeline using `backend` and a recent list of `recent_capacity`.
    pub fn new(backend: B, recent_capacity: usize) -> Self {
        Self {
            backend,
            recent: RecentFiles::new(recent_capacity),
            tab_states: OpenTabStates::default(),
            phases: OpenPhases::new(),
        }
    }

    /// Borrow the backend.
    pub fn backend(&self) -> &B {
        &self.backend
    }

    /// Borrow the recent-file list.
    pub fn recent(&self) -> &RecentFiles {
        &self.recent
    }

    /// Mutably borrow the recent-file list.
    pub fn recent_mut(&mut self) -> &mut RecentFiles {
        &mut self.recent
    }

    /// Borrow all per-tab open states.
    pub fn tab_states(&self) -> &OpenTabStates {
        &self.tab_states
    }

    /// Mutably borrow all per-tab open states.
    pub fn tab_states_mut(&mut self) -> &mut OpenTabStates {
        &mut self.tab_states
    }

    /// Borrow the per-tab open lifecycle tracker.
    pub fn phases(&self) -> &OpenPhases {
        &self.phases
    }

    /// Mark `tab` as loading before dispatching a background open.
    ///
    /// This is the async-friendly entry point: a host calls it on the UI thread,
    /// runs I/O on a worker, then calls [`Self::complete_open`] with the result.
    pub fn begin_open(&mut self, tab: TabId) -> OpenPhase {
        self.phases.begin(tab)
    }

    /// Record a completed open for `tab` from a background job.
    ///
    /// A successful result with a path is added to the recent list; byte-backed
    /// documents have no path identity and are never added. Returns the phase
    /// the tab moved to.
    pub fn complete_open(&mut self, tab: TabId, result: OpenResult) -> OpenPhase {
        let path_for_recent = result
            .document()
            .and_then(|document| document.path.as_deref())
            .map(Path::to_path_buf);
        self.finish_open(tab, result, path_for_recent.as_deref())
    }

    fn finish_open(
        &mut self,
        tab: TabId,
        result: OpenResult,
        path_for_recent: Option<&Path>,
    ) -> OpenPhase {
        let phase = self.phases.finish(tab, result.clone());
        if result.is_success()
            && let Some(path) = path_for_recent
        {
            self.recent.record(path);
        }
        self.tab_states.record(tab, result);
        phase
    }

    /// Build a deterministic report of all recorded open activity.
    ///
    /// Entries are ordered by ascending [`TabId`] because both trackers use
    /// ordered maps; repeated calls on unchanged state return identical reports.
    pub fn report(&self) -> OpenReport {
        let mut entries = Vec::with_capacity(self.phases.len());
        let mut ready = 0usize;
        let mut failed = 0usize;
        let mut loading = 0usize;
        for (tab, state) in &self.phases.states {
            match state.phase {
                OpenPhase::Loading => loading += 1,
                OpenPhase::Ready => ready += 1,
                OpenPhase::Failed => failed += 1,
            }
            let (label, outcome) = match &state.last_result {
                Some(OpenResult::Success(document)) => (
                    document.label.clone(),
                    format!(
                        "ready: {} page(s), '{}'",
                        document.page_count,
                        document.effective_title()
                    ),
                ),
                Some(OpenResult::Failure(error)) => {
                    ("<failed>".to_owned(), format!("failed: {error}"))
                }
                None => ("<pending>".to_owned(), "loading".to_owned()),
            };
            entries.push(OpenReportEntry {
                tab: *tab,
                phase: state.phase,
                label,
                outcome,
            });
        }
        let summary = format!(
            "open report: {} tab(s), {} loading, {} ready, {} failed, {} recent file(s)",
            entries.len(),
            loading,
            ready,
            failed,
            self.recent.len()
        );
        OpenReport { entries, summary }
    }

    /// Open `path` for `tab`, recording the typed outcome against that tab.
    ///
    /// Cancellation is checked before delegating to the backend and again
    /// afterwards; a cancelled open never touches the recent list.
    pub fn open_path(
        &mut self,
        tab: TabId,
        path: &Path,
        cancellation: &CancellationToken,
    ) -> OpenResult {
        self.phases.begin(tab);
        let result = if cancellation.is_cancelled() {
            OpenResult::Failure(OpenError::Cancelled)
        } else {
            match self.backend.open_path(path, cancellation) {
                Ok(_document) if cancellation.is_cancelled() => {
                    OpenResult::Failure(OpenError::Cancelled)
                }
                Ok(document) if document.page_count == 0 => {
                    OpenResult::Failure(OpenError::EmptyDocument)
                }
                Ok(document) => OpenResult::Success(document),
                Err(error) => OpenResult::Failure(error),
            }
        };
        self.finish_open(tab, result.clone(), Some(path));
        result
    }

    /// Open an in-memory `label`/`bytes` source for `tab` and record the result.
    ///
    /// Byte sources have no path identity, so they are not added to recents.
    pub fn open_bytes(
        &mut self,
        tab: TabId,
        label: &str,
        bytes: &[u8],
        cancellation: &CancellationToken,
    ) -> OpenResult {
        self.phases.begin(tab);
        let result = if cancellation.is_cancelled() {
            OpenResult::Failure(OpenError::Cancelled)
        } else {
            match self.backend.open_bytes(label, bytes, cancellation) {
                Ok(_document) if cancellation.is_cancelled() => {
                    OpenResult::Failure(OpenError::Cancelled)
                }
                Ok(document) if document.page_count == 0 => {
                    OpenResult::Failure(OpenError::EmptyDocument)
                }
                Ok(document) => OpenResult::Success(document),
                Err(error) => OpenResult::Failure(error),
            }
        };
        self.finish_open(tab, result.clone(), None);
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn tab(n: u64) -> TabId {
        TabId::from_u64(n)
    }

    fn path(name: &str) -> PathBuf {
        PathBuf::from(name)
    }

    #[test]
    fn open_success_records_page_count_and_title() {
        let backend = MockDocumentOpener::new(MockResponse::Document {
            page_count: 7,
            title: Some("Annual Report".to_owned()),
            encryption: EncryptionState::Unencrypted,
        });
        let mut pipeline = DocumentOpenPipeline::new(backend, 5);
        let result = pipeline.open_path(
            tab(1),
            Path::new("C:/docs/report.pdf"),
            &CancellationToken::new(),
        );
        let document = result.document().expect("successful open");
        assert_eq!(document.page_count, 7);
        assert_eq!(document.title.as_deref(), Some("Annual Report"));
        assert_eq!(document.effective_title(), "Annual Report");
        assert_eq!(document.backend, OpenBackendKind::Mock);
        assert!(!document.is_encrypted());
    }

    #[test]
    fn open_error_is_typed_and_recorded() {
        let backend = MockDocumentOpener::new(MockResponse::Error(OpenError::Io("denied".into())));
        let mut pipeline = DocumentOpenPipeline::new(backend, 5);
        let result = pipeline.open_path(
            tab(2),
            Path::new("C:/docs/locked.pdf"),
            &CancellationToken::new(),
        );
        assert_eq!(result, OpenResult::Failure(OpenError::Io("denied".into())));
        assert_eq!(
            pipeline.tab_states().last_result(tab(2)),
            Some(&OpenResult::Failure(OpenError::Io("denied".into())))
        );
        assert!(pipeline.recent().is_empty(), "failed opens are not recent");
    }

    #[test]
    fn encrypted_document_state_is_recorded() {
        let backend = MockDocumentOpener::new(MockResponse::Document {
            page_count: 3,
            title: None,
            encryption: EncryptionState::Encrypted { revision: 4 },
        });
        let mut pipeline = DocumentOpenPipeline::new(backend, 5);
        let result = pipeline.open_bytes(tab(3), "secret", b"pdf-bytes", &CancellationToken::new());
        let document = result.document().expect("opened encrypted doc");
        assert!(document.is_encrypted());
        assert_eq!(document.encryption.revision(), Some(4));
        assert_eq!(document.effective_title(), "secret");
    }

    #[test]
    fn unsupported_renderer_error_is_typed() {
        let backend = MockDocumentOpener::new(MockResponse::Error(OpenError::UnsupportedRenderer));
        let mut pipeline = DocumentOpenPipeline::new(backend, 5);
        let result = pipeline.open_bytes(tab(4), "x", b"bytes", &CancellationToken::new());
        assert_eq!(result, OpenResult::Failure(OpenError::UnsupportedRenderer));
    }

    #[test]
    fn open_bytes_uses_lowercased_label_key() {
        let backend =
            MockDocumentOpener::new(MockResponse::simple(1)).with("memo", MockResponse::simple(9));
        let mut pipeline = DocumentOpenPipeline::new(backend, 5);
        let result = pipeline.open_bytes(tab(5), "MEMO", b"bytes", &CancellationToken::new());
        assert_eq!(result.document().map(|d| d.page_count), Some(9));
    }

    #[test]
    fn recent_files_are_bounded_and_most_recent_first() {
        let mut recent = RecentFiles::new(3);
        recent.record(Path::new("C:/a.pdf"));
        recent.record(Path::new("C:/b.pdf"));
        recent.record(Path::new("C:/c.pdf"));
        recent.record(Path::new("C:/d.pdf"));
        assert_eq!(recent.len(), 3);
        assert_eq!(recent.entries()[0], path("C:/d.pdf"));
        assert_eq!(recent.entries()[1], path("C:/c.pdf"));
        assert_eq!(recent.entries()[2], path("C:/b.pdf"));

        // Re-recording an existing path promotes it without growing the list.
        recent.record(Path::new("C:/b.pdf"));
        assert_eq!(recent.len(), 3);
        assert_eq!(recent.entries()[0], path("C:/b.pdf"));
        assert!(recent.contains(Path::new("C:/b.pdf")));
        recent.clear();
        assert!(recent.is_empty());
    }

    #[test]
    fn zero_capacity_recent_list_drops_everything() {
        let mut recent = RecentFiles::new(0);
        recent.record(Path::new("C:/a.pdf"));
        assert!(recent.is_empty());
        assert_eq!(recent.capacity(), 0);
    }

    #[test]
    fn per_tab_open_state_is_isolated() {
        let backend = MockDocumentOpener::new(MockResponse::simple(1))
            .with("c:/ok.pdf", MockResponse::simple(4))
            .with(
                "c:/bad.pdf",
                MockResponse::Error(OpenError::UnsupportedFormat),
            );
        let mut pipeline = DocumentOpenPipeline::new(backend, 5);
        assert!(
            pipeline
                .open_path(tab(10), Path::new("C:/ok.pdf"), &CancellationToken::new())
                .is_success()
        );
        assert_eq!(
            pipeline.open_path(tab(11), Path::new("C:/bad.pdf"), &CancellationToken::new()),
            OpenResult::Failure(OpenError::UnsupportedFormat)
        );

        let first = pipeline.tab_states().last_result(tab(10)).expect("tab 10");
        assert_eq!(first.document().map(|d| d.page_count), Some(4));
        let second = pipeline.tab_states().last_result(tab(11)).expect("tab 11");
        assert_eq!(second.error(), Some(&OpenError::UnsupportedFormat));
        assert_eq!(pipeline.tab_states().len(), 2);

        pipeline.tab_states_mut().clear(tab(10));
        assert!(pipeline.tab_states().last_result(tab(10)).is_none());
        assert!(pipeline.tab_states().last_result(tab(11)).is_some());
    }

    #[test]
    fn attempts_counter_tracks_reopen_on_same_tab() {
        let mut pipeline = DocumentOpenPipeline::new(MockDocumentOpener::default(), 5);
        let path = Path::new("C:/a.pdf");
        pipeline.open_path(tab(1), path, &CancellationToken::new());
        pipeline.open_path(tab(1), path, &CancellationToken::new());
        let state = pipeline.tab_states().state(tab(1)).expect("state");
        assert_eq!(state.attempts, 2);
    }

    #[test]
    fn cancellation_before_open_does_not_touch_backend_or_recent() {
        let mut pipeline = DocumentOpenPipeline::new(MockDocumentOpener::default(), 5);
        let token = CancellationToken::new();
        token.cancel();
        let result = pipeline.open_path(tab(1), Path::new("C:/a.pdf"), &token);
        assert_eq!(result, OpenResult::Failure(OpenError::Cancelled));
        assert!(pipeline.recent().is_empty());
        assert_eq!(
            pipeline.tab_states().last_result(tab(1)),
            Some(&OpenResult::Failure(OpenError::Cancelled))
        );
    }

    /// An opener that fires the shared token while the load is in progress.
    #[derive(Clone, Debug)]
    struct MidFlightCancel {
        token: CancellationToken,
    }

    impl DocumentOpener for MidFlightCancel {
        fn open_path(
            &self,
            _path: &Path,
            _cancellation: &CancellationToken,
        ) -> Result<OpenedDocument, OpenError> {
            self.token.cancel();
            Ok(OpenedDocument {
                label: "late".to_owned(),
                path: None,
                page_count: 2,
                title: None,
                encryption: EncryptionState::Unencrypted,
                backend: OpenBackendKind::Mock,
            })
        }

        fn open_bytes(
            &self,
            _label: &str,
            _bytes: &[u8],
            _cancellation: &CancellationToken,
        ) -> Result<OpenedDocument, OpenError> {
            self.token.cancel();
            Ok(OpenedDocument {
                label: "late".to_owned(),
                path: None,
                page_count: 2,
                title: None,
                encryption: EncryptionState::Unencrypted,
                backend: OpenBackendKind::Mock,
            })
        }
    }

    #[test]
    fn cancellation_after_backend_result_is_honoured() {
        let token = CancellationToken::new();
        let backend = MidFlightCancel {
            token: token.clone(),
        };
        let mut pipeline = DocumentOpenPipeline::new(backend, 5);
        let result = pipeline.open_path(tab(1), Path::new("C:/a.pdf"), &token);
        assert_eq!(result, OpenResult::Failure(OpenError::Cancelled));
        assert!(pipeline.recent().is_empty(), "cancelled open is not recent");
    }

    #[test]
    fn opening_bytes_never_touches_recent_files() {
        let mut pipeline = DocumentOpenPipeline::new(MockDocumentOpener::default(), 5);
        assert!(
            pipeline
                .open_bytes(tab(1), "buffer", b"bytes", &CancellationToken::new())
                .is_success()
        );
        assert!(pipeline.recent().is_empty());
    }

    // ---- T-0167: open lifecycle states and deterministic reports ----------

    #[test]
    fn open_phase_transitions_loading_ready_failed() {
        let mut phases = OpenPhases::new();
        assert!(phases.is_empty());
        assert_eq!(phases.begin(tab(1)), OpenPhase::Loading);
        assert_eq!(phases.phase(tab(1)), Some(OpenPhase::Loading));
        let state = phases.state(tab(1)).expect("started");
        assert_eq!(state.attempts, 1);
        assert!(state.last_result.is_none());

        assert_eq!(
            phases.finish(
                tab(1),
                OpenResult::Success(OpenedDocument {
                    label: "doc".into(),
                    path: None,
                    page_count: 2,
                    title: None,
                    encryption: EncryptionState::Unencrypted,
                    backend: OpenBackendKind::Mock,
                })
            ),
            OpenPhase::Ready
        );
        assert_eq!(phases.phase(tab(1)), Some(OpenPhase::Ready));

        // A second attempt returns to Loading, then lands in Failed.
        assert_eq!(phases.begin(tab(1)), OpenPhase::Loading);
        assert_eq!(
            phases.finish(tab(1), OpenResult::Failure(OpenError::Io("boom".into()))),
            OpenPhase::Failed
        );
        let state = phases.state(tab(1)).expect("started");
        assert_eq!(state.attempts, 2);
        assert_eq!(state.phase, OpenPhase::Failed);
        assert_eq!(
            state.last_result.as_ref().and_then(OpenResult::error),
            Some(&OpenError::Io("boom".into()))
        );
        assert_eq!(phases.len(), 1);
        assert_eq!(OpenPhase::Loading.label(), "loading");
        assert_eq!(OpenPhase::Ready.label(), "ready");
        assert_eq!(OpenPhase::Failed.label(), "failed");
    }

    #[test]
    fn failed_open_phase_and_typed_error_are_recorded() {
        let backend = MockDocumentOpener::new(MockResponse::Error(OpenError::Encrypted));
        let mut pipeline = DocumentOpenPipeline::new(backend, 5);
        let result = pipeline.open_path(
            tab(7),
            Path::new("C:/secret.pdf"),
            &CancellationToken::new(),
        );
        assert_eq!(result, OpenResult::Failure(OpenError::Encrypted));
        assert_eq!(pipeline.phases().phase(tab(7)), Some(OpenPhase::Failed));
        assert_eq!(pipeline.phases().state(tab(7)).map(|s| s.attempts), Some(1));
        assert!(pipeline.recent().is_empty());

        let report = pipeline.report();
        assert_eq!(report.entries.len(), 1);
        assert_eq!(report.entries[0].phase, OpenPhase::Failed);
        assert_eq!(report.entries[0].label, "<failed>");
        assert_eq!(
            report.entries[0].outcome,
            "failed: document is encrypted or password protected"
        );
        assert_eq!(
            report.summary,
            "open report: 1 tab(s), 0 loading, 0 ready, 1 failed, 0 recent file(s)"
        );
    }

    #[test]
    fn recent_files_ordering_dedup_and_report_are_deterministic() {
        let mut pipeline = DocumentOpenPipeline::new(MockDocumentOpener::default(), 3);
        for name in ["a.pdf", "b.pdf", "c.pdf", "d.pdf", "b.pdf"] {
            let path = PathBuf::from(format!("C:/{name}"));
            pipeline.open_path(tab(1), &path, &CancellationToken::new());
        }
        let entries: Vec<&PathBuf> = pipeline.recent().entries().iter().collect();
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0], &PathBuf::from("C:/b.pdf"), "re-open promotes");
        assert_eq!(entries[1], &PathBuf::from("C:/d.pdf"));
        assert_eq!(entries[2], &PathBuf::from("C:/c.pdf"));
        assert!(pipeline.recent().contains(Path::new("C:/b.pdf")));
        assert!(
            !pipeline.recent().contains(Path::new("C:/a.pdf")),
            "evicted"
        );
        assert_eq!(pipeline.phases().state(tab(1)).map(|s| s.attempts), Some(5));

        let first = pipeline.report();
        assert_eq!(first, pipeline.report(), "report is stable across calls");
        assert_eq!(
            first.summary,
            "open report: 1 tab(s), 0 loading, 1 ready, 0 failed, 3 recent file(s)"
        );
    }

    #[test]
    fn open_report_is_sorted_by_tab_and_asynchronous_handoff_is_typed() {
        let mut pipeline = DocumentOpenPipeline::new(MockDocumentOpener::default(), 5);
        // Start tab 9 first; the report must still order entries by tab id.
        pipeline.begin_open(tab(9));
        pipeline.begin_open(tab(2));
        assert_eq!(pipeline.phases().phase(tab(9)), Some(OpenPhase::Loading));
        let report = pipeline.report();
        assert_eq!(report.entries.len(), 2);
        assert_eq!(report.entries[0].tab, tab(2));
        assert_eq!(report.entries[0].phase, OpenPhase::Loading);
        assert_eq!(report.entries[0].outcome, "loading");
        assert_eq!(report.entries[1].tab, tab(9));
        assert_eq!(
            report.summary,
            "open report: 2 tab(s), 2 loading, 0 ready, 0 failed, 0 recent file(s)"
        );

        // Complete tab 9 from a simulated background job with a path.
        let document = OpenedDocument {
            label: "async.pdf".into(),
            path: Some(PathBuf::from("C:/async.pdf")),
            page_count: 4,
            title: Some("Async".into()),
            encryption: EncryptionState::Unencrypted,
            backend: OpenBackendKind::Mock,
        };
        assert_eq!(
            pipeline.complete_open(tab(9), OpenResult::Success(document)),
            OpenPhase::Ready
        );
        assert!(pipeline.recent().contains(Path::new("C:/async.pdf")));
        let report = pipeline.report();
        assert_eq!(report.entries[1].phase, OpenPhase::Ready);
        assert_eq!(report.entries[1].label, "async.pdf");
        assert_eq!(report.entries[1].outcome, "ready: 4 page(s), 'Async'");
    }

    #[cfg(feature = "pdfium")]
    #[test]
    fn pdfium_opener_reads_the_real_fixture() {
        let fixture = crate::raster::workspace_fixture_path("minimal.pdf");
        let mut pipeline = DocumentOpenPipeline::new(PdfiumDocumentOpener, 5);
        let result = pipeline.open_path(tab(1), &fixture, &CancellationToken::new());
        let document = result.document().expect("fixture opens with PDFium");
        assert_eq!(document.backend, OpenBackendKind::Pdfium);
        assert!(document.page_count >= 1);
        assert!(!document.is_encrypted());
        assert!(!pipeline.recent().is_empty());
    }
}
