//! Pre-print panel, progress window, and the background print job.
//!
//! The panel is deliberately not a system dialog: it collects the choices the
//! Windows dialog does not expose on every driver (fit vs actual size and
//! orientation) plus an initial page range and copy count. "Print..." then opens
//! the real `PrintDlgW`, and the chosen job runs on a named worker thread so the
//! window keeps painting. Cancel is checked between pages.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};

use pdfxi_render::Orientation;
use pdfxi_render::PageScaleMode;
use pdfxi_security::Permission;

use crate::print::{self, PrintModel, PrintPageRange};
use crate::ui::{AppCommand, PdfxiApp};

/// One progress or completion message from the print worker.
enum PrintEvent {
    /// `done` of `total` page images have reached the driver.
    Progress { done: usize, total: usize },
    /// The worker finished; the payload is the status line.
    Done(Result<String, String>),
}

/// A running print job.
struct PrintJob {
    events: Receiver<PrintEvent>,
    cancel: Arc<AtomicBool>,
    done: usize,
    total: usize,
}

/// Window-free-ish state for the print surfaces.
///
/// It owns the editable model and the worker handle; every mutation goes
/// through a command or a method here so the shell stays testable.
#[derive(Default)]
pub(crate) struct PrintUiState {
    panel_open: bool,
    model: PrintModel,
    range_text: String,
    job: Option<PrintJob>,
    message: Option<String>,
}

impl PrintUiState {
    /// Whether a job is currently running.
    pub(crate) fn is_running(&self) -> bool {
        self.job.is_some()
    }

    /// Open the panel with a fresh message area.
    pub(crate) fn open(&mut self) {
        self.panel_open = true;
        self.message = None;
    }

    /// Hide the panel without starting a job.
    pub(crate) fn close(&mut self) {
        self.panel_open = false;
    }
}

impl PdfxiApp {
    /// Whether the active document's permissions deny printing.
    ///
    /// Unencrypted and owner-password documents are allowed. A user-password
    /// session is bound by its `/P` mask.
    #[must_use]
    pub fn print_denied(&self) -> bool {
        let Some(id) = self.state.active_tab_id() else {
            return false;
        };
        let Some(session) = self.sessions.get(&id) else {
            return false;
        };
        match session.decryption_report() {
            Some(report) => !report.permissions.allows(Permission::PRINT),
            None => false,
        }
    }

    /// Open the pre-print panel, refusing when there is nothing to print or the
    /// permissions deny printing.
    pub(crate) fn open_print_panel(&mut self) {
        if self.state.active_tab().is_none() {
            self.status = "Open a document before printing".to_owned();
            return;
        }
        if self.print_denied() {
            let message = "This document's permissions do not allow printing.";
            self.open_error_banner = Some(message.to_owned());
            self.status = message.to_owned();
            return;
        }
        if self.print_ui.is_running() {
            self.status = "A print job is already running".to_owned();
            return;
        }
        self.print_ui.open();
    }

    /// Cancel the running job between pages.
    pub(crate) fn cancel_print_job(&mut self) {
        if let Some(job) = &self.print_ui.job {
            job.cancel.store(true, Ordering::Relaxed);
            self.status = "Cancelling the print job...".to_owned();
        }
        self.print_ui.close();
    }

    /// Draw the pre-print panel and the progress window, and poll the worker.
    pub(crate) fn print_ui_ui(&mut self, ctx: &egui::Context) {
        self.poll_print_job(ctx);
        self.print_progress_ui(ctx);
        self.print_panel_ui(ctx);
    }

