//! Native PDFXI viewer executable, available with the `ui` feature.
//!
//! This binary is a thin wrapper around [`pdfxi_app::ui::run`]. All of the
//! viewer behavior lives in the library so it can be tested without a window.

fn main() {
    pdfxi_app::ui::run();
}
