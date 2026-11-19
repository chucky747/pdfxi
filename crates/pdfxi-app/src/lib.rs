//! Desktop application shell, document tabs, toolsets, panels, and canvas.
//!
//! This crate provides a pure-Rust model of the viewer shell: open document
//! tabs, per-tab layout/zoom/rotation state, and the active selection tool. The
//! core model has no `egui`, renderer, or platform dependency so the state
//! machine can be tested deterministically. The [`canvas`] module arranges
//! pages and answers hit-testing/coordinate questions, and [`selection`] maps
//! drag points to ordered text ranges — both without any renderer dependency.
//! An optional native viewer shell built on `egui`/`eframe` is available behind
//! the `ui` feature.

#![deny(missing_docs)]
#![forbid(unsafe_code)]

pub mod canvas;
pub mod selection;
mod state;
mod tabs;
mod toolset;
mod view;

/// Native egui/eframe viewer shell, available behind the `ui` feature.
#[cfg(feature = "ui")]
pub mod ui;

pub use canvas::{
    CanvasHit, CanvasLayout, CanvasPoint, CanvasRect, CanvasSize, PageGeometry, PageSize,
};
pub use selection::{
    OwnedSpan, OwnedTextPage, SelectableTextPage, Selection, SelectionModel, SpanView, TextAnchor,
};
pub use state::{AppError, AppState};
pub use tabs::{DocumentTab, TabId, display_name_from_path};
pub use toolset::Toolset;
pub use view::{DEFAULT_ZOOM, MAX_ZOOM, MIN_ZOOM, Rotation, ViewMode, Zoom};

/// Return the implementation name for diagnostics.
pub fn app_name() -> &'static str {
    "PDFXI"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_name_is_stable() {
        assert_eq!(app_name(), "PDFXI");
    }
}
