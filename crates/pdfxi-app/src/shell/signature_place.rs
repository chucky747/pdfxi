//! Fill & Sign signature placement (A-25): typed name, freehand and image.
//!
//! Each mode places an annotation or page content on the active page and
//! commits it as one `DocumentSession::edit`, so it is undoable, saved and
//! reopened. This is Fill & Sign placement, not a certificate signature.

use std::path::PathBuf;

use pdfxi_edit::{ImageFormat, Point};

use super::capabilities::Capability;
use crate::TabId;
use crate::ui::{AppCommand, PdfxiApp};

/// How the Place signature card builds the signature.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum PlaceMode {
    /// A typed name stamp.
    #[default]
    Typed,
    /// A freehand ink signature.
    Drawn,
    /// An image signature.
    Image,
}

/// State of the Place signature card.
#[derive(Default)]
pub(crate) struct SignaturePlaceState {
    /// Whether the card is visible.
    pub(crate) open: bool,
    /// Tab the card was opened for.
    pub(crate) tab: Option<TabId>,
    /// Selected placement mode.
    pub(crate) mode: PlaceMode,
    /// Typed signer name.
    pub(crate) typed: String,
    /// Normalized freehand points (0..1 in the pad).
    pub(crate) draw_points: Vec<(f32, f32)>,
    /// Chosen image file.
    pub(crate) image_path: Option<PathBuf>,
    /// Typed placement error.
    pub(crate) error: Option<String>,
}

impl PdfxiApp {
    /// Handle [`AppCommand::PlaceSignature`]: closed opens the card, open
    /// places the selected mode.
    pub(crate) fn handle_place_signature_command(&mut self) {
        if self.signature_place.open {
            match self.signature_place.mode {
                PlaceMode::Typed => self.place_typed_signature(),
                PlaceMode::Drawn => self.place_drawn_signature(),
                PlaceMode::Image => self.place_image_signature(),
            }
        } else {
            self.open_place_signature_dialog();
        }
    }

    /// Open the Place signature card for the active writable document.
    pub(crate) fn open_place_signature_dialog(&mut self) {
        if !self.capabilities.contains(Capability::Signing) {
            self.status = "Signature placement is not enabled in this build".to_owned();
            return;
        }
        let Some(id) = self.state.active_tab_id() else {
            self.status = "Open a document before placing a signature".to_owned();
            return;
        };
        if self.read_only_tabs.contains(&id) {
            self.status = "The document permissions do not allow changes".to_owned();
            return;
        }
        self.signature_place.open = true;
        self.signature_place.tab = Some(id);
        self.signature_place.mode = PlaceMode::Typed;
        self.signature_place.error = None;
        self.signature_place.draw_points.clear();
        self.signature_place.image_path = None;
        if self.signature_place.typed.trim().is_empty() {
            self.signature_place.typed = self.settings.display_name.clone();
        }
        self.status = "Place signature: choose Type, Draw or Image".to_owned();
    }

    /// Close the card without changing the document.
    pub(crate) fn close_place_signature_dialog(&mut self) {
        self.signature_place.open = false;
    }

    /// Shared writable target: tab, page, rect and a document clone.
    fn placement_target(
        &self,
    ) -> Result<(TabId, usize, pdfxi_edit::Rect, pdfxi_core::Document), String> {
        let Some(id) = self.state.active_tab_id() else {
            return Err("Open a document first".to_owned());
        };
        if self.signature_place.tab != Some(id) {
            return Err("The signature card belongs to another tab".to_owned());
        }
        if self.read_only_tabs.contains(&id) {
            return Err("The document permissions do not allow changes".to_owned());
        }
        let session = self
            .sessions
            .get(&id)
            .ok_or_else(|| "The document has no open session".to_owned())?;
        let page = self.state.active_tab().map_or(0, |tab| tab.current_page());
        let media_box = session
            .document()
            .page(page)
            .map_err(|error| error.to_string())?
            .media_box;
        let rect = pdfxi_edit::Rect::new(
            (media_box.right - 36.0 - 220.0).max(media_box.left) as f32,
            (media_box.bottom + 36.0) as f32,
            (media_box.right - 36.0) as f32,
            (media_box.bottom + 36.0 + 56.0) as f32,
        )
        .map_err(|error| error.to_string())?;
        Ok((id, page, rect, session.document().clone()))
    }

