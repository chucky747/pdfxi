//! Convert to PDF card (A-28): build a PDF from text, Markdown, HTML and
//! PNG/JPEG sources.
//!
//! Native sources only. Office input needs an extractor this build does not
//! have, so `.docx`/`.xlsx`/`.pptx` are not offered and a typed unsupported
//! error is reported if one is loaded anyway.
//!
//! Sources are read and classified when they are added, so the list the user
//! sees is exactly what `convert_to_path` sends to the converter. Conversion is
//! synchronous on the UI thread and page size is fixed to Letter.

use std::path::{Path, PathBuf};

use pdfxi_convert::{
    ConvertInput, ConvertOptions, ConvertSource, CreateImageFormat, convert_to_pdf,
};

use crate::ui::PdfxiApp;

/// File extensions the native converter accepts.
pub(crate) const CONVERT_EXTENSIONS: [&str; 7] = ["png", "jpg", "jpeg", "txt", "md", "html", "htm"];

/// State of the Convert to PDF card.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ConvertUiState {
    /// Whether the window is visible.
    pub(crate) open: bool,
    /// Loaded sources in conversion order.
    pub(crate) sources: Vec<ConvertSource>,
    /// Display name for each source, parallel to `sources`.
    pub(crate) source_names: Vec<String>,
    /// Chosen PDF output path.
    pub(crate) output: Option<PathBuf>,
    /// Letter page size; this build always converts to Letter.
    pub(crate) page_size_letter: bool,
    /// Optional PDF title.
    pub(crate) title: String,
    /// Typed conversion or read error.
    pub(crate) error: Option<String>,
    /// Progress note while the converter runs.
    pub(crate) progress: Option<String>,
}

impl Default for ConvertUiState {
    fn default() -> Self {
        Self {
            open: false,
            sources: Vec::new(),
            source_names: Vec::new(),
            output: None,
            page_size_letter: true,
            title: String::new(),
            error: None,
            progress: None,
        }
    }
}

impl PdfxiApp {
    /// Handle [`AppCommand::ConvertToPdf`]: open the card. No document is
    /// required because conversion creates one.
    pub(crate) fn handle_convert_command(&mut self) {
        self.convert.open = true;
        self.convert.error = None;
        self.status = "Convert to PDF: choose files, then Convert".to_owned();
    }

    /// Whether the Convert to PDF card is open (harness accessor).
    #[must_use]
    pub fn convert_card_open(&self) -> bool {
        self.convert.open
    }

