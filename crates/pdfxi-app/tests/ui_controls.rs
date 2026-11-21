//! UI interaction harness and no-dead-controls gate (T-0212).
//!
//! Three independent checks live here:
//!
//! 1. **Interaction harness** — every covered [`AppCommand`] is dispatched
//!    through the single public entry point, [`PdfxiApp::dispatch`], and must
//!    change observable application state. A command whose only effect is a
//!    viewport message is proven through the commands an `egui::Context`
//!    records for the frame. The commands whose effect is intentionally
//!    invisible to a window-free process are listed in
//!    [`UNOBSERVABLE_ALLOWLIST`] with one reason each.
//! 2. **Source-derived coverage guard** — `src/shell/commands.rs` is parsed at
//!    test time and its variant set is compared with [`all_commands`] plus
//!    [`PENDING_COVERAGE`]. A new variant cannot be silently missed: it fails
//!    [`source_variants_are_all_covered`] until it has a proof or an explicit,
//!    reasoned triage entry.
//! 3. **Static no-dead-controls scanner** — a line-based scan of the shell
//!    sources fails the build when a clickable control is created without a
//!    following `dispatch` (or `AppCommand`) in its enclosing function. The
//!    scanner is exercised by synthetic positive and negative snippets so the
//!    gate itself is tested, then run against `src/shell/**` and `src/ui.rs`.
//!
//! The file is gated on `ui` + `pdfium`: the harness opens real fixture PDFs,
//! and the page/find commands can only be proven when the native renderer
//! supplies real page counts and text. Registering a new `AppCommand` without
//! adding it to [`PROVEN_LABELS`] or [`UNOBSERVABLE_ALLOWLIST`] fails
//! [`every_app_command_is_proven_or_allowlisted`]; adding it to the enum
//! without any entry fails [`source_variants_are_all_covered`].

#![cfg(all(feature = "ui", feature = "pdfium"))]

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use pdfxi_app::find::FindOptions;
use pdfxi_app::ui::{AppCommand, CaptionAction, PdfxiApp, RailItem, UiClosePolicy};
use pdfxi_app::{OpenError, TabId, ThemePreference, Toolset, ViewMode, WindowControlStyle};

/// One document with three text pages, used for navigation and find proofs.
const THREE_PAGE_FIXTURE: &str = "fixtures/multipage-3.pdf";
/// A second, distinct document used for tab proofs.
const TWO_PAGE_FIXTURE: &str = "fixtures/multipage-2.pdf";

/// Fixture path resolved against the workspace root, not the process CWD.
fn fixture(relative: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(relative)
}

/// Build an app with one real three-page document open.
///
/// Asserts the renderer actually produced three pages and text: if PDFium is
/// missing, every page/text proof would silently degrade to a no-op and the
/// gate would pass for the wrong reason. The message says how to fix it.
fn with_three_page_document() -> PdfxiApp {
    let path = fixture(THREE_PAGE_FIXTURE);
    let mut app = PdfxiApp::new();
    assert!(
        app.open_path_text(path.to_string_lossy().as_ref()),
        "fixture {} did not open",
        path.display()
    );
    let page_count = app
        .state()
        .active_tab()
        .map(|tab| tab.page_count())
        .unwrap_or(0);
    assert_eq!(
        page_count,
        3,
        "PDFium did not report 3 pages for {}; run scripts/fetch-pdfium.ps1 \
         (or set PDFIUM_PATH) before the ui,pdfium tests",
        path.display()
    );
    app
}

/// Build an app with two distinct documents open; the second is active.
fn with_two_documents() -> PdfxiApp {
    let mut app = PdfxiApp::new();
    for relative in [THREE_PAGE_FIXTURE, TWO_PAGE_FIXTURE] {
        let path = fixture(relative);
        assert!(
            app.open_path_text(path.to_string_lossy().as_ref()),
            "fixture {} did not open",
            path.display()
        );
    }
    assert_eq!(app.state().tab_count(), 2);
    app
}

/// The tab ids in strip order.
fn tab_ids(app: &PdfxiApp) -> Vec<TabId> {
    app.tab_strip_snapshot()
        .into_iter()
        .map(|(id, _, _)| id)
        .collect()
}

/// Everything a window-free caller can observe about the app.
///
/// Deliberately includes status and banner strings: several shell commands use
/// them as their only public feedback channel, and a status change is real
/// observable behavior (it is what the user sees in the shell).
#[derive(Debug, PartialEq, Eq)]
struct Snapshot {
    tabs: Vec<(u64, String, bool)>,
    page: Option<usize>,
    page_count: Option<usize>,
    zoom: Option<String>,
    rotation: Option<i16>,
    view_mode: Option<&'static str>,
    tool: &'static str,
    find_open: bool,
    find_query: String,
    find_options: FindOptions,
    find_current: Option<usize>,
    find_matches: usize,
    status: String,
    theme: ThemePreference,
    window_controls: WindowControlStyle,
    banner: Option<String>,
    errors: usize,
    close_policy: UiClosePolicy,
    selection: String,
    display_name: String,
    dirty: Option<bool>,
    thumbnail_page: usize,
    tools_open: bool,
    grid_view: bool,
    active_rail: RailItem,
    annotation_count: usize,
    annotation_digest: Vec<(String, String, Option<String>, Option<String>)>,
    selected_annotation: Option<(usize, String)>,
    note_tool_active: bool,
    note_draft_open: bool,
    note_draft_text: Option<String>,
    help_open: bool,
    can_undo: bool,
    can_redo: bool,
    comments_hint: Option<String>,
    comments_error: Option<String>,
    form_field_count: usize,
    form_field_values: Vec<(String, String)>,
    forms_panel_open: bool,
    forms_banner: Option<String>,
    forms_hint: Option<String>,
    active_form_field: Option<String>,
    form_draft: Option<String>,
    forms_editable: bool,
}

fn snapshot(app: &PdfxiApp) -> Snapshot {
    let state = app.state();
    let tab = state.active_tab();
    Snapshot {
        tabs: app
            .tab_strip_snapshot()
            .into_iter()
            .map(|(id, title, active)| (id.as_u64(), title, active))
            .collect(),
        page: tab.map(|tab| tab.current_page()),
        page_count: tab.map(|tab| tab.page_count()),
        zoom: tab.map(|tab| tab.zoom().to_string()),
        rotation: tab.map(|tab| tab.rotation().degrees()),
        view_mode: tab.map(|tab| tab.view_mode().label()),
        tool: state.active_tool().label(),
        find_open: app.is_find_open(),
        find_query: app.find_state().query().to_owned(),
        find_options: app.find_state().options(),
        find_current: app.find_state().current_index(),
        find_matches: app.find_state().match_count(),
        status: app.status().to_owned(),
        theme: app.theme_preference(),
        window_controls: app.window_controls(),
        banner: app.open_error_banner().map(str::to_owned),
        errors: app.open_errors().len(),
        close_policy: app.close_policy(),
        selection: app.selected_text(),
        display_name: app.display_name().to_owned(),
        dirty: tab.map(|tab| tab.is_dirty()),
        thumbnail_page: app.thumbnail_state().active_page(),
        tools_open: app.tools_open(),
        grid_view: app.grid_view(),
        active_rail: app.active_rail(),
        annotation_count: app.annotation_count(),
        annotation_digest: app
            .annotations()
            .iter()
            .map(|entry| {
                (
                    entry.view.id.clone(),
                    entry.view.contents.clone(),
                    entry.view.state.clone(),
                    entry.view.parent_id.clone(),
                )
            })
            .collect(),
        selected_annotation: app
            .selected_annotation()
            .map(|(page, view)| (page, view.id.clone())),
        note_tool_active: app.note_tool_active(),
        note_draft_open: app.note_draft_open(),
        note_draft_text: app.note_draft_text().map(str::to_owned),
        help_open: app.help_open(),
        can_undo: app.can_undo(),
        can_redo: app.can_redo(),
        comments_hint: app.comments_hint().map(str::to_owned),
        comments_error: app.comments_error().map(str::to_owned),
        form_field_count: app.form_field_count(),
        form_field_values: app.form_field_values(),
        forms_panel_open: app.forms_panel_open(),
        forms_banner: app.forms_banner().map(str::to_owned),
        forms_hint: app.forms_hint().map(str::to_owned),
        active_form_field: app.active_form_field().map(str::to_owned),
        form_draft: app.form_draft().map(str::to_owned),
        forms_editable: app.forms_editable(),
    }
}

/// Dispatch `command` and require a change in public observable state.
///
/// The panic names the command label, the before/after snapshots, and a
/// pointer at the fix, so a dead control is actionable without reading this
/// harness.
fn assert_dispatch_acts(label: &str, app: &mut PdfxiApp, command: AppCommand) {
    let before = snapshot(app);
    app.dispatch(command.clone(), None);
    let after = snapshot(app);
    assert_ne!(
        before, after,
        "AppCommand `{label}` (dispatched as {command:?}) changed no observable \
         state (tabs, page, zoom, rotation, view mode, tool, find, status, \
         theme, controls, banner, errors, close policy, selection, thumbnails). \
         Wire it to state, or add it to UNOBSERVABLE_ALLOWLIST with a reason."
    );
}

/// Dispatch into a headless egui frame and return the viewport commands the
/// command produced for the root viewport.
fn viewport_commands(app: &mut PdfxiApp, command: AppCommand) -> Vec<egui::ViewportCommand> {
    let ctx = egui::Context::default();
    let output = ctx.run(egui::RawInput::default(), |ctx| {
        app.dispatch(command.clone(), Some(ctx));
    });
    output
        .viewport_output
        .get(&egui::ViewportId::ROOT)
        .map(|viewport| viewport.commands.clone())
        .unwrap_or_default()
}

