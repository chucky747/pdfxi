//! Export card (A-27): export the active document to text, Office, HTML, RTF
//! or page images.
//!
//! The card is opened by [`AppCommand::Export`] and lives under the Tools panel
//! in the right side panel. Exporting is synchronous on the UI thread; the
//! converter's single dispatch validates the format, selection and raster
//! options before writing anything.
//!
//! [`PdfxiApp::export_to_path`] is the testable path: it never opens a file
//! dialog and it writes every part the converter returns. The file picker is
//! confined to [`PdfxiApp::choose_export_path`].

use std::path::{Path, PathBuf};

use pdfxi_convert::pdf_export::{ExportFormat, ExportOptions, ExportedFile, export_pdf};
use pdfxi_convert::{DEFAULT_EXPORT_DPI, DEFAULT_JPEG_QUALITY, TextPageSelection};

use crate::design::Palette;
use crate::ui::PdfxiApp;

/// The eight A-27 export targets.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum ExportChoice {
    /// Plain UTF-8 text.
    #[default]
    Text,
    /// WordprocessingML.
    Docx,
    /// SpreadsheetML.
    Xlsx,
    /// PresentationML.
    Pptx,
    /// Page images as PNG.
    Png,
    /// Page images as JPEG.
    Jpeg,
    /// A single HTML document.
    Html,
    /// Rich Text Format.
    Rtf,
}

impl ExportChoice {
    /// Every target, in the order the format picker lists them.
    pub(crate) const ALL: [Self; 8] = [
        Self::Text,
        Self::Docx,
        Self::Xlsx,
        Self::Pptx,
        Self::Png,
        Self::Jpeg,
        Self::Html,
        Self::Rtf,
    ];

    /// Human-readable picker label.
    #[must_use]
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Text => "Plain text (.txt)",
            Self::Docx => "Word document (.docx)",
            Self::Xlsx => "Excel workbook (.xlsx)",
            Self::Pptx => "PowerPoint (.pptx)",
            Self::Png => "PNG images (.png)",
            Self::Jpeg => "JPEG images (.jpg)",
            Self::Html => "Web page (.html)",
            Self::Rtf => "Rich Text (.rtf)",
        }
    }

    /// File extension without the dot.
    #[must_use]
    pub(crate) fn extension(self) -> &'static str {
        match self {
            Self::Text => "txt",
            Self::Docx => "docx",
            Self::Xlsx => "xlsx",
            Self::Pptx => "pptx",
            Self::Png => "png",
            Self::Jpeg => "jpg",
            Self::Html => "html",
            Self::Rtf => "rtf",
        }
    }

    /// Converter format for this choice.
    #[must_use]
    pub(crate) fn to_format(self) -> ExportFormat {
        match self {
            Self::Text => ExportFormat::Text,
            Self::Docx => ExportFormat::Docx,
            Self::Xlsx => ExportFormat::Xlsx,
            Self::Pptx => ExportFormat::Pptx,
            Self::Png => ExportFormat::Png,
            Self::Jpeg => ExportFormat::Jpeg,
            Self::Html => ExportFormat::Html,
            Self::Rtf => ExportFormat::Rtf,
        }
    }
}

/// State of the Export card.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExportUiState {
    /// Whether the card is visible.
    pub(crate) open: bool,
    /// Selected target format.
    pub(crate) format: ExportChoice,
    /// Baseline JPEG quality, `1..=100`; ignored by other formats.
    pub(crate) jpeg_quality: u8,
    /// Raster resolution for PNG and JPEG.
    pub(crate) dpi: u32,
    /// Path of the last successful export.
    pub(crate) last_path: Option<PathBuf>,
    /// Typed error from the last export.
    pub(crate) error: Option<String>,
    /// Progress note shown while an export runs.
    pub(crate) progress: Option<String>,
}

impl Default for ExportUiState {
    fn default() -> Self {
        Self {
            open: false,
            format: ExportChoice::default(),
            jpeg_quality: DEFAULT_JPEG_QUALITY,
            dpi: DEFAULT_EXPORT_DPI,
            last_path: None,
            error: None,
            progress: None,
        }
    }
}