    /// Draw the Convert to PDF window.
    pub(crate) fn convert_card_ui(&mut self, ctx: &egui::Context) {
        if !self.convert.open {
            return;
        }
        let mut open = true;
        let mut add_files = false;
        let mut choose_output = false;
        let mut convert = false;
        let mut remove = None;

        egui::Window::new("Convert to PDF")
            .open(&mut open)
            .resizable(true)
            .default_width(400.0)
            .show(ctx, |ui| {
                ui.heading("Convert to PDF");
                ui.label(
                    egui::RichText::new(
                        "Text, Markdown, HTML, PNG and JPEG. Office files need an \
                         extractor this build does not have.",
                    )
                    .size(10.0)
                    .weak(),
                );
                if ui.button("Add files").clicked() {
                    add_files = true;
                }
                if self.convert.source_names.is_empty() {
                    ui.label("No files chosen");
                }
                for (index, name) in self.convert.source_names.iter().enumerate() {
                    ui.horizontal(|ui| {
                        ui.label(name);
                        if ui.button("Remove").clicked() {
                            remove = Some(index);
                        }
                    });
                }
                ui.separator();
                let output = self.convert.output.as_ref().map_or_else(
                    || "No output chosen".to_owned(),
                    |path| path.display().to_string(),
                );
                ui.label(format!("Output: {output}"));
                if ui.button("Choose output").clicked() {
                    choose_output = true;
                }
                ui.horizontal(|ui| {
                    ui.label("Title");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.convert.title)
                            .hint_text("Optional PDF title")
                            .desired_width(240.0),
                    );
                });
                ui.label("Page size: Letter");
                if let Some(progress) = &self.convert.progress {
                    ui.label(progress);
                }
                if let Some(error) = &self.convert.error {
                    ui.colored_label(ui.visuals().error_fg_color, error);
                }
                ui.separator();
                if ui
                    .add_enabled(
                        !self.convert.sources.is_empty() && self.convert.output.is_some(),
                        egui::Button::new("Convert"),
                    )
                    .on_disabled_hover_text("Add at least one file and choose an output")
                    .clicked()
                {
                    convert = true;
                }
            });

        if add_files {
            self.choose_convert_sources();
        }
        if choose_output {
            self.choose_convert_output();
        }
        if let Some(index) = remove {
            self.remove_convert_source(index);
        }
        if convert && let Some(output) = self.convert.output.clone() {
            let _ = self.convert_to_path(&output);
        }
        if !open {
            self.convert.open = false;
        }
    }

    /// Show the native multi-file picker and add the chosen sources.
    pub(crate) fn choose_convert_sources(&mut self) {
        let Some(paths) = rfd::FileDialog::new()
            .set_title("Add files to convert")
            .add_filter("Documents", &CONVERT_EXTENSIONS)
            .pick_files()
        else {
            return;
        };
        let _ = self.add_convert_paths(&paths);
    }

    /// Show the native save dialog for the PDF output.
    pub(crate) fn choose_convert_output(&mut self) {
        let suggested = self
            .convert
            .output
            .clone()
            .or_else(|| {
                self.convert
                    .source_names
                    .first()
                    .map(|name| PathBuf::from(format!("{name}.pdf")))
            })
            .unwrap_or_else(|| PathBuf::from("converted.pdf"));
        let file_name = suggested.file_name().map_or_else(
            || "converted.pdf".to_owned(),
            |name| name.to_string_lossy().into_owned(),
        );
        if let Some(path) = rfd::FileDialog::new()
            .set_title("Save the converted PDF")
            .add_filter("PDF documents", &["pdf"])
            .set_file_name(file_name)
            .save_file()
        {
            self.convert.output = Some(path);
        }
    }

    /// Read and append `paths` as converter sources.
    ///
    /// The first chosen file also suggests `<stem>.pdf` next to it when no
    /// output has been chosen yet.
    pub(crate) fn add_convert_paths(&mut self, paths: &[PathBuf]) -> Result<(), String> {
        let sources = load_sources(paths)?;
        for (source, path) in sources.into_iter().zip(paths) {
            self.convert.sources.push(source);
            self.convert.source_names.push(path.file_name().map_or_else(
                || path.display().to_string(),
                |name| name.to_string_lossy().into_owned(),
            ));
        }
        if self.convert.output.is_none()
            && let Some(first) = paths.first()
        {
            self.convert.output = Some(first.with_extension("pdf"));
        }
        self.convert.error = None;
        Ok(())
    }

    /// Remove the source at `index`, keeping names and sources in step.
    pub(crate) fn remove_convert_source(&mut self, index: usize) {
        if index < self.convert.sources.len() {
            self.convert.sources.remove(index);
        }
        if index < self.convert.source_names.len() {
            self.convert.source_names.remove(index);
        }
    }

    /// Convert the loaded sources and write the PDF to `output`.
    ///
    /// Returns the output page count. Empty input and a missing output are
    /// typed errors; converter failures keep the source list intact.
    pub(crate) fn convert_to_path(&mut self, output: &Path) -> Result<usize, String> {
        let result = self.write_converted(output);
        match result {
            Ok(pages) => {
                self.convert.error = None;
                self.convert.progress = None;
                self.convert.output = Some(output.to_path_buf());
                Ok(pages)
            }
            Err(error) => {
                self.convert.error = Some(error.clone());
                self.convert.progress = None;
                self.status = format!("Convert failed: {error}");
                Err(error)
            }
        }
    }

    /// Run the converter and write the file.
    fn write_converted(&mut self, output: &Path) -> Result<usize, String> {
        if self.convert.sources.is_empty() {
            return Err("Add at least one file to convert".to_owned());
        }
        let mut options = ConvertOptions::new();
        if !self.convert.title.trim().is_empty() {
            options = options.with_title(self.convert.title.trim());
        }
        self.convert.progress = Some("Converting...".to_owned());
        let converted = convert_to_pdf(
            ConvertInput::from_sources(self.convert.sources.clone()),
            options,
        )
        .map_err(|error| error.to_string())?;
        if let Some(parent) = output
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("Could not create {}: {error}", parent.display()))?;
        }
        converted
            .write_to(output)
            .map_err(|error| format!("Could not write {}: {error}", output.display()))?;
        let pages = converted.page_count();
        let count = self.convert.sources.len();
        self.status = format!(
            "Converted {count} file(s) to a PDF with {pages} page(s) at {}",
            output.display()
        );
        self.notify_real_event("PDF converted", &output.display().to_string());
        Ok(pages)
    }
}

