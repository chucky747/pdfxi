//! Certificate signing card and signature verification panel.
//!
//! The card is capability-gated by [`Capability::Signing`] and opens only for an
//! open, writable document. Loading a PKCS#12 identity, drawing the typed
//! appearance and the session signature all run on the UI thread: the measured
//! signing call is about 0.13 s, so there is no progress window to fake and no
//! Stop button to leave dead.
//!
//! The panel never parses PDF or certificate bytes itself: it calls the
//! `pdfxi-security`, `pdfxi-edit` and `pdfxi-document` entry points and shows
//! their typed results.

use std::path::PathBuf;

use pdfxi_edit::{Point, add_text_note};
use pdfxi_forms::{FormFieldKind, read_acroform};
use pdfxi_security::{
    SignOptions, SignatureStatus, SignatureVerification, SigningIdentity, verify_signature,
};

use super::capabilities::Capability;
use crate::ui::{AppCommand, PdfxiApp};

/// State of the Sign card and the last verification readout.
#[derive(Default)]
pub(crate) struct SigningUiState {
    /// Whether the card is visible.
    pub(crate) open: bool,
    /// Tab the card was opened for.
    pub(crate) tab: Option<crate::TabId>,
    /// Chosen PKCS#12 container.
    pub(crate) pfx_path: Option<PathBuf>,
    /// Masked container password.
    ///
    /// The value is cleared as soon as the identity is imported, so the panel
    /// never keeps the secret longer than the import call.
    pub(crate) password: String,
    /// Imported identity. Held in memory only; never written to disk.
    pub(crate) identity: Option<SigningIdentity>,
    /// Signer subject after a successful identity load.
    pub(crate) subject: Option<String>,
    /// Number of certificates in the imported chain.
    pub(crate) chain_len: usize,
    /// Typed identity-load error.
    pub(crate) identity_error: Option<String>,
    /// Optional signature reason.
    pub(crate) reason: String,
    /// Optional signature location.
    pub(crate) location: String,
    /// Zero-based page that receives the signature.
    pub(crate) page: usize,
    /// Typed appearance text drawn as a stamp on the active page.
    pub(crate) appearance: String,
    /// Signatures written through this card, used for unique field names.
    pub(crate) signatures: usize,
    /// Progress message while an identity or signing worker runs.
    pub(crate) progress: Option<String>,
    /// Typed signing error.
    pub(crate) error: Option<String>,
    /// Last verification result for [`Self::verification_tab`].
    pub(crate) verification: Option<SignatureVerification>,
    /// Tab the verification result belongs to.
    pub(crate) verification_tab: Option<crate::TabId>,
    /// Typed verification error.
    pub(crate) verification_error: Option<String>,
}

/// One signature request assembled from the card's fields.
struct SignatureRequest<'a> {
    /// Zero-based page that receives the signature.
    page: usize,
    /// Typed appearance text drawn as a note.
    appearance: &'a str,
    /// Comment author for the appearance note.
    author: &'a str,
    /// Non-empty `/Reason` value.
    reason: &'a str,
    /// Non-empty `/Location` value.
    location: &'a str,
    /// Unique AcroForm signature field name.
    field_name: &'a str,
    /// Unique `/NM` identifier for the appearance note.
    note_id: &'a str,
}

/// Which step of the signing flow failed.
enum SignFailure {
    /// The appearance or its session edit failed, so nothing was signed.
    Appearance(String),
    /// The certificate signature itself failed.
    Signature(String),
    /// The signature was written, but its own bytes did not verify.
    Verification(String),
}

impl SignFailure {
    /// User-facing message for the card.
    fn message(self) -> String {
        match self {
            Self::Appearance(message) | Self::Signature(message) | Self::Verification(message) => {
                message
            }
        }
    }
}

impl PdfxiApp {
    /// Handle [`AppCommand::Sign`] from the command bar, tools panel or a deep
    /// link.
    ///
    /// Closed, the command opens the card; already open, it starts the signed
    /// flow that the card's own Sign button drives. Keeping one command makes
    /// the keyboard path and the button path identical.
    pub(crate) fn handle_sign_command(&mut self) {
        if self.signing.open {
            self.start_signing();
        } else {
            self.open_sign_dialog();
        }
    }

    /// Open the Sign card for the active document.
    ///
    /// The card is capability-gated and needs an open document, exactly like
    /// every other authoring surface: when either is missing the status line
    /// says why and no window opens.
    pub(crate) fn open_sign_dialog(&mut self) {
        if !self.capabilities.contains(Capability::Signing) {
            self.status = "Signing is not enabled in this build".to_owned();
            return;
        }
        let Some(id) = self.state.active_tab_id() else {
            self.status = "Open a document before signing".to_owned();
            return;
        };
        if self.read_only_tabs.contains(&id) {
            self.status = "The document permissions do not allow changes".to_owned();
            return;
        }
        self.signing.open = true;
        self.signing.tab = Some(id);
        self.signing.error = None;
        self.signing.page = self.state.active_tab().map_or(0, |tab| tab.current_page());
        if self.signing.appearance.trim().is_empty() {
            let who = self
                .signing
                .subject
                .clone()
                .unwrap_or_else(|| self.settings.display_name.clone());
            self.signing.appearance = format!("Digitally signed by {who}");
        }
        self.status = if self.signing.subject.is_some() {
            "Sign: identity loaded".to_owned()
        } else {
            "Sign: choose a PFX identity".to_owned()
        };
    }

    /// Whether the Sign card is open.
    #[must_use]
    pub fn sign_dialog_open(&self) -> bool {
        self.signing.open
    }

    /// The signer subject shown by the card, once an identity is loaded.
    #[must_use]
    pub fn sign_signer_subject(&self) -> Option<&str> {
        self.signing.subject.as_deref()
    }

    /// Whether the card's Sign action is enabled.
    ///
    /// An imported identity and an active writable document are both required,
    /// and no worker may already be running for the card.
    #[must_use]
    pub fn signing_ready(&self) -> bool {
        let Some(id) = self.state.active_tab_id() else {
            return false;
        };
        self.signing.open
            && self.signing.identity.is_some()
            && self.signing.progress.is_none()
            && self.signing.tab == Some(id)
            && !self.read_only_tabs.contains(&id)
    }