/// Every `AppCommand` variant the harness covers, once, with dummy arguments.
///
/// The first tuple element is the exact enum variant name from
/// `src/shell/commands.rs`. `source_variants_are_all_covered` parses that file
/// and compares the result against these names plus `PENDING_COVERAGE`, so a
/// new variant cannot be added without either a proof or an explicit,
/// reasoned triage entry.
fn all_commands() -> Vec<(&'static str, AppCommand)> {
    vec![
        ("OpenDialog", AppCommand::OpenDialog),
        (
            "OpenPath",
            AppCommand::OpenPath("fixtures/minimal.pdf".to_owned()),
        ),
        (
            "OpenDropped",
            AppCommand::OpenDropped(vec![PathBuf::from("fixtures/minimal.pdf")]),
        ),
        ("CloseActiveTab", AppCommand::CloseActiveTab),
        ("CloseTab", AppCommand::CloseTab(TabId::from_u64(1))),
        ("CloseOtherTabs", AppCommand::CloseOtherTabs),
        ("ActivateTab", AppCommand::ActivateTab(TabId::from_u64(1))),
        ("ActivateNextTab", AppCommand::ActivateNextTab),
        ("ActivatePreviousTab", AppCommand::ActivatePreviousTab),
        (
            "ReorderTab",
            AppCommand::ReorderTab {
                id: TabId::from_u64(1),
                new_index: 0,
            },
        ),
        ("NextPage", AppCommand::NextPage),
        ("PreviousPage", AppCommand::PreviousPage),
        ("GoToPage", AppCommand::GoToPage(1)),
        ("ZoomIn", AppCommand::ZoomIn),
        ("ZoomOut", AppCommand::ZoomOut),
        ("FitPage", AppCommand::FitPage),
        ("FitWidth", AppCommand::FitWidth),
        ("ActualSize", AppCommand::ActualSize),
        ("RotateClockwise", AppCommand::RotateClockwise),
        ("RotateCounterclockwise", AppCommand::RotateCounterclockwise),
        ("ResetRotation", AppCommand::ResetRotation),
        ("SetViewMode", AppCommand::SetViewMode(ViewMode::SinglePage)),
        ("SelectTool", AppCommand::SelectTool(Toolset::Hand)),
        ("ToggleFind", AppCommand::ToggleFind),
        ("OpenFind", AppCommand::OpenFind),
        ("CloseFind", AppCommand::CloseFind),
        ("FindNext", AppCommand::FindNext),
        ("FindPrevious", AppCommand::FindPrevious),
        ("SetFindQuery", AppCommand::SetFindQuery("Page".to_owned())),
        (
            "SetFindOptions",
            AppCommand::SetFindOptions {
                case_sensitive: true,
                whole_word: true,
            },
        ),
        ("CopySelection", AppCommand::CopySelection),
        ("ClearSelection", AppCommand::ClearSelection),
        (
            "SetThemePreference",
            AppCommand::SetThemePreference(ThemePreference::Dark),
        ),
        (
            "SetWindowControls",
            AppCommand::SetWindowControls(WindowControlStyle::MacOs),
        ),
        ("ToggleTheme", AppCommand::ToggleTheme),
        ("Caption", AppCommand::Caption(CaptionAction::Close)),
        ("StartWindowDrag", AppCommand::StartWindowDrag),
        ("DismissOpenError", AppCommand::DismissOpenError),
        ("ClearOpenErrors", AppCommand::ClearOpenErrors),
        ("SelectThumbnailPage", AppCommand::SelectThumbnailPage(1)),
        (
            "SetClosePolicy",
            AppCommand::SetClosePolicy(UiClosePolicy::DiscardDirty),
        ),
        // T-0220 acceptance-shell commands.
        ("EditDocument", AppCommand::EditDocument),
        ("Comment", AppCommand::Comment),
        ("Highlight", AppCommand::Highlight),
        ("FillAndSign", AppCommand::FillAndSign),
        ("Sign", AppCommand::Sign),
        ("PlaceSignature", AppCommand::PlaceSignature),
        ("RequestSignature", AppCommand::RequestSignature),
        ("DocumentProperties", AppCommand::DocumentProperties),
        ("Organize", AppCommand::Organize),
        ("Export", AppCommand::Export),
        ("ConvertToPdf", AppCommand::ConvertToPdf),
        ("CombineFiles", AppCommand::CombineFiles),
        ("Protect", AppCommand::Protect),
        ("AiAssistant", AppCommand::AiAssistant),
        ("ToggleTools", AppCommand::ToggleTools),
        ("ToggleGrid", AppCommand::ToggleGrid),
        ("ToggleFullscreen", AppCommand::ToggleFullscreen),
        ("SetZoomPercent", AppCommand::SetZoomPercent(150)),
        (
            "SubmitPageNumber",
            AppCommand::SubmitPageNumber("2".to_owned()),
        ),
        ("ShowRail", AppCommand::ShowRail(RailItem::Pages)),
        ("AddPage", AppCommand::AddPage),
        ("PageMenu", AppCommand::PageMenu),
        // T-0221/T-0222 surfaces proven through public accessors.
        ("OpenHelp", AppCommand::OpenHelp),
        ("CloseHelp", AppCommand::CloseHelp),
        ("ToggleNotifications", AppCommand::ToggleNotifications),
        ("Undo", AppCommand::Undo),
        ("Redo", AppCommand::Redo),
        (
            "SetDisplayName",
            AppCommand::SetDisplayName("QA Reviewer".to_owned()),
        ),
        (
            "SetDocumentTitle",
            AppCommand::SetDocumentTitle("QA Title".to_owned()),
        ),
        ("Save", AppCommand::Save),
        // T-0242 comments and markup commands.
        (
            "AddNote",
            AppCommand::AddNote {
                page: 0,
                x: 72.0,
                y: 720.0,
                contents: "QA note".to_owned(),
            },
        ),
        ("CancelNoteDraft", AppCommand::CancelNoteDraft),
        ("SaveNoteDraft", AppCommand::SaveNoteDraft),
        ("AddHighlight", AppCommand::AddHighlight),
        (
            "OpenAnnotation",
            AppCommand::OpenAnnotation {
                page: 0,
                id: "pdfxi-note-1".to_owned(),
            },
        ),
        ("CloseAnnotationPopup", AppCommand::CloseAnnotationPopup),
        (
            "ReplyToAnnotation",
            AppCommand::ReplyToAnnotation {
                page: 0,
                id: "pdfxi-note-1".to_owned(),
                contents: "QA reply".to_owned(),
            },
        ),
        (
            "SetAnnotationResolved",
            AppCommand::SetAnnotationResolved {
                page: 0,
                id: "pdfxi-note-1".to_owned(),
                resolved: true,
            },
        ),
        (
            "UpdateAnnotation",
            AppCommand::UpdateAnnotation {
                page: 0,
                id: "pdfxi-note-1".to_owned(),
                contents: "QA note updated".to_owned(),
            },
        ),
        (
            "DeleteAnnotation",
            AppCommand::DeleteAnnotation {
                page: 0,
                id: "pdfxi-note-1".to_owned(),
            },
        ),
        // T-0243 AcroForm commands.
        (
            "SetFormText",
            AppCommand::SetFormText {
                name: "text-field".to_owned(),
                value: "QA value".to_owned(),
            },
        ),
        (
            "ToggleFormCheckbox",
            AppCommand::ToggleFormCheckbox {
                name: "checkbox-field".to_owned(),
            },
        ),
        // T-0243 round B: choice, radio, apply/reset, flatten and form data.
        (
            "SetFormChoice",
            AppCommand::SetFormChoice {
                name: "choice-field".to_owned(),
                value: Some("three".to_owned()),
            },
        ),
        (
            "SetFormRadio",
            AppCommand::SetFormRadio {
                name: "radio-field".to_owned(),
                on_state: "B".to_owned(),
            },
        ),
        ("ApplyFormEdit", AppCommand::ApplyFormEdit),
        ("ResetFormFields", AppCommand::ResetFormFields),
        ("FlattenForm", AppCommand::FlattenForm),
        (
            "FocusFormWidget",
            AppCommand::FocusFormWidget {
                name: "choice-field".to_owned(),
                widget: 0,
                anchor: [120.0, 240.0],
            },
        ),
        ("ImportXfdfDialog", AppCommand::ImportXfdfDialog),
        ("ExportXfdfDialog", AppCommand::ExportXfdfDialog),
        ("ImportFdfDialog", AppCommand::ImportFdfDialog),
        ("ExportFdfDialog", AppCommand::ExportFdfDialog),
    ]
}

/// Commands whose dispatch effect is proven by the tests below.
const PROVEN_LABELS: &[&str] = &[
    "open-path",
    "open-dropped",
    "close-active-tab",
    "close-tab",
    "close-other-tabs",
    "activate-tab",
    "activate-next-tab",
    "activate-previous-tab",
    "reorder-tab",
    "next-page",
    "previous-page",
    "goto-page",
    "zoom-in",
    "zoom-out",
    "fit-page",
    "fit-width",
    "actual-size",
    "rotate-clockwise",
    "rotate-counterclockwise",
    "reset-rotation",
    "set-view-mode",
    "select-tool",
    "toggle-find",
    "open-find",
    "close-find",
    "find-next",
    "find-previous",
    "set-find-query",
    "set-find-options",
    "clear-selection",
    "set-theme-preference",
    "set-window-controls",
    "toggle-theme",
    "caption",
    "start-window-drag",
    "dismiss-open-error",
    "clear-open-errors",
    "select-thumbnail-page",
    "set-close-policy",
    "edit-document",
    "comment",
    "highlight",
    "fill-and-sign",
    "sign",
    "place-signature",
    "request-signature",
    "document-properties",
    "organize",
    "export",
    "convert-to-pdf",
    "combine-files",
    "protect",
    "ai-assistant",
    "toggle-fullscreen",
    "set-zoom-percent",
    "submit-page-number",
    "add-page",
    "page-menu",
    "toggle-tools",
    "toggle-grid",
    "show-rail",
    "add-note",
    "cancel-note-draft",
    "save-note-draft",
    "add-highlight",
    "open-annotation",
    "close-annotation-popup",
    "reply-to-annotation",
    "set-annotation-resolved",
    "update-annotation",
    "delete-annotation",
    "open-help",
    "close-help",
    "toggle-notifications",
    "undo",
    "redo",
    "set-display-name",
    "set-document-title",
    "save",
    "set-form-text",
    "toggle-form-checkbox",
    "set-form-choice",
    "set-form-radio",
    "apply-form-edit",
    "reset-form-fields",
    "flatten-form",
    "focus-form-widget",
    "import-xfdf-dialog",
    "export-xfdf-dialog",
    "import-fdf-dialog",
    "export-fdf-dialog",
];