impl PdfxiApp {
    /// Handle [`AppCommand::Export`]: open the card or explain the missing
    /// document. It never exports on its own.
    pub(crate) fn handle_export_command(&mut self) {
        if self.state.active_tab_id().is_none() {
            self.status = "Open a document to export".to_owned();
            return;
        }
        self.export_ui.open = true;
        self.export_ui.error = None;
        self.status = "Export: choose a format, then Export".to_owned();
    }

    /// Whether the Export card is open (test and harness accessor).
    #[must_use]
    pub fn export_card_open(&self) -> bool {
        self.export_ui.open
    }

    /// Draw the Export card under the Tools panel.
    pub(crate) fn export_card_ui(&mut self, ui: &mut egui::Ui, palette: Palette) {
        if !self.export_ui.open {
            return;
        }
        let document_open = self.state.active_tab_id().is_some();
        ui.separator();
        ui.label(egui::RichText::new("Export").strong());
        ui.label(
            egui::RichText::new("Convert this PDF to:")
                .size(11.0)
                .color(palette.text_muted),
        );
        // Only formats this build can actually write are offered; an
        // unimplemented target is absent, not a disabled ghost control.
        let implemented: Vec<ExportChoice> = ExportChoice::ALL
            .into_iter()
            .filter(|choice| choice.to_format().is_implemented())
            .collect();
        if !self.export_ui.format.to_format().is_implemented() {
            self.export_ui.format = implemented.first().copied().unwrap_or_default();
        }
        egui::ComboBox::from_id_salt("export_format")
            .selected_text(self.export_ui.format.label())
            .width(190.0)
            .show_ui(ui, |ui| {
                for choice in implemented {
                    ui.selectable_value(&mut self.export_ui.format, choice, choice.label());
                }
            });
        if self.export_ui.format == ExportChoice::Jpeg {
            ui.add(
                egui::Slider::new(&mut self.export_ui.jpeg_quality, 1..=100).text("JPEG quality"),
            );
        }
        let mut export = false;
        if ui
            .add_enabled(
                document_open && self.export_ui.format.to_format().is_implemented(),
                egui::Button::new("Export"),
            )
            .on_disabled_hover_text("Open a document and choose an implemented format")
            .clicked()
        {
            export = true;
        }
        if let Some(progress) = &self.export_ui.progress {
            ui.label(progress);
        }
        if let Some(error) = &self.export_ui.error {
            ui.colored_label(ui.visuals().error_fg_color, error);
        }
        if let Some(path) = &self.export_ui.last_path {
            ui.label(
                egui::RichText::new(format!("Last export: {}", path.display()))
                    .size(11.0)
                    .color(palette.text_muted),
            );
        }
        if export {
            self.choose_export_path();
        }
    }

    /// Ask for a destination with the native picker and export there.
    ///
    /// This is the only file-dialog call; tests drive
    /// [`Self::export_to_path`] instead.
    pub(crate) fn choose_export_path(&mut self) {
        let Some((stem, _title)) = self.export_document_name() else {
            self.status = "Open a document to export".to_owned();
            return;
        };
        let extension = self.export_ui.format.extension();
        let Some(path) = rfd::FileDialog::new()
            .add_filter(extension, &[extension])
            .set_file_name(format!("{stem}.{extension}"))
            .save_file()
        else {
            return;
        };
        let _ = self.export_to_path(&path);
    }