    /// Whether the card can import the chosen container.
    #[must_use]
    pub fn signing_can_load(&self) -> bool {
        self.signing.pfx_path.is_some()
            && !self.signing.password.is_empty()
            && self.signing.progress.is_none()
    }

    /// Whether a valid, whole-file signature covers the active document.
    ///
    /// This is the only condition that may show the E-signed badge: the result
    /// must belong to the active tab, the digest must match, the signature must
    /// cover the whole file, and no revision may have been appended after
    /// signing. Chain trust is reported separately and never turns an
    /// unverified identity into a trusted one.
    #[must_use]
    pub(crate) fn signature_badge_visible(&self) -> bool {
        // Both the card's tab and the readout's tab must be the active one: a
        // reopened card on another tab must not inherit the previous result.
        if self.signing.tab != self.state.active_tab_id() || !self.verification_is_for_active_tab()
        {
            return false;
        }
        self.signing
            .verification
            .as_ref()
            .is_some_and(|verification| {
                badge_visible_for(
                    verification.status(),
                    verification.bytes_match(),
                    verification.covers_entire_file(),
                    verification.appended_after_signature(),
                )
            })
    }

    /// Whether the stored verification readout belongs to the active document.
    ///
    /// Switching tabs must not show one document's signer under another one.
    #[must_use]
    fn verification_is_for_active_tab(&self) -> bool {
        self.signing.verification_tab.is_some()
            && self.signing.verification_tab == self.state.active_tab_id()
    }

    /// Check the newly opened document's signature once, when it binds.
    ///
    /// An unsigned document clears the readout and reports nothing: the
    /// library's `SignatureNotFound` is the normal case, not an error. Any
    /// other failure (malformed `/ByteRange`, DER/CMS, unsupported algorithm)
    /// is kept for that tab so the panel can show it. This runs only on open;
    /// [`Self::after_session_edit`] clears the readout after an edit.
    pub(crate) fn verify_signature_on_open(&mut self, id: crate::TabId) {
        // Snapshot the bytes and the typed note while the session is borrowed,
        // then update card state from the verification result.
        let (result, appearance) = {
            let Some(session) = self.sessions.get(&id) else {
                return;
            };
            (
                verify_signature(session.bytes()),
                signature_appearance_from_document(session.document()),
            )
        };
        match result {
            Ok(verification) => {
                self.signing.appearance = appearance.unwrap_or_else(|| {
                    format!("Digitally signed by {}", verification.signer_subject())
                });
                self.signing.verification = Some(verification);
                self.signing.verification_tab = Some(id);
                self.signing.tab = Some(id);
                self.signing.page = 0;
                self.signing.verification_error = None;
                self.pending_signature_scroll = Some(0);
            }
            Err(pdfxi_security::SecurityError::SignatureNotFound) => {
                self.signing.verification = None;
                self.signing.verification_tab = None;
                self.signing.tab = None;
                self.signing.verification_error = None;
            }
            Err(error) => {
                self.signing.verification = None;
                self.signing.verification_tab = Some(id);
                self.signing.tab = Some(id);
                self.signing.verification_error = Some(error.to_string());
            }
        }
    }

    /// Open the app's file dialog and remember the chosen PKCS#12 container.
    ///
    /// The file is only imported after the user asks to load the identity, so
    /// a cancelled picker changes nothing but the status line.
    pub(crate) fn choose_signing_identity(&mut self) {
        let chosen = rfd::FileDialog::new()
            .set_title("Choose signing identity")
            .add_filter("PKCS#12 container", &["pfx", "p12"])
            .pick_file();
        if let Some(path) = chosen {
            self.signing.pfx_path = Some(path);
            self.signing.identity = None;
            self.signing.subject = None;
            self.signing.chain_len = 0;
            self.signing.identity_error = None;
            self.signing.error = None;
            self.status = "Sign: enter the container password".to_owned();
        }
    }