/// Enum variants that exist but are deliberately not covered by this harness
/// yet.
///
/// This is triage, not an exemption: `source_variants_are_all_covered` fails
/// for any variant that is neither in `all_commands()` nor here, so a new
/// variant still breaks the build until someone records why it has no proof.
/// Each entry names the owning task and the concrete missing piece.
const PENDING_COVERAGE: &[(&str, &str)] = &[
    (
        "AssistantConnect",
        "T-0253 assistant panel; starts a discovery/loopback probe, needs an isolated service",
    ),
    (
        "AssistantAsk",
        "T-0253 assistant panel; needs a connected loopback turn; assistant_chat lib tests cover it",
    ),
    (
        "AssistantStop",
        "T-0253 assistant panel; no-op when idle; assistant_chat lib tests cover the in-flight cancel",
    ),
    (
        "AssistantDisconnect",
        "T-0253 assistant panel; connection state only; assistant_chat/connect lib tests cover it",
    ),
    (
        "AssistantPair",
        "T-0253 assistant panel; explicit endpoint probe; assistant_connect lib tests cover it",
    ),
    (
        "AssistantSelectModel",
        "T-0253 assistant panel; needs a connected model list; assistant_chat lib tests cover it",
    ),
    (
        "AssistantCitation",
        "T-0253 assistant panel; citation chip navigation needs an open document and an answer; assistant_chat lib tests cover it",
    ),
    (
        "AssistantSummarize",
        "T-0253 assistant panel; preset sends a fixed prompt with the grounded excerpt; assistant_chat lib tests cover it",
    ),
    (
        "AssistantKeyInsights",
        "T-0253 assistant panel; preset sends a fixed prompt with the grounded excerpt; assistant_chat lib tests cover it",
    ),
    (
        "ToggleStar",
        "T-0221 library state; mutates persisted library state, needs an isolated library root",
    ),
    (
        "RemoveRecent",
        "T-0221 recent list; mutates persisted library state, needs an isolated library root",
    ),
    (
        "OpenStorageFolder",
        "T-0221 host side effect: opens the OS file manager; must not run in a fast headless test",
    ),
    (
        "RevealInExplorer",
        "T-0221 host side effect: opens the OS file manager; must not run in a fast headless test",
    ),
    (
        "OpenDocsFolder",
        "T-0221 host side effect: opens the OS file manager; must not run in a fast headless test",
    ),
    (
        "RefreshLibrary",
        "T-0221 storage refresh; filesystem-dependent and needs an isolated library root",
    ),
    (
        "SaveAs",
        "T-0222 native save dialog; blocking/modal, cannot run headlessly",
    ),
    (
        "SubmitPassword",
        "T-0222 password modal; requires an encrypted fixture and an open modal",
    ),
    (
        "CancelPasswordPrompt",
        "T-0222 password modal; requires the modal to be open first",
    ),
    (
        "ResolveClose",
        "T-0222 unsaved-close prompt; requires a dirty tab and modal state",
    ),
    (
        "OverwriteDocument",
        "T-0222 save-conflict dialog; requires a real on-disk conflict fixture",
    ),
    (
        "DismissSaveConflict",
        "T-0222 save-conflict dialog; requires the dialog to be open first",
    ),
    (
        "Print",
        "T-0222 pre-print panel; needs a print-panel accessor in Snapshot",
    ),
    (
        "ConfirmPrint",
        "T-0222 starts the native print dialog; blocking/modal, cannot run headlessly",
    ),
    (
        "CancelPrint",
        "T-0222 cancellation; requires a running print job",
    ),
];

/// Commands that cannot change observable state in a window-free process.
///
/// Each entry needs a concrete reason; "it is just a view command" is not
/// accepted here, because view commands are proven through egui's viewport
/// command queue instead. The reviewed list is exactly two entries
/// (`open-dialog`, `copy-selection`); adding a third is a deliberate decision,
/// enforced by `allowlisted_commands_have_a_reason`.
const UNOBSERVABLE_ALLOWLIST: &[(&str, &str)] = &[
    (
        "open-dialog",
        "sets the private `open_dialog_open` request flag, which the next frame \
         consumes by opening the native rfd file picker; there is no public \
         getter, and painting the frame headlessly would block on a modal OS \
         dialog, so the only honest headless proof is that dispatch is wired",
    ),
    (
        "copy-selection",
        "copies `selected_text()` to the clipboard and reports it in the status \
         line only when a selection exists; the public shell has no selection \
         setter (selection is created by a canvas drag), so a headless dispatch \
         is the documented no-op for an empty selection",
    ),
];

#[test]
fn command_labels_are_stable_unique_and_non_empty() {
    let expected: &[(&str, &str)] = &[
        ("open-dialog", "open-dialog"),
        ("open-path", "open-path"),
        ("open-dropped", "open-dropped"),
        ("close-active-tab", "close-active-tab"),
        ("close-tab", "close-tab"),
        ("close-other-tabs", "close-other-tabs"),
        ("activate-tab", "activate-tab"),
        ("activate-next-tab", "activate-next-tab"),
        ("activate-previous-tab", "activate-previous-tab"),
        ("reorder-tab", "reorder-tab"),
        ("next-page", "next-page"),
        ("previous-page", "previous-page"),
        ("goto-page", "goto-page"),
        ("zoom-in", "zoom-in"),
        ("zoom-out", "zoom-out"),
        ("fit-page", "fit-page"),
        ("fit-width", "fit-width"),
        ("actual-size", "actual-size"),
        ("rotate-clockwise", "rotate-clockwise"),
        ("rotate-counterclockwise", "rotate-counterclockwise"),
        ("reset-rotation", "reset-rotation"),
        ("set-view-mode", "set-view-mode"),
        ("select-tool", "select-tool"),
        ("toggle-find", "toggle-find"),
        ("open-find", "open-find"),
        ("close-find", "close-find"),
        ("find-next", "find-next"),
        ("find-previous", "find-previous"),
        ("set-find-query", "set-find-query"),
        ("set-find-options", "set-find-options"),
        ("copy-selection", "copy-selection"),
        ("clear-selection", "clear-selection"),
        ("set-theme-preference", "set-theme-preference"),
        ("set-window-controls", "set-window-controls"),
        ("toggle-theme", "toggle-theme"),
        ("caption", "caption"),
        ("start-window-drag", "start-window-drag"),
        ("dismiss-open-error", "dismiss-open-error"),
        ("clear-open-errors", "clear-open-errors"),
        ("select-thumbnail-page", "select-thumbnail-page"),
        ("set-close-policy", "set-close-policy"),
        ("edit-document", "edit-document"),
        ("comment", "comment"),
        ("highlight", "highlight"),
        ("fill-and-sign", "fill-and-sign"),
        ("sign", "sign"),
        ("place-signature", "place-signature"),
        ("request-signature", "request-signature"),
        ("document-properties", "document-properties"),
        ("organize", "organize"),
        ("export", "export"),
        ("convert-to-pdf", "convert-to-pdf"),
        ("combine-files", "combine-files"),
        ("protect", "protect"),
        ("ai-assistant", "ai-assistant"),
        ("toggle-tools", "toggle-tools"),
        ("toggle-grid", "toggle-grid"),
        ("toggle-fullscreen", "toggle-fullscreen"),
        ("set-zoom-percent", "set-zoom-percent"),
        ("submit-page-number", "submit-page-number"),
        ("show-rail", "show-rail"),
        ("add-page", "add-page"),
        ("page-menu", "page-menu"),
        ("open-help", "open-help"),
        ("close-help", "close-help"),
        ("toggle-notifications", "toggle-notifications"),
        ("undo", "undo"),
        ("redo", "redo"),
        ("set-display-name", "set-display-name"),
        ("set-document-title", "set-document-title"),
        ("save", "save"),
        ("add-note", "add-note"),
        ("cancel-note-draft", "cancel-note-draft"),
        ("save-note-draft", "save-note-draft"),
        ("add-highlight", "add-highlight"),
        ("open-annotation", "open-annotation"),
        ("close-annotation-popup", "close-annotation-popup"),
        ("reply-to-annotation", "reply-to-annotation"),
        ("set-annotation-resolved", "set-annotation-resolved"),
        ("update-annotation", "update-annotation"),
        ("delete-annotation", "delete-annotation"),
        ("set-form-text", "set-form-text"),
        ("toggle-form-checkbox", "toggle-form-checkbox"),
        ("set-form-choice", "set-form-choice"),
        ("set-form-radio", "set-form-radio"),
        ("apply-form-edit", "apply-form-edit"),
        ("reset-form-fields", "reset-form-fields"),
        ("flatten-form", "flatten-form"),
        ("focus-form-widget", "focus-form-widget"),
        ("import-xfdf-dialog", "import-xfdf-dialog"),
        ("export-xfdf-dialog", "export-xfdf-dialog"),
        ("import-fdf-dialog", "import-fdf-dialog"),
        ("export-fdf-dialog", "export-fdf-dialog"),
    ];

    let commands = all_commands();
    assert_eq!(
        commands.len(),
        expected.len(),
        "all_commands() and the label snapshot must list the same variants"
    );
    let mut seen = BTreeSet::new();
    for ((variant, command), (label, _)) in commands.iter().zip(expected) {
        assert_eq!(
            command.label(),
            *label,
            "label changed for variant `{variant}` ({command:?})"
        );
        assert!(!label.is_empty(), "empty label for `{variant}`");
        assert!(
            seen.insert(*label),
            "duplicate label `{label}`; labels are the stable test surface"
        );
    }
}