    /// Commit a placed signature as one undoable session edit.
    fn commit_placement(
        &mut self,
        id: TabId,
        candidate: pdfxi_core::Document,
    ) -> Result<(), String> {
        self.sessions
            .get_mut(&id)
            .ok_or_else(|| "The document has no open session".to_owned())?
            .edit(|document| {
                *document = candidate;
                Ok(())
            })
            .map_err(|error| error.to_string())?;
        self.after_session_edit(id);
        Ok(())
    }

    /// Place the typed name as an `Approved` stamp on the active page.
    pub(crate) fn place_typed_signature(&mut self) {
        match self.place_typed_signature_impl() {
            Ok(page) => {
                self.signature_place.open = false;
                self.signature_place.error = None;
                self.status = format!("Placed typed signature on page {}", page + 1);
            }
            Err(error) => self.signature_place.error = Some(error),
        }
    }

    fn place_typed_signature_impl(&mut self) -> Result<usize, String> {
        let text = self.signature_place.typed.trim().to_owned();
        if text.is_empty() {
            return Err("Type a signature name first".to_owned());
        }
        let author = self.settings.display_name.clone();
        let (id, page, rect, mut candidate) = self.placement_target()?;
        // "Approved" is a standard /Name; the typed text is the visible label.
        pdfxi_edit::add_stamp(&mut candidate, page, rect, "Approved", &text, &author)
            .map_err(|error| error.to_string())?;
        self.commit_placement(id, candidate)?;
        Ok(page)
    }

    /// Place the freehand pad as an `/Ink` annotation on the active page.
    pub(crate) fn place_drawn_signature(&mut self) {
        match self.place_drawn_signature_impl() {
            Ok(page) => {
                self.signature_place.open = false;
                self.signature_place.error = None;
                self.signature_place.draw_points.clear();
                self.status = format!("Placed drawn signature on page {}", page + 1);
            }
            Err(error) => self.signature_place.error = Some(error),
        }
    }

    fn place_drawn_signature_impl(&mut self) -> Result<usize, String> {
        if self.signature_place.draw_points.len() < 2 {
            return Err("Draw a signature first".to_owned());
        }
        let author = self.settings.display_name.clone();
        let (id, page, rect, mut candidate) = self.placement_target()?;
        let points: Vec<Point> = self
            .signature_place
            .draw_points
            .iter()
            .map(|(nx, ny)| {
                Point::new(
                    rect.x0() + nx * rect.width(),
                    rect.y0() + ny * rect.height(),
                )
            })
            .collect();
        pdfxi_edit::add_ink(
            &mut candidate,
            page,
            &[points],
            1.5_f32,
            "Signature",
            &author,
        )
        .map_err(|error| error.to_string())?;
        self.commit_placement(id, candidate)?;
        Ok(page)
    }

    /// Choose a PNG or JPEG image for the Image mode.
    pub(crate) fn choose_signature_image(&mut self) {
        if let Some(path) = rfd::FileDialog::new()
            .add_filter("Image", &["png", "jpg", "jpeg"])
            .pick_file()
        {
            self.signature_place.image_path = Some(path);
            self.signature_place.error = None;
        }
    }

    /// Place the chosen image as page content on the active page.
    pub(crate) fn place_image_signature(&mut self) {
        match self.place_image_signature_impl() {
            Ok(page) => {
                self.signature_place.open = false;
                self.signature_place.error = None;
                self.status = format!("Placed image signature on page {}", page + 1);
            }
            Err(error) => self.signature_place.error = Some(error),
        }
    }

    fn place_image_signature_impl(&mut self) -> Result<usize, String> {
        let path = self
            .signature_place
            .image_path
            .clone()
            .ok_or_else(|| "Choose an image first".to_owned())?;
        let bytes = std::fs::read(&path).map_err(|error| error.to_string())?;
        let format = if bytes.starts_with(b"\x89PNG") {
            ImageFormat::Png
        } else if bytes.starts_with(&[0xFF, 0xD8]) {
            ImageFormat::Jpeg
        } else {
            return Err("Choose a PNG or JPEG image".to_owned());
        };
        let (id, page, rect, mut candidate) = self.placement_target()?;
        pdfxi_edit::add_image(&mut candidate, page, rect, &bytes, format)
            .map_err(|error| error.to_string())?;
        self.commit_placement(id, candidate)?;
        Ok(page)
    }