    /// Run the converter for the active document.
    ///
    /// The card's format, DPI and JPEG quality are used with an all-pages
    /// selection and the active tab title. `path` only supplies the fallback
    /// title when the tab has none.
    pub(crate) fn export_document_to(&mut self, path: &Path) -> Result<ExportedFile, String> {
        let Some(id) = self.state.active_tab_id() else {
            return Err("Open a document to export".to_owned());
        };
        let Some(session) = self.sessions.get(&id) else {
            return Err("The document has no open session".to_owned());
        };
        let title = self.state.tab_title(id).unwrap_or_else(|| {
            path.file_stem().map_or_else(
                || "document".to_owned(),
                |stem| stem.to_string_lossy().into_owned(),
            )
        });
        let mut options = ExportOptions::new();
        options.title = title;
        options.selection = TextPageSelection::All;
        options.dpi = self.export_ui.dpi;
        options.jpeg_quality = self.export_ui.jpeg_quality;
        export_pdf(
            session.document(),
            self.export_ui.format.to_format(),
            &options,
        )
        .map_err(|error| error.to_string())
    }

    /// Export to `path` and write every part.
    ///
    /// A single part is written exactly at `path`. Multiple parts (page
    /// images) are written beside it as `<stem>-<part name>.<extension>`.
    pub(crate) fn export_to_path(&mut self, path: &Path) -> Result<(), String> {
        let result = self.write_export(path);
        match result {
            Ok(()) => {
                self.export_ui.error = None;
                self.export_ui.progress = None;
                self.export_ui.last_path = Some(path.to_path_buf());
                self.notify_real_event("Export finished", &path.display().to_string());
                Ok(())
            }
            Err(error) => {
                self.export_ui.error = Some(error.clone());
                self.export_ui.progress = None;
                self.status = format!("Export failed: {error}");
                Err(error)
            }
        }
    }