#[test]
fn every_app_command_is_proven_or_allowlisted() {
    let mut covered: Vec<&str> = PROVEN_LABELS.to_vec();
    covered.extend(UNOBSERVABLE_ALLOWLIST.iter().map(|(label, _)| *label));
    covered.sort_unstable();

    let mut labels: Vec<&str> = all_commands()
        .iter()
        .map(|(_, command)| command.label())
        .collect();
    labels.sort_unstable();

    assert_eq!(
        labels, covered,
        "every covered AppCommand must be proven by a dispatch test or listed \
         in UNOBSERVABLE_ALLOWLIST; the two lists differ"
    );
}

#[test]
fn source_variants_are_all_covered() {
    let source = commands_source();
    let discovered: BTreeSet<String> = source_variants(&source).into_iter().collect();
    assert!(
        discovered.len() > 60,
        "the enum parser found only {} variants; parsing broke",
        discovered.len()
    );

    let mut covered: BTreeSet<String> = all_commands()
        .iter()
        .map(|(variant, _)| (*variant).to_owned())
        .collect();
    covered.extend(
        PENDING_COVERAGE
            .iter()
            .map(|(variant, _)| (*variant).to_owned()),
    );

    for (variant, reason) in PENDING_COVERAGE {
        assert!(
            all_commands()
                .iter()
                .all(|(covered_variant, _)| covered_variant != variant),
            "`{variant}` is both covered and pending; remove one entry"
        );
        assert!(
            !reason.trim().is_empty(),
            "pending variant `{variant}` needs a concrete reason"
        );
    }

    if let Err(message) = coverage_report(&discovered, &covered) {
        panic!("{message}");
    }
}

#[test]
fn source_guard_reports_a_missing_variant() {
    let discovered: BTreeSet<String> = ["AddNote", "DeleteAnnotation"]
        .into_iter()
        .map(str::to_owned)
        .collect();
    let covered: BTreeSet<String> = ["DeleteAnnotation"]
        .into_iter()
        .map(str::to_owned)
        .collect();
    let message = coverage_report(&discovered, &covered).expect_err("a missing variant must fail");
    assert!(
        message.contains("AddNote"),
        "failure must name the missing variant: {message}"
    );
    assert!(
        message.contains("missing from"),
        "failure must explain the direction: {message}"
    );
}

/// Read the shell command enum at test time, like the dead-control scanner
/// reads the shell sources. No build-time copy can go stale.
fn commands_source() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/shell/commands.rs");
    std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()))
}

/// Variant names in `pub enum AppCommand { … }`, in declaration order.
///
/// Line-based: a variant is an identifier at exactly four spaces of
/// indentation whose next non-space character is `,`, `(`, or `{`. Doc
/// comments, attributes, and struct-variant fields (eight spaces) are ignored.
fn source_variants(source: &str) -> Vec<String> {
    let mut variants = Vec::new();
    let mut in_enum = false;
    let mut depth = 0i32;
    for line in source.lines() {
        if !in_enum {
            if line.trim_end() == "pub enum AppCommand {" {
                in_enum = true;
                depth = brace_delta(line);
            }
            continue;
        }
        if depth > 0
            && let Some(name) = variant_name(line)
        {
            variants.push(name);
        }
        depth += brace_delta(line);
        if depth <= 0 {
            break;
        }
    }
    variants
}

/// The variant name on one enum-body line, if the line declares one.
fn variant_name(line: &str) -> Option<String> {
    let body = line.strip_prefix("    ")?;
    if body.starts_with(' ') {
        return None;
    }
    let name_len = body
        .find(|character: char| !(character.is_ascii_alphanumeric() || character == '_'))
        .unwrap_or(body.len());
    if name_len == 0 {
        return None;
    }
    let name = &body[..name_len];
    if !name
        .chars()
        .next()
        .is_some_and(|character| character.is_ascii_uppercase())
    {
        return None;
    }
    let tail = body[name_len..].trim_start();
    if matches!(tail.chars().next(), Some(',' | '(' | '{')) {
        Some(name.to_owned())
    } else {
        None
    }
}

/// Compare the parsed enum against the harness's variant set.
fn coverage_report(
    discovered: &BTreeSet<String>,
    covered: &BTreeSet<String>,
) -> Result<(), String> {
    let missing: Vec<&str> = discovered.difference(covered).map(String::as_str).collect();
    let extra: Vec<&str> = covered.difference(discovered).map(String::as_str).collect();
    if missing.is_empty() && extra.is_empty() {
        return Ok(());
    }
    Err(format!(
        "AppCommand coverage drift (T-0212 source guard): source variants missing from \
         all_commands()/PENDING_COVERAGE: [{}]; harness names with no source variant: [{}]. \
         Add each missing variant to all_commands() with a real proof, or record it in \
         PENDING_COVERAGE with a reason.",
        missing.join(", "),
        extra.join(", ")
    ))
}

#[test]
fn dispatched_navigation_and_view_commands_act() {
    let mut app = with_three_page_document();
    assert_eq!(
        app.state().active_tab().map(|tab| tab.current_page()),
        Some(0)
    );

    assert_dispatch_acts("next-page", &mut app, AppCommand::NextPage);
    assert_eq!(
        app.state().active_tab().map(|tab| tab.current_page()),
        Some(1)
    );

    assert_dispatch_acts("goto-page", &mut app, AppCommand::GoToPage(2));
    assert_eq!(
        app.state().active_tab().map(|tab| tab.current_page()),
        Some(2)
    );

    assert_dispatch_acts("previous-page", &mut app, AppCommand::PreviousPage);
    assert_eq!(
        app.state().active_tab().map(|tab| tab.current_page()),
        Some(1)
    );

    assert_dispatch_acts(
        "select-thumbnail-page",
        &mut app,
        AppCommand::SelectThumbnailPage(0),
    );
    assert_eq!(
        app.state().active_tab().map(|tab| tab.current_page()),
        Some(0)
    );

    assert_dispatch_acts("zoom-in", &mut app, AppCommand::ZoomIn);
    assert_dispatch_acts("zoom-out", &mut app, AppCommand::ZoomOut);

    assert_dispatch_acts("rotate-clockwise", &mut app, AppCommand::RotateClockwise);
    assert_eq!(
        app.state().active_tab().map(|tab| tab.rotation().degrees()),
        Some(90)
    );
    assert_dispatch_acts(
        "rotate-counterclockwise",
        &mut app,
        AppCommand::RotateCounterclockwise,
    );
    assert_eq!(
        app.state().active_tab().map(|tab| tab.rotation().degrees()),
        Some(0)
    );

    app.dispatch(AppCommand::RotateClockwise, None);
    assert_dispatch_acts("reset-rotation", &mut app, AppCommand::ResetRotation);
    assert_eq!(
        app.state().active_tab().map(|tab| tab.rotation().degrees()),
        Some(0)
    );

    // Fit commands only change state when the zoom was already custom.
    app.dispatch(AppCommand::ZoomIn, None);
    assert_dispatch_acts("fit-page", &mut app, AppCommand::FitPage);
    app.dispatch(AppCommand::ZoomIn, None);
    assert_dispatch_acts("fit-width", &mut app, AppCommand::FitWidth);
    app.dispatch(AppCommand::ZoomIn, None);
    assert_dispatch_acts("actual-size", &mut app, AppCommand::ActualSize);

    assert_dispatch_acts(
        "set-view-mode",
        &mut app,
        AppCommand::SetViewMode(ViewMode::SinglePage),
    );
    assert_eq!(
        app.state().active_tab().map(|tab| tab.view_mode().label()),
        Some("Single Page")
    );

    assert_dispatch_acts(
        "select-tool",
        &mut app,
        AppCommand::SelectTool(Toolset::Hand),
    );
    assert_eq!(app.state().active_tool().label(), "Hand");
}

#[test]
fn dispatched_tab_commands_act() {
    // Opening a fresh document is itself a user-facing control.
    {
        let mut app = PdfxiApp::new();
        assert_dispatch_acts(
            "open-path",
            &mut app,
            AppCommand::OpenPath(fixture(THREE_PAGE_FIXTURE).to_string_lossy().into_owned()),
        );
        assert_eq!(app.state().tab_count(), 1);
    }
    {
        let mut app = PdfxiApp::new();
        assert_dispatch_acts(
            "open-dropped",
            &mut app,
            AppCommand::OpenDropped(vec![fixture(TWO_PAGE_FIXTURE)]),
        );
        assert_eq!(app.state().tab_count(), 1);
    }
    {
        let mut app = with_two_documents();
        let ids = tab_ids(&app);
        assert_dispatch_acts("activate-tab", &mut app, AppCommand::ActivateTab(ids[0]));
        assert_eq!(app.state().active_tab_id(), Some(ids[0]));
    }
    {
        let mut app = with_two_documents();
        let ids = tab_ids(&app);
        assert_dispatch_acts(
            "reorder-tab",
            &mut app,
            AppCommand::ReorderTab {
                id: ids[1],
                new_index: 0,
            },
        );
        assert_eq!(tab_ids(&app), vec![ids[1], ids[0]]);
    }
    {
        let mut app = with_two_documents();
        let ids = tab_ids(&app);
        // A third tab makes "next" wrap instead of clamping on the last tab.
        let third = fixture("fixtures/multipage-4.pdf");
        assert!(app.open_path_text(third.to_string_lossy().as_ref()));
        assert_eq!(app.state().tab_count(), 3);
        assert_dispatch_acts("activate-next-tab", &mut app, AppCommand::ActivateNextTab);
        assert_eq!(app.state().active_tab_id(), Some(ids[0]));
    }
    {
        let mut app = with_two_documents();
        let ids = tab_ids(&app);
        assert_dispatch_acts(
            "activate-previous-tab",
            &mut app,
            AppCommand::ActivatePreviousTab,
        );
        assert_eq!(app.state().active_tab_id(), Some(ids[0]));
    }
    {
        let mut app = with_two_documents();
        assert_dispatch_acts("close-other-tabs", &mut app, AppCommand::CloseOtherTabs);
        assert_eq!(app.state().tab_count(), 1);
    }
    {
        let mut app = with_two_documents();
        let ids = tab_ids(&app);
        assert_dispatch_acts("close-tab", &mut app, AppCommand::CloseTab(ids[1]));
        assert_eq!(app.state().tab_count(), 1);
    }
    {
        let mut app = with_two_documents();
        assert_dispatch_acts("close-active-tab", &mut app, AppCommand::CloseActiveTab);
        assert_eq!(app.state().tab_count(), 1);
    }
}

