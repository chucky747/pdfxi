//! UI interaction harness and no-dead-controls gate (T-0212).
//!
//! Two independent checks live here:
//!
//! 1. **Interaction harness** — every [`AppCommand`] the shell can emit is
//!    dispatched through the single public entry point,
//!    [`PdfxiApp::dispatch`], and must change observable application state. A
//!    command whose only effect is a viewport message is proven through the
//!    commands an `egui::Context` records for the frame. The few commands whose
//!    effect is intentionally invisible to a window-free process are listed in
//!    [`UNOBSERVABLE_ALLOWLIST`] with one reason each.
//! 2. **Static no-dead-controls scanner** — a line-based scan of the shell
//!    sources fails the build when a clickable control is created without a
//!    following `dispatch` (or `AppCommand`) in its enclosing function. The
//!    scanner is exercised by synthetic positive and negative snippets so the
//!    gate itself is tested, then run against `src/shell/**` and `src/ui.rs`.
//!
//! The file is gated on `ui` + `pdfium`: the harness opens real fixture PDFs,
//! and the page/find commands can only be proven when the native renderer
//! supplies real page counts and text. Registering a new `AppCommand` without
//! adding it to [`PROVEN_LABELS`] or [`UNOBSERVABLE_ALLOWLIST`] fails
//! [`every_app_command_is_proven_or_allowlisted`].

#![cfg(all(feature = "ui", feature = "pdfium"))]

use std::collections::BTreeSet;
use std::path::PathBuf;

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
    thumbnail_page: usize,
    tools_open: bool,
    grid_view: bool,
    active_rail: RailItem,
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
        thumbnail_page: app.thumbnail_state().active_page(),
        tools_open: app.tools_open(),
        grid_view: app.grid_view(),
        active_rail: app.active_rail(),
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

/// Every `AppCommand` variant, once, with dummy arguments.
///
/// `close-tab` and friends share a family label, but every variant has its own
/// label; the uniqueness assertion below is therefore per variant.
fn all_commands() -> Vec<AppCommand> {
    vec![
        AppCommand::OpenDialog,
        AppCommand::OpenPath("fixtures/minimal.pdf".to_owned()),
        AppCommand::OpenDropped(vec![PathBuf::from("fixtures/minimal.pdf")]),
        AppCommand::CloseActiveTab,
        AppCommand::CloseTab(TabId::from_u64(1)),
        AppCommand::CloseOtherTabs,
        AppCommand::ActivateTab(TabId::from_u64(1)),
        AppCommand::ActivateNextTab,
        AppCommand::ActivatePreviousTab,
        AppCommand::ReorderTab {
            id: TabId::from_u64(1),
            new_index: 0,
        },
        AppCommand::NextPage,
        AppCommand::PreviousPage,
        AppCommand::GoToPage(1),
        AppCommand::ZoomIn,
        AppCommand::ZoomOut,
        AppCommand::FitPage,
        AppCommand::FitWidth,
        AppCommand::ActualSize,
        AppCommand::RotateClockwise,
        AppCommand::RotateCounterclockwise,
        AppCommand::ResetRotation,
        AppCommand::SetViewMode(ViewMode::SinglePage),
        AppCommand::SelectTool(Toolset::Hand),
        AppCommand::ToggleFind,
        AppCommand::OpenFind,
        AppCommand::CloseFind,
        AppCommand::FindNext,
        AppCommand::FindPrevious,
        AppCommand::SetFindQuery("Page".to_owned()),
        AppCommand::SetFindOptions {
            case_sensitive: true,
            whole_word: true,
        },
        AppCommand::CopySelection,
        AppCommand::ClearSelection,
        AppCommand::SetThemePreference(ThemePreference::Dark),
        AppCommand::SetWindowControls(WindowControlStyle::MacOs),
        AppCommand::ToggleTheme,
        AppCommand::Caption(CaptionAction::Close),
        AppCommand::StartWindowDrag,
        AppCommand::DismissOpenError,
        AppCommand::ClearOpenErrors,
        AppCommand::SelectThumbnailPage(1),
        AppCommand::SetClosePolicy(UiClosePolicy::DiscardDirty),
        // T-0220 acceptance-shell commands.
        AppCommand::EditDocument,
        AppCommand::Comment,
        AppCommand::Highlight,
        AppCommand::FillAndSign,
        AppCommand::Sign,
        AppCommand::Organize,
        AppCommand::Export,
        AppCommand::ConvertToPdf,
        AppCommand::CombineFiles,
        AppCommand::Protect,
        AppCommand::AiAssistant,
        AppCommand::ToggleTools,
        AppCommand::ToggleGrid,
        AppCommand::ToggleFullscreen,
        AppCommand::SetZoomPercent(150),
        AppCommand::SubmitPageNumber("2".to_owned()),
        AppCommand::ShowRail(RailItem::Pages),
        AppCommand::AddPage,
        AppCommand::PageMenu,
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
    ];

    let commands = all_commands();
    assert_eq!(
        commands.len(),
        expected.len(),
        "all_commands() and the label snapshot must list the same variants"
    );
    let mut seen = BTreeSet::new();
    for (command, (label, _)) in commands.iter().zip(expected) {
        assert_eq!(command.label(), *label, "label changed for {command:?}");
        assert!(!label.is_empty(), "empty label for {command:?}");
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

    let mut labels: Vec<&str> = all_commands().iter().map(AppCommand::label).collect();
    labels.sort_unstable();

    assert_eq!(
        labels, covered,
        "every AppCommand must be proven by a dispatch test or listed in \
         UNOBSERVABLE_ALLOWLIST; the two lists differ"
    );
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
    "checkbox(",
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