    /// Import the chosen PKCS#12 container with the typed password.
    ///
    /// The file is read and parsed on the UI thread, which is small for a PFX.
    /// On success the password is cleared immediately and only the imported
    /// identity stays in memory.
    pub(crate) fn load_signing_identity(&mut self) {
        let Some(path) = self.signing.pfx_path.clone() else {
            self.signing.identity_error = Some("Choose a PKCS#12 container first.".to_owned());
            return;
        };
        let password = self.signing.password.clone();
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) => {
                self.signing.identity = None;
                self.signing.subject = None;
                self.signing.identity_error =
                    Some(format!("Cannot read {}: {error}", path.display()));
                return;
            }
        };
        match SigningIdentity::from_pfx(&bytes, &password) {
            Ok(identity) => {
                self.signing.subject = Some(identity.signer_subject());
                self.signing.chain_len = identity.chain_len();
                self.signing.identity = Some(identity);
                self.signing.password.clear();
                self.signing.identity_error = None;
                self.signing.error = None;
                self.status = format!(
                    "Sign: identity loaded ({} certificate(s))",
                    self.signing.chain_len
                );
            }
            Err(error) => {
                self.signing.identity = None;
                self.signing.subject = None;
                self.signing.chain_len = 0;
                self.signing.identity_error = Some(error.to_string());
                self.status = "Sign: the identity could not be loaded".to_owned();
            }
        }
    }

    /// Close the Sign card. The loaded identity and verification result are
    /// kept so reopening the card is cheap and history is visible.
    pub(crate) fn close_sign_dialog(&mut self) {
        self.signing.open = false;
        self.signing.progress = None;
    }

    /// The next free `Signature-N` sequence for the session's document.
    ///
    /// A reopened signed PDF already carries `Signature-1`, so the counter is
    /// seeded from its `/AcroForm /Fields` signature entries instead of from
    /// this run's own count. A damaged AcroForm falls back to the run counter:
    /// the signature attempt will then report the library's own error.
    fn next_signature_sequence(&self, id: crate::TabId) -> usize {
        let start = self.signing.signatures.saturating_add(1);
        let Some(session) = self.sessions.get(&id) else {
            return start;
        };
        let Ok(model) = read_acroform(session.document()) else {
            return start;
        };
        let existing: Vec<&str> = model
            .fields()
            .iter()
            .filter(|field| field.kind == FormFieldKind::Signature)
            .map(|field| field.name.as_str())
            .collect();
        next_free_signature_sequence(&existing, start)
    }

    /// Draw the green E-signed chip above the typed signature note.
    ///
    /// This is a label, not a control, and it disappears as soon as the badge
    /// predicate is false (stale readout, another tab, modified bytes). The
    /// chip is placed with the same page-to-screen transform as the annotation
    /// and find overlays.
    pub(crate) fn signature_badge_ui(
        &self,
        ui: &egui::Ui,
        layout: &crate::canvas::CanvasLayout,
        origin: egui::Pos2,
        pad_x: f32,
    ) {
        if !self.signature_badge_visible() {
            return;
        }
        let page = self.signing.page;
        let Some(geometry) = layout.page(page) else {
            return;
        };
        let width_pt = geometry.width_pt;
        let height_pt = self
            .page_sizes
            .get(page)
            .map_or(geometry.height_pt, |size| size.height_pt);
        let block = signature_block_local(width_pt, height_pt);
        let chip = signature_chip_local(block);
        let to_screen = |local: crate::canvas::CanvasRect| {
            layout.page_rect_to_canvas(page, local).map(|mapped| {
                egui::Rect::from_min_size(
                    egui::pos2(
                        origin.x + mapped.x - self.scroll_x + pad_x,
                        origin.y + mapped.y - self.scroll_y,
                    ),
                    egui::vec2(mapped.width, mapped.height),
                )
            })
        };
        let (Some(block_rect), Some(chip_rect)) = (to_screen(block), to_screen(chip)) else {
            return;
        };
        let page_rect = egui::Rect::from_min_size(
            egui::pos2(
                origin.x + geometry.rect.x - self.scroll_x + pad_x,
                origin.y + geometry.rect.y - self.scroll_y,
            ),
            egui::vec2(geometry.rect.width, geometry.rect.height),
        );
        let clip = ui.clip_rect().intersect(page_rect);
        if !clip.is_positive() {
            return;
        }
        let painter = ui.painter().with_clip_rect(clip);
        let surface = ui.visuals().extreme_bg_color;
        let border = ui.visuals().widgets.noninteractive.bg_stroke.color;
        let text = ui.visuals().text_color();
        painter.rect_filled(block_rect, 6.0, surface);
        painter.rect_stroke(
            block_rect,
            6.0,
            egui::Stroke::new(1.0_f32, border),
            egui::StrokeKind::Inside,
        );
        let appearance = if self.signing.appearance.trim().is_empty() {
            format!("Digitally signed by {}", self.settings.display_name)
        } else {
            self.signing.appearance.trim().to_owned()
        };
        let (signer, when) = self.signing.verification.as_ref().map_or_else(
            || (self.settings.display_name.clone(), "unknown".to_owned()),
            |verification| {
                (
                    verification.signer_subject().to_owned(),
                    signing_time_label(verification.signing_time_unix()),
                )
            },
        );
        painter.text(
            block_rect.left_top() + egui::vec2(8.0, 7.0),
            egui::Align2::LEFT_TOP,
            truncate_label(&appearance, 36),
            egui::FontId::proportional(10.0),
            text,
        );
        painter.text(
            block_rect.left_top() + egui::vec2(8.0, 28.0),
            egui::Align2::LEFT_TOP,
            truncate_label(&format!("{signer} - {when}"), 40),
            egui::FontId::proportional(8.5),
            text,
        );
        painter.rect_filled(chip_rect, 4.0, egui::Color32::from_rgb(0x1f, 0x7a, 0x3d));
        painter.text(
            chip_rect.center(),
            egui::Align2::CENTER_CENTER,
            "✓ E-signed",
            egui::FontId::proportional(11.0),
            egui::Color32::WHITE,
        );
    }

    /// Scroll offset that brings the signature block fully into the viewport.
    pub(crate) fn signature_scroll_target(
        &self,
        layout: &crate::canvas::CanvasLayout,
        page: usize,
        viewport_height: f32,
    ) -> f32 {
        let Some(geometry) = layout.page(page) else {
            return 0.0;
        };
        let block = signature_block_local(geometry.width_pt, geometry.height_pt);
        let Some(mapped) = layout.page_rect_to_canvas(page, block) else {
            return 0.0;
        };
        layout
            .clamp_scroll(0.0, mapped.y + mapped.height + 24.0 - viewport_height)
            .y
    }

    /// Draw the typed appearance, sign the document and verify the result.
    ///
    /// The appearance is a `/Text` note so it is visible in every viewer, then
    /// [`pdfxi_document::DocumentSession::sign`] appends the certificate
    /// signature as one incremental revision. Both steps are synchronous: the
    /// measured signing call is short enough that a worker thread would only
    /// add a Stop button the user can never click.
    pub(crate) fn start_signing(&mut self) {
        if !self.signing_ready() {
            self.signing.error =
                Some("Choose a PFX identity and load it before signing.".to_owned());
            return;
        }
        let Some(id) = self.state.active_tab_id() else {
            self.signing.error = Some("Open a document before signing.".to_owned());
            return;
        };
        let Some(identity) = self.signing.identity.take() else {
            self.signing.error = Some("Load a PFX identity before signing.".to_owned());
            return;
        };
        let page = self.signing.page;
        let appearance = if self.signing.appearance.trim().is_empty() {
            format!("Digitally signed by {}", self.settings.display_name)
        } else {
            self.signing.appearance.trim().to_owned()
        };
        let author = self.settings.display_name.clone();
        let reason = non_empty(&self.signing.reason, "Digitally signed");
        let location = non_empty(&self.signing.location, "PDFXI");
        let sequence = self.next_signature_sequence(id);
        let field_name = format!("Signature-{sequence}");
        let note_id = format!("pdfxi-signature-{sequence}");

        let outcome = self.sign_document(
            id,
            &identity,
            &SignatureRequest {
                page,
                appearance: &appearance,
                author: &author,
                reason: &reason,
                location: &location,
                field_name: &field_name,
                note_id: &note_id,
            },
        );
        // The identity is kept in memory so a second signature does not need
        // the container again.
        self.signing.identity = Some(identity);
        match outcome {
            Ok(verification) => {
                // Refresh the tab first: the post-edit hook drops a stale
                // readout, and this signature's own result must survive it.
                self.after_session_edit(id);
                self.signing.signatures = sequence;
                self.signing.verification = Some(verification);
                self.signing.verification_tab = Some(id);
                self.signing.verification_error = None;
                self.signing.error = None;
                self.signing.progress = None;
                self.status = format!("Signed page {} as {field_name}", page.saturating_add(1));
            }
            Err(SignFailure::Verification(message)) => {
                // The signature is in the document even though this build
                // could not verify it, so keep the field sequence and report
                // the verification failure separately.
                self.after_session_edit(id);
                self.signing.signatures = sequence;
                self.signing.progress = None;
                self.signing.error = None;
                self.signing.verification_error = Some(message);
                self.signing.verification_tab = Some(id);
                self.status = "Sign: the signature was written but did not verify".to_owned();
            }
            Err(failure) => {
                // The appearance may already be one committed revision even
                // when the signature itself failed, so refresh the tab too.
                self.after_session_edit(id);
                self.signing.progress = None;
                self.signing.error = Some(failure.message());
                self.status = "Sign: the signature was not written".to_owned();
            }
        }
    }

    /// Add the appearance and sign one session, then verify its signed bytes.
    ///
    /// The annotation library error is not convertible to
    /// [`pdfxi_document::SessionError`], so the appearance is drawn on a clone
    /// and committed as one session edit, exactly like the comments flow.
    fn sign_document(
        &mut self,
        id: crate::TabId,
        identity: &SigningIdentity,
        request: &SignatureRequest<'_>,
    ) -> Result<SignatureVerification, SignFailure> {
        let page = request.page;
        let session = self.sessions.get_mut(&id).ok_or_else(|| {
            SignFailure::Appearance("The document has no open session".to_owned())
        })?;
        let media_box = session
            .document()
            .page(page)
            .map_err(|error| SignFailure::Appearance(error.to_string()))?
            .media_box;
        let mut candidate = session.document().clone();
        // Bottom-right corner, inset by a margin, so the note is visible on
        // every page and does not sit under the page content's first line.
        let note_x = (media_box.right - 24.0 - 36.0).max(media_box.left);
        let note_y = (media_box.bottom + 36.0).max(media_box.bottom);
        add_text_note(
            &mut candidate,
            page,
            Point::new(note_x as f32, note_y as f32),
            request.appearance,
            request.author,
            request.note_id,
        )
        .map_err(|error| SignFailure::Appearance(error.to_string()))?;
        session
            .edit(|document| {
                *document = candidate;
                Ok(())
            })
            .map_err(|error| SignFailure::Appearance(error.to_string()))?;
        let options = SignOptions {
            page_index: page,
            field_name: request.field_name.to_owned(),
            reason: Some(request.reason.to_owned()),
            location: Some(request.location.to_owned()),
            ..SignOptions::default()
        };
        session
            .sign(identity, &options)
            .map_err(|error| SignFailure::Signature(error.to_string()))?;
        verify_signature(session.bytes())
            .map_err(|error| SignFailure::Verification(error.to_string()))
    }

    /// Draw the Sign card and the verification panel.
    pub(crate) fn signing_dialog_ui(&mut self, ctx: &egui::Context) {
        if !self.signing.open {
            return;
        }
        let mut close = false;
        let mut choose = false;
        let mut load = false;
        let mut sign = false;
        let page_count = self
            .state
            .active_tab()
            .map_or(1, |tab| tab.page_count().max(1));
        egui::Window::new("Sign")
            .collapsible(false)
            .resizable(true)
            .default_width(430.0)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                ui.heading("Certificate signature");
                ui.label(
                    egui::RichText::new(
                        "Choose a PKCS#12 identity, set the appearance text, then sign. \
                         The signature is written as an incremental revision.",
                    )
                    .size(11.0)
                    .color(ui.visuals().weak_text_color()),
                );
                ui.separator();

                ui.label("Identity");
                let path_label = self.signing.pfx_path.as_ref().map_or_else(
                    || "No PKCS#12 container chosen".to_owned(),
                    |path| path.display().to_string(),
                );
                ui.label(path_label);
                if ui.button("Choose PFX...").clicked() {
                    choose = true;
                }
                ui.add(
                    egui::TextEdit::singleline(&mut self.signing.password)
                        .password(true)
                        .hint_text("Container password")
                        .desired_width(260.0),
                );
                match &self.signing.subject {
                    Some(subject) => {
                        ui.label(format!("Signer: {subject}"));
                        ui.label(format!(
                            "Certificate chain: {} certificate(s)",
                            self.signing.chain_len
                        ));
                    }
                    None => {
                        ui.label("Signer: identity not loaded");
                    }
                };
                if ui
                    .add_enabled(self.signing_can_load(), egui::Button::new("Load identity"))
                    .on_disabled_hover_text("Choose a PFX file and type its password first")
                    .clicked()
                {
                    load = true;
                }
                if let Some(error) = &self.signing.identity_error {
                    ui.colored_label(ui.visuals().error_fg_color, error);
                }
                if let Some(progress) = &self.signing.progress {
                    ui.label(progress);
                }

                ui.separator();
                ui.label("Signature options");
                ui.horizontal(|ui| {
                    ui.label("Reason");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.signing.reason)
                            .hint_text("Optional reason")
                            .desired_width(280.0),
                    );
                });
                ui.horizontal(|ui| {
                    ui.label("Location");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.signing.location)
                            .hint_text("Optional location")
                            .desired_width(280.0),
                    );
                });
                ui.horizontal(|ui| {
                    ui.label("Page");
                    let mut page_number = self.signing.page.saturating_add(1);
                    if ui
                        .add(egui::DragValue::new(&mut page_number).range(1..=page_count))
                        .changed()
                    {
                        self.signing.page = page_number.saturating_sub(1);
                    }
                    ui.label(format!("of {page_count}"));
                });
                ui.label("Appearance");
                ui.add(
                    egui::TextEdit::multiline(&mut self.signing.appearance)
                        .hint_text("Text drawn on the page")
                        .desired_rows(2)
                        .desired_width(390.0),
                );

                if let Some(error) = &self.signing.error {
                    ui.colored_label(ui.visuals().error_fg_color, error);
                }
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(self.signing_ready(), egui::Button::new("Sign"))
                        .clicked()
                    {
                        sign = true;
                    }
                    if ui.button("Close").clicked() {
                        close = true;
                    }
                });

                self.signature_verification_ui(ui);
            });
        if choose {
            self.choose_signing_identity();
        }
        if load {
            self.load_signing_identity();
        }
        if sign {
            self.dispatch(AppCommand::Sign, Some(ctx));
        }
        if close {
            self.close_sign_dialog();
        }
    }

    /// Draw the verification readout for the last signature of the active tab.
    fn signature_verification_ui(&self, ui: &mut egui::Ui) {
        if !self.verification_is_for_active_tab() {
            if self.signing.verification.is_some() || self.signing.verification_error.is_some() {
                ui.separator();
                ui.label(egui::RichText::new("Not checked for this document").weak());
            }
            return;
        }
        let Some(verification) = &self.signing.verification else {
            if let Some(error) = &self.signing.verification_error {
                ui.separator();
                ui.colored_label(ui.visuals().error_fg_color, error);
            }
            return;
        };
        ui.separator();
        ui.heading("Signature verification");
        if self.signature_badge_visible() {
            ui.label(egui::RichText::new("E-signed — digest verified").strong());
        }
        ui.label(format!("Status: {:?}", verification.status()));
        ui.label(format!("Signer: {}", verification.signer_subject()));
        ui.label(format!(
            "SHA-256 fingerprint: {}",
            verification.signer_fingerprint_hex()
        ));
        ui.label(format!(
            "Signing time: {}",
            signing_time_label(verification.signing_time_unix())
        ));
        ui.label(format!(
            "Covers entire file: {}",
            yes_no(verification.covers_entire_file())
        ));
        ui.label(format!(
            "Appended after signature: {}",
            yes_no(verification.appended_after_signature())
        ));
        ui.label(format!(
            "Certificate chain: {}",
            chain_trust_label(verification.certificate_chain_trusted())
        ));
        if !verification.bytes_match()
            || verification.status() != pdfxi_security::SignatureStatus::Valid
        {
            ui.colored_label(
                ui.visuals().error_fg_color,
                "Signature invalid: the signed bytes do not match.",
            );
        }
        if verification.appended_after_signature() || !verification.covers_entire_file() {
            ui.colored_label(
                ui.visuals().error_fg_color,
                "Document changed after signing; this signature does not cover the current revision.",
            );
        }
    }
}

