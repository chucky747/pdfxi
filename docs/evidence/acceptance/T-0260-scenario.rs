//! T-0260 end-to-end acceptance scenario on the committed real-world corpus.
//!
//! Every step calls the same shipping functions the desktop shell calls, in the
//! same order the app uses them: clone the parsed document, run the library
//! mutation on the clone, commit it with `DocumentSession::edit`, then save and
//! reopen (`crates/pdfxi-app/src/shell/{edit_tool,comments,forms}.rs`,
//! `shell/signing.rs`, `shell/export_card.rs`).
//!
//! Run it through `T-0260-scenario.ps1`; it needs the lane release rlibs.
//! Fixtures are opened read-only; every output goes to the scratch directory.

use std::fs;
use std::path::PathBuf;

use pdfxi_convert::pdf_export::{ExportFormat, ExportOptions, export_pdf};
use pdfxi_document::DocumentSession;
use pdfxi_edit::content_edit::{
    ObjectKind, ObjectReplacement, list_objects, replace_object, text_blocks,
};
use pdfxi_edit::document_annotations::{add_highlight, add_text_note, list_annotations, reply_to};
use pdfxi_edit::{Point, Rect};
use pdfxi_forms::document_form::{import_xfdf, read_acroform};
use pdfxi_security::{SignOptions, SignatureStatus, SigningIdentity, verify_signature};

const TEXT_MARKER: &str = "T-0260 edited paragraph marker";
const ANNOT_MARKER: &str = "T-0260 review marker";
const FORM_MARKER: &str = "T-0260 form value";
const AUTHOR: &str = "T-0260 QA";
/// Public password of the local test PKCS#12 container (never a secret).
const PFX_PASSWORD: &str = "pdfxi-test-pfx";

struct Report {
    failures: Vec<String>,
}

impl Report {
    fn new() -> Self {
        Self {
            failures: Vec::new(),
        }
    }

