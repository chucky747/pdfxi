//! Split modules for the native viewer shell.
//!
//! The shell was one `ui.rs` file. These modules keep the same behaviour while
//! giving each surface (title bar, command bar, side panel, document view, find
//! bar, empty state, dialogs, shared widgets) its own file. Every control builds
//! an [`commands::AppCommand`] and routes it through
//! [`crate::ui::PdfxiApp::dispatch`]; no click handler mutates application state
//! directly.

pub mod bottom_bar;
pub mod capabilities;
pub mod command_bar;
pub mod commands;
pub mod comments;
pub mod dialogs;
pub mod document_view;
pub mod empty_state;
pub mod find_bar;
pub mod help;
pub mod preferences;
pub mod print_dialog;
pub mod rail;
pub mod session;
pub mod side_panel;
pub mod title_bar;
pub mod tools_panel;
pub mod widgets;