#[test]
fn dispatched_find_commands_act() {
    let mut app = with_three_page_document();

    assert_dispatch_acts("open-find", &mut app, AppCommand::OpenFind);
    assert!(app.is_find_open());
    assert_dispatch_acts("close-find", &mut app, AppCommand::CloseFind);
    assert!(!app.is_find_open());
    assert_dispatch_acts("toggle-find", &mut app, AppCommand::ToggleFind);
    assert!(app.is_find_open());

    assert_dispatch_acts(
        "set-find-query",
        &mut app,
        AppCommand::SetFindQuery("Page".to_owned()),
    );
    assert_eq!(app.find_state().query(), "Page");
    assert!(
        app.find_state().match_count() > 0,
        "expected real text matches for `Page`; PDFium text extraction is not \
         working, so find navigation cannot be proven"
    );
    assert_eq!(app.find_state().current_index(), Some(0));

    assert_dispatch_acts("find-next", &mut app, AppCommand::FindNext);
    assert_eq!(app.find_state().current_index(), Some(1));
    assert_dispatch_acts("find-previous", &mut app, AppCommand::FindPrevious);
    assert_eq!(app.find_state().current_index(), Some(0));

    assert_dispatch_acts(
        "set-find-options",
        &mut app,
        AppCommand::SetFindOptions {
            case_sensitive: true,
            whole_word: true,
        },
    );
    assert_eq!(
        app.find_state().options(),
        FindOptions::new(true).with_whole_word(true)
    );
}

/// Copy the Excel fixture to a per-process temp path under the test temp dir.
///
/// Tests never write to `fixtures/**`; the caller owns the returned file and
/// must remove it.
fn temp_fixture_copy(label: &str) -> PathBuf {
    let source = fixture("fixtures/realworld/microsoft-excel-2019-sample.pdf");
    let directory = std::env::temp_dir();
    std::fs::create_dir_all(&directory).expect("create the test temp directory");
    let path = directory.join(format!("pdfxi-t0212-{label}-{}.pdf", std::process::id()));
    std::fs::copy(&source, &path).expect("copy the fixture to the temp path");
    path
}

/// Open a temp fixture copy and assert it produced a real page.
fn open_temp_copy(path: &Path) -> PdfxiApp {
    let mut app = PdfxiApp::new();
    assert!(
        app.open_path_text(path.to_string_lossy().as_ref()),
        "temp copy {} did not open",
        path.display()
    );
    assert!(
        app.state()
            .active_tab()
            .is_some_and(|tab| tab.page_count() > 0),
        "temp copy must have at least one page"
    );
    app
}

/// The display value of one form field, if present.
fn form_value(values: &[(String, String)], name: &str) -> Option<String> {
    values
        .iter()
        .find(|(field, _)| field == name)
        .map(|(_, value)| value.clone())
}

/// Open the real-world Excel fixture used for annotation proofs.
///
/// It has no pre-existing `/Annots`, so annotation counts start at zero and a
/// new note is unambiguous.
fn with_excel_document() -> PdfxiApp {
    let path = fixture("fixtures/realworld/microsoft-excel-2019-sample.pdf");
    let mut app = PdfxiApp::new();
    assert!(
        app.open_path_text(path.to_string_lossy().as_ref()),
        "annotation fixture {} did not open",
        path.display()
    );
    assert!(
        app.state()
            .active_tab()
            .is_some_and(|tab| tab.page_count() > 0),
        "annotation fixture must have at least one page"
    );
    assert_eq!(
        app.annotation_count(),
        0,
        "the annotation fixture must start without annotations"
    );
    app
}

#[test]
fn dispatched_annotation_commands_act() {
    let mut app = with_excel_document();

    assert_dispatch_acts(
        "add-note",
        &mut app,
        AppCommand::AddNote {
            page: 0,
            x: 72.0,
            y: 720.0,
            contents: "QA note".to_owned(),
        },
    );
    assert_eq!(app.annotation_count(), 1);
    let note = app
        .annotations()
        .first()
        .expect("the note must be listed")
        .view
        .id
        .clone();

    // `Comment` arms the note tool; `CancelNoteDraft` disarms it. The draft
    // commit path is proven in `save_note_draft_commits_and_cancels_the_public_draft`.
    app.dispatch(AppCommand::Comment, None);
    assert!(app.note_tool_active());
    assert_dispatch_acts("cancel-note-draft", &mut app, AppCommand::CancelNoteDraft);
    assert!(!app.note_tool_active());

    // No public selection setter: prove the documented no-selection guard.
    assert_dispatch_acts("add-highlight", &mut app, AppCommand::AddHighlight);
    assert!(app.comments_hint().is_some());

    assert_dispatch_acts(
        "open-annotation",
        &mut app,
        AppCommand::OpenAnnotation {
            page: 0,
            id: note.clone(),
        },
    );
    assert_eq!(
        app.selected_annotation()
            .map(|(page, view)| (page, view.id.clone())),
        Some((0, note.clone()))
    );

    assert_dispatch_acts(
        "close-annotation-popup",
        &mut app,
        AppCommand::CloseAnnotationPopup,
    );
    assert!(app.selected_annotation().is_none());

    assert_dispatch_acts(
        "reply-to-annotation",
        &mut app,
        AppCommand::ReplyToAnnotation {
            page: 0,
            id: note.clone(),
            contents: "QA reply".to_owned(),
        },
    );
    assert_eq!(app.annotation_count(), 2);
    let reply = app
        .annotations()
        .iter()
        .find(|entry| entry.view.parent_id.as_deref() == Some(note.as_str()))
        .map(|entry| entry.view.id.clone())
        .expect("the reply must be listed");

    assert_dispatch_acts(
        "set-annotation-resolved",
        &mut app,
        AppCommand::SetAnnotationResolved {
            page: 0,
            id: note.clone(),
            resolved: true,
        },
    );
    assert_eq!(
        app.annotations()
            .iter()
            .find(|entry| entry.view.id == note)
            .and_then(|entry| entry.view.state.as_deref()),
        Some("Accepted")
    );

    assert_dispatch_acts(
        "update-annotation",
        &mut app,
        AppCommand::UpdateAnnotation {
            page: 0,
            id: note.clone(),
            contents: "QA note updated".to_owned(),
        },
    );
    assert_eq!(
        app.annotations()
            .iter()
            .find(|entry| entry.view.id == note)
            .map(|entry| entry.view.contents.as_str()),
        Some("QA note updated")
    );

    assert_dispatch_acts(
        "delete-annotation",
        &mut app,
        AppCommand::DeleteAnnotation {
            page: 0,
            id: note.clone(),
        },
    );
    assert_eq!(
        app.annotation_count(),
        0,
        "deleting the note must remove its reply too"
    );
    assert!(app.annotations().iter().all(|entry| entry.view.id != reply));
}

#[test]
fn save_note_draft_commits_and_cancels_the_public_draft() {
    let mut app = with_excel_document();

    // 1. A public draft entry point opens a draft without arming the tool.
    app.begin_note_draft_at(0, 72.0, 720.0);
    assert!(app.note_draft_open());
    assert!(!app.note_tool_active());
    assert_eq!(app.note_draft_text(), Some(""));

    // 2. Saving empty text keeps the draft open, reports an error, and
    //    creates no annotation.
    assert_dispatch_acts("save-note-draft", &mut app, AppCommand::SaveNoteDraft);
    assert!(app.note_draft_open());
    assert!(app.comments_error().is_some());
    assert_eq!(app.annotation_count(), 0);

    // 3. Fill the draft through the public setter, then commit it.
    app.set_note_draft_text("Draft written by the harness".to_owned());
    assert_eq!(app.note_draft_text(), Some("Draft written by the harness"));
    assert_dispatch_acts("save-note-draft", &mut app, AppCommand::SaveNoteDraft);
    assert!(!app.note_draft_open());
    assert!(app.comments_error().is_none());
    assert_eq!(app.annotation_count(), 1);
    assert_eq!(
        app.annotations()[0].view.contents,
        "Draft written by the harness"
    );

    // 4. Cancel discards a fresh draft without creating an annotation.
    let before = app.annotation_count();
    app.begin_note_draft_at(0, 72.0, 720.0);
    assert!(app.note_draft_open());
    assert_dispatch_acts("cancel-note-draft", &mut app, AppCommand::CancelNoteDraft);
    assert!(!app.note_draft_open());
    assert_eq!(app.annotation_count(), before);
}