    fn check(&mut self, name: &str, ok: bool, detail: impl AsRef<str>) {
        let detail = detail.as_ref();
        if ok {
            println!("step={name} status=ok {detail}");
        } else {
            println!("step={name} status=FAIL {detail}");
            self.failures.push(name.to_owned());
        }
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let root = PathBuf::from(args.next().unwrap_or_else(|| ".".to_owned()));
    let scratch = PathBuf::from(args.next().unwrap_or_else(|| "target/tmp/t0260".to_owned()));
    fs::create_dir_all(&scratch).expect("create scratch directory");
    let mut report = Report::new();

    println!(
        "T-0260 acceptance scenario\nroot={}\nscratch={}",
        root.display(),
        scratch.display()
    );

    // Step 1: route and open.
    let route = root.join("fixtures/realworld/powerpoint-slides.pdf");
    let working = scratch.join("t0260-powerpoint-slides.pdf");
    let original = fs::read(&route).expect("read the committed real-world fixture");
    fs::write(&working, &original).expect("copy the fixture into scratch");
    println!(
        "route=fixtures/realworld/powerpoint-slides.pdf producer=PowerPoint16.0 bytes={} note=Word COM PDF save is blocked on this host (orchestration/blockers/T-0215-word-com.md); this task uses the committed PowerPoint-produced multi-page fixture and claims no Word document",
        original.len()
    );

    let mut session = DocumentSession::open(&working).expect("DocumentSession::open");
    let pages = session.document().page_count().expect("page count");
    let text_len: usize = (0..pages)
        .map(|page| {
            session
                .document()
                .page_text(page)
                .map(|text| text.chars().count())
                .unwrap_or(0)
        })
        .sum();
    let bytes = fs::read(&working).expect("read fixture bytes");
    let parsed = pdfxi_core::Document::from_bytes(bytes).expect("Document::from_bytes");
    let parsed_pages = parsed.page_count().expect("document page count");
    let parsed_len: usize = (0..parsed_pages)
        .map(|page| {
            parsed
                .page_text(page)
                .map(|t| t.chars().count())
                .unwrap_or(0)
        })
        .sum();
    report.check(
        "open",
        pages == 3 && parsed_pages == 3 && text_len > 0 && parsed_len == text_len,
        format!(
            "session_pages={pages} session_text_chars={text_len} from_bytes_pages={parsed_pages} from_bytes_text_chars={parsed_len} revisions={}",
            session.revision_count()
        ),
    );

    // Step 2: edit a text block, save, reopen, look for the marker.
    let objects = list_objects(session.document(), 0).expect("list objects on page 1");
    println!(
        "page1_objects={} text_blocks={} images={}",
        objects.len(),
        objects
            .iter()
            .filter(|o| o.kind == ObjectKind::TextBlock)
            .count(),
        objects
            .iter()
            .filter(|o| o.kind == ObjectKind::Image)
            .count()
    );
    let target = objects
        .iter()
        .find(|object| object.kind == ObjectKind::TextBlock)
        .map(|object| (object.id.clone(), object.text_preview.clone()));
    let Some((target_id, before_preview)) = target else {
        report.check("edit_text", false, "no text block found on page 1");
        finish(report);
    };
    let model = text_blocks(session.document(), 0).expect("text block model");
    println!(
        "target={target_id} blocks={} before_preview={before_preview:?}",
        model.blocks.len()
    );

    let mut candidate = session.document().clone();
    let replaced = replace_object(
        &mut candidate,
        0,
        &target_id,
        ObjectReplacement::Text(TEXT_MARKER),
    );
    match replaced {
        Ok(outcome) => {
            let committed = session.edit(|document| {
                *document = candidate;
                Ok(())
            });
            let edit_ok = committed.is_ok();
            let edit_note = match &committed {
                Ok(()) => "session_edit=ok".to_owned(),
                Err(error) => format!("session_edit_error={error}"),
            };
            println!("replace_outcome={outcome:?} {edit_note}");
            if edit_ok {
                let saved = session.save();
                let reopen = DocumentSession::open(&working);
                let (marker_ok, detail) = match (&saved, &reopen) {
                    (Ok(()), Ok(reopened)) => {
                        let page_text = reopened.document().page_text(0).unwrap_or_default();
                        (
                            page_text.contains(TEXT_MARKER),
                            format!(
                                "page1_chars={} revisions={} dirty={} contains_marker={}",
                                page_text.chars().count(),
                                reopened.revision_count(),
                                reopened.is_dirty(),
                                page_text.contains(TEXT_MARKER)
                            ),
                        )
                    }
                    (Err(error), _) => (false, format!("save_error={error}")),
                    (_, Err(error)) => (false, format!("reopen_error={error}")),
                };
                report.check("edit_text", marker_ok, detail);
            } else {
                report.check("edit_text", false, "the session refused the edit");
            }
        }
        Err(error) => report.check("edit_text", false, format!("replace_object_error={error}")),
    }

    // Step 3: highlight, save, reopen, confirm it is listed.
    let rect = Rect::new(60.0, 520.0, 500.0, 560.0).expect("valid highlight rectangle");
    let mut candidate = session.document().clone();
    match add_highlight(&mut candidate, 0, &[rect], ANNOT_MARKER, AUTHOR) {
        Ok(added) => {
            let committed = session.edit(|document| {
                *document = candidate;
                Ok(())
            });
            if let Err(error) = committed {
                report.check("highlight", false, format!("session_edit_error={error}"));
            } else if let Err(error) = session.save() {
                report.check("highlight", false, format!("save_error={error}"));
            } else {
                match DocumentSession::open(&working) {
                    Ok(reopened) => {
                        let annotations =
                            list_annotations(reopened.document(), 0).unwrap_or_default();
                        let found = annotations.iter().find(|view| {
                            view.contents.contains(ANNOT_MARKER) && view.author == AUTHOR
                        });
                        for view in &annotations {
                            println!(
                                "annotation id={:?} kind={:?} subtype={:?} author={:?} contents={:?} parent={:?} state={:?}",
                                view.id,
                                view.kind,
                                view.subtype,
                                view.author,
                                view.contents,
                                view.parent_id,
                                view.state
                            );
                        }
                        report.check(
                            "highlight",
                            found.is_some(),
                            format!(
                                "id={:?} subtype={:?} listed={} page_annotations={}",
                                added.id,
                                added.subtype,
                                found.is_some(),
                                annotations.len()
                            ),
                        );
                    }
                    Err(error) => report.check("highlight", false, format!("reopen_error={error}")),
                }
            }
        }
        Err(error) => report.check("highlight", false, format!("add_highlight_error={error}")),
    }

    // Step 4: note plus reply, save, reopen, confirm the thread.
    let mut candidate = session.document().clone();
    let note = add_text_note(
        &mut candidate,
        0,
        Point::new(72.0, 480.0),
        ANNOT_MARKER,
        AUTHOR,
        "pdfxi-t0260-note-1",
    );
    match note {
        Ok(parent) => {
            let reply = reply_to(&mut candidate, 0, &parent.id, "T-0260 reply text", AUTHOR);
            match reply {
                Ok(_) => {
                    let committed = session.edit(|document| {
                        *document = candidate;
                        Ok(())
                    });
                    if let Err(error) = committed {
                        report.check(
                            "comment_thread",
                            false,
                            format!("session_edit_error={error}"),
                        );
                    } else if let Err(error) = session.save() {
                        report.check("comment_thread", false, format!("save_error={error}"));
                    } else {
                        match DocumentSession::open(&working) {
                            Ok(reopened) => {
                                let annotations =
                                    list_annotations(reopened.document(), 0).unwrap_or_default();
                                let parent_id = parent.id.clone();
                                let thread_parent = annotations.iter().find(|view| {
                                    view.id == parent_id && view.contents.contains(ANNOT_MARKER)
                                });
                                let thread_reply = annotations.iter().find(|view| {
                                    view.parent_id.as_deref() == Some(parent_id.as_str())
                                });
                                for view in &annotations {
                                    println!(
                                        "thread id={:?} parent={:?} author={:?} contents={:?}",
                                        view.id, view.parent_id, view.author, view.contents
                                    );
                                }
                                report.check(
                                    "comment_thread",
                                    thread_parent.is_some() && thread_reply.is_some(),
                                    format!(
                                        "parent={:?} reply={:?} annotations={}",
                                        thread_parent.map(|v| v.id.as_str()),
                                        thread_reply.map(|v| v.id.as_str()),
                                        annotations.len()
                                    ),
                                );
                            }
                            Err(error) => report.check(
                                "comment_thread",
                                false,
                                format!("reopen_error={error}"),
                            ),
                        }
                    }
                }
                Err(error) => report.check("comment_thread", false, format!("reply_error={error}")),
            }
        }
        Err(error) => report.check("comment_thread", false, format!("note_error={error}")),
    }

    // Step 5: fill a form by importing an XFDF value, save, reopen, read it.
    let forms_route = root.join("fixtures/realworld/pdfxi-forms-selfmade.pdf");
    let forms_work = scratch.join("t0260-forms.pdf");
    fs::copy(&forms_route, &forms_work).expect("copy the forms fixture");
    match DocumentSession::open(&forms_work) {
        Ok(mut forms_session) => match read_acroform(forms_session.document()) {
            Ok(model) => {
                let names: Vec<&str> = model.fields.iter().map(|f| f.name.as_str()).collect();
                println!("form_fields={names:?}");
                let xfdf = format!(
                    "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<xfdf xmlns=\"http://ns.adobe.com/xfdf/\"><fields><field name=\"text-field\"><value>{FORM_MARKER}</value></field></fields></xfdf>\n"
                );
                let mut candidate = forms_session.document().clone();
                match import_xfdf(&mut candidate, &xfdf) {
                    Ok(applied) => {
                        let committed = forms_session.edit(|document| {
                            *document = candidate;
                            Ok(())
                        });
                        if let Err(error) = committed {
                            report.check("fill_form", false, format!("session_edit_error={error}"));
                        } else if let Err(error) = forms_session.save() {
                            report.check("fill_form", false, format!("save_error={error}"));
                        } else {
                            match DocumentSession::open(&forms_work) {
                                Ok(reopened) => {
                                    let value = read_acroform(reopened.document())
                                        .ok()
                                        .and_then(|model| {
                                            model
                                                .field("text-field")
                                                .map(|field| format!("{:?}", field.value))
                                        })
                                        .unwrap_or_else(|| "<unreadable>".to_owned());
                                    report.check(
                                            "fill_form",
                                            value.contains(FORM_MARKER),
                                            format!(
                                                "imported_fields={applied} text_field_value={value} revisions={}",
                                                reopened.revision_count()
                                            ),
                                        );
                                }
                                Err(error) => report.check(
                                    "fill_form",
                                    false,
                                    format!("reopen_error={error}"),
                                ),
                            }
                        }
                    }
                    Err(error) => {
                        report.check("fill_form", false, format!("import_xfdf_error={error}"))
                    }
                }
            }
            Err(error) => report.check("fill_form", false, format!("read_acroform_error={error}")),
        },
        Err(error) => report.check("fill_form", false, format!("open_error={error}")),
    }

    // Step 6: sign the edited, annotated document, save, verify, keep it for PDFium.
    let signed_path = scratch.join("t0260-signed.pdf");
    fs::write(&signed_path, session.bytes()).expect("copy the scenario document");
    println!(
        "signed_source_bytes={} revisions={}",
        session.bytes().len(),
        session.revision_count()
    );
    match DocumentSession::open(&signed_path) {
        Ok(mut signing_session) => {
            let pfx_path = root.join("crates/pdfxi-security/tests/fixtures/pfx/leaf-aes256.pfx");
            match fs::read(&pfx_path) {
                Ok(container) => match SigningIdentity::from_pfx(&container, PFX_PASSWORD) {
                    Ok(identity) => {
                        let options = SignOptions {
                            page_index: 0,
                            field_name: "PDFXI_T0260_Signature".to_owned(),
                            reason: Some("T-0260 acceptance scenario".to_owned()),
                            signing_time_unix: Some(1_700_000_000),
                            ..SignOptions::default()
                        };
                        match signing_session.sign(&identity, &options) {
                            Ok(report_signed) => {
                                let saved = signing_session.save();
                                let signed_bytes = fs::read(&signed_path).unwrap_or_default();
                                match saved {
                                    Ok(()) => match verify_signature(&signed_bytes) {
                                        Ok(verification) => {
                                            let ok = verification.status()
                                                == SignatureStatus::Valid
                                                && verification.bytes_match()
                                                && verification.covers_entire_file();
                                            report.check(
                                                "sign",
                                                ok,
                                                format!(
                                                    "status={:?} signature_valid={} bytes_match={} covers_entire_file={} signer={:?} signing_time={:?} byte_range={:?} saved_bytes={}",
                                                    verification.status(),
                                                    verification.signature_valid(),
                                                    verification.bytes_match(),
                                                    verification.covers_entire_file(),
                                                    report_signed.signer_subject(),
                                                    verification.signing_time_unix(),
                                                    verification.byte_range(),
                                                    signed_bytes.len()
                                                ),
                                            );
                                            match DocumentSession::open(&signed_path) {
                                                Ok(reopened) => {
                                                    let page_count = reopened
                                                        .document()
                                                        .page_count()
                                                        .unwrap_or(0);
                                                    println!(
                                                        "signed_reopen_pages={page_count} revisions={}",
                                                        reopened.revision_count()
                                                    );
                                                }
                                                Err(error) => {
                                                    println!("signed_reopen_error={error}")
                                                }
                                            }
                                        }
                                        Err(error) => report.check(
                                            "sign",
                                            false,
                                            format!("verify_error={error}"),
                                        ),
                                    },
                                    Err(error) => {
                                        report.check("sign", false, format!("save_error={error}"))
                                    }
                                }
                            }
                            Err(error) => {
                                report.check("sign", false, format!("sign_error={error}"))
                            }
                        }
                    }
                    Err(error) => report.check("sign", false, format!("pfx_error={error}")),
                },
                Err(error) => report.check("sign", false, format!("pfx_missing={error}")),
            }
        }
        Err(error) => report.check("sign", false, format!("open_error={error}")),
    }

    // Step 7: export the final (signed) document to Word.
    match DocumentSession::open(&signed_path) {
        Ok(export_session) => {
            let options = ExportOptions::new().with_title("T-0260 acceptance export");
            match export_pdf(export_session.document(), ExportFormat::Docx, &options) {
                Ok(exported) => {
                    let part = exported.parts.first();
                    let docx_bytes = part.map(|part| part.bytes.clone()).unwrap_or_default();
                    let zip_magic = docx_bytes.starts_with(b"PK");
                    let marker_in_bytes = docx_bytes
                        .windows(TEXT_MARKER.len())
                        .any(|window| window == TEXT_MARKER.as_bytes());
                    let out = scratch.join("t0260-export.docx");
                    let _ = fs::write(&out, &docx_bytes);
                    println!(
                        "export extension={} page_count={} parts={} bytes={} path={}",
                        exported.extension(),
                        exported.page_count,
                        exported.parts.len(),
                        docx_bytes.len(),
                        out.display()
                    );
                    report.check(
                        "export_docx",
                        exported.extension() == "docx"
                            && zip_magic
                            && marker_in_bytes
                            && docx_bytes.windows(4).any(|w| w == b"word"),
                        format!(
                            "pk_magic={zip_magic} marker_in_bytes={marker_in_bytes} unsupported={:?}",
                            exported.unsupported
                        ),
                    );
                }
                Err(error) => report.check("export_docx", false, format!("export_error={error}")),
            }
        }
        Err(error) => report.check("export_docx", false, format!("open_error={error}")),
    }

    println!("signed_pdf={}", signed_path.display());
    finish(report);
}

fn finish(report: Report) -> ! {
    if report.failures.is_empty() {
        println!("SCENARIO OK");
        std::process::exit(0);
    }
    println!("SCENARIO FAILED failed={:?}", report.failures);
    std::process::exit(1);
}