    /// Poll the worker without blocking; keep repainting while it runs.
    fn poll_print_job(&mut self, ctx: &egui::Context) {
        let mut finished = None;
        if let Some(job) = &mut self.print_ui.job {
            loop {
                match job.events.try_recv() {
                    Ok(PrintEvent::Progress { done, total }) => {
                        job.done = done;
                        job.total = total;
                    }
                    Ok(PrintEvent::Done(result)) => {
                        finished = Some(result);
                        break;
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        finished = Some(Err("The print worker stopped unexpectedly".to_owned()));
                        break;
                    }
                }
            }
        }
        if let Some(result) = finished {
            self.print_ui.job = None;
            match result {
                Ok(message) => {
                    self.status = message.clone();
                    self.print_ui.message = Some(message);
                }
                Err(message) => {
                    self.status = message.clone();
                    self.print_ui.message = Some(message);
                }
            }
        }
        if self.print_ui.job.is_some() {
            ctx.request_repaint();
        }
    }

    /// The modal-looking pre-print panel.
    fn print_panel_ui(&mut self, ctx: &egui::Context) {
        if !self.print_ui.panel_open || self.print_ui.job.is_some() {
            return;
        }
        let mut start = false;
        let mut cancel = false;
        egui::Window::new("Print")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                ui.label("PDFXI then opens the Windows print dialog for printer, paper and final copies.");
                ui.separator();
                ui.label("Pages");
                let mut all = matches!(self.print_ui.model.range, PrintPageRange::All);
                let mut current = matches!(self.print_ui.model.range, PrintPageRange::Current);
                let mut pages = matches!(self.print_ui.model.range, PrintPageRange::Custom(_));
                ui.horizontal(|ui| {
                    ui.radio_value(&mut all, true, "All");
                    ui.radio_value(&mut current, true, "Current");
                    ui.radio_value(&mut pages, true, "Pages");
                });
                if pages {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.print_ui.range_text)
                            .hint_text("e.g. 1-3,5")
                            .desired_width(160.0),
                    );
                    self.print_ui.model.range =
                        PrintPageRange::Custom(self.print_ui.range_text.clone());
                } else if current {
                    self.print_ui.model.range = PrintPageRange::Current;
                } else {
                    self.print_ui.model.range = PrintPageRange::All;
                }
                ui.add(
                    egui::DragValue::new(&mut self.print_ui.model.copies)
                        .range(1..=999)
                        .prefix("Copies: "),
                );
                ui.horizontal(|ui| {
                    ui.label("Scale");
                    ui.radio_value(
                        &mut self.print_ui.model.scale,
                        PageScaleMode::Fit,
                        "Fit to paper",
                    );
                    ui.radio_value(
                        &mut self.print_ui.model.scale,
                        PageScaleMode::Actual,
                        "Actual size",
                    );
                });
                ui.horizontal(|ui| {
                    ui.label("Orientation");
                    ui.radio_value(
                        &mut self.print_ui.model.orientation,
                        Orientation::Portrait,
                        "Portrait",
                    );
                    ui.radio_value(
                        &mut self.print_ui.model.orientation,
                        Orientation::Landscape,
                        "Landscape",
                    );
                });
                if let Some(message) = &self.print_ui.message {
                    ui.colored_label(ui.visuals().error_fg_color, message);
                }
                ui.separator();
                ui.horizontal(|ui| {
                    if ui.button("Print...").clicked() {
                        start = true;
                    }
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                });
            });
        if start {
            self.dispatch(AppCommand::ConfirmPrint, Some(ctx));
        }
        if cancel {
            self.dispatch(AppCommand::CancelPrint, Some(ctx));
        }
    }

    /// Progress window for a running job.
    fn print_progress_ui(&mut self, ctx: &egui::Context) {
        let Some(job) = &self.print_ui.job else {
            return;
        };
        let done = job.done;
        let total = job.total.max(1);
        let fraction = done as f32 / total as f32;
        egui::Window::new("Printing")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                ui.label(format!("Sending page {} of {total}", done + 1));
                ui.add(egui::ProgressBar::new(fraction).show_percentage());
                if ui.button("Cancel").clicked() {
                    self.dispatch(AppCommand::CancelPrint, Some(ctx));
                }
            });
    }

    /// Build the model, validate it against the document, then start the worker.
    pub(crate) fn start_print_job(&mut self) {
        let Some(id) = self.state.active_tab_id() else {
            self.print_ui.message = Some("No document is open".to_owned());
            return;
        };
        let page_count = self.state.active_tab().map_or(0, |tab| tab.page_count());
        let current_page = self.state.active_tab().map_or(0, |tab| tab.current_page());
        if page_count == 0 {
            self.print_ui.message = Some("The document has no pages to print".to_owned());
            return;
        }
        let context = print::PrintContext::new(page_count, current_page, &[]);
        if let Err(error) = self.print_ui.model.build_plan(&context) {
            self.print_ui.message = Some(error.to_string());
            return;
        }
        let Some(session) = self.sessions.get(&id) else {
            self.print_ui.message = Some("The document has no open session".to_owned());
            return;
        };
        let bytes = session.bytes().to_vec();
        let model = self.print_ui.model.clone();
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = Arc::clone(&cancel);
        let (events, receiver) = mpsc::channel();
        let spawned = std::thread::Builder::new()
            .name("pdfxi-print".to_owned())
            .spawn(move || {
                run_print_worker(
                    bytes,
                    page_count,
                    current_page,
                    model,
                    worker_cancel,
                    events,
                )
            });
        match spawned {
            Ok(_) => {
                self.print_ui.job = Some(PrintJob {
                    events: receiver,
                    cancel,
                    done: 0,
                    total: page_count,
                });
                self.print_ui.panel_open = false;
                self.status = "Printing...".to_owned();
            }
            Err(error) => {
                self.print_ui.message = Some(format!("Could not start the print worker: {error}"));
            }
        }
    }
}