    /// Draw the Place signature card.
    pub(crate) fn place_signature_dialog_ui(&mut self, ctx: &egui::Context) {
        if !self.signature_place.open {
            return;
        }
        let mut open = true;
        let mut place = false;
        let mut choose_image = false;
        egui::Window::new("Place signature")
            .open(&mut open)
            .resizable(false)
            .default_width(340.0)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut self.signature_place.mode, PlaceMode::Typed, "Type");
                    ui.selectable_value(&mut self.signature_place.mode, PlaceMode::Drawn, "Draw");
                    ui.selectable_value(&mut self.signature_place.mode, PlaceMode::Image, "Image");
                });
                ui.separator();
                match self.signature_place.mode {
                    PlaceMode::Typed => {
                        ui.label("Signature name");
                        ui.add(
                            egui::TextEdit::singleline(&mut self.signature_place.typed)
                                .hint_text("Type your name")
                                .desired_width(260.0),
                        );
                        if ui
                            .add_enabled(
                                !self.signature_place.typed.trim().is_empty(),
                                egui::Button::new("Place typed signature"),
                            )
                            .clicked()
                        {
                            place = true;
                        }
                    }
                    PlaceMode::Drawn => {
                        ui.label("Draw your signature in the box");
                        let (response, painter) =
                            ui.allocate_painter(egui::vec2(260.0, 80.0), egui::Sense::drag());
                        let pad = response.rect;
                        painter.rect_filled(pad, 4.0, ui.visuals().extreme_bg_color);
                        if response.dragged()
                            && let Some(pos) = response.interact_pointer_pos()
                        {
                            let nx = ((pos.x - pad.left()) / pad.width()).clamp(0.0, 1.0);
                            let ny = ((pos.y - pad.top()) / pad.height()).clamp(0.0, 1.0);
                            let last = self.signature_place.draw_points.last().copied();
                            if last.is_none_or(|(lx, ly)| (lx - nx).abs() + (ly - ny).abs() > 0.002)
                            {
                                self.signature_place.draw_points.push((nx, ny));
                            }
                        }
                        if self.signature_place.draw_points.len() >= 2 {
                            let points: Vec<egui::Pos2> = self
                                .signature_place
                                .draw_points
                                .iter()
                                .map(|(x, y)| {
                                    egui::pos2(
                                        pad.left() + x * pad.width(),
                                        pad.top() + y * pad.height(),
                                    )
                                })
                                .collect();
                            painter.add(egui::Shape::line(
                                points,
                                egui::Stroke::new(1.5_f32, ui.visuals().text_color()),
                            ));
                        }
                        if ui
                            .add_enabled(
                                self.signature_place.draw_points.len() >= 2,
                                egui::Button::new("Place drawn signature"),
                            )
                            .clicked()
                        {
                            place = true;
                        }
                    }
                    PlaceMode::Image => {
                        if ui.button("Choose image...").clicked() {
                            choose_image = true;
                        }
                        if let Some(path) = &self.signature_place.image_path {
                            ui.label(path.file_name().map_or_else(
                                || path.display().to_string(),
                                |name| name.to_string_lossy().into_owned(),
                            ));
                        }
                        if ui
                            .add_enabled(
                                self.signature_place.image_path.is_some(),
                                egui::Button::new("Place image signature"),
                            )
                            .clicked()
                        {
                            place = true;
                        }
                    }
                }
                if let Some(error) = &self.signature_place.error {
                    ui.colored_label(ui.visuals().error_fg_color, error);
                }
            });
        if !open {
            self.close_place_signature_dialog();
        }
        if choose_image {
            self.choose_signature_image();
        }
        if place {
            self.dispatch(AppCommand::PlaceSignature, Some(ctx));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pdfxi_document::DocumentSession;
    use pdfxi_edit::{AnnotationKind, list_annotations};

    fn excel_fixture() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/realworld/microsoft-excel-2019-sample.pdf")
    }

    fn temp_dir() -> PathBuf {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/tmp/place");
        std::fs::create_dir_all(&dir).expect("create the temp directory");
        dir
    }

    #[test]
    fn typed_signature_stamp_persists_through_save_and_reopen() {
        let mut app = PdfxiApp::new();
        app.open_document(&excel_fixture());
        let id = app.state.active_tab_id().expect("the fixture opens");
        app.dispatch(AppCommand::PlaceSignature, None);
        assert!(app.signature_place.open);
        app.signature_place.typed = "Alex Signer".to_owned();
        app.dispatch(AppCommand::PlaceSignature, None);
        assert!(!app.signature_place.open, "placing closes the card");
        assert!(app.sessions.get(&id).expect("session").is_dirty());

        let annotations = list_annotations(app.sessions.get(&id).expect("session").document(), 0)
            .expect("annotations read");
        let stamp = annotations
            .iter()
            .find(|annotation| annotation.kind == Some(AnnotationKind::Stamp))
            .expect("a stamp annotation was added");
        assert_eq!(stamp.contents, "Alex Signer");
        assert_eq!(stamp.stamp_name.as_deref(), Some("Approved"));

        let target = temp_dir().join(format!("typed-{}.pdf", std::process::id()));
        app.sessions
            .get_mut(&id)
            .expect("session")
            .save_as(&target)
            .expect("save");
        let reopened = DocumentSession::open(&target).expect("reopen");
        assert!(
            list_annotations(reopened.document(), 0)
                .expect("annotations")
                .iter()
                .any(|annotation| annotation.kind == Some(AnnotationKind::Stamp)
                    && annotation.contents == "Alex Signer")
        );
        let _ = std::fs::remove_file(&target);
    }

    #[test]
    fn drawn_signature_adds_ink_and_persists() {
        let mut app = PdfxiApp::new();
        app.open_document(&excel_fixture());
        let id = app.state.active_tab_id().expect("the fixture opens");
        app.dispatch(AppCommand::PlaceSignature, None);
        app.signature_place.mode = PlaceMode::Drawn;
        app.signature_place.draw_points = vec![(0.1, 0.5), (0.5, 0.2), (0.9, 0.6)];
        app.dispatch(AppCommand::PlaceSignature, None);
        assert!(!app.signature_place.open);
        assert!(app.sessions.get(&id).expect("session").is_dirty());
        assert!(
            list_annotations(app.sessions.get(&id).expect("session").document(), 0)
                .expect("annotations")
                .iter()
                .any(|annotation| annotation.kind == Some(AnnotationKind::Pencil))
        );

        let target = temp_dir().join(format!("drawn-{}.pdf", std::process::id()));
        app.sessions
            .get_mut(&id)
            .expect("session")
            .save_as(&target)
            .expect("save");
        let reopened = DocumentSession::open(&target).expect("reopen");
        assert!(
            list_annotations(reopened.document(), 0)
                .expect("annotations")
                .iter()
                .any(|annotation| annotation.kind == Some(AnnotationKind::Pencil))
        );
        let _ = std::fs::remove_file(&target);
    }

    #[test]
    fn image_signature_adds_page_content_and_persists() {
        // Valid 1x1 RGBA PNG.
        const TINY_PNG: &[u8] = &[
            0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48,
            0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00,
            0x00, 0x1F, 0x15, 0xC4, 0x89, 0x00, 0x00, 0x00, 0x0A, 0x49, 0x44, 0x41, 0x54, 0x78,
            0x9C, 0x63, 0x00, 0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00,
            0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
        ];

        let mut app = PdfxiApp::new();
        app.open_document(&excel_fixture());
        let id = app.state.active_tab_id().expect("the fixture opens");
        let image = temp_dir().join("signature.png");
        std::fs::write(&image, TINY_PNG).expect("write the png");

        app.dispatch(AppCommand::PlaceSignature, None);
        app.signature_place.mode = PlaceMode::Image;
        app.signature_place.image_path = Some(image);
        app.dispatch(AppCommand::PlaceSignature, None);
        assert!(!app.signature_place.open);
        assert!(
            app.signature_place.error.is_none(),
            "image placement failed: {:?}",
            app.signature_place.error
        );
        assert!(app.sessions.get(&id).expect("session").is_dirty());

        let target = temp_dir().join(format!("image-{}.pdf", std::process::id()));
        app.sessions
            .get_mut(&id)
            .expect("session")
            .save_as(&target)
            .expect("save");
        let reopened = DocumentSession::open(&target).expect("reopen");
        let content = reopened.document().page_content(0).expect("page content");
        assert!(
            String::from_utf8_lossy(&content).contains("Do"),
            "the image paint operator is present"
        );
        let _ = std::fs::remove_file(&target);
    }
}