#[test]
fn undo_redo_and_help_commands_act() {
    let mut app = with_excel_document();

    // One undoable session edit gives Undo/Redo real observable state.
    app.dispatch(
        AppCommand::AddNote {
            page: 0,
            x: 72.0,
            y: 720.0,
            contents: "QA undo note".to_owned(),
        },
        None,
    );
    assert_eq!(app.annotation_count(), 1);
    assert!(app.can_undo());

    assert_dispatch_acts("undo", &mut app, AppCommand::Undo);
    assert_eq!(app.annotation_count(), 0);
    assert!(app.can_redo());

    assert_dispatch_acts("redo", &mut app, AppCommand::Redo);
    assert_eq!(app.annotation_count(), 1);

    assert!(!app.help_open());
    assert_dispatch_acts("open-help", &mut app, AppCommand::OpenHelp);
    assert!(app.help_open());
    assert_dispatch_acts("close-help", &mut app, AppCommand::CloseHelp);
    assert!(!app.help_open());
}

#[test]
fn toggle_notifications_opens_the_window_and_an_empty_store_says_so() {
    let mut app = PdfxiApp::new();
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/tmp/notifications");
    std::fs::create_dir_all(&dir).expect("create the temporary directory");
    let path = dir.join(format!("ui-{}.tsv", std::process::id()));
    let _ = std::fs::remove_file(&path);
    app.set_notifications_path(path.clone());

    app.dispatch(AppCommand::ToggleNotifications, None);
    assert!(app.notifications_open());
    assert_eq!(app.notification_count(), 0);
    assert_eq!(
        app.notifications_empty_label(),
        Some("No notifications yet.")
    );
    assert_eq!(app.unread_notification_count(), 0);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn convert_to_pdf_opens_the_card_without_a_document() {
    let mut app = PdfxiApp::new();
    app.dispatch(AppCommand::ConvertToPdf, None);
    assert!(app.convert_card_open());
    assert_eq!(app.status(), "Convert to PDF: choose files, then Convert");
}

#[test]
fn set_display_name_and_document_title_act() {
    let mut app = with_excel_document();

    // Display name: public accessor plus the annotation author path.
    let prior = app.display_name().to_owned();
    assert_ne!(prior, "QA Reviewer");
    assert_dispatch_acts(
        "set-display-name",
        &mut app,
        AppCommand::SetDisplayName("QA Reviewer".to_owned()),
    );
    assert_eq!(app.display_name(), "QA Reviewer");
    app.dispatch(
        AppCommand::AddNote {
            page: 0,
            x: 72.0,
            y: 720.0,
            contents: "Authored note".to_owned(),
        },
        None,
    );
    assert_eq!(app.annotations()[0].view.author, "QA Reviewer");

    // Restore the previous value so the test leaves no identity behind.
    assert_dispatch_acts(
        "set-display-name",
        &mut app,
        AppCommand::SetDisplayName(prior.clone()),
    );
    assert_eq!(app.display_name(), prior);

    // Document title: use a temp copy; no repository fixture is written.
    let temp = temp_fixture_copy("title");
    let mut titled = open_temp_copy(&temp);
    assert!(
        !titled
            .state()
            .active_tab()
            .is_some_and(|tab| tab.is_dirty())
    );
    assert_dispatch_acts(
        "set-document-title",
        &mut titled,
        AppCommand::SetDocumentTitle("QA Title".to_owned()),
    );
    assert!(
        titled
            .state()
            .active_tab()
            .is_some_and(|tab| tab.is_dirty()),
        "a title edit must mark the tab dirty"
    );
    // There is no public in-memory Info-title getter; the saved file is read
    // back in `save_writes_a_temp_copy_and_clears_dirty`.
    drop(titled);
    std::fs::remove_file(&temp).expect("remove the title temp copy");
}

#[test]
fn save_writes_a_temp_copy_and_clears_dirty() {
    let temp = temp_fixture_copy("save");
    let mut app = open_temp_copy(&temp);
    let before = std::fs::read(&temp).expect("read the original temp copy");

    app.dispatch(AppCommand::SetDocumentTitle("Saved Title".to_owned()), None);
    app.dispatch(
        AppCommand::AddNote {
            page: 0,
            x: 72.0,
            y: 720.0,
            contents: "Saved note".to_owned(),
        },
        None,
    );
    assert_eq!(app.annotation_count(), 1);
    assert!(
        app.state().active_tab().is_some_and(|tab| tab.is_dirty()),
        "edits must mark the tab dirty before Save"
    );

    assert_dispatch_acts("save", &mut app, AppCommand::Save);
    assert_eq!(app.status(), "Saved");
    assert!(
        !app.state().active_tab().is_some_and(|tab| tab.is_dirty()),
        "Save must clear the dirty flag"
    );

    let after = std::fs::read(&temp).expect("read the saved temp copy");
    assert_ne!(before, after, "Save must rewrite the saved file");

    // The Info title is only readable through the saved bytes (no public app
    // accessor), which also proves SetDocumentTitle persisted.
    let saved = pdfxi_core::Document::from_bytes(after).expect("parse the saved PDF");
    let info = saved.info().expect("read saved Info").expect("saved Info");
    assert_eq!(info.title.as_deref(), Some("Saved Title"));

    drop(app);

    let reopened = open_temp_copy(&temp);
    assert_eq!(reopened.annotation_count(), 1);
    assert_eq!(reopened.annotations()[0].view.contents, "Saved note");
    drop(reopened);
    std::fs::remove_file(&temp).expect("remove the save temp copy");
}

#[test]
fn dispatched_form_commands_act() {
    let path = fixture("fixtures/realworld/pdfxi-forms-selfmade.pdf");
    let mut app = PdfxiApp::new();
    assert!(
        app.open_path_text(path.to_string_lossy().as_ref()),
        "form fixture {} did not open",
        path.display()
    );
    app.refresh_forms();
    assert_eq!(app.form_field_count(), 3);
    assert!(app.has_acroform());
    assert!(app.forms_editable());

    let initial = app.form_field_values();
    assert_eq!(
        form_value(&initial, "text-field").as_deref(),
        Some("typed value")
    );
    let checkbox_initial =
        form_value(&initial, "checkbox-field").expect("the fixture has a checkbox field");

    assert_dispatch_acts(
        "set-form-text",
        &mut app,
        AppCommand::SetFormText {
            name: "text-field".to_owned(),
            value: "QA value".to_owned(),
        },
    );
    assert_eq!(
        form_value(&app.form_field_values(), "text-field").as_deref(),
        Some("QA value")
    );
    assert!(
        app.state().active_tab().is_some_and(|tab| tab.is_dirty()),
        "a committed form edit must mark the tab dirty"
    );

    assert_dispatch_acts(
        "toggle-form-checkbox",
        &mut app,
        AppCommand::ToggleFormCheckbox {
            name: "checkbox-field".to_owned(),
        },
    );
    let checkbox_toggled = form_value(&app.form_field_values(), "checkbox-field")
        .expect("the checkbox is still listed");
    assert_ne!(checkbox_toggled, checkbox_initial);

    // Undo/redo restore the session; `refresh_forms` is the public hook the
    // shell uses to rebuild the cached model after a revision change.
    app.dispatch(AppCommand::Undo, None);
    app.refresh_forms();
    assert_eq!(
        form_value(&app.form_field_values(), "checkbox-field").as_deref(),
        Some(checkbox_initial.as_str())
    );

    app.dispatch(AppCommand::Undo, None);
    app.refresh_forms();
    assert_eq!(
        form_value(&app.form_field_values(), "text-field").as_deref(),
        Some("typed value")
    );

    app.dispatch(AppCommand::Redo, None);
    app.refresh_forms();
    assert_eq!(
        form_value(&app.form_field_values(), "text-field").as_deref(),
        Some("QA value")
    );

    app.dispatch(AppCommand::Redo, None);
    app.refresh_forms();
    assert_eq!(
        form_value(&app.form_field_values(), "checkbox-field").as_deref(),
        Some(checkbox_toggled.as_str())
    );
}

#[test]
fn dispatched_round_b_form_commands_act() {
    let path = fixture("fixtures/realworld/pdfxi-forms-selfmade.pdf");
    let mut app = PdfxiApp::new();
    assert!(
        app.open_path_text(path.to_string_lossy().as_ref()),
        "form fixture {} did not open",
        path.display()
    );
    app.refresh_forms();
    app.dispatch(AppCommand::FillAndSign, None);

    // Focusing a choice widget opens its real option popup (no OS input needed).
    assert_dispatch_acts(
        "focus-form-widget",
        &mut app,
        AppCommand::FocusFormWidget {
            name: "choice-field".to_owned(),
            widget: 0,
            anchor: [120.0, 240.0],
        },
    );
    assert_eq!(app.form_choice_popup(), Some("choice-field"));
    assert_eq!(app.active_form_field(), Some("choice-field"));

    // Choice: the real setter writes /V, /AS and a regenerated appearance.
    assert_dispatch_acts(
        "set-form-choice",
        &mut app,
        AppCommand::SetFormChoice {
            name: "choice-field".to_owned(),
            value: Some("three".to_owned()),
        },
    );
    assert_eq!(
        form_value(&app.form_field_values(), "choice-field").as_deref(),
        Some("three")
    );

    // Radio reaches set_form_radio: on a form without a radio group the typed
    // refusal is a visible hint (the full group behaviour is proven in the
    // forms.rs integration tests on a generated real radio fixture).
    assert_dispatch_acts(
        "set-form-radio",
        &mut app,
        AppCommand::SetFormRadio {
            name: "radio-field".to_owned(),
            on_state: "B".to_owned(),
        },
    );

    // Apply with no active draft reports why there is nothing to commit.
    assert_dispatch_acts("apply-form-edit", &mut app, AppCommand::ApplyFormEdit);

    // Reset walks the model and commits one session edit.
    assert_dispatch_acts("reset-form-fields", &mut app, AppCommand::ResetFormFields);

    // Native-dialog commands reach their explicit-path methods; with no window
    // they surface the typed NoPath error instead of blocking on a modal.
    for command in [
        AppCommand::ImportXfdfDialog,
        AppCommand::ExportXfdfDialog,
        AppCommand::ImportFdfDialog,
        AppCommand::ExportFdfDialog,
    ] {
        let label = command.label();
        assert_dispatch_acts(label, &mut app, command);
        assert!(
            app.forms_hint()
                .is_some_and(|hint| hint.contains("no file was chosen")),
            "{label} must report the typed NoPath hint"
        );
    }

    // Flatten is last: it removes the AcroForm from the open fixture.
    assert_dispatch_acts("flatten-form", &mut app, AppCommand::FlattenForm);
    assert_eq!(app.form_field_count(), 0);
    assert!(app.forms_flatten_report().is_some());
}

#[test]
fn dispatched_shell_and_settings_commands_act() {
    let mut app = with_three_page_document();

    // Toggle theme starts from the default `System` preference, where the
    // last-painted theme is light, so the toggle must persist `Dark`.
    assert_eq!(app.theme_preference(), ThemePreference::System);
    assert_dispatch_acts("toggle-theme", &mut app, AppCommand::ToggleTheme);
    assert_eq!(app.theme_preference(), ThemePreference::Dark);

    assert_dispatch_acts(
        "set-theme-preference",
        &mut app,
        AppCommand::SetThemePreference(ThemePreference::Light),
    );
    assert_eq!(app.theme_preference(), ThemePreference::Light);

    assert_dispatch_acts(
        "set-window-controls",
        &mut app,
        AppCommand::SetWindowControls(WindowControlStyle::MacOs),
    );
    assert_eq!(app.window_controls(), WindowControlStyle::MacOs);

    // T-0220 exposed accessors, so the three shell toggles are now provable.
    let tools_before = app.tools_open();
    assert_dispatch_acts("toggle-tools", &mut app, AppCommand::ToggleTools);
    assert_eq!(app.tools_open(), !tools_before);

    let grid_before = app.grid_view();
    assert_dispatch_acts("toggle-grid", &mut app, AppCommand::ToggleGrid);
    assert_eq!(app.grid_view(), !grid_before);

    // Opening a document parks the rail on Pages; Home is enabled by default.
    assert_eq!(app.active_rail(), RailItem::Pages);
    assert_dispatch_acts("show-rail", &mut app, AppCommand::ShowRail(RailItem::Home));
    assert_eq!(app.active_rail(), RailItem::Home);

    assert_dispatch_acts(
        "set-zoom-percent",
        &mut app,
        AppCommand::SetZoomPercent(150),
    );
    assert_eq!(
        app.state().active_tab().map(|tab| tab.zoom().to_string()),
        Some("150%".to_owned())
    );

    assert_dispatch_acts(
        "submit-page-number",
        &mut app,
        AppCommand::SubmitPageNumber("2".to_owned()),
    );
    assert_eq!(
        app.state().active_tab().map(|tab| tab.current_page()),
        Some(1)
    );

    assert_dispatch_acts(
        "set-close-policy",
        &mut app,
        AppCommand::SetClosePolicy(UiClosePolicy::DiscardDirty),
    );
    assert_eq!(app.close_policy(), UiClosePolicy::DiscardDirty);

    assert_dispatch_acts("clear-selection", &mut app, AppCommand::ClearSelection);

    // The open-error banner and the typed error list are separate surfaces.
    assert!(!app.open_path_text(""));
    assert!(app.open_error_banner().is_some());
    assert_dispatch_acts("dismiss-open-error", &mut app, AppCommand::DismissOpenError);
    assert!(app.open_error_banner().is_none());

    app.record_open_error(OpenError::UnsupportedFormat);
    assert_eq!(app.open_errors().len(), 1);
    assert_dispatch_acts("clear-open-errors", &mut app, AppCommand::ClearOpenErrors);
    assert!(app.open_errors().is_empty());
}

#[test]
fn dispatched_capability_commands_report_unavailable() {
    let mut app = PdfxiApp::new();
    for command in [
        AppCommand::EditDocument,
        AppCommand::Comment,
        AppCommand::Highlight,
        AppCommand::FillAndSign,
        AppCommand::Sign,
        AppCommand::PlaceSignature,
        AppCommand::RequestSignature,
        AppCommand::DocumentProperties,
        AppCommand::ToggleNotifications,
        AppCommand::Organize,
        AppCommand::Export,
        AppCommand::ConvertToPdf,
        AppCommand::CombineFiles,
        AppCommand::Protect,
        AppCommand::AiAssistant,
        AppCommand::AddPage,
        AppCommand::PageMenu,
    ] {
        assert_dispatch_acts(command.label(), &mut app, command);
    }
}

#[test]
fn place_signature_tool_row_appears_with_a_document_and_opens_the_card() {
    let app = PdfxiApp::new();
    assert!(
        !app.tools_panel_rows()
            .iter()
            .any(|row| row.command.label() == "place-signature"),
        "Place signature must not be offered without an open document"
    );

    let mut app = with_excel_document();
    let row = app
        .tools_panel_rows()
        .into_iter()
        .find(|row| row.command.label() == "place-signature")
        .expect("the Place signature row is offered for an open document");
    app.dispatch(row.command, None);
    assert_eq!(app.status(), "Place signature: choose Type, Draw or Image");
}

#[test]
fn request_signature_tool_row_appears_with_a_document_and_opens_the_window() {
    let app = PdfxiApp::new();
    assert!(
        !app.tools_panel_rows()
            .iter()
            .any(|row| row.command.label() == "request-signature"),
        "Request signature must not be offered without an open document"
    );

    let mut app = with_excel_document();
    let row = app
        .tools_panel_rows()
        .into_iter()
        .find(|row| row.command.label() == "request-signature")
        .expect("the Request signature row is offered for an open document");
    app.dispatch(row.command, None);
    assert!(
        app.sign_dialog_open(),
        "the Request signature command opens the Sign card that holds the request form"
    );
    assert_eq!(app.status(), "Signature request: type the recipient");
}

#[test]
fn document_properties_tool_row_appears_with_a_document_and_opens_the_window() {
    let app = PdfxiApp::new();
    assert!(
        !app.tools_panel_rows()
            .iter()
            .any(|row| row.command.label() == "document-properties"),
        "Document properties must not be offered without an open document"
    );

    let mut app = with_excel_document();
    let row = app
        .tools_panel_rows()
        .into_iter()
        .find(|row| row.command.label() == "document-properties")
        .expect("the Document properties row is offered for an open document");
    app.dispatch(row.command, None);
    assert!(app.document_properties_open());
    assert_eq!(app.status(), "Document properties");
}

#[test]
fn export_command_opens_the_card_with_a_document_and_explains_without_one() {
    let mut app = PdfxiApp::new();
    app.dispatch(AppCommand::Export, None);
    assert_eq!(app.status(), "Open a document to export");
    assert!(!app.export_card_open());

    let mut app = with_excel_document();
    app.dispatch(AppCommand::Export, None);
    assert!(app.export_card_open());
    assert!(app.status().starts_with("Export:"));
}

#[test]
fn viewport_commands_reach_the_egui_context() {
    let mut app = PdfxiApp::new();

    let caption = viewport_commands(&mut app, AppCommand::Caption(CaptionAction::Minimize));
    assert!(
        caption.contains(&egui::ViewportCommand::Minimized(true)),
        "caption(Minimize) produced {caption:?}, expected ViewportCommand::Minimized(true)"
    );

    let close = viewport_commands(&mut app, AppCommand::Caption(CaptionAction::Close));
    assert!(
        close.contains(&egui::ViewportCommand::Close),
        "caption(Close) produced {close:?}, expected ViewportCommand::Close"
    );

    let drag = viewport_commands(&mut app, AppCommand::StartWindowDrag);
    assert!(
        drag.contains(&egui::ViewportCommand::StartDrag),
        "start-window-drag produced {drag:?}, expected ViewportCommand::StartDrag"
    );

    let fullscreen = viewport_commands(&mut app, AppCommand::ToggleFullscreen);
    assert!(
        fullscreen.contains(&egui::ViewportCommand::Fullscreen(true)),
        "toggle-fullscreen produced {fullscreen:?}, expected ViewportCommand::Fullscreen(true)"
    );
}

#[test]
fn allowlisted_commands_have_a_reason() {
    assert_eq!(
        UNOBSERVABLE_ALLOWLIST.len(),
        2,
        "the allowlist is reviewed at exactly two entries (open-dialog, \
         copy-selection); adding one needs a deliberate decision and evidence"
    );
    for (label, reason) in UNOBSERVABLE_ALLOWLIST {
        assert!(
            !reason.trim().is_empty(),
            "allowlisted command `{label}` needs a concrete reason"
        );
        assert!(
            reason.len() >= 40,
            "reason for `{label}` is too vague to be auditable: {reason}"
        );
    }
}

// ---------------------------------------------------------------------------
// Static no-dead-controls scanner
// ---------------------------------------------------------------------------

/// A control found in a function that never dispatches.
#[derive(Debug, PartialEq, Eq)]
struct Finding {
    file: String,
    line: usize,
    kind: String,
}

impl std::fmt::Display for Finding {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "{}:{}: `{}` has no dispatch/AppCommand in its enclosing function",
            self.file, self.line, self.kind
        )
    }
}