/// Render and print on the worker thread.
fn run_print_worker(
    bytes: Vec<u8>,
    page_count: usize,
    current_page: usize,
    model: PrintModel,
    cancel: Arc<AtomicBool>,
    events: mpsc::Sender<PrintEvent>,
) {
    #[cfg(feature = "pdfium")]
    {
        let renderer = match pdfxi_render::PdfiumRenderer::from_bytes(bytes) {
            Ok(renderer) => renderer,
            Err(error) => {
                let _ = events.send(PrintEvent::Done(Err(format!(
                    "Could not render the document for printing: {error}"
                ))));
                return;
            }
        };
        let last_page = u16::try_from(page_count).unwrap_or(u16::MAX);
        let (seed_from, seed_to) = match &model.range {
            PrintPageRange::Current => {
                let page = u16::try_from(current_page.saturating_add(1)).unwrap_or(u16::MAX);
                (page, page)
            }
            PrintPageRange::Custom(expression) => {
                let resolved =
                    pdfxi_render::parse_page_ranges(expression, page_count).unwrap_or_default();
                match (resolved.first(), resolved.last()) {
                    (Some(first), Some(last)) => (
                        u16::try_from(first.saturating_add(1)).unwrap_or(u16::MAX),
                        u16::try_from(last.saturating_add(1)).unwrap_or(u16::MAX),
                    ),
                    _ => (1, last_page),
                }
            }
            PrintPageRange::All | PrintPageRange::Selection => (1, last_page),
        };
        let result = print::print_with_dialog(
            &renderer,
            page_count,
            current_page,
            seed_from,
            seed_to,
            model.copies.min(u32::from(u16::MAX)) as u16,
            model.scale,
            model.orientation,
            &cancel,
            &mut |done, total| {
                let _ = events.send(PrintEvent::Progress { done, total });
            },
        );
        let message = match result {
            Ok(outcome) => Ok(format!("Printed {} page(s)", outcome.pages_printed)),
            Err(error) => Err(error.to_string()),
        };
        let _ = events.send(PrintEvent::Done(message));
    }
    #[cfg(not(feature = "pdfium"))]
    {
        let _ = (bytes, page_count, current_page, model, cancel);
        let _ = events.send(PrintEvent::Done(Err(
            "Printing requires the pdfium feature".to_owned(),
        )));
    }
}