    /// Write the converter's parts for `path` and set the success status.
    fn write_export(&mut self, path: &Path) -> Result<(), String> {
        self.export_ui.progress = Some("Exporting...".to_owned());
        let exported = self.export_document_to(path)?;
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("Could not create {}: {error}", parent.display()))?;
        }
        let extension = self.export_ui.format.extension();
        let stem = path.file_stem().map_or_else(
            || "document".to_owned(),
            |stem| stem.to_string_lossy().into_owned(),
        );
        if exported.parts.len() == 1 {
            std::fs::write(path, &exported.parts[0].bytes)
                .map_err(|error| format!("Could not write {}: {error}", path.display()))?;
        } else {
            for part in &exported.parts {
                let target = path.with_file_name(format!("{stem}-{}.{extension}", part.name));
                std::fs::write(&target, &part.bytes)
                    .map_err(|error| format!("Could not write {}: {error}", target.display()))?;
            }
        }
        let mut status = format!(
            "Exported {} page(s) to {}",
            exported.page_count,
            path.display()
        );
        if !exported.unsupported.is_empty() {
            let finding = &exported.unsupported[0];
            status.push_str(&format!(
                "; {} unsupported item(s), e.g. page {} {}: {}",
                exported.unsupported.len(),
                finding.page + 1,
                finding.kind,
                finding.detail
            ));
            self.export_ui.progress = Some(status.clone());
        }
        self.status = status;
        Ok(())
    }

    /// File stem and tab title for the active document.
    fn export_document_name(&self) -> Option<(String, String)> {
        let id = self.state.active_tab_id()?;
        let stem = self
            .sessions
            .get(&id)
            .and_then(|session| session.path())
            .and_then(Path::file_stem)
            .map_or_else(
                || "document".to_owned(),
                |stem| stem.to_string_lossy().into_owned(),
            );
        let title = self.state.tab_title(id).unwrap_or_else(|| stem.clone());
        Some((stem, title))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    fn fixture() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/realworld/microsoft-excel-2019-sample.pdf")
    }

    fn export_dir(tag: &str) -> PathBuf {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/tmp/export-card")
            .join(format!("{}-{tag}-{unique}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create the export directory");
        dir
    }

    fn app_with_excel() -> PdfxiApp {
        let mut app = PdfxiApp::new();
        app.open_document(&fixture());
        assert!(
            app.state.active_tab_id().is_some(),
            "the Excel fixture opens"
        );
        app
    }

    fn starts_with(bytes: &[u8], signature: &[u8]) -> bool {
        bytes.starts_with(signature)
    }

    #[test]
    fn every_export_choice_writes_real_bytes() {
        for choice in ExportChoice::ALL {
            let mut app = app_with_excel();
            app.handle_export_command();
            assert!(app.export_card_open());
            app.export_ui.format = choice;
            // Small raster pages keep the image tests fast; PNG/JPEG honour DPI.
            app.export_ui.dpi = 72;

            let dir = export_dir(choice.extension());
            let chosen = dir.join(format!("document.{}", choice.extension()));
            assert!(
                choice.to_format().is_implemented(),
                "{choice:?} must be implemented when the app is built with pdfium"
            );
            app.export_to_path(&chosen)
                .unwrap_or_else(|error| panic!("{choice:?} export failed: {error}"));
            assert!(
                app.status().starts_with("Exported"),
                "{choice:?} status: {}",
                app.status()
            );

            let bytes = match choice {
                ExportChoice::Png | ExportChoice::Jpeg => {
                    std::fs::read(dir.join(format!("document-page-1.{}", choice.extension())))
                        .unwrap_or_else(|error| panic!("{choice:?} part missing: {error}"))
                }
                _ => std::fs::read(&chosen)
                    .unwrap_or_else(|error| panic!("{choice:?} output missing: {error}")),
            };

            match choice {
                ExportChoice::Text => {
                    let text = String::from_utf8_lossy(&bytes);
                    assert!(
                        text.contains("PDFXI"),
                        "text export lacks the fixture marker: {text:?}"
                    );
                }
                ExportChoice::Docx | ExportChoice::Xlsx | ExportChoice::Pptx => {
                    assert!(
                        starts_with(&bytes, b"PK"),
                        "{choice:?} is not a ZIP package"
                    );
                }
                ExportChoice::Png => {
                    assert!(
                        starts_with(&bytes, &[0x89, b'P', b'N', b'G']),
                        "PNG signature"
                    );
                }
                ExportChoice::Jpeg => assert!(starts_with(&bytes, &[0xFF, 0xD8]), "JPEG SOI"),
                ExportChoice::Html => {
                    let html = String::from_utf8_lossy(&bytes).to_lowercase();
                    assert!(html.contains("<html"), "HTML root element");
                    assert!(
                        html.contains("microsoft-excel-2019-sample"),
                        "HTML carries the document title"
                    );
                }
                ExportChoice::Rtf => {
                    assert!(starts_with(&bytes, b"{\\rtf"), "RTF header");
                }
            }
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    #[test]
    fn export_without_a_document_reports_it_and_does_not_open_the_card() {
        let mut app = PdfxiApp::new();
        app.handle_export_command();
        assert_eq!(app.status(), "Open a document to export");
        assert!(!app.export_card_open());
        let error = app
            .export_document_to(Path::new("unused.txt"))
            .expect_err("no document");
        assert_eq!(error, "Open a document to export");
    }

    #[test]
    fn a_missing_parent_directory_is_created() {
        let mut app = app_with_excel();
        app.export_ui.format = ExportChoice::Text;
        let dir = export_dir("nested");
        let chosen = dir.join("deep").join("nested").join("doc.txt");
        app.export_to_path(&chosen).expect("nested export");
        assert!(chosen.is_file());
        assert!(
            app.status().contains("2 page(s)"),
            "status reports the page count: {}",
            app.status()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn page_images_are_written_beside_the_chosen_path() {
        assert!(
            ExportFormat::Png.is_implemented(),
            "PNG must be implemented with the pdfium feature"
        );
        let mut app = app_with_excel();
        app.export_ui.format = ExportChoice::Png;
        app.export_ui.dpi = 72;
        let dir = export_dir("parts");
        let chosen = dir.join("doc.png");
        app.export_to_path(&chosen).expect("multi-part export");
        assert!(dir.join("doc-page-1.png").is_file());
        assert!(dir.join("doc-page-2.png").is_file());
        assert!(
            !chosen.is_file(),
            "multi-part output does not use the chosen path"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