/// `true`/`false` as a user-facing word.
fn yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}

/// A blank optional field falls back to a non-empty signer-supplied value.
fn non_empty(value: &str, fallback: &str) -> String {
    if value.trim().is_empty() {
        fallback.to_owned()
    } else {
        value.trim().to_owned()
    }
}

/// The E-signed badge rule: a valid signature that covers the whole document.
///
/// Chain trust is deliberately not part of this predicate; it is reported
/// separately so an unverified identity never looks like a verified one.
fn badge_visible_for(
    status: SignatureStatus,
    bytes_match: bool,
    covers_entire_file: bool,
    appended: bool,
) -> bool {
    status == SignatureStatus::Valid && bytes_match && covers_entire_file && !appended
}

/// Signature overlay block width in page-local points.
const SIGNATURE_BLOCK_WIDTH_PT: f32 = 220.0;
/// Signature overlay block height in page-local points.
const SIGNATURE_BLOCK_HEIGHT_PT: f32 = 56.0;
/// Inset of the block from the page's bottom/right edges.
const SIGNATURE_BLOCK_MARGIN_PT: f32 = 36.0;
/// Top edge of the `/Text` note in PDF bottom-up points (bottom 36 + 24).
const SIGNATURE_NOTE_TOP_PT: f32 = 60.0;
/// E-signed chip width in page-local points.
const SIGNATURE_CHIP_WIDTH_PT: f32 = 96.0;
/// E-signed chip height in page-local points.
const SIGNATURE_CHIP_HEIGHT_PT: f32 = 18.0;

