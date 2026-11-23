//! Widget-level tests for the modals and tool cards the review exercised.
//!
//! These tests render the real `egui` widgets through AccessKit instead of
//! dispatching `AppCommand` directly, so a dropped pending state or an
//! unreachable button fails here.

use super::*;

use super::shell::session::{PendingClose, SaveConflict};
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use std::path::{Path, PathBuf};

fn fixture(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(relative)
}

#[test]
fn close_prompt_discard_closes_the_tab_and_requests_the_window_close() {
    let mut app = PdfxiApp::new();
    app.open_document(&fixture("../../fixtures/minimal.pdf"));
    app.set_active_document_title("dirty")
        .expect("mark the fixture dirty");
    let id = app.state().active_tab_id().expect("one active tab");
    app.close_window_after_resolve = true;
    app.pending_close = Some(PendingClose {
        id,
        title: "dirty".to_owned(),
    });

    let mut harness = Harness::builder()
        .with_size(egui::vec2(1000.0, 700.0))
        .build_state(|ctx, app| app.close_prompt_ui(ctx), app);
    harness.run();
    harness.get_by_label("Unsaved changes");
    harness.get_by_label("Discard").click();
    harness.run();

    assert_eq!(harness.state().state().tab_count(), 0);
    assert!(harness.state().request_window_close);
    assert!(harness.state().pending_close.is_none());
}

#[test]
fn password_dialog_keeps_the_prompt_after_a_wrong_password() {
    let mut app = PdfxiApp::new();
    app.open_document(&fixture(
        "../../fixtures/realworld/pdfxi-encrypted-selfmade.pdf",
    ));
    assert!(app.password_prompt_open());
    app.password_prompt.as_mut().expect("password prompt").input =
        "definitely-not-the-password".to_owned();

    let mut harness = Harness::builder()
        .with_size(egui::vec2(1000.0, 700.0))
        .build_state(|ctx, app| app.password_dialog_ui(ctx), app);
    harness.run();
    harness.get_by_label("This document is encrypted. Enter its open password.");
    harness.get_by_label("Open").click();
    harness.run();

    assert!(harness.state().password_prompt_open());
    assert!(harness.state().password_prompt_error().is_some());
    assert_eq!(harness.state().state().tab_count(), 0);
}

#[test]
fn organize_card_enables_rotation_after_a_page_is_selected() {
    let mut app = PdfxiApp::new();
    app.open_document(&fixture("../../fixtures/realworld/powerpoint-slides.pdf"));
    assert_eq!(
        app.state().active_tab().map(crate::DocumentTab::page_count),
        Some(3)
    );
    app.organize.open = true;

    let mut harness = Harness::builder()
        .with_size(egui::vec2(1200.0, 800.0))
        .build_state(|ctx, app| app.organize_card_ui(ctx), app);
    harness.run();
    harness.get_by_label("0 of 3 page(s) selected");
    harness.state_mut().organize_select_page(0, false, false);
    harness.run();
    harness.get_by_label("1 of 3 page(s) selected");
    harness.get_by_label("Rotate left").click();
    harness.run();

    assert!(harness.state().status().contains("Rotated 1 page"));
}

#[test]
fn redact_card_disables_redaction_without_a_selection() {
    let mut app = PdfxiApp::new();
    app.open_document(&fixture("../../fixtures/minimal.pdf"));
    app.redact.open = true;

    let mut harness = Harness::builder()
        .with_size(egui::vec2(1000.0, 700.0))
        .build_state(|ctx, app| app.redact_card_ui(ctx), app);
    harness.run();
    harness.get_by_label("Redact and sanitize");
    let button = harness.get_by_label("Redact current selection");
    assert!(button.accesskit_node().is_disabled());
}

#[test]
fn save_conflict_dialog_keeps_the_conflict_until_a_choice_is_made() {
    let mut app = PdfxiApp::new();
    let path = fixture("../../fixtures/minimal.pdf");
    app.open_document(&path);
    let id = app.state().active_tab_id().expect("one active tab");
    app.save_conflict = Some(SaveConflict {
        id,
        path: path.clone(),
    });

    let mut harness = Harness::builder()
        .with_size(egui::vec2(1000.0, 700.0))
        .build_state(|ctx, app| app.save_conflict_ui(ctx), app);
    harness.run();
    harness.get_by_label("File changed on disk");
    assert!(harness.state().save_conflict.is_some());
    harness.get_by_label("Cancel").click();
    harness.run();

    assert!(
        harness.state().save_conflict.is_none(),
        "Cancel must resolve the pending conflict"
    );
}

#[test]
fn convert_card_starts_with_no_sources_and_disables_convert() {
    let mut app = PdfxiApp::new();
    app.convert.open = true;

    let mut harness = Harness::builder()
        .with_size(egui::vec2(1000.0, 700.0))
        .build_state(|ctx, app| app.convert_card_ui(ctx), app);
    harness.run();
    harness.get_by_label("No files chosen");
    let button = harness.get_by_label("Convert");
    assert!(button.accesskit_node().is_disabled());
    assert!(harness.state().convert.source_names.is_empty());
}

#[test]
fn protect_card_toggles_permissions_in_place() {
    let mut app = PdfxiApp::new();
    app.open_document(&fixture("../../fixtures/minimal.pdf"));
    app.protect.open = true;
    let before = app.protect.allow_print;

    let mut harness = Harness::builder()
        .with_size(egui::vec2(1000.0, 700.0))
        .build_state(|ctx, app| app.protect_card_ui(ctx), app);
    harness.run();
    harness.get_by_label("Password protection");
    harness.get_by_label("Printing").click();
    harness.run();

    assert_eq!(harness.state().protect.allow_print, !before);
    assert!(
        harness.state().protect.open,
        "toggling a permission must not close the card"
    );
}

#[test]
fn document_properties_lists_file_facts_for_the_open_document() {
    let mut app = PdfxiApp::new();
    app.open_document(&fixture("../../fixtures/minimal.pdf"));
    app.open_document_properties();
    assert!(app.document_properties_open());

    let mut harness = Harness::builder()
        .with_size(egui::vec2(1000.0, 700.0))
        .build_state(|ctx, app| app.document_properties_ui(ctx), app);
    harness.run();
    harness.get_by_label("Document properties");
    harness.get_by_label("File name");
    harness.get_by_label("minimal.pdf");
}

#[test]
fn signing_dialog_exposes_identity_controls_without_a_document() {
    let mut app = PdfxiApp::new();
    app.signing.open = true;

    let mut harness = Harness::builder()
        .with_size(egui::vec2(1000.0, 700.0))
        .build_state(|ctx, app| app.signing_dialog_ui(ctx), app);
    harness.run();
    harness.get_by_label("Certificate signature");
    harness.get_by_label("Choose PFX...");
}