/// Control constructors whose response is (or should be) acted on.
///
/// Name-based on purpose: it is cheap and stable, but a new helper wrapper is a
/// known false negative. Add helpers here when introducing one.
const CONTROL_PATTERNS: &[&str] = &[
    ".button(",
    "menu_button(",
    "selectable_label(",
    "selectable_value(",
    ".checkbox(",
    "add(Button",
    "shell_tool_button(",
    "shell_icon_button(",
    "title_icon_button(",
    "traffic_light(",
    "text_edit_singleline(",
    "text_edit_multiline(",
    "DragValue",
];

/// A click on a response, including custom-drawn controls that use
/// `ui.interact` instead of a widget constructor.
const CLICK_PATTERN: &str = ".clicked()";

/// Interactions that legitimately do not dispatch themselves.
///
/// Text entry and drag handles push their value through a binding; the dispatch
/// happens on `.changed()` elsewhere or in the model layer. Keep this list
/// short: every entry weakens the gate.
const LINE_ALLOWLIST: &[(&str, &str)] = &[
    (
        "text_edit_singleline(",
        "text field: the query is mirrored and dispatched on .changed()",
    ),
    (
        "text_edit_multiline(",
        "multi-line text field: committed through the bound value",
    ),
    (
        "DragValue",
        "drag handle: edits the bound numeric value directly",
    ),
];

