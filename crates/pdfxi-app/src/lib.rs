//! Desktop application shell, document tabs, toolsets, panels, and canvas.
//!
//! This crate provides a pure-Rust model of the viewer shell: open document
//! tabs, per-tab layout/zoom/rotation state, and the active selection tool. The
//! core model has no `egui`, renderer, or platform dependency so the state
//! machine can be tested deterministically. An optional native viewer shell
//! built on `egui`/`eframe` is available behind the `ui` feature.

#![deny(missing_docs)]
#![forbid(unsafe_code)]

mod state;
mod tabs;
mod toolset;
mod view;

/// Native egui/eframe viewer shell, available behind the `ui` feature.
#[cfg(feature = "ui")]
pub mod ui;

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
