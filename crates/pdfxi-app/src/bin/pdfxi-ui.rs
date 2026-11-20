//! Native PDFXI viewer executable, available with the `ui` feature.
//!
//! This binary is a thin wrapper around [`pdfxi_app::ui::run`]. All of the
//! viewer behavior lives in the library so it can be tested without a window.
//!
//! # Release behavior
//!
//! Release builds are linked as a Windows GUI binary, so no console window
//! appears when the viewer is launched from Explorer, a shortcut, or the
//! installer. Because stderr is then invisible to the user, every panic is
//! written to `%LOCALAPPDATA%\PDFXI\logs\pdfxi-ui.log` and shown in a native
//! message box instead. `ui::run` returns `Err(message)` when the viewer window
//! cannot start; that message is logged and shown the same way before the
//! process exits with code 1.
//!
//! Debug builds keep the console subsystem so `cargo run` shows messages.

#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use std::path::PathBuf;

/// File name of the append-only crash log inside the per-user log directory.
const LOG_FILE: &str = "pdfxi-ui.log";

/// Caption used by the native failure dialog.
#[cfg(windows)]
const DIALOG_CAPTION: &str = "PDFXI";

/// Install the failure handler, then start the viewer.
///
/// The hook never panics itself: every step degrades to showing the message
/// with no log path when the per-user directory is unavailable.
fn main() {
    install_panic_hook();
    if let Err(message) = pdfxi_app::ui::run() {
        report_startup_failure(&message);
        std::process::exit(1);
    }
}

/// Log a startup failure and show it, then return to the caller to exit.
fn report_startup_failure(message: &str) {
    let log_path = append_log(message);
    let detail = match &log_path {
        Some(path) => format!("Details were written to:\n{}", path.display()),
        None => "No log file could be written.".to_owned(),
    };
    show_message_box(&format!(
        "PDFXI viewer could not start.\n\n{message}\n\n{detail}"
    ));
}

/// Route panics to the log file and a native message box.
fn install_panic_hook() {
    std::panic::set_hook(Box::new(|info| {
        let message = panic_message(info);
        let location = info
            .location()
            .map(|location| format!("{}:{}", location.file(), location.line()))
            .unwrap_or_else(|| "unknown location".to_owned());
        let log_path = append_log(&format!("{message}\n  at {location}"));
        let detail = match &log_path {
            Some(path) => format!("Details were written to:\n{}", path.display()),
            None => "No log file could be written.".to_owned(),
        };
        show_message_box(&format!(
            "PDFXI viewer stopped unexpectedly.\n\n{message}\n\n{detail}"
        ));
    }));
}
/// Extract a printable message from a panic payload.
fn panic_message(info: &std::panic::PanicHookInfo<'_>) -> String {
    if let Some(message) = info.payload().downcast_ref::<&str>() {
        return (*message).to_owned();
    }
    if let Some(message) = info.payload().downcast_ref::<String>() {
        return message.clone();
    }
    "the viewer panicked with a non-text message".to_owned()
}

/// Directory for per-user logs, or `None` when `%LOCALAPPDATA%` is unset.
fn log_directory() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA")
        .filter(|base| !base.is_empty())
        .map(|base| PathBuf::from(base).join("PDFXI").join("logs"))
}

/// Append one timestamped entry to the crash log and return its path.
fn append_log(text: &str) -> Option<PathBuf> {
    use std::io::Write;

    let directory = log_directory()?;
    std::fs::create_dir_all(&directory).ok()?;
    let path = directory.join(LOG_FILE);
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0);
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .ok()?;
    // A failed log write must not replace the original error message.
    let _ = writeln!(file, "[unix {seconds}] {text}");
    Some(path)
}
/// Show a modal error box owned by no window.
#[cfg(windows)]
fn show_message_box(text: &str) {
    use std::ptr;

    #[link(name = "user32")]
    unsafe extern "system" {
        fn MessageBoxW(
            window: *mut core::ffi::c_void,
            text: *const u16,
            caption: *const u16,
            kind: u32,
        ) -> i32;
    }

    const MB_OK: u32 = 0x0000_0000;
    const MB_ICONERROR: u32 = 0x0000_0010;
    const MB_SETFOREGROUND: u32 = 0x0001_0000;

    let caption: Vec<u16> = DIALOG_CAPTION
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let body: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: both UTF-16 buffers are NUL-terminated and outlive the call, and
    // a null owner window is explicitly allowed by MessageBoxW.
    unsafe {
        MessageBoxW(
            ptr::null_mut(),
            body.as_ptr(),
            caption.as_ptr(),
            MB_OK | MB_ICONERROR | MB_SETFOREGROUND,
        );
    }
}

/// Non-Windows fallback: there is no native message box to show.
#[cfg(not(windows))]
fn show_message_box(text: &str) {
    eprintln!("{text}");
}