/// Function bodies that defer their action to the caller on purpose.
const BODY_ALLOWLIST: &[(&str, &str)] = &[(
    "return TabAction::",
    "returns a deferred TabAction; the caller dispatches it after the UI closure",
)];

/// A `fn` item, including nested closures.
#[derive(Debug)]
struct Function {
    name: String,
    start: usize,
    end: usize,
}

/// Control factories: they build and return a response; the caller dispatches.
///
/// Allowing them by name (rather than by return type) keeps the exemption
/// narrow: a new factory needs an explicit entry and a reason.
const FUNCTION_ALLOWLIST: &[(&str, &str)] = &[
    (
        "convert_card_ui",
        "convert to PDF window: Add files/Choose output call the rfd-backed methods and Convert calls convert_to_path; source loading and conversion are unit-tested without a window",
    ),
    (
        "shared_panel_ui",
        "shared folder panel: rows open documents and the review buttons call the sync/import methods; the folder and review state are unit-tested without a window",
    ),
    (
        "notifications_ui",
        "notifications window: the Mark all read button writes the already-loaded store; the ToggleNotifications command proof covers opening, and the store itself is unit-tested",
    ),
    (
        "export_card_ui",
        "export card: the format picker is state only and the Export button calls choose_export_path, whose result the Export command proof covers; the native picker is never opened in tests",
    ),
    (
        "place_signature_dialog_ui",
        "placement dialog: the typed/drawn/image buttons call the methods the PlaceSignature command proof covers; the mode selector is state only and the draw pad is a gesture",
    ),
    (
        "shell_icon_button",
        "control factory: returns egui::Response; the caller dispatches the action",
    ),
    (
        "shell_tool_button",
        "control factory: returns egui::Response; the caller dispatches the action",
    ),
    (
        "title_icon_button",
        "control factory: returns egui::Response; the caller dispatches the action",
    ),
    (
        "traffic_light",
        "control factory: returns egui::Response; the caller dispatches Caption",
    ),
];

fn function_name(line: &str) -> Option<String> {
    let trimmed = line.trim_start();
    let after_fn = trimmed.find("fn ")? + 3;
    let rest = &trimmed[after_fn..];
    let end = rest
        .find(|character: char| character == '(' || character == '<' || character.is_whitespace())
        .unwrap_or(rest.len());
    if end == 0 {
        return None;
    }
    Some(rest[..end].to_owned())
}

fn is_fn_start(line: &str) -> bool {
    let trimmed = line.trim_start();
    if trimmed.starts_with("//") {
        return false;
    }
    let prefixes = [
        "fn ",
        "pub fn ",
        "pub(crate) fn ",
        "pub(super) fn ",
        "async fn ",
        "pub async fn ",
        "pub(crate) async fn ",
    ];
    prefixes.iter().any(|prefix| trimmed.starts_with(prefix))
}

/// Net `{` minus `}` on one line, ignoring string literals and line comments.
///
/// Deliberately does not understand raw strings, char literals, or block
/// comments: they do not occur in the scanned shell sources today, and a
/// mis-count degrades to a missed function boundary (a false negative), never
/// to a false alarm about live code.
fn brace_delta(line: &str) -> i32 {
    if line.trim_start().starts_with("//") {
        return 0;
    }
    let bytes = line.as_bytes();
    let mut delta = 0i32;
    let mut index = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    while index < bytes.len() {
        let byte = bytes[index];
        if in_string {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            }
            index += 1;
            continue;
        }
        if byte == b'/' && bytes.get(index + 1) == Some(&b'/') {
            break;
        }
        if byte == b'"' {
            in_string = true;
        } else if byte == b'{' {
            delta += 1;
        } else if byte == b'}' {
            delta -= 1;
        }
        index += 1;
    }
    delta
}

fn functions(source: &str) -> Vec<Function> {
    let lines: Vec<&str> = source.lines().collect();
    let mut found = Vec::new();
    let mut index = 0usize;
    while index < lines.len() {
        if !is_fn_start(lines[index]) {
            index += 1;
            continue;
        }
        let start = index;
        let name = function_name(lines[index]).unwrap_or_default();
        let mut depth = 0i32;
        let mut started = false;
        let mut end = index;
        let mut cursor = index;
        while cursor < lines.len() {
            if cursor > start && !started && is_fn_start(lines[cursor]) {
                break;
            }
            depth += brace_delta(lines[cursor]);
            if lines[cursor].contains('{') {
                started = true;
            }
            end = cursor;
            if started && depth <= 0 {
                break;
            }
            cursor += 1;
        }
        found.push(Function { name, start, end });
        index = end + 1;
    }
    found
}

fn line_is_allowed(line: &str) -> bool {
    LINE_ALLOWLIST
        .iter()
        .any(|(needle, _)| line.contains(needle))
}

/// Scan one source file and return every control with no dispatch in its
/// enclosing function.
fn scan_source(source: &str, file: &str) -> Vec<Finding> {
    let lines: Vec<&str> = source.lines().collect();
    let mut findings = Vec::new();
    for function in functions(source) {
        let body = lines[function.start..=function.end].join("\n");
        if body.contains("dispatch(") || body.contains("AppCommand") {
            continue;
        }
        if BODY_ALLOWLIST
            .iter()
            .any(|(needle, _)| body.contains(needle))
        {
            continue;
        }
        if FUNCTION_ALLOWLIST
            .iter()
            .any(|(name, _)| *name == function.name)
        {
            continue;
        }
        for (line_number, line) in lines
            .iter()
            .enumerate()
            .take(function.end + 1)
            .skip(function.start)
        {
            let line = *line;
            let trimmed = line.trim_start();
            if trimmed.starts_with("//") || line_is_allowed(line) {
                continue;
            }
            if let Some(pattern) = CONTROL_PATTERNS
                .iter()
                .find(|pattern| line.contains(**pattern))
            {
                findings.push(Finding {
                    file: file.to_owned(),
                    line: line_number + 1,
                    kind: (*pattern).to_owned(),
                });
            } else if line.contains(CLICK_PATTERN) {
                findings.push(Finding {
                    file: file.to_owned(),
                    line: line_number + 1,
                    kind: CLICK_PATTERN.to_owned(),
                });
            }
        }
    }
    findings
}

#[test]
fn scanner_flags_a_dead_button() {
    let snippet = "\
impl X {
    fn dead(&mut self, ui: &mut egui::Ui) {
        if ui.button(\"Do nothing\").clicked() {
            // no dispatch
        }
    }
}
";
    let findings = scan_source(snippet, "dead.rs");
    assert_eq!(findings.len(), 1, "findings: {findings:?}");
    assert_eq!(findings[0].kind, ".button(");
    assert_eq!(findings[0].line, 3);
}

#[test]
fn scanner_flags_a_dead_custom_click_target() {
    let snippet = "\
impl X {
    fn dead(&mut self, ui: &mut egui::Ui) {
        let response = ui.interact(rect, id, egui::Sense::click());
        if response.clicked() {
            // no dispatch
        }
    }
}
";
    let findings = scan_source(snippet, "dead.rs");
    assert_eq!(findings.len(), 1, "findings: {findings:?}");
    assert_eq!(findings[0].kind, ".clicked()");
}

#[test]
fn scanner_accepts_a_dispatched_button() {
    let snippet = "\
impl X {
    fn live(&mut self, ui: &mut egui::Ui) {
        if ui.button(\"Zoom\").clicked() {
            self.dispatch(AppCommand::ZoomIn, Some(ui.ctx()));
        }
    }
}
";
    assert!(scan_source(snippet, "live.rs").is_empty());
}

#[test]
fn scanner_accepts_an_allowlisted_text_field() {
    let snippet = "\
impl X {
    fn field(&mut self, ui: &mut egui::Ui) {
        ui.text_edit_singleline(&mut self.find_query);
    }
}
";
    assert!(scan_source(snippet, "field.rs").is_empty());
}

#[test]
fn scanner_accepts_a_deferred_tab_action() {
    let snippet = "\
impl X {
    fn tab(&mut self, ui: &mut egui::Ui) -> TabAction {
        if response.clicked() {
            return TabAction::Close;
        }
        TabAction::None
    }
}
";
    assert!(scan_source(snippet, "tab.rs").is_empty());
}

/// Every `.rs` file under `src/shell` plus `src/ui.rs`.
fn shell_sources() -> Vec<(PathBuf, String)> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut paths: Vec<PathBuf> = std::fs::read_dir(root.join("src/shell"))
        .expect("src/shell must exist")
        .map(|entry| entry.expect("readable dir entry").path())
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("rs"))
        .collect();
    paths.sort();
    paths.push(root.join("src/ui.rs"));
    paths
        .into_iter()
        .map(|path| {
            let source = std::fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()));
            (path, source)
        })
        .collect()
}

#[test]
fn repository_shell_has_no_dead_controls() {
    let mut problems: Vec<String> = Vec::new();
    for (path, source) in shell_sources() {
        let name = path
            .strip_prefix(env!("CARGO_MANIFEST_DIR"))
            .unwrap_or(&path)
            .display()
            .to_string();
        problems.extend(scan_source(&source, &name).into_iter().map(|finding| {
            format!(
                "{finding}\n  fix: route the control through \
                 PdfxiApp::dispatch(AppCommand::...) or allowlist it with a reason"
            )
        }));
    }
    assert!(
        problems.is_empty(),
        "dead UI controls found (T-0212 gate):\n{}",
        problems.join("\n")
    );
}