/// The visible signature block in page-local top-down points.
///
/// Anchored to the bottom-right of the signing page, independent of the
/// `/Text` note's icon size, so the overlay is drawn even when the page raster
/// omits annotation appearances.
fn signature_block_local(width_pt: f32, height_pt: f32) -> crate::canvas::CanvasRect {
    crate::canvas::CanvasRect::new(
        (width_pt - SIGNATURE_BLOCK_MARGIN_PT - SIGNATURE_BLOCK_WIDTH_PT).max(0.0),
        (height_pt - SIGNATURE_NOTE_TOP_PT - 12.0 - SIGNATURE_BLOCK_HEIGHT_PT).max(0.0),
        SIGNATURE_BLOCK_WIDTH_PT.min(width_pt.max(0.0)),
        SIGNATURE_BLOCK_HEIGHT_PT.min(height_pt.max(0.0)),
    )
}

/// The green chip directly above `block`, in page-local top-down points.
fn signature_chip_local(block: crate::canvas::CanvasRect) -> crate::canvas::CanvasRect {
    crate::canvas::CanvasRect::new(
        (block.x + block.width - SIGNATURE_CHIP_WIDTH_PT).max(0.0),
        (block.y - 4.0 - SIGNATURE_CHIP_HEIGHT_PT).max(0.0),
        SIGNATURE_CHIP_WIDTH_PT,
        SIGNATURE_CHIP_HEIGHT_PT,
    )
}

/// Read the typed signature note's contents from a reopened document.
///
/// The app writes the appearance as a `/Text` note whose `/NM` starts with
/// `pdfxi-signature-`; that text is what the signer actually approved, so a
/// reopened signature must show it instead of the local display name.
fn signature_appearance_from_document(document: &pdfxi_core::Document) -> Option<String> {
    pdfxi_edit::list_annotations(document, 0)
        .ok()?
        .into_iter()
        .find(|annotation| {
            annotation.id.starts_with("pdfxi-signature-") && !annotation.contents.trim().is_empty()
        })
        .map(|annotation| annotation.contents.trim().to_owned())
}

