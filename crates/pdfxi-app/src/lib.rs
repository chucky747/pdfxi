//! Desktop application shell, document tabs, toolsets, panels, and canvas.
//!
//! This crate currently provides a pure-Rust model of the viewer shell: open
//! document tabs, per-tab layout/zoom/rotation state, and the active selection
//! tool. It intentionally has no `egui`, renderer, or platform dependency so
//! the state machine can be tested deterministically. The rendering and widget
//! layers will be built on top of these types in later milestones.

#![deny(missing_docs)]
#![forbid(unsafe_code)]

mod state;
mod tabs;
mod toolset;
mod view;

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