/// Read `paths` into converter sources, classifying by extension.
///
/// HTML keeps its directory as the base for relative references. Unsupported
/// extensions (including Office archives) are rejected with a typed message.
pub(crate) fn load_sources(paths: &[PathBuf]) -> Result<Vec<ConvertSource>, String> {
    let mut sources = Vec::with_capacity(paths.len());
    for path in paths {
        let name = path.file_name().map_or_else(
            || path.display().to_string(),
            |name| name.to_string_lossy().into_owned(),
        );
        let extension = path.extension().map_or_else(String::new, |ext| {
            ext.to_string_lossy().to_ascii_lowercase()
        });
        let bytes = std::fs::read(path)
            .map_err(|error| format!("Could not read {}: {error}", path.display()))?;
        let source = match extension.as_str() {
            "txt" => {
                let text =
                    String::from_utf8(bytes).map_err(|_| format!("{name} is not valid UTF-8"))?;
                ConvertSource::text(text).with_name(name)
            }
            "md" => {
                let text =
                    String::from_utf8(bytes).map_err(|_| format!("{name} is not valid UTF-8"))?;
                ConvertSource::markdown(text).with_name(name)
            }
            "html" | "htm" => {
                let markup =
                    String::from_utf8(bytes).map_err(|_| format!("{name} is not valid UTF-8"))?;
                ConvertSource::html(markup, path.parent().map(Path::to_path_buf)).with_name(name)
            }
            "png" => ConvertSource::image(bytes, CreateImageFormat::Png).with_name(name),
            "jpg" | "jpeg" => ConvertSource::image(bytes, CreateImageFormat::Jpeg).with_name(name),
            "" => {
                return Err(format!(
                    "{name} has no extension; supported: {}",
                    CONVERT_EXTENSIONS.join(", ")
                ));
            }
            other => {
                return Err(format!(
                    "Unsupported convert-to-PDF source format '.{other}' for {name} \
                     (supported: {}; Office input needs an extractor this build does not have)",
                    CONVERT_EXTENSIONS.join(", ")
                ));
            }
        };
        sources.push(source);
    }
    Ok(sources)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// A 2x2 RGB PNG without an alpha channel. `encode_png` emits RGBA, which
    /// the converter's image path rejects, so the test embeds a known RGB PNG.
    const TINY_RGB_PNG: &[u8] = &[
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x02, 0x08, 0x02, 0x00, 0x00, 0x00, 0xFD,
        0xD4, 0x9A, 0x73, 0x00, 0x00, 0x00, 0x01, 0x73, 0x52, 0x47, 0x42, 0x00, 0xAE, 0xCE, 0x1C,
        0xE9, 0x00, 0x00, 0x00, 0x04, 0x67, 0x41, 0x4D, 0x41, 0x00, 0x00, 0xB1, 0x8F, 0x0B, 0xFC,
        0x61, 0x05, 0x00, 0x00, 0x00, 0x09, 0x70, 0x48, 0x59, 0x73, 0x00, 0x00, 0x0E, 0xC3, 0x00,
        0x00, 0x0E, 0xC3, 0x01, 0xC7, 0x6F, 0xA8, 0x64, 0x00, 0x00, 0x00, 0x11, 0x49, 0x44, 0x41,
        0x54, 0x18, 0x57, 0x63, 0xF8, 0xCF, 0xC0, 0xF0, 0x9F, 0x01, 0x8C, 0x81, 0x14, 0x00, 0x1F,
        0xEE, 0x03, 0xFD, 0x89, 0xED, 0x1A, 0xA7, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44,
        0xAE, 0x42, 0x60, 0x82,
    ];

    fn temp_dir(tag: &str) -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/tmp/convert-card")
            .join(format!("{}-{unique}-{tag}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create the convert directory");
        dir
    }

    fn write_sources(dir: &Path) -> Vec<PathBuf> {
        let text = dir.join("note.txt");
        std::fs::write(&text, "PDFXI convert marker\nSecond line").expect("write text");
        let markdown = dir.join("note.md");
        std::fs::write(&markdown, "# Heading\n\nMarkdown body").expect("write markdown");
        let html = dir.join("page.html");
        std::fs::write(&html, "<html><body><h1>Converted page</h1></body></html>")
            .expect("write html");
        let png = dir.join("pixel.png");
        std::fs::write(&png, TINY_RGB_PNG).expect("write png");
        vec![text, markdown, html, png]
    }

    #[test]
    fn loads_every_native_source_and_classifies_by_extension() {
        let dir = temp_dir("load");
        let paths = write_sources(&dir);
        let sources = load_sources(&paths).expect("load the sources");
        assert_eq!(sources.len(), 4);
        let kinds: Vec<&str> = sources
            .iter()
            .map(|source| match source {
                ConvertSource::Text { .. } => "text",
                ConvertSource::Markdown { .. } => "markdown",
                ConvertSource::Html { .. } => "html",
                ConvertSource::Image { .. } => "image",
            })
            .collect();
        assert_eq!(kinds, vec!["text", "markdown", "html", "image"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn converts_all_sources_to_a_pdf_with_the_text_marker() {
        let dir = temp_dir("convert");
        let paths = write_sources(&dir);
        let output = dir.join("converted.pdf");

        let mut app = PdfxiApp::new();
        app.handle_convert_command();
        assert!(app.convert_card_open());
        app.add_convert_paths(&paths).expect("add the sources");
        assert_eq!(app.convert.sources.len(), 4);
        assert_eq!(app.convert.source_names.len(), 4);
        assert_eq!(app.convert.output, Some(paths[0].with_extension("pdf")));

        let pages = app.convert_to_path(&output).expect("convert");
        assert!(pages >= 4, "one page per source at minimum, got {pages}");
        assert!(app.status().starts_with("Converted 4 file(s)"));
        assert!(app.convert.error.is_none());

        let bytes = std::fs::read(&output).expect("read the converted PDF");
        let document = pdfxi_core::Document::from_bytes(bytes).expect("reopen the PDF");
        assert_eq!(document.page_count().expect("page count"), pages);
        let text = document.page_text(0).expect("extract page text");
        assert!(
            text.contains("PDFXI convert marker"),
            "page text lost the source marker: {text:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn empty_input_and_bad_extensions_are_typed_errors() {
        let dir = temp_dir("errors");
        let output = dir.join("empty.pdf");
        let mut app = PdfxiApp::new();
        let error = app
            .convert_to_path(&output)
            .expect_err("empty input must fail");
        assert!(error.contains("Add at least one file"));
        assert!(!output.exists());

        let bad = dir.join("document.docx");
        std::fs::write(&bad, b"PK\x03\x04not really office").expect("write a docx");
        let error = load_sources(&[bad]).expect_err("Office input must be rejected");
        assert!(
            error.contains("Unsupported") && error.contains("docx"),
            "{error}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn removing_a_source_keeps_names_in_step() {
        let dir = temp_dir("remove");
        let paths = write_sources(&dir);
        let mut app = PdfxiApp::new();
        app.add_convert_paths(&paths).expect("add");
        app.remove_convert_source(1);
        assert_eq!(app.convert.sources.len(), 3);
        assert_eq!(app.convert.source_names.len(), 3);
        assert_eq!(app.convert.source_names[1], "page.html");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