/// First `max_chars` characters of `text`, with an ellipsis when truncated.
fn truncate_label(text: &str, max_chars: usize) -> String {
    let mut out: String = text.chars().take(max_chars).collect();
    if text.chars().count() > max_chars {
        out.pop();
        out.push('…');
    }
    out
}

/// The first `Signature-N` name at or above `start` that is not in `existing`.
fn next_free_signature_sequence(existing: &[&str], start: usize) -> usize {
    let mut sequence = start.max(1);
    loop {
        let name = format!("Signature-{sequence}");
        if existing.iter().all(|existing| *existing != name) {
            return sequence;
        }
        sequence = sequence.saturating_add(1);
    }
}

/// Human-readable chain trust. `None` is *not evaluated*, never "trusted".
fn chain_trust_label(trusted: Option<bool>) -> &'static str {
    match trusted {
        Some(true) => "trusted anchor",
        Some(false) => "not trusted",
        None => "identity not verified",
    }
}

/// Format an asserted signing time as UTC, or the raw seconds when the value
/// is out of range.
fn signing_time_label(unix_seconds: Option<u64>) -> String {
    let Some(seconds) = unix_seconds else {
        return "unknown".to_owned();
    };
    const FORMAT: &[time::format_description::FormatItem<'static>] =
        time::macros::format_description!("[year]-[month]-[day] [hour]:[minute] UTC");
    time::OffsetDateTime::from_unix_timestamp(i64::try_from(seconds).unwrap_or(i64::MAX))
        .ok()
        .and_then(|time| time.format(FORMAT).ok())
        .unwrap_or_else(|| format!("{seconds} (Unix seconds)"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pdfxi_document::DocumentSession;

    #[test]
    fn signature_overlay_geometry_sits_inside_the_page() {
        let block = signature_block_local(612.0, 792.0);
        let chip = signature_chip_local(block);

        assert_eq!(block.width, 220.0);
        assert_eq!(block.height, 56.0);
        assert_eq!(block.x, 612.0 - 36.0 - 220.0);
        // The block's bottom edge clears the 24 pt note at the page bottom.
        assert_eq!(block.y + block.height, 792.0 - 72.0);
        // The chip sits directly above with a 4 pt gap and shares the right edge.
        assert_eq!(chip.y + chip.height + 4.0, block.y);
        assert_eq!(chip.x + chip.width, block.x + block.width);
        assert_eq!(chip.width, 96.0);
        assert_eq!(chip.height, 18.0);

        assert!(block.x >= 0.0 && block.y >= 0.0);
        assert!(block.x + block.width <= 612.0);
        assert!(block.y + block.height <= 792.0);
        assert!(chip.y >= 0.0);
    }

    #[test]
    fn signature_scroll_target_reveals_the_block() {
        let app = PdfxiApp::new();
        let layout = crate::canvas::CanvasLayout::new(
            crate::ViewMode::Continuous,
            crate::Zoom::FitWidth,
            crate::canvas::CanvasSize::new(900.0, 600.0),
            &[crate::canvas::PageSize::new(612.0, 792.0)],
            16.0,
            18.0,
        );
        let target = app.signature_scroll_target(&layout, 0, 600.0);
        let block = signature_block_local(612.0, 792.0);
        let mapped = layout
            .page_rect_to_canvas(0, block)
            .expect("page 0 is laid out");
        assert!(
            target > 0.0,
            "the block starts below the top of the viewport"
        );
        assert!(mapped.y >= target, "the block starts inside the viewport");
        assert!(
            mapped.y + mapped.height <= target + 600.0,
            "the block ends inside the viewport"
        );
    }

    #[test]
    fn badge_requires_a_valid_whole_file_signature() {
        for (status, bytes_match, covers_entire_file, appended) in [
            (SignatureStatus::Valid, true, true, false),
            (SignatureStatus::Valid, true, true, true),
            (SignatureStatus::Valid, true, false, false),
            (SignatureStatus::Valid, false, true, false),
            (SignatureStatus::Tampered, true, true, false),
            (SignatureStatus::Unknown, true, true, false),
        ] {
            let expected =
                status == SignatureStatus::Valid && bytes_match && covers_entire_file && !appended;
            assert_eq!(
                badge_visible_for(status, bytes_match, covers_entire_file, appended),
                expected,
                "status={status:?} bytes_match={bytes_match} \
                 covers_entire_file={covers_entire_file} appended={appended}"
            );
        }
        assert!(!badge_visible_for(
            SignatureStatus::Valid,
            false,
            true,
            true
        ));
    }

    #[test]
    fn signature_sequence_skips_names_already_in_the_document() {
        assert_eq!(next_free_signature_sequence(&[], 1), 1);
        assert_eq!(next_free_signature_sequence(&["Signature-1"], 1), 2);
        assert_eq!(
            next_free_signature_sequence(&["Signature-1", "Signature-2"], 1),
            3
        );
        assert_eq!(next_free_signature_sequence(&["Signature-1"], 2), 2);
    }

    #[test]
    fn verification_from_another_tab_is_not_badged_or_shown() {
        let mut app = PdfxiApp::new();
        let a = app.state.open_tab("sign-a.pdf");
        let b = app.state.open_tab("sign-b.pdf");
        app.dispatch(AppCommand::ActivateTab(a), None);
        app.signing.tab = Some(a);
        app.signing.verification_tab = Some(a);
        assert!(app.verification_is_for_active_tab());

        app.dispatch(AppCommand::ActivateTab(b), None);
        assert_eq!(app.state.active_tab_id(), Some(b));
        assert!(!app.verification_is_for_active_tab());
        assert!(!app.signature_badge_visible());
    }

    #[test]
    fn sign_command_opens_the_card_and_close_closes_it() {
        let mut app = PdfxiApp::new();
        assert!(!app.sign_dialog_open());

        // No document open: the command explains why instead of opening a card.
        app.dispatch(AppCommand::Sign, None);
        assert!(!app.sign_dialog_open());
        assert_eq!(app.status, "Open a document before signing");

        let id = app.state.open_tab("sign-test.pdf");
        app.dispatch(AppCommand::Sign, None);
        assert!(app.sign_dialog_open());
        assert_eq!(app.signing.tab, Some(id));
        assert_eq!(app.signing.page, 0);
        assert!(app.sign_signer_subject().is_none());

        app.close_sign_dialog();
        assert!(!app.sign_dialog_open());
    }

    /// Sign a real fixture with the pinned PFX and prove the badge rule tracks
    /// the digest, not the presence of a signature.
    ///
    /// Ignored by default because it parses and signs a real PDF. Run with
    /// `cargo test -p pdfxi-app --features ui,pdfium --lib -- --ignored --nocapture`.
    #[test]
    #[ignore = "signs a real PDF with the pinned PFX fixture"]
    fn signed_document_verifies_and_tampering_clears_the_badge() {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let pdf = root.join("../../fixtures/realworld/microsoft-excel-2019-sample.pdf");
        let pfx = root.join("../pdfxi-security/tests/fixtures/pfx/leaf-aes256.pfx");
        let identity_bytes = std::fs::read(&pfx).expect("the PFX fixture is readable");
        let identity = SigningIdentity::from_pfx(&identity_bytes, "pdfxi-test-pfx")
            .expect("the pinned PFX imports");
        let mut session = DocumentSession::open(&pdf).expect("the fixture opens");
        let original_pages = session
            .document()
            .page_count()
            .expect("the fixture reports its page count");
        let appearance_text = "Digitally signed by PDFXI test";
        let media_box = session.document().page(0).expect("page 0").media_box;
        let mut candidate = session.document().clone();
        add_text_note(
            &mut candidate,
            0,
            Point::new(
                (media_box.right - 60.0) as f32,
                (media_box.bottom + 36.0) as f32,
            ),
            appearance_text,
            "PDFXI test",
            "pdfxi-signature-1",
        )
        .expect("the typed appearance is added");
        session
            .edit(|document| {
                *document = candidate;
                Ok(())
            })
            .expect("the appearance is one revision");

        session
            .sign(
                &identity,
                &SignOptions {
                    page_index: 0,
                    field_name: "Signature-1".to_owned(),
                    reason: Some("Test approval".to_owned()),
                    location: Some("Test lab".to_owned()),
                    ..SignOptions::default()
                },
            )
            .expect("the session signs");
        let verification = verify_signature(session.bytes()).expect("the signed bytes verify");
        assert_eq!(verification.status(), SignatureStatus::Valid);
        assert!(verification.bytes_match());
        assert!(badge_visible_for(
            verification.status(),
            verification.bytes_match(),
            verification.covers_entire_file(),
            verification.appended_after_signature()
        ));

        // The signed revision itself carries the typed appearance, not just
        // the in-memory draft.
        let annotations =
            pdfxi_edit::list_annotations(session.document(), 0).expect("page annotations read");
        let note = annotations
            .iter()
            .find(|annotation| annotation.id.starts_with("pdfxi-signature-"))
            .expect("the typed signature note is in the signed document");
        assert_eq!(note.contents, appearance_text);

        // An independent renderer must still open the signed bytes with the
        // original page count and a usable first page.
        #[cfg(feature = "pdfium")]
        {
            use pdfxi_render::{PageIndex, PdfiumRenderer, Renderer};
            let renderer = PdfiumRenderer::from_bytes(session.bytes().to_vec())
                .expect("PDFium opens the signed bytes");
            assert_eq!(
                renderer.page_count().expect("the signed PDF reports pages"),
                original_pages
            );
            let size = renderer
                .page_size(PageIndex::new(0))
                .expect("the signed page 0 has geometry");
            assert!(size.width_pt > 0.0 && size.height_pt > 0.0);
        }

        let [_, _, start, length] = verification.byte_range();
        assert!(
            length > 20,
            "the signed tail is large enough to flip a byte"
        );
        let mut tampered = session.bytes().to_vec();
        tampered[start + length / 2] ^= 0x01;
        let after =
            verify_signature(&tampered).expect("the tampered file still carries a signature");
        assert!(!badge_visible_for(
            after.status(),
            after.bytes_match(),
            after.covers_entire_file(),
            after.appended_after_signature()
        ));

        // The signed file already carries Signature-1, so a reopened document
        // must continue at Signature-2 instead of reusing the name.
        let reopened = DocumentSession::open_bytes(session.bytes()).expect("the signed bytes open");
        let model = read_acroform(reopened.document()).expect("the signed AcroForm reads");
        let names: Vec<&str> = model
            .fields()
            .iter()
            .filter(|field| field.kind == FormFieldKind::Signature)
            .map(|field| field.name.as_str())
            .collect();
        assert!(names.contains(&"Signature-1"), "the PoC field is present");
        assert_eq!(next_free_signature_sequence(&names, 1), 2);
    }

    /// Prove the app-level badge and readout are scoped to the signing tab.
    ///
    /// Ignored by default because it loads the real fixture and PFX.
    #[test]
    #[ignore = "signs a real PDF with the pinned PFX fixture"]
    fn signed_badge_and_readout_are_scoped_to_the_signing_tab() {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let pdf = root.join("../../fixtures/realworld/microsoft-excel-2019-sample.pdf");
        let pfx = root.join("../pdfxi-security/tests/fixtures/pfx/leaf-aes256.pfx");

        let mut app = PdfxiApp::new();
        let a = app.state.open_tab("signed-a.pdf");
        app.sessions
            .insert(a, DocumentSession::open(&pdf).expect("the fixture opens"));
        app.dispatch(AppCommand::Sign, None);
        assert!(app.sign_dialog_open());
        app.signing.pfx_path = Some(pfx);
        app.signing.password = "pdfxi-test-pfx".to_owned();
        app.load_signing_identity();
        assert!(app.sign_signer_subject().is_some());
        assert!(app.signing.password.is_empty(), "the password is cleared");

        app.dispatch(AppCommand::Sign, None);
        assert!(app.signing.error.is_none(), "{:?}", app.signing.error);
        assert!(app.signature_badge_visible());
        assert_eq!(app.signing.verification_tab, Some(a));
        assert!(app.verification_is_for_active_tab());

        let b = app.state.open_tab("other.pdf");
        assert_eq!(app.state.active_tab_id(), Some(b));
        assert!(!app.signature_badge_visible());
        assert!(!app.verification_is_for_active_tab());

        app.dispatch(AppCommand::ActivateTab(a), None);
        assert!(app.signature_badge_visible());
        assert!(app.verification_is_for_active_tab());
    }

    /// An unsigned document opens cleanly: no readout, no error, no badge.
    #[test]
    fn opening_an_unsigned_document_clears_the_signature_readout() {
        let pdf = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/realworld/microsoft-excel-2019-sample.pdf");
        let mut app = PdfxiApp::new();
        app.open_document(&pdf);
        assert!(app.state.active_tab_id().is_some());
        assert!(app.signing.verification.is_none());
        assert!(app.signing.verification_tab.is_none());
        assert!(app.signing.verification_error.is_none());
        assert!(!app.signature_badge_visible());
    }

    /// Sign the real Excel fixture exactly as the UI does and return the bytes.
    ///
    /// Shared by the open-path tests and the capture fixture so all of them
    /// produce the same two-revision signed document.
    fn signed_fixture_bytes() -> Vec<u8> {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let pdf = root.join("../../fixtures/realworld/microsoft-excel-2019-sample.pdf");
        let pfx = root.join("../pdfxi-security/tests/fixtures/pfx/leaf-aes256.pfx");
        let identity_bytes = std::fs::read(&pfx).expect("the PFX fixture is readable");
        let identity = SigningIdentity::from_pfx(&identity_bytes, "pdfxi-test-pfx")
            .expect("the pinned PFX imports");

        let mut session = DocumentSession::open(&pdf).expect("the fixture opens");
        let media_box = session.document().page(0).expect("page 0").media_box;
        let mut candidate = session.document().clone();
        add_text_note(
            &mut candidate,
            0,
            Point::new(
                (media_box.right - 60.0) as f32,
                (media_box.bottom + 36.0) as f32,
            ),
            "Digitally signed by PDFXI test",
            "PDFXI test",
            "pdfxi-signature-1",
        )
        .expect("the typed appearance is added");
        session
            .edit(|document| {
                *document = candidate;
                Ok(())
            })
            .expect("the appearance is one revision");
        session
            .sign(
                &identity,
                &SignOptions {
                    page_index: 0,
                    field_name: "Signature-1".to_owned(),
                    reason: Some("Test approval".to_owned()),
                    location: Some("Test lab".to_owned()),
                    ..SignOptions::default()
                },
            )
            .expect("the session signs");
        session.bytes().to_vec()
    }

    /// A signed file opened through the normal path shows the chip and the
    /// verification panel without loading an identity or signing again.
    #[test]
    #[ignore = "opens a PDF signed with the pinned PFX fixture"]
    fn signature_is_verified_when_the_signed_document_opens() {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let dir = root.join("../../target/tmp/signing");
        std::fs::create_dir_all(&dir).expect("create the signing temp directory");
        let path = dir.join(format!("open-signed-{}.pdf", std::process::id()));
        std::fs::write(&path, signed_fixture_bytes()).expect("write the signed PDF");

        let mut app = PdfxiApp::new();
        app.open_document(&path);
        let id = app.state.active_tab_id().expect("the signed file opened");
        assert!(app.signing.verification.is_some());
        assert_eq!(app.signing.verification_tab, Some(id));
        assert!(app.signing.verification_error.is_none());
        assert!(app.signature_badge_visible());
        assert!(
            app.signing.appearance.contains("PDFXI test"),
            "the reopened appearance comes from the document note: {:?}",
            app.signing.appearance
        );
        assert!(!app.signing.appearance.contains("Reader"));

        let _ = std::fs::remove_file(&path);
    }

    /// Write the persistent capture fixture under `target/tmp/signing`.
    ///
    /// Capture-only helper: it signs the real fixture with the pinned PFX and
    /// leaves `capture-signed.pdf` in place for the reviewer's screenshots.
    /// Run with
    /// `cargo test -p pdfxi-app --features ui,pdfium --lib write_signed_capture_fixture -- --ignored`.
    #[test]
    #[ignore = "writes the signed capture fixture with the pinned PFX"]
    fn write_signed_capture_fixture() {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let dir = root.join("../../target/tmp/signing");
        std::fs::create_dir_all(&dir).expect("create the signing temp directory");
        let path = dir.join("capture-signed.pdf");
        let bytes = signed_fixture_bytes();
        std::fs::write(&path, &bytes).expect("write the signed capture fixture");

        // Reopen the written file so the fixture is a real signed PDF, not a
        // stale copy of an in-memory buffer.
        let pdf = root.join("../../fixtures/realworld/microsoft-excel-2019-sample.pdf");
        let original_pages = DocumentSession::open(&pdf)
            .expect("the source fixture opens")
            .document()
            .page_count()
            .expect("page count");
        let reopened = DocumentSession::open(&path).expect("the capture fixture reopens");
        assert_eq!(
            reopened.document().page_count().expect("page count"),
            original_pages
        );
        let verification = verify_signature(reopened.bytes()).expect("the fixture is signed");
        assert_eq!(verification.status(), SignatureStatus::Valid);
        assert!(verification.bytes_match());
        assert!(verification.covers_entire_file());
        eprintln!("capture fixture: {}", path.display());
    }

    #[test]
    fn chain_trust_none_is_reported_as_unverified() {
        assert_eq!(chain_trust_label(None), "identity not verified");
        assert_eq!(chain_trust_label(Some(true)), "trusted anchor");
        assert_eq!(chain_trust_label(Some(false)), "not trusted");
    }

    #[test]
    fn signing_time_formats_utc_and_falls_back_to_seconds() {
        assert_eq!(signing_time_label(Some(0)), "1970-01-01 00:00 UTC");
        assert_eq!(signing_time_label(None), "unknown");
        assert_eq!(
            signing_time_label(Some(u64::MAX)),
            format!("{} (Unix seconds)", u64::MAX)
        );
    }
}
