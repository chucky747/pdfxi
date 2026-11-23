//! Permanent redaction of a real [`pdfxi_core::Document`].
//!
//! This is the document-level half of redaction: a caller marks page
//! rectangles, and the marked text-showing operators, annotations, links,
//! form fields and (optionally) metadata are permanently removed from the
//! object graph. Nothing is merely covered. The page content streams are
//! rewritten in place, so a later full [`pdfxi_core::Document::save`] cannot
//! leave the original operators behind in an unreferenced object.
//!
//! # Content rewrite
//!
//! Each page's content is tokenized with the same lexer as
//! [`crate::content_edit`]. Text-showing operators (`Tj`, `TJ`, `'`, `"`) are
//! examined glyph by glyph. A glyph whose transformed bounding box shares
//! positive area with a mark is removed. When only part of an operator is
//! marked, the kept glyphs are re-emitted inside a `TJ` array at their original
//! positions: removed advances become numeric `TJ` adjustments and kept glyphs
//! keep their original encoded bytes. No font re-encoding is involved, so a
//! subset, composite or symbol font cannot corrupt the surviving text. When the
//! advances cannot be represented (a missing font resource, a non-positive
//! horizontal scale, or a non-finite advance), the whole text-showing operator
//! is neutralised with an adjustment-only `TJ` and the report says so.
//!
//! A fully removed operator is likewise replaced by an adjustment-only `TJ`,
//! which preserves the text-matrix advance so following text does not slide
//! into the redacted area. The implicit line move of `'` and `"` is preserved
//! with `T*`, and `"` also re-emits its word and character spacing operands.
//!
//! Inline images (`BI ... ID ... EI`) keep their dictionary and exact sample
//! span in the token stream. An intersecting inline image is decoded and
//! pixel-refilled through the same image path as an XObject, then its bytes are
//! replaced in place inside the surrounding content stream. The `EI` terminator
//! is delimited with the PDF white-space/delimiter rules, and an unfiltered
//! image's length is also derived from its dimensions so binary sample bytes
//! that spell `EI` cannot move the span. Filtered images, an unclear sample
//! span, an unsupported colour space/filter or a non-invertible placement fail
//! closed with [`RedactionError::UnsupportedImage`]; an inline image outside
//! every mark is left byte-for-byte untouched.
//!
//! # Form XObjects
//!
//! A `Do` naming a `/Subtype /Form` XObject is recursed into when its painted
//! region (its `/BBox` under the current CTM and the form `/Matrix`) intersects
//! a mark. The form content is decoded and rewritten with the same glyph and
//! image logic as a page, with the form matrix composed into the CTM so glyph
//! and image rectangles stay in page space. Nested forms are followed to a
//! depth of sixteen levels, with a cycle check on the active object ids and a
//! total visit budget. A cycle, a depth or visit overflow, an
//! undecodable form stream or an unsupported nested XObject fails closed with
//! [`RedactionError::UnsupportedForm`] before anything is staged. A rewritten
//! form is stored copy-on-write in its own object; the source page dictionary
//! and the original bytes are never mutated. A form whose placement does not
//! intersect a mark is not read or rewritten. Form vector graphics are outside
//! this pass, exactly as they are for page content: only text and image
//! XObjects are removed.
//!
//! With [`RedactionRequest::keep_images`] set to `true`, an intersecting form is
//! not recursed into: it becomes a `<form XObject ...>` survivor and `clean` is
//! false, so an opt-out can never produce a silently clean result.
//!
//! # Image XObjects
//!
//! With the default [`RedactionRequest::keep_images`] of `false`, an image
//! XObject whose placement intersects a mark is rewritten: supported 8-bit
//! DeviceGray, DeviceRGB, Indexed and DCTDecode JPEG samples are decoded, the
//! mark is mapped into pixel space through the inverse image transform, the
//! covered pixels are filled with opaque black (and the matching `/SMask` alpha
//! samples with 255), and the samples are re-encoded as Flate. The old
//! compressed bytes are replaced in the same object, so a full save cannot
//! retain them. A `true` value is an explicit opt-out: intersecting images are
//! reported as surviving spans and `clean` is false.
//!
//! Unsupported images (JPX, CCITTFax, JBIG2, unknown filters, non-8-bit,
//! Separation/DeviceN, CMYK without a JPEG conversion, ICCBased with more than
//! four components, `/ImageMask`, `/Decode`) fail closed with
//! [`RedactionError::UnsupportedImage`] when `keep_images` is false. Dimensions
//! above 100 megapixels or a decoded sample buffer above 64 MiB fail with
//! [`RedactionError::ImageLimitExceeded`]. No image stream is staged when an
//! error is returned.
//!
//! # Annotations, fields and metadata
//!
//! Annotations whose `/Rect` intersects a mark are removed from `/Annots`, and
//! the annotation object plus every indirect appearance stream it references is
//! replaced with `null`. Links are removed when [`RedactionRequest::remove_links`]
//! is set; form widgets when [`RedactionRequest::remove_form_fields`] is set.
//! Removed widgets are also filtered out of `/AcroForm /Fields` and
//! `/AcroForm /CO`, and emptied field objects are nulled. When
//! [`RedactionRequest::remove_metadata`] is set, the six identifying `/Info`
//! strings are cleared and the catalog `/Metadata` XMP stream is dropped.
//! Broader sanitisation (thumbnails, optional content, revision remnants) is
//! T-0257's job.
//!
//! # Verification
//!
//! [`verify_redaction`] saves a full rewrite, reparses it, and checks three
//! independent surfaces: extracted page text, every stream's stored and
//! Flate-decompressed bytes (UTF-8 and UTF-16LE), and the surviving annotation
//! and form-field rectangles. It fails closed: any survivor or unreadable
//! Flate stream makes `clean` false.

use std::collections::{BTreeMap, BTreeSet};

use pdfxi_core::{Dictionary, Document, Filter, FlateParams, Object, ObjectId, decode_stream};
use thiserror::Error;

use crate::content_edit::{
    FontInfo, InlineImage, Matrix, Operand, TextEditError, load_fonts, tokenize,
};
use crate::geometry::{GeometryError, Rect};

/// A page-scoped rectangle to redact.
#[derive(Clone, Debug, PartialEq)]
pub struct RedactionMark {
    /// Zero-based page index.
    pub page: usize,
    /// Rectangle in default user space.
    pub rect: Rect,
}

impl RedactionMark {
    /// Construct a mark for `page` covering `rect`.
    pub fn new(page: usize, rect: Rect) -> Self {
        Self { page, rect }
    }
}

/// What to remove and which secondary content to clean up.
///
/// The flags are independent: a caller can remove ordinary annotations while
/// keeping links and form widgets, or remove only links.
#[derive(Clone, Debug, PartialEq)]
pub struct RedactionRequest {
    /// Rectangles to redact. An empty list is a no-op.
    pub marks: Vec<RedactionMark>,
    /// `false` (the default) redacts intersecting image pixels. `true` is an
    /// explicit opt-out: an image whose placement overlaps a mark is reported
    /// as a surviving span and `clean` is false. Unsupported images only fail
    /// closed when this is `false`.
    pub keep_images: bool,
    /// Remove intersecting annotations other than links and widgets.
    pub remove_annotations: bool,
    /// Remove intersecting `/Link` annotations and their destinations.
    pub remove_links: bool,
    /// Remove intersecting `/Widget` annotations and their form fields.
    pub remove_form_fields: bool,
    /// Clear identifying `/Info` strings and the XMP metadata stream.
    pub remove_metadata: bool,
}

impl Default for RedactionRequest {
    fn default() -> Self {
        Self {
            marks: Vec::new(),
            keep_images: false,
            remove_annotations: true,
            remove_links: true,
            remove_form_fields: true,
            remove_metadata: false,
        }
    }
}

/// One text-showing operator that lost at least one glyph.
#[derive(Clone, Debug, PartialEq)]
pub struct RemovedTextRun {
    /// Zero-based page index.
    pub page: usize,
    /// Decoded text of the affected operator.
    pub text: String,
    /// Number of glyphs removed.
    pub removed_glyphs: usize,
    /// Number of glyphs kept and re-emitted.
    pub kept_glyphs: usize,
    /// `true` when the whole operator was neutralised rather than split.
    pub whole: bool,
    /// `true` when the fallback (whole-run removal) was used because advances
    /// could not be represented safely.
    pub fallback: bool,
}

/// A text span that still overlaps a mark after rewriting.
///
/// A correct application produces none; the field exists so a caller can see
/// what the internal post-rewrite scan found without trusting a boolean.
#[derive(Clone, Debug, PartialEq)]
pub struct SurvivingSpan {
    /// Zero-based page index.
    pub page: usize,
    /// Decoded text of the surviving span.
    pub text: String,
    /// Bounding rectangle of the surviving span.
    pub rect: Rect,
}

/// Deterministic result of [`apply_redaction`].
#[derive(Clone, Debug, PartialEq)]
pub struct RedactionReport {
    /// The marks that were applied, in request order.
    pub marks: Vec<RedactionMark>,
    /// Text operators that lost glyphs.
    pub removed_runs: Vec<RemovedTextRun>,
    /// Number of annotation objects removed from `/Annots`.
    pub removed_annotations: usize,
    /// Number of link annotations removed.
    pub removed_links: usize,
    /// Number of form fields removed from `/AcroForm /Fields`.
    pub removed_fields: usize,
    /// Text still inside a mark after rewriting (normally empty).
    pub surviving_spans: Vec<SurvivingSpan>,
    /// `true` when `/Info` or XMP metadata was requested and processed.
    pub metadata_scrubbed: bool,
    /// `true` when the internal post-rewrite scan found no surviving span.
    pub clean: bool,
}

/// The independent surface on which a secret was still observable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SurvivorKind {
    /// The page text extractor still returned the secret.
    ExtractedText,
    /// A raw stored or decompressed stream byte span contained the secret.
    RawStream,
    /// An annotation rectangle still intersects a redaction mark.
    Annotation,
    /// A form-field or widget rectangle still intersects a redaction mark.
    FormField,
    /// A Flate stream could not be decompressed, so it could not be checked.
    UnreadableStream,
}

/// One reason [`RedactionVerification::clean`] is false.
#[derive(Clone, Debug, PartialEq)]
pub struct Survivor {
    /// Which check found it.
    pub kind: SurvivorKind,
    /// Page index, when the survivor is page-scoped.
    pub page: Option<usize>,
    /// Object number of the stream or annotation, when known.
    pub object: Option<ObjectId>,
    /// Byte offset in the saved file for a stream survivor.
    pub stream_offset: Option<usize>,
    /// Human-readable detail naming the secret or the failed check.
    pub detail: String,
}

/// Fail-closed result of [`verify_redaction`].
#[derive(Clone, Debug, PartialEq)]
pub struct RedactionVerification {
    /// `true` only when every check found no survivor.
    pub clean: bool,
    /// Every secret occurrence or unverifiable surface that was found.
    pub survivors: Vec<Survivor>,
    /// Number of secrets that were searched for.
    pub secrets_checked: usize,
}

/// A typed redaction failure.
#[derive(Debug, Error)]
pub enum RedactionError {
    /// The core reader or writer rejected the document.
    #[error(transparent)]
    Core(#[from] pdfxi_core::Error),
    /// The content tokenizer or font model rejected a page.
    #[error(transparent)]
    Content(#[from] TextEditError),
    /// A mark was not a valid rectangle.
    #[error(transparent)]
    Geometry(#[from] GeometryError),
    /// A mark referenced a page that does not exist.
    #[error("redaction mark references page {page}, but the document has {pages} pages")]
    UnknownPage {
        /// Offending zero-based page index.
        page: usize,
        /// Number of pages in the document.
        pages: usize,
    },
    /// The PDF object-number space is exhausted.
    #[error("the PDF object number space is exhausted")]
    ObjectSpaceExhausted,
    /// A page or catalog entry had an unexpected shape.
    #[error("malformed document structure: {0}")]
    MalformedStructure(String),
    /// An intersecting image cannot be redacted safely.
    #[error("unsupported image on page {page}: {detail}")]
    UnsupportedImage {
        /// Zero-based page index.
        page: usize,
        /// Image XObject, when the image is an XObject rather than inline.
        object: Option<ObjectId>,
        /// Stable explanation naming the unsupported filter or layout.
        detail: String,
    },
    /// An intersecting form XObject cannot be recursed into safely.
    #[error("unsupported form XObject on page {page}: {detail}")]
    UnsupportedForm {
        /// Zero-based page index.
        page: usize,
        /// The form XObject that could not be processed.
        object: Option<ObjectId>,
        /// Stable explanation naming the cycle, limit or unsupported content.
        detail: String,
    },
    /// An image exceeded the pixel or decoded-sample budget.
    #[error("image limit exceeded on page {page}: {detail}")]
    ImageLimitExceeded {
        /// Zero-based page index.
        page: usize,
        /// Image XObject, when known.
        object: Option<ObjectId>,
        /// Stable explanation with the measured dimensions.
        detail: String,
    },
}

// ---------------------------------------------------------------------------
// Public entry points
// ---------------------------------------------------------------------------

/// Permanently remove the requested content from `document`.
///
/// Content streams are rewritten in place, annotations and their appearance
/// streams are nulled, and the page dictionary is staged through copy-on-write
/// changes. The caller is expected to write the result with a full
/// [`Document::save`] rewrite; an incremental save would keep earlier bytes and
/// is not a safe redaction output.
///
/// # Errors
///
/// Returns [`RedactionError::UnknownPage`] for an out-of-range mark, a typed
/// core or content error for a document that cannot be read or rewritten, and
/// [`RedactionError::MalformedStructure`] when an annotation, field tree or
/// catalog entry cannot be processed. The function never skips a marked page
/// silently.
pub fn apply_redaction(
    document: &mut Document,
    request: &RedactionRequest,
) -> Result<RedactionReport, RedactionError> {
    let effective = effective_document(document)?;
    let pages = effective.page_count().map_err(RedactionError::Core)?;
    for mark in &request.marks {
        if mark.page >= pages {
            return Err(RedactionError::UnknownPage {
                page: mark.page,
                pages,
            });
        }
    }

    let mut by_page: BTreeMap<usize, Vec<Rect>> = BTreeMap::new();
    for mark in &request.marks {
        by_page.entry(mark.page).or_default().push(mark.rect);
    }

    let mut report = RedactionReport {
        marks: request.marks.clone(),
        removed_runs: Vec::new(),
        removed_annotations: 0,
        removed_links: 0,
        removed_fields: 0,
        surviving_spans: Vec::new(),
        metadata_scrubbed: false,
        clean: true,
    };
    let mut removed_widgets: BTreeSet<ObjectId> = BTreeSet::new();
    let mut image_regions: BTreeMap<ObjectId, (usize, Vec<ImageRegion>)> = BTreeMap::new();
    let mut forms = FormRecursion::default();

    // Phase 1: read and rewrite every marked page in memory. Nothing is staged
    // yet, so an unsupported image or a failed decode returns without a partial
    // mutation of the document.
    let mut page_work = Vec::new();
    for (page_index, page_marks) in &by_page {
        let page = effective.page(*page_index).map_err(RedactionError::Core)?;
        let content = effective
            .page_content(*page_index)
            .map_err(RedactionError::Core)?;
        let fonts = load_fonts(&effective, &page)?;
        let resources = crate::content_edit::page_resources(&effective, &page)?;
        let xobjects = xobject_dictionary(&effective, resources.as_ref())?;
        let content_resources = ContentResources {
            fonts: &fonts,
            xobjects: xobjects.as_ref(),
        };
        let outcome = rewrite_content(
            *page_index,
            &effective,
            &content,
            &content_resources,
            Matrix::identity(),
            page_marks,
            request.keep_images,
            &mut image_regions,
            &mut forms,
        )?;
        report
            .removed_runs
            .extend(outcome.removed_runs.iter().cloned());
        report
            .surviving_spans
            .extend(outcome.surviving_spans.iter().cloned());
        page_work.push(PageWork {
            page_index: *page_index,
            page,
            page_marks: page_marks.clone(),
            outcome,
        });
    }
    report
        .removed_runs
        .extend(forms.removed_runs.iter().cloned());
    report
        .surviving_spans
        .extend(forms.surviving_spans.iter().cloned());

    // Decode and fill every intersecting image before staging any page change.
    let image_edits = prepare_image_edits(&effective, &image_regions)?;

    // Phase 2: stage the already-validated page content, image objects and
    // rewritten forms.
    for edit in image_edits {
        document.set_object(edit.object, edit.base)?;
        if let Some((id, stream)) = edit.smask {
            document.set_object(id, stream)?;
        }
    }

    for (id, entry) in &forms.store {
        if !entry.changed {
            continue;
        }
        let mut dictionary = entry.dictionary.clone();
        dictionary.remove("Filter");
        dictionary.remove("DecodeParms");
        dictionary.remove("Length");
        document.set_object(
            *id,
            Object::Stream {
                dictionary,
                data: entry.content.clone(),
            },
        )?;
    }

    for work in &page_work {
        let mut contents_change = None;
        if work.outcome.changed {
            let first = work.page.contents.first().copied().ok_or_else(|| {
                RedactionError::MalformedStructure(
                    "a modified page has no /Contents stream".to_owned(),
                )
            })?;
            document.set_object(
                first,
                Object::Stream {
                    dictionary: Dictionary::new(),
                    data: work.outcome.bytes.clone(),
                },
            )?;
            for other in work.page.contents.iter().skip(1) {
                document.set_object(*other, Object::Null)?;
            }
            contents_change = Some(first);
        }

        let annots_change = remove_intersecting_annotations(
            document,
            &effective,
            work.page_index,
            &work.page_marks,
            request,
            &mut report,
            &mut removed_widgets,
        )?;

        // Every page in `page_work` carries at least one redaction mark. Its
        // `/Thumb` is a pre-rendered image of the page before the redaction,
        // and leaving it in place can reveal the redacted content in any
        // viewer that draws page thumbnails, so drop it even when the content
        // stream itself did not change.
        stage_page_dictionary(
            document,
            &effective,
            work.page.id,
            contents_change,
            annots_change,
        )?;
    }

    if request.remove_form_fields && !removed_widgets.is_empty() {
        remove_form_fields(document, &effective, &removed_widgets, &mut report)?;
    }

    if request.remove_metadata {
        scrub_metadata(document, &effective)?;
        report.metadata_scrubbed = true;
    }

    report.clean = report.surviving_spans.is_empty();
    Ok(report)
}

/// Prove that `secrets` are no longer recoverable from `document`.
///
/// The document is saved as a full rewrite and the saved bytes are reparsed, so
/// the checks see exactly what a caller would receive. Every page's extracted
/// text is searched case-insensitively; every stream's stored bytes and, for
/// Flate streams, its decompressed bytes are searched in UTF-8 and UTF-16LE;
/// and every annotation or form-field rectangle is checked against the marks in
/// `report`. A Flate stream that cannot be decompressed is reported as a
/// survivor rather than ignored.
///
/// # Errors
///
/// Returns a typed core or content error when the document cannot be saved or
/// reparsed.
pub fn verify_redaction(
    document: &Document,
    secrets: &[&str],
    report: &RedactionReport,
) -> Result<RedactionVerification, RedactionError> {
    let saved_bytes = document.save().map_err(RedactionError::Core)?;
    let saved = Document::from_bytes(saved_bytes.clone()).map_err(RedactionError::Core)?;
    let mut survivors = Vec::new();

    for page_index in 0..saved.page_count().map_err(RedactionError::Core)? {
        let text = saved.page_text(page_index).map_err(RedactionError::Core)?;
        let folded = text.to_lowercase();
        for secret in secrets {
            if !secret.is_empty() && folded.contains(&secret.to_lowercase()) {
                survivors.push(Survivor {
                    kind: SurvivorKind::ExtractedText,
                    page: Some(page_index),
                    object: None,
                    stream_offset: None,
                    detail: format!("page {page_index} text still contains {secret:?}"),
                });
            }
        }
    }

    for secret in secrets {
        if secret.is_empty() {
            continue;
        }
        // Only the UTF-8 form is scanned over the whole file. A raw UTF-16
        // match is not evidence by itself: font /ToUnicode CMaps legitimately
        // contain UTF-16 ranges such as 0041 0042 0043..., and the same bytes
        // also match the UTF-16LE needle one byte later. Text-bearing streams
        // are scanned for both UTF-16 encodings below, and form values and
        // bookmark titles are decoded structurally.
        if let Some(offset) = find_subslice(&saved_bytes, secret.as_bytes()) {
            survivors.push(Survivor {
                kind: SurvivorKind::RawStream,
                page: None,
                object: None,
                stream_offset: Some(offset),
                detail: format!("saved file still contains {secret:?} as UTF-8"),
            });
        }
    }

    for region in stream_regions(&saved_bytes) {
        if !region.text_candidate {
            continue;
        }
        for secret in secrets {
            if secret.is_empty() {
                continue;
            }
            let needles = [secret.as_bytes().to_vec(), utf16le(secret), utf16be(secret)];
            let mut found = false;
            for needle in &needles {
                if contains_bytes(&region.stored, needle) {
                    survivors.push(Survivor {
                        kind: SurvivorKind::RawStream,
                        page: None,
                        object: ObjectId::new(region.object_number, 0).into(),
                        stream_offset: Some(region.data_start),
                        detail: format!("stored stream bytes still contain {secret:?}"),
                    });
                    found = true;
                    break;
                }
            }
            if found {
                continue;
            }
            match &region.decoded {
                Some(decoded) => {
                    for needle in &needles {
                        if contains_bytes(decoded, needle) {
                            survivors.push(Survivor {
                                kind: SurvivorKind::RawStream,
                                page: None,
                                object: ObjectId::new(region.object_number, 0).into(),
                                stream_offset: Some(region.data_start),
                                detail: format!(
                                    "decompressed stream bytes still contain {secret:?}"
                                ),
                            });
                            break;
                        }
                    }
                }
                None if region.flate => survivors.push(Survivor {
                    kind: SurvivorKind::UnreadableStream,
                    page: None,
                    object: ObjectId::new(region.object_number, 0).into(),
                    stream_offset: Some(region.data_start),
                    detail: "a Flate stream could not be decompressed".to_owned(),
                }),
                None => {}
            }
        }
    }

    let page_ids = page_id_map(&saved)?;
    for page_index in 0..saved.page_count().map_err(RedactionError::Core)? {
        let marks = marks_for_page(report, page_index);
        if marks.is_empty() {
            continue;
        }
        let page = saved.page(page_index).map_err(RedactionError::Core)?;
        let dictionary = dictionary_of(&saved, page.id)?;
        for annot in annotation_references(&saved, &dictionary)? {
            let Some(rect) = dictionary_of(&saved, annot)
                .ok()
                .and_then(|dictionary| rect_from_dictionary(&dictionary))
            else {
                continue;
            };
            if marks.iter().any(|mark| rect.intersects(mark)) {
                survivors.push(Survivor {
                    kind: SurvivorKind::Annotation,
                    page: Some(page_index),
                    object: Some(annot),
                    stream_offset: None,
                    detail: "an annotation rectangle still intersects a mark".to_owned(),
                });
            }
        }
    }

    check_surviving_fields(&saved, &page_ids, report, secrets, &mut survivors)?;
    check_outline_titles(&saved, secrets, &mut survivors);

    Ok(RedactionVerification {
        clean: survivors.is_empty(),
        survivors,
        secrets_checked: secrets.iter().filter(|secret| !secret.is_empty()).count(),
    })
}

// ---------------------------------------------------------------------------
// Effective document and page staging
// ---------------------------------------------------------------------------

/// Reparse the staged document so every read sees the caller's pending changes.
fn effective_document(document: &Document) -> Result<Document, RedactionError> {
    let bytes = match document.save_incremental() {
        Ok(bytes) => bytes,
        Err(_) => document.save()?,
    };
    Ok(Document::from_bytes(bytes)?)
}

/// Stage one page dictionary with the content and annotation changes computed
/// from the same effective snapshot.
fn stage_page_dictionary(
    document: &mut Document,
    effective: &Document,
    page_id: ObjectId,
    contents: Option<ObjectId>,
    annots: Option<Option<Object>>,
) -> Result<(), RedactionError> {
    let mut dictionary = dictionary_of(effective, page_id)?;
    if let Some(first) = contents {
        dictionary.insert("Contents".to_owned(), Object::Reference(first));
    }
    if let Some(annots) = annots {
        match annots {
            Some(array) => {
                dictionary.insert("Annots".to_owned(), array);
            }
            None => {
                dictionary.remove("Annots");
            }
        }
    }
    // The thumbnail is an image of the page as it looked before redaction.
    // Remove the entry and null its stream so the payload is gone too.
    if let Some(thumb) = dictionary.get("Thumb")
        && let Some(id) = thumb.as_reference()
    {
        document.set_object(id, Object::Null)?;
    }
    dictionary.remove("Thumb");
    document.set_object(page_id, Object::Dictionary(dictionary))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Content rewrite
// ---------------------------------------------------------------------------

/// Result of rewriting one page's content stream.
struct RewriteOutcome {
    bytes: Vec<u8>,
    changed: bool,
    removed_runs: Vec<RemovedTextRun>,
    surviving_spans: Vec<SurvivingSpan>,
}

/// One page whose content has been rewritten but not staged.
struct PageWork {
    page_index: usize,
    page: pdfxi_core::Page,
    page_marks: Vec<Rect>,
    outcome: RewriteOutcome,
}

/// A mark mapped into an image's normalised [0,1] unit square.
#[derive(Clone, Copy, Debug)]
struct ImageRegion {
    u0: f64,
    v0: f64,
    u1: f64,
    v1: f64,
}

/// A decoded and re-encoded image ready to stage.
struct ImageEdit {
    object: ObjectId,
    base: Object,
    smask: Option<(ObjectId, Object)>,
}

/// The current text and graphics state while scanning a content stream.
#[derive(Clone)]
struct State {
    ctm: Matrix,
    font: String,
    font_size: f64,
    leading: f64,
    char_spacing: f64,
    word_spacing: f64,
    horizontal_scale: f64,
}

impl State {
    fn new() -> Self {
        Self {
            ctm: Matrix::identity(),
            font: String::new(),
            font_size: 0.0,
            leading: 0.0,
            char_spacing: 0.0,
            word_spacing: 0.0,
            horizontal_scale: 1.0,
        }
    }
}

/// One drawable glyph or a `TJ` numeric adjustment.
enum Piece {
    Glyph(Glyph),
    Number(f64),
}

/// One glyph with its encoded bytes, advance and page-space bounds.
struct Glyph {
    bytes: Vec<u8>,
    advance: f64,
    rect: Rect,
    removed: bool,
    text: String,
}

/// Glyph analysis for one text-showing operator.
struct Analysis {
    pieces: Vec<Piece>,
    text: String,
    removed: usize,
    total: usize,
    fallback: bool,
    advance_ok: bool,
}

/// Fonts and XObjects that name the resources of one content stream.
struct ContentResources<'a> {
    fonts: &'a BTreeMap<String, FontInfo>,
    xobjects: Option<&'a Dictionary>,
}

/// One form XObject whose content is being (or has been) rewritten.
struct FormEntry {
    dictionary: Dictionary,
    content: Vec<u8>,
    changed: bool,
}

/// Shared recursion state for form XObjects across every marked page.
#[derive(Default)]
struct FormRecursion {
    active: Vec<ObjectId>,
    visits: usize,
    store: BTreeMap<ObjectId, FormEntry>,
    removed_runs: Vec<RemovedTextRun>,
    surviving_spans: Vec<SurvivingSpan>,
}

/// Maximum form XObject nesting depth followed from one page.
const MAX_FORM_DEPTH: usize = 16;
/// Maximum form XObject visits (including repeats) in one redaction call.
const MAX_FORM_VISITS: usize = 512;

/// Tokenize, scan and rewrite one content stream.
#[allow(clippy::too_many_arguments)]
fn rewrite_content(
    page_index: usize,
    effective: &Document,
    content: &[u8],
    resources: &ContentResources<'_>,
    initial_ctm: Matrix,
    marks: &[Rect],
    keep_images: bool,
    image_regions: &mut BTreeMap<ObjectId, (usize, Vec<ImageRegion>)>,
    forms: &mut FormRecursion,
) -> Result<RewriteOutcome, RedactionError> {
    let operators = tokenize(content)?;
    let mut out = Vec::with_capacity(content.len());
    let mut cursor = 0usize;
    let mut changed = false;
    let mut removed_runs = Vec::new();
    let mut surviving_spans = Vec::new();
    let mut state = State::new();
    state.ctm = initial_ctm;
    let mut saved = Vec::new();
    let mut text_matrix = Matrix::identity();
    let mut line_matrix = Matrix::identity();

    for operator in &operators {
        let mut replacement: Option<Vec<u8>> = None;
        let mut replaced_span: Option<(usize, usize)> = None;
        let mut pending_entry: Option<RemovedTextRun> = None;
        match operator.name.as_str() {
            "q" => saved.push(state.clone()),
            "Q" => {
                if let Some(previous) = saved.pop() {
                    state = previous;
                }
            }
            "cm" => {
                if let Some(matrix) = matrix_from(&operator.operands) {
                    state.ctm = Matrix::multiply(matrix, state.ctm);
                }
            }
            "Do" => {
                if let Some(Operand::Name(name)) = operator.operands.last()
                    && let Some(span) = handle_xobject(
                        page_index,
                        effective,
                        resources,
                        name,
                        state.ctm,
                        marks,
                        keep_images,
                        image_regions,
                        forms,
                    )?
                {
                    surviving_spans.push(span);
                }
            }
            "BI" => {
                if let Some(inline) = operator.inline.as_ref() {
                    match handle_inline_image(
                        page_index,
                        effective,
                        content,
                        inline,
                        state.ctm,
                        marks,
                        keep_images,
                    )? {
                        InlineOutcome::Untouched => {}
                        InlineOutcome::Survivor(span) => surviving_spans.push(span),
                        InlineOutcome::Edit(bytes) => {
                            replacement = Some(bytes);
                            replaced_span = Some((operator.start, inline.object_end));
                        }
                    }
                } else if let Some(span) =
                    inline_without_span(page_index, state.ctm, marks, keep_images)?
                {
                    surviving_spans.push(span);
                }
            }
            "BT" => {
                text_matrix = Matrix::identity();
                line_matrix = Matrix::identity();
            }
            "Tf" => {
                if let Some(Operand::Name(name)) = operator.operands.first() {
                    state.font = name.clone();
                }
                if let Some(size) = operator.operands.get(1).and_then(Operand::as_number) {
                    state.font_size = size;
                }
            }
            "Td" | "TD" => {
                let tx = operator
                    .operands
                    .first()
                    .and_then(Operand::as_number)
                    .unwrap_or(0.0);
                let ty = operator
                    .operands
                    .get(1)
                    .and_then(Operand::as_number)
                    .unwrap_or(0.0);
                if operator.name == "TD" {
                    state.leading = -ty;
                }
                line_matrix = Matrix::multiply(Matrix::translate(tx, ty), line_matrix);
                text_matrix = line_matrix;
            }
            "Tm" => {
                if let Some(matrix) = matrix_from(&operator.operands) {
                    text_matrix = matrix;
                    line_matrix = matrix;
                }
            }
            "T*" => {
                line_matrix = Matrix::multiply(Matrix::translate(0.0, -state.leading), line_matrix);
                text_matrix = line_matrix;
            }
            "TL" => {
                if let Some(leading) = operator.operands.first().and_then(Operand::as_number) {
                    state.leading = leading;
                }
            }
            "Tc" => {
                if let Some(value) = operator.operands.first().and_then(Operand::as_number) {
                    state.char_spacing = value;
                }
            }
            "Tw" => {
                if let Some(value) = operator.operands.first().and_then(Operand::as_number) {
                    state.word_spacing = value;
                }
            }
            "Tz" => {
                if let Some(value) = operator.operands.first().and_then(Operand::as_number) {
                    state.horizontal_scale = value / 100.0;
                }
            }
            "Tj" | "TJ" => {
                if let Some(text) = text_operand(&operator.operands) {
                    let analysis = analyze_text(
                        text,
                        resources.fonts.get(&state.font),
                        &state,
                        text_matrix,
                        marks,
                    )?;
                    advance_analysis(&analysis, &state, &mut text_matrix);
                    if let Some((bytes, entry)) = reemit(&analysis, &state, None)? {
                        replacement = Some(bytes);
                        pending_entry = Some(entry);
                    }
                }
            }
            "'" => {
                line_matrix = Matrix::multiply(Matrix::translate(0.0, -state.leading), line_matrix);
                text_matrix = line_matrix;
                if let Some(text) = text_operand(&operator.operands) {
                    let analysis = analyze_text(
                        text,
                        resources.fonts.get(&state.font),
                        &state,
                        text_matrix,
                        marks,
                    )?;
                    advance_analysis(&analysis, &state, &mut text_matrix);
                    if let Some((bytes, entry)) = reemit(&analysis, &state, Some(Quote::Single))? {
                        replacement = Some(bytes);
                        pending_entry = Some(entry);
                    }
                }
            }
            "\"" => {
                if operator.operands.len() >= 3 {
                    if let Some(value) = operator.operands[0].as_number() {
                        state.word_spacing = value;
                    }
                    if let Some(value) = operator.operands[1].as_number() {
                        state.char_spacing = value;
                    }
                }
                line_matrix = Matrix::multiply(Matrix::translate(0.0, -state.leading), line_matrix);
                text_matrix = line_matrix;
                if let Some(text) = text_operand(&operator.operands) {
                    let analysis = analyze_text(
                        text,
                        resources.fonts.get(&state.font),
                        &state,
                        text_matrix,
                        marks,
                    )?;
                    advance_analysis(&analysis, &state, &mut text_matrix);
                    if let Some((bytes, entry)) = reemit(&analysis, &state, Some(Quote::Double))? {
                        replacement = Some(bytes);
                        pending_entry = Some(entry);
                    }
                }
            }
            _ => {}
        }

        if let Some(replacement) = replacement {
            let (start, end) = replaced_span.unwrap_or((operator.start, operator.end));
            out.extend_from_slice(&content[cursor..start]);
            if replaced_span.is_none() {
                // Text replacements need separator tokens; an inline image is
                // emitted as a complete, self-delimited BI..EI object.
                out.push(b' ');
            }
            out.extend_from_slice(&replacement);
            if replaced_span.is_none() {
                out.push(b'\n');
            }
            cursor = end;
            changed = true;
            if let Some(mut entry) = pending_entry {
                entry.page = page_index;
                removed_runs.push(entry);
            }
        }
    }
    out.extend_from_slice(&content[cursor..]);

    let bytes = if changed { out } else { content.to_vec() };
    if changed {
        surviving_spans.extend(scan_survivors(
            page_index,
            &bytes,
            resources.fonts,
            initial_ctm,
            marks,
        )?);
    }
    Ok(RewriteOutcome {
        bytes,
        changed,
        removed_runs,
        surviving_spans,
    })
}

/// Quote operators that carry an implicit line move.
#[derive(Clone, Copy)]
enum Quote {
    Single,
    Double,
}

/// Return the text operand of `Tj`, `'`, `"` or the array of `TJ`.
fn text_operand(operands: &[Operand]) -> Option<&Operand> {
    operands.last()
}

/// Analyse one text operand into glyphs and positions.
fn analyze_text(
    operand: &Operand,
    font: Option<&FontInfo>,
    state: &State,
    text_matrix: Matrix,
    marks: &[Rect],
) -> Result<Analysis, RedactionError> {
    let mut pieces = Vec::new();
    let mut text = String::new();
    let mut removed = 0usize;
    let mut total = 0usize;
    let mut fallback = false;
    let mut advance_ok = true;
    let matrix = Matrix::multiply(text_matrix, state.ctm);
    let size = state.font_size;
    let factor = size * state.horizontal_scale;
    let mut cursor = matrix;

    match operand {
        Operand::Text(bytes) => {
            let _ = accumulate_text(
                bytes,
                font,
                state,
                cursor,
                size,
                marks,
                &mut pieces,
                &mut text,
                &mut removed,
                &mut total,
                &mut fallback,
                &mut advance_ok,
            );
        }
        Operand::Array(items) => {
            for item in items {
                match item {
                    Operand::Text(bytes) => {
                        cursor = accumulate_text(
                            bytes,
                            font,
                            state,
                            cursor,
                            size,
                            marks,
                            &mut pieces,
                            &mut text,
                            &mut removed,
                            &mut total,
                            &mut fallback,
                            &mut advance_ok,
                        );
                    }
                    Operand::Number(number) => {
                        pieces.push(Piece::Number(*number));
                        if !number.is_finite() {
                            advance_ok = false;
                        }
                        let shift = -number / 1000.0 * state.font_size * state.horizontal_scale;
                        cursor = Matrix::multiply(Matrix::translate(shift, 0.0), cursor);
                    }
                    _ => {}
                }
            }
        }
        _ => {
            return Err(RedactionError::Content(TextEditError::MalformedContent(
                "text operator has no string operand".to_owned(),
            )));
        }
    }

    Ok(Analysis {
        pieces,
        text,
        removed,
        total,
        fallback,
        advance_ok: advance_ok && factor.abs() > 0.0,
    })
}

/// Append one encoded text string's glyphs to the analysis.
#[allow(clippy::too_many_arguments)]
fn accumulate_text(
    bytes: &[u8],
    font: Option<&FontInfo>,
    state: &State,
    mut cursor: Matrix,
    size: f64,
    marks: &[Rect],
    pieces: &mut Vec<Piece>,
    text: &mut String,
    removed: &mut usize,
    total: &mut usize,
    fallback: &mut bool,
    advance_ok: &mut bool,
) -> Matrix {
    if let Some(font) = font {
        text.push_str(&font.decode(bytes));
        let mut offset = 0usize;
        for (code, length) in font.code_units(bytes) {
            let start = offset.min(bytes.len());
            let end = (start + length).min(bytes.len());
            offset = end;
            let piece = &bytes[start..end];
            let advance = glyph_advance(font, code, state);
            if !advance.is_finite() {
                *advance_ok = false;
            }
            let rect = glyph_rect(cursor, size, advance);
            let is_removed = marks.iter().any(|mark| rect.intersects(mark));
            if is_removed {
                *removed += 1;
            }
            *total += 1;
            pieces.push(Piece::Glyph(Glyph {
                bytes: piece.to_vec(),
                advance,
                rect,
                removed: is_removed,
                text: font.decode(piece),
            }));
            cursor = Matrix::multiply(Matrix::translate(advance, 0.0), cursor);
        }
    } else {
        *fallback = true;
        let nominal = (size.abs().max(1.0)) * 0.5;
        for byte in bytes {
            let rect = glyph_rect(cursor, size, nominal);
            let is_removed = marks.iter().any(|mark| rect.intersects(mark));
            if is_removed {
                *removed += 1;
            }
            *total += 1;
            pieces.push(Piece::Glyph(Glyph {
                bytes: vec![*byte],
                advance: nominal,
                rect,
                removed: is_removed,
                text: char::from(*byte).to_string(),
            }));
            cursor = Matrix::multiply(Matrix::translate(nominal, 0.0), cursor);
        }
        text.push_str(&String::from_utf8_lossy(bytes));
    }
    cursor
}

/// Advance `text_matrix` over the analysed pieces exactly as the scanner does.
fn advance_analysis(analysis: &Analysis, state: &State, text_matrix: &mut Matrix) {
    for piece in &analysis.pieces {
        match piece {
            Piece::Glyph(glyph) => {
                *text_matrix =
                    Matrix::multiply(Matrix::translate(glyph.advance, 0.0), *text_matrix);
            }
            Piece::Number(number) => {
                let shift = -number / 1000.0 * state.font_size * state.horizontal_scale;
                *text_matrix = Matrix::multiply(Matrix::translate(shift, 0.0), *text_matrix);
            }
        }
    }
}

/// Build the replacement operator for an analysed text-showing operator.
///
/// Returns `None` when nothing needs to change. When part of the operator is
/// marked and advances are trustworthy, only the marked glyphs are dropped;
/// otherwise the whole operator is neutralised with an adjustment-only `TJ`.
fn reemit(
    analysis: &Analysis,
    state: &State,
    quote: Option<Quote>,
) -> Result<Option<(Vec<u8>, RemovedTextRun)>, RedactionError> {
    if analysis.removed == 0 {
        return Ok(None);
    }
    let can_split = !analysis.fallback && analysis.advance_ok && analysis.removed < analysis.total;
    let whole = !can_split || analysis.removed == analysis.total;
    let fallback = analysis.fallback || (!can_split && analysis.removed < analysis.total);

    let mut pieces = Vec::with_capacity(analysis.pieces.len());
    for piece in &analysis.pieces {
        match piece {
            Piece::Glyph(glyph) => pieces.push(Piece::Glyph(Glyph {
                bytes: glyph.bytes.clone(),
                advance: glyph.advance,
                rect: glyph.rect,
                removed: if can_split { glyph.removed } else { true },
                text: glyph.text.clone(),
            })),
            Piece::Number(number) => pieces.push(Piece::Number(*number)),
        }
    }

    let mut out = Vec::new();
    if let Some(quote) = quote {
        match quote {
            Quote::Single => out.extend_from_slice(b"T* "),
            Quote::Double => {
                // Re-emit the spacing operands captured by the scanner.
                out.extend_from_slice(
                    format!(
                        "{} Tw {} Tc T* ",
                        format_number(state.word_spacing),
                        format_number(state.char_spacing)
                    )
                    .as_bytes(),
                );
            }
        }
    }
    match build_tj(&pieces, state) {
        Some(tj) => out.extend_from_slice(&tj),
        None => {
            // Even the advance cannot be represented. The line move above is
            // kept; the drawing itself is gone.
            if quote.is_none() {
                out.clear();
            }
        }
    }

    let kept = if whole {
        0
    } else {
        analysis.total.saturating_sub(analysis.removed)
    };
    Ok(Some((
        out,
        RemovedTextRun {
            page: 0,
            text: analysis.text.clone(),
            removed_glyphs: if whole {
                analysis.total
            } else {
                analysis.removed
            },
            kept_glyphs: kept,
            whole,
            fallback,
        },
    )))
}

/// Build a `TJ` array from glyph pieces. Returns `None` when no adjustment can
/// be represented because the font size or horizontal scale is zero.
fn build_tj(pieces: &[Piece], state: &State) -> Option<Vec<u8>> {
    let factor = state.font_size * state.horizontal_scale;
    if factor == 0.0 || !factor.is_finite() {
        return None;
    }
    let mut out = Vec::new();
    out.extend_from_slice(b"[");
    let mut text = Vec::new();
    let mut pending = 0.0f64;
    let mut wrote = false;
    let flush_text = |text: &mut Vec<u8>, out: &mut Vec<u8>| {
        if !text.is_empty() {
            out.push(b' ');
            out.push(b'<');
            for byte in text.iter() {
                out.extend_from_slice(format!("{byte:02X}").as_bytes());
            }
            out.push(b'>');
            text.clear();
        }
    };
    let flush_pending = |pending: &mut f64, out: &mut Vec<u8>| {
        if pending.abs() > 1e-9 {
            out.push(b' ');
            out.extend_from_slice(format_number(*pending).as_bytes());
            *pending = 0.0;
        }
    };
    for piece in pieces {
        match piece {
            Piece::Glyph(glyph) => {
                if glyph.removed {
                    flush_text(&mut text, &mut out);
                    let adjustment = -glyph.advance * 1000.0 / factor;
                    if !adjustment.is_finite() {
                        return None;
                    }
                    pending += adjustment;
                    wrote = true;
                } else {
                    flush_pending(&mut pending, &mut out);
                    text.extend_from_slice(&glyph.bytes);
                    wrote = true;
                }
            }
            Piece::Number(number) => {
                flush_text(&mut text, &mut out);
                pending += number;
                wrote = true;
            }
        }
    }
    flush_text(&mut text, &mut out);
    flush_pending(&mut pending, &mut out);
    out.push(b' ');
    out.extend_from_slice(b"] TJ");
    wrote.then_some(out)
}

/// Compute one glyph's text-space advance.
fn glyph_advance(font: &FontInfo, code: u32, state: &State) -> f64 {
    let mut width = font.width_of_code(code) / 1000.0 * state.font_size;
    width += state.char_spacing;
    if code == 32 {
        width += state.word_spacing;
    }
    width * state.horizontal_scale
}

/// Compute a glyph's page-space bounding box from its text matrix.
fn glyph_rect(matrix: Matrix, font_size: f64, advance: f64) -> Rect {
    let y0 = -0.2 * font_size;
    let y1 = 0.8 * font_size;
    let corners = [
        transform(matrix, 0.0, y0),
        transform(matrix, advance, y0),
        transform(matrix, advance, y1),
        transform(matrix, 0.0, y1),
    ];
    Rect::saturating(
        corners
            .iter()
            .map(|point| point.0 as f32)
            .fold(f32::INFINITY, f32::min),
        corners
            .iter()
            .map(|point| point.1 as f32)
            .fold(f32::INFINITY, f32::min),
        corners
            .iter()
            .map(|point| point.0 as f32)
            .fold(f32::NEG_INFINITY, f32::max),
        corners
            .iter()
            .map(|point| point.1 as f32)
            .fold(f32::NEG_INFINITY, f32::max),
    )
}

/// Apply a matrix to a point.
fn transform(matrix: Matrix, x: f64, y: f64) -> (f64, f64) {
    let values = matrix.0;
    (
        values[0] * x + values[2] * y + values[4],
        values[1] * x + values[3] * y + values[5],
    )
}

/// Build a matrix from the six leading numeric operands.
fn matrix_from(operands: &[Operand]) -> Option<Matrix> {
    if operands.len() < 6 {
        return None;
    }
    let values: Option<Vec<f64>> = operands.iter().take(6).map(Operand::as_number).collect();
    let values = values?;
    Some(Matrix([
        values[0], values[1], values[2], values[3], values[4], values[5],
    ]))
}

/// Re-scan rewritten content for any glyph that still overlaps a mark.
fn scan_survivors(
    page_index: usize,
    content: &[u8],
    fonts: &BTreeMap<String, FontInfo>,
    initial_ctm: Matrix,
    marks: &[Rect],
) -> Result<Vec<SurvivingSpan>, RedactionError> {
    let operators = tokenize(content)?;
    let mut state = State::new();
    state.ctm = initial_ctm;
    let mut saved = Vec::new();
    let mut text_matrix = Matrix::identity();
    let mut line_matrix = Matrix::identity();
    let mut spans = Vec::new();
    for operator in &operators {
        match operator.name.as_str() {
            "q" => saved.push(state.clone()),
            "Q" => {
                if let Some(previous) = saved.pop() {
                    state = previous;
                }
            }
            "cm" => {
                if let Some(matrix) = matrix_from(&operator.operands) {
                    state.ctm = Matrix::multiply(matrix, state.ctm);
                }
            }
            "BT" => {
                text_matrix = Matrix::identity();
                line_matrix = Matrix::identity();
            }
            "Tf" => {
                if let Some(Operand::Name(name)) = operator.operands.first() {
                    state.font = name.clone();
                }
                if let Some(size) = operator.operands.get(1).and_then(Operand::as_number) {
                    state.font_size = size;
                }
            }
            "Td" | "TD" => {
                let tx = operator
                    .operands
                    .first()
                    .and_then(Operand::as_number)
                    .unwrap_or(0.0);
                let ty = operator
                    .operands
                    .get(1)
                    .and_then(Operand::as_number)
                    .unwrap_or(0.0);
                if operator.name == "TD" {
                    state.leading = -ty;
                }
                line_matrix = Matrix::multiply(Matrix::translate(tx, ty), line_matrix);
                text_matrix = line_matrix;
            }
            "Tm" => {
                if let Some(matrix) = matrix_from(&operator.operands) {
                    text_matrix = matrix;
                    line_matrix = matrix;
                }
            }
            "T*" => {
                line_matrix = Matrix::multiply(Matrix::translate(0.0, -state.leading), line_matrix);
                text_matrix = line_matrix;
            }
            "TL" => {
                if let Some(leading) = operator.operands.first().and_then(Operand::as_number) {
                    state.leading = leading;
                }
            }
            "Tc" => {
                if let Some(value) = operator.operands.first().and_then(Operand::as_number) {
                    state.char_spacing = value;
                }
            }
            "Tw" => {
                if let Some(value) = operator.operands.first().and_then(Operand::as_number) {
                    state.word_spacing = value;
                }
            }
            "Tz" => {
                if let Some(value) = operator.operands.first().and_then(Operand::as_number) {
                    state.horizontal_scale = value / 100.0;
                }
            }
            "Tj" | "TJ" | "'" | "\"" => {
                if operator.name == "\"" && operator.operands.len() >= 3 {
                    if let Some(value) = operator.operands[0].as_number() {
                        state.word_spacing = value;
                    }
                    if let Some(value) = operator.operands[1].as_number() {
                        state.char_spacing = value;
                    }
                }
                if operator.name == "'" || operator.name == "\"" {
                    line_matrix =
                        Matrix::multiply(Matrix::translate(0.0, -state.leading), line_matrix);
                    text_matrix = line_matrix;
                }
                if let Some(operand) = text_operand(&operator.operands) {
                    let analysis =
                        analyze_text(operand, fonts.get(&state.font), &state, text_matrix, marks)?;
                    let mut text = String::new();
                    let mut rect: Option<Rect> = None;
                    for piece in &analysis.pieces {
                        if let Piece::Glyph(glyph) = piece
                            && glyph.removed
                        {
                            text.push_str(&glyph.text);
                            rect = Some(match rect {
                                Some(existing) => existing.union(&glyph.rect),
                                None => glyph.rect,
                            });
                        }
                    }
                    if let Some(rect) = rect {
                        spans.push(SurvivingSpan {
                            page: page_index,
                            text,
                            rect,
                        });
                    }
                    advance_analysis(&analysis, &state, &mut text_matrix);
                }
            }
            _ => {}
        }
    }
    Ok(spans)
}

// ---------------------------------------------------------------------------
// Image XObject redaction
// ---------------------------------------------------------------------------

/// Return the page's `/Resources /XObject` dictionary, when present.
fn xobject_dictionary(
    effective: &Document,
    resources: Option<&Object>,
) -> Result<Option<Dictionary>, RedactionError> {
    let Some(resources) = resources.and_then(Object::as_dictionary) else {
        return Ok(None);
    };
    match crate::content_edit::resolve(effective, resources.get("XObject"))? {
        Some(Object::Dictionary(dictionary)) => Ok(Some(dictionary)),
        _ => Ok(None),
    }
}

/// Handle one `Do` XObject placement.
///
/// A supported image under a mark is scheduled for pixel redaction; a form is
/// recursed into; any other intersecting XObject — a direct object, an
/// unresolvable reference or a missing name — fails closed instead of being
/// silently skipped.
#[allow(clippy::too_many_arguments)]
fn handle_xobject(
    page_index: usize,
    effective: &Document,
    resources: &ContentResources<'_>,
    name: &str,
    ctm: Matrix,
    marks: &[Rect],
    keep_images: bool,
    image_regions: &mut BTreeMap<ObjectId, (usize, Vec<ImageRegion>)>,
    forms: &mut FormRecursion,
) -> Result<Option<SurvivingSpan>, RedactionError> {
    let placement = unit_square_rect(ctm);
    let intersects = marks.iter().any(|mark| placement.intersects(mark));
    let unhandled = |object: Option<ObjectId>, detail: String, label: String| {
        // An unresolved or direct XObject has no trustworthy bounding box: the
        // unit square is only the placement of a *resolved* image. When the
        // page is marked, or the CTM cannot be inverted, we cannot prove the
        // name lies outside every mark, so fail closed instead of skipping it.
        let unknown_placement = !marks.is_empty() || invert(ctm).is_none();
        if intersects || unknown_placement {
            xobject_fail_closed(page_index, object, keep_images, placement, detail, label)
        } else {
            Ok(None)
        }
    };
    let Some(xobjects) = resources.xobjects else {
        return unhandled(
            None,
            "the page has no /Resources /XObject dictionary".to_owned(),
            format!("<unresolved XObject {name}>"),
        );
    };
    let Some(value) = xobjects.get(name) else {
        return unhandled(
            None,
            format!("XObject /{name} is not present in the page resources"),
            format!("<unresolved XObject {name}>"),
        );
    };
    let Some(id) = value.as_reference() else {
        return unhandled(
            None,
            format!("XObject /{name} is a direct object and cannot be rewritten"),
            format!("<unresolved XObject {name}>"),
        );
    };
    let Ok(object) = effective.object(id) else {
        return unhandled(
            Some(id),
            format!("XObject /{name} reference cannot be resolved"),
            format!("<unresolved XObject {name}>"),
        );
    };
    let Object::Stream { dictionary, data } = object else {
        return unhandled(
            Some(id),
            format!("XObject /{name} is not a stream"),
            format!("<unresolved XObject {name}>"),
        );
    };
    match dictionary.get("Subtype").and_then(Object::as_name) {
        Some("Image") => {}
        Some("Form") => {
            return handle_form_xobject(
                page_index,
                effective,
                id,
                name,
                &dictionary,
                &data,
                resources,
                ctm,
                marks,
                keep_images,
                image_regions,
                forms,
            );
        }
        Some(other) => {
            return unhandled(
                Some(id),
                format!("XObject /{name} has unsupported subtype /{other}"),
                format!("<unresolved XObject {name}>"),
            );
        }
        None => {
            return unhandled(
                Some(id),
                format!("XObject /{name} has no /Subtype"),
                format!("<unresolved XObject {name}>"),
            );
        }
    }
    if !intersects {
        return Ok(None);
    }
    if keep_images {
        return Ok(Some(SurvivingSpan {
            page: page_index,
            text: format!("<image XObject {name}>"),
            rect: placement,
        }));
    }
    let support = validate_image_support(page_index, Some(id), effective, &dictionary, data.len())?;
    let Some(inverse) = invert(ctm) else {
        return Err(RedactionError::UnsupportedImage {
            page: page_index,
            object: Some(id),
            detail: "image placement transform is not invertible".to_owned(),
        });
    };
    let mut regions = Vec::new();
    for mark in marks {
        if !placement.intersects(mark) {
            continue;
        }
        if let Some(region) = mark_image_region(*mark, inverse) {
            regions.push(region);
        }
    }
    if regions.is_empty() {
        return Ok(None);
    }
    let _ = (support.width, support.height);
    image_regions
        .entry(id)
        .or_insert_with(|| (page_index, Vec::new()))
        .1
        .extend(regions);
    Ok(None)
}

/// Recursively rewrite one intersecting form XObject.
///
/// The form `/Matrix` is composed with the current CTM, so the rewritten content
/// is analysed in page space. A form that is placed more than once is rewritten
/// cumulatively: the second pass starts from the bytes produced by the first.
#[allow(clippy::too_many_arguments)]
fn handle_form_xobject(
    page_index: usize,
    effective: &Document,
    id: ObjectId,
    name: &str,
    dictionary: &Dictionary,
    data: &[u8],
    parent: &ContentResources<'_>,
    ctm: Matrix,
    marks: &[Rect],
    keep_images: bool,
    image_regions: &mut BTreeMap<ObjectId, (usize, Vec<ImageRegion>)>,
    forms: &mut FormRecursion,
) -> Result<Option<SurvivingSpan>, RedactionError> {
    let matrix = form_matrix(effective, page_index, id, dictionary)?;
    let full = Matrix::multiply(matrix, ctm);
    let bbox = form_bbox(effective, dictionary)?;
    let placement = form_placement(full, bbox);
    if let Some(rect) = &placement
        && !marks.iter().any(|mark| rect.intersects(mark))
    {
        return Ok(None);
    }
    // A form with no usable `/BBox` has no trustworthy geometry. It must never
    // be skipped: recurse into it and let its content decide, or fail closed on
    // anything unsupported inside.
    if keep_images {
        return Ok(Some(SurvivingSpan {
            page: page_index,
            text: format!("<form XObject {name}>"),
            rect: placement.unwrap_or_else(|| unit_square_rect(full)),
        }));
    }
    if forms.active.contains(&id) {
        return Err(RedactionError::UnsupportedForm {
            page: page_index,
            object: Some(id),
            detail: "form XObject cycle detected".to_owned(),
        });
    }
    if forms.active.len() >= MAX_FORM_DEPTH {
        return Err(RedactionError::UnsupportedForm {
            page: page_index,
            object: Some(id),
            detail: format!("form nesting exceeds {MAX_FORM_DEPTH} levels"),
        });
    }
    forms.visits = forms.visits.saturating_add(1);
    if forms.visits > MAX_FORM_VISITS {
        return Err(RedactionError::UnsupportedForm {
            page: page_index,
            object: Some(id),
            detail: format!("form XObject budget of {MAX_FORM_VISITS} visits exceeded"),
        });
    }

    // Reuse the current rewritten bytes when a shared form is placed twice, so
    // marks from every placement accumulate instead of the last one winning.
    let (content, dictionary) = match forms.store.get(&id) {
        Some(entry) => (entry.content.clone(), entry.dictionary.clone()),
        None => (
            decode_form_content(page_index, id, dictionary, data)?,
            dictionary.clone(),
        ),
    };

    // A form's own `/Resources` replaces the parent's; without one it inherits
    // the parent page's fonts and XObjects.
    let form_resources = match dictionary.get("Resources") {
        None | Some(Object::Null) => None,
        Some(Object::Reference(resources_id)) => Some(effective.object(*resources_id)?),
        Some(value) => Some(value.clone()),
    };
    let form_fonts = match &form_resources {
        Some(resources) => {
            crate::content_edit::load_fonts_from_resources(effective, Some(resources))?
        }
        None => parent.fonts.clone(),
    };
    let form_xobjects = match &form_resources {
        Some(resources) => xobject_dictionary(effective, Some(resources))?,
        None => parent.xobjects.cloned(),
    };
    let resources = ContentResources {
        fonts: &form_fonts,
        xobjects: form_xobjects.as_ref(),
    };

    forms.active.push(id);
    let outcome = rewrite_content(
        page_index,
        effective,
        &content,
        &resources,
        full,
        marks,
        keep_images,
        image_regions,
        forms,
    );
    forms.active.pop();
    let outcome = outcome?;

    forms.removed_runs.extend(outcome.removed_runs);
    forms.surviving_spans.extend(outcome.surviving_spans);
    let entry = forms.store.entry(id).or_insert_with(|| FormEntry {
        dictionary: dictionary.clone(),
        content: content.clone(),
        changed: false,
    });
    if outcome.changed {
        entry.content = outcome.bytes;
        entry.changed = true;
    }
    Ok(None)
}

/// Decode a form XObject's content stream, failing closed on any filter this
/// pass cannot decode safely.
fn decode_form_content(
    page: usize,
    object: ObjectId,
    dictionary: &Dictionary,
    data: &[u8],
) -> Result<Vec<u8>, RedactionError> {
    let filters = form_content_filters(page, object, dictionary)?;
    if filters.is_empty() {
        return Ok(data.to_vec());
    }
    decode_stream(data, &filters).map_err(|error| RedactionError::UnsupportedForm {
        page,
        object: Some(object),
        detail: format!("form content stream cannot be decoded: {error}"),
    })
}

/// Return the decode filter chain for a form content stream.
///
/// Only an absent filter or a single `FlateDecode` is supported. A multi-filter
/// chain is rejected rather than guessed, because `/DecodeParms` array
/// alignment cannot be validated here.
fn form_content_filters(
    page: usize,
    object: ObjectId,
    dictionary: &Dictionary,
) -> Result<Vec<Filter>, RedactionError> {
    let unsupported = |detail: String| RedactionError::UnsupportedForm {
        page,
        object: Some(object),
        detail,
    };
    match dictionary.get("Filter") {
        None | Some(Object::Null) => Ok(Vec::new()),
        Some(Object::Name(name)) if name == "FlateDecode" => {
            let params = flate_params(dictionary)
                .ok_or_else(|| unsupported("unsupported form /DecodeParms".to_owned()))?;
            Ok(vec![Filter::FlateDecode(params)])
        }
        Some(Object::Name(name)) => Err(unsupported(format!(
            "unsupported form content filter /{name}"
        ))),
        Some(Object::Array(items)) if items.is_empty() => Ok(Vec::new()),
        Some(Object::Array(items)) => match items.as_slice() {
            [Object::Name(name)] if name == "FlateDecode" => {
                let params = flate_params(dictionary)
                    .ok_or_else(|| unsupported("unsupported form /DecodeParms".to_owned()))?;
                Ok(vec![Filter::FlateDecode(params)])
            }
            _ => Err(unsupported(
                "form content filter arrays other than a single FlateDecode are not supported"
                    .to_owned(),
            )),
        },
        Some(_) => Err(unsupported("unsupported form /Filter value".to_owned())),
    }
}

/// Resolve and parse a form `/Matrix`.
///
/// An absent `/Matrix` is the identity, as the specification requires. An
/// indirect value is resolved through the effective document. A present value
/// that is not a finite six-number array fails closed: substituting the
/// identity could silently skip a form that really lies under a mark.
fn form_matrix(
    effective: &Document,
    page: usize,
    object: ObjectId,
    dictionary: &Dictionary,
) -> Result<Matrix, RedactionError> {
    let Some(value) = dictionary.get("Matrix") else {
        return Ok(Matrix::identity());
    };
    let resolved = match value {
        Object::Reference(id) => effective.object(*id).map_err(RedactionError::Core)?,
        other => other.clone(),
    };
    let invalid = || RedactionError::UnsupportedForm {
        page,
        object: Some(object),
        detail: "form /Matrix is present but not a finite six-number array".to_owned(),
    };
    let Some(items) = resolved.as_array() else {
        return Err(invalid());
    };
    if items.len() < 6 {
        return Err(invalid());
    }
    let values: Option<Vec<f64>> = items.iter().take(6).map(Object::as_f64).collect();
    let Some(values) = values else {
        return Err(invalid());
    };
    if !values.iter().all(|value| value.is_finite()) {
        return Err(invalid());
    }
    Ok(Matrix([
        values[0], values[1], values[2], values[3], values[4], values[5],
    ]))
}

/// Resolve a form `/BBox`. Absent, malformed or non-finite values yield `None`,
/// which callers treat as "possibly intersecting" — never as a reason to skip.
fn form_bbox(
    effective: &Document,
    dictionary: &Dictionary,
) -> Result<Option<(f64, f64, f64, f64)>, RedactionError> {
    let Some(value) = dictionary.get("BBox") else {
        return Ok(None);
    };
    let resolved = match value {
        Object::Reference(id) => effective.object(*id).map_err(RedactionError::Core)?,
        other => other.clone(),
    };
    let Some(items) = resolved.as_array() else {
        return Ok(None);
    };
    if items.len() < 4 {
        return Ok(None);
    }
    let values: Option<Vec<f64>> = items.iter().map(Object::as_f64).collect();
    let Some(values) = values else {
        return Ok(None);
    };
    if !values.iter().all(|value| value.is_finite()) {
        return Ok(None);
    }
    Ok(Some((
        values[0].min(values[2]),
        values[1].min(values[3]),
        values[0].max(values[2]),
        values[1].max(values[3]),
    )))
}

/// Page-space placement of a form with a usable `/BBox`. `None` means the
/// geometry is unknown and the form must be treated as possibly intersecting.
fn form_placement(matrix: Matrix, bbox: Option<(f64, f64, f64, f64)>) -> Option<Rect> {
    bbox.map(|(x0, y0, x1, y1)| corners_rect(matrix, x0, y0, x1, y1))
}

/// Fail closed or record a survivor for an intersecting non-image XObject.
fn xobject_fail_closed(
    page: usize,
    object: Option<ObjectId>,
    keep_images: bool,
    rect: Rect,
    detail: String,
    label: String,
) -> Result<Option<SurvivingSpan>, RedactionError> {
    if keep_images {
        Ok(Some(SurvivingSpan {
            page,
            text: label,
            rect,
        }))
    } else {
        Err(RedactionError::UnsupportedImage {
            page,
            object,
            detail,
        })
    }
}

/// Outcome of examining one inline image against the redaction marks.
enum InlineOutcome {
    /// No mark intersects the image; its bytes are kept verbatim.
    Untouched,
    /// `keep_images` opted out; report the survivor.
    Survivor(SurvivingSpan),
    /// A complete, re-encoded `BI ... EI` object replacing the original bytes.
    Edit(Vec<u8>),
}

/// Fail closed for a `BI` whose dictionary/sample span the tokenizer could not
/// retain (defensive: well-formed streams always carry the inline data).
fn inline_without_span(
    page_index: usize,
    ctm: Matrix,
    marks: &[Rect],
    keep_images: bool,
) -> Result<Option<SurvivingSpan>, RedactionError> {
    let placement = unit_square_rect(ctm);
    if !marks.iter().any(|mark| placement.intersects(mark)) {
        return Ok(None);
    }
    if keep_images {
        return Ok(Some(SurvivingSpan {
            page: page_index,
            text: "<inline image>".to_owned(),
            rect: placement,
        }));
    }
    Err(RedactionError::UnsupportedImage {
        page: page_index,
        object: None,
        detail: "inline image has no retained dictionary or sample span".to_owned(),
    })
}

/// Decode and pixel-redact one intersecting inline image, then rebuild its
/// `BI ... EI` bytes in place inside the surrounding content stream.
fn handle_inline_image(
    page_index: usize,
    effective: &Document,
    content: &[u8],
    inline: &InlineImage,
    ctm: Matrix,
    marks: &[Rect],
    keep_images: bool,
) -> Result<InlineOutcome, RedactionError> {
    let placement = unit_square_rect(ctm);
    if !marks.iter().any(|mark| placement.intersects(mark)) {
        return Ok(InlineOutcome::Untouched);
    }
    if keep_images {
        return Ok(InlineOutcome::Survivor(SurvivingSpan {
            page: page_index,
            text: "<inline image>".to_owned(),
            rect: placement,
        }));
    }
    if !inline.sample_confident || inline.sample_end <= inline.sample_start {
        return Err(RedactionError::UnsupportedImage {
            page: page_index,
            object: None,
            detail: "inline image has no clear sample span".to_owned(),
        });
    }
    let inverse = invert(ctm).ok_or_else(|| RedactionError::UnsupportedImage {
        page: page_index,
        object: None,
        detail: "inline image placement transform is not invertible".to_owned(),
    })?;
    let regions: Vec<ImageRegion> = marks
        .iter()
        .filter_map(|mark| mark_image_region(*mark, inverse))
        .collect();
    if regions.is_empty() {
        return Ok(InlineOutcome::Untouched);
    }

    let dictionary = inline_dictionary(page_index, &inline.dictionary)?;
    let sample_len = inline.sample_end - inline.sample_start;
    let _support = validate_image_support(page_index, None, effective, &dictionary, sample_len)?;
    let data = content
        .get(inline.sample_start..inline.sample_end)
        .ok_or_else(|| RedactionError::UnsupportedImage {
            page: page_index,
            object: None,
            detail: "inline image sample span is out of bounds".to_owned(),
        })?;
    let mut decoded = decode_inline_samples(effective, page_index, &dictionary, data)?;
    fill_image_regions(
        &mut decoded.samples,
        decoded.width,
        decoded.height,
        decoded.components,
        0,
        &regions,
    );
    if decoded.components != 1 && decoded.components != 3 {
        return Err(RedactionError::UnsupportedImage {
            page: page_index,
            object: None,
            detail: format!(
                "inline image has {} components after decode",
                decoded.components
            ),
        });
    }
    let replacement = build_inline_image(
        decoded.width,
        decoded.height,
        decoded.components,
        &decoded.samples,
    );
    Ok(InlineOutcome::Edit(replacement))
}

/// One decoded inline image, with Indexed palettes already expanded to RGB.
struct DecodedInline {
    width: u32,
    height: u32,
    components: usize,
    samples: Vec<u8>,
}

/// Decode inline image samples to 8-bit components using the same filter and
/// colour-space path as image XObjects. Indexed images are expanded to RGB so
/// the rebuilt dictionary stays simple and the black fill is unambiguous.
fn decode_inline_samples(
    effective: &Document,
    page: usize,
    dictionary: &Dictionary,
    data: &[u8],
) -> Result<DecodedInline, RedactionError> {
    let width = dictionary
        .get("Width")
        .and_then(Object::as_i64)
        .and_then(|value| u32::try_from(value).ok())
        .ok_or_else(|| RedactionError::UnsupportedImage {
            page,
            object: None,
            detail: "inline image has no usable /W".to_owned(),
        })?;
    let height = dictionary
        .get("Height")
        .and_then(Object::as_i64)
        .and_then(|value| u32::try_from(value).ok())
        .ok_or_else(|| RedactionError::UnsupportedImage {
            page,
            object: None,
            detail: "inline image has no usable /H".to_owned(),
        })?;
    let filters = image_filter_names(dictionary);
    let (mut components, mut samples, color_space) = if filters == ["DCTDecode"] {
        let (components, samples, color_space) = decode_jpeg(data, width, height, page, None)?;
        (components, samples, color_space)
    } else if filters.is_empty() || filters == ["FlateDecode"] {
        let raw = if filters.is_empty() {
            data.to_vec()
        } else {
            let params =
                flate_params(dictionary).ok_or_else(|| RedactionError::UnsupportedImage {
                    page,
                    object: None,
                    detail: "inline image has unsupported /DecodeParms".to_owned(),
                })?;
            decode_stream(data, &[Filter::FlateDecode(params)]).map_err(|error| {
                RedactionError::UnsupportedImage {
                    page,
                    object: None,
                    detail: format!("inline image Flate decode failed: {error}"),
                }
            })?
        };
        let (components, color_space, _fill) = raw_color_layout(effective, dictionary, page, None)?;
        check_sample_budget(page, None, u64::from(width) * u64::from(height), components)?;
        let expected = width as usize * height as usize * components;
        if raw.len() < expected {
            return Err(RedactionError::UnsupportedImage {
                page,
                object: None,
                detail: format!(
                    "inline image sample buffer is {} bytes, expected {expected}",
                    raw.len()
                ),
            });
        }
        (components, raw[..expected].to_vec(), color_space)
    } else {
        return Err(RedactionError::UnsupportedImage {
            page,
            object: None,
            detail: format!("unsupported inline image filter chain {filters:?}"),
        });
    };
    if let Object::Array(items) = &color_space
        && items.first().and_then(Object::as_name) == Some("Indexed")
    {
        samples = expand_indexed_samples(page, &samples, items)?;
        components = 3;
    }
    Ok(DecodedInline {
        width,
        height,
        components,
        samples,
    })
}

/// Expand one 8-bit Indexed palette into RGB samples.
fn expand_indexed_samples(
    page: usize,
    indices: &[u8],
    items: &[Object],
) -> Result<Vec<u8>, RedactionError> {
    let base = items
        .get(1)
        .and_then(Object::as_name)
        .unwrap_or("DeviceRGB");
    let lookup = match items.get(3) {
        Some(Object::String(bytes)) => bytes.as_slice(),
        _ => {
            return Err(RedactionError::UnsupportedImage {
                page,
                object: None,
                detail: "Indexed inline image has no direct lookup table".to_owned(),
            });
        }
    };
    let stride = if base == "DeviceGray" || base == "G" {
        1
    } else {
        3
    };
    let mut out = Vec::with_capacity(indices.len() * 3);
    for &index in indices {
        let start = index as usize * stride;
        let pixel =
            lookup
                .get(start..start + stride)
                .ok_or_else(|| RedactionError::UnsupportedImage {
                    page,
                    object: None,
                    detail: "Indexed inline sample is outside the palette".to_owned(),
                })?;
        if stride == 1 {
            out.extend_from_slice(&[pixel[0], pixel[0], pixel[0]]);
        } else {
            out.extend_from_slice(pixel);
        }
    }
    Ok(out)
}

/// Translate an inline dictionary into the core dictionary shape used by the
/// image decode helpers, mapping inline abbreviations to standard names.
fn inline_dictionary(
    page: usize,
    entries: &[(String, Operand)],
) -> Result<Dictionary, RedactionError> {
    let mut out = Dictionary::new();
    for (key, value) in entries {
        match key.as_str() {
            "W" => {
                out.insert("Width".to_owned(), inline_object(value)?);
            }
            "H" => {
                out.insert("Height".to_owned(), inline_object(value)?);
            }
            "BPC" => {
                out.insert("BitsPerComponent".to_owned(), inline_object(value)?);
            }
            "CS" => {
                out.insert("ColorSpace".to_owned(), inline_color_space(page, value)?);
            }
            "F" => {
                out.insert("Filter".to_owned(), inline_object(value)?);
            }
            "DP" => {
                out.insert("DecodeParms".to_owned(), inline_object(value)?);
            }
            "D" | "IM" => {
                return Err(RedactionError::UnsupportedImage {
                    page,
                    object: None,
                    detail: format!("inline image /{key} is not supported"),
                });
            }
            _ => {}
        }
    }
    Ok(out)
}

/// Convert one inline operand into a core object.
fn inline_object(value: &Operand) -> Result<Object, RedactionError> {
    match value {
        Operand::Number(number) => {
            if number.fract() == 0.0 && *number >= i64::MIN as f64 && *number <= i64::MAX as f64 {
                Ok(Object::Integer(*number as i64))
            } else {
                Ok(Object::Real(*number))
            }
        }
        Operand::Name(name) => Ok(Object::Name(name.clone())),
        Operand::Text(bytes) => Ok(Object::String(bytes.clone())),
        Operand::Array(items) => items
            .iter()
            .map(inline_object)
            .collect::<Result<Vec<_>, _>>()
            .map(Object::Array),
        Operand::Dictionary(items) => {
            let mut dictionary = Dictionary::new();
            for (key, value) in items {
                dictionary.insert(key.clone(), inline_object(value)?);
            }
            Ok(Object::Dictionary(dictionary))
        }
        Operand::Other => Ok(Object::Null),
    }
}

/// Convert an inline `/CS` value, expanding the inline abbreviations.
fn inline_color_space(page: usize, value: &Operand) -> Result<Object, RedactionError> {
    let canonical = |name: &str| -> Object {
        let standard = match name {
            "G" => "DeviceGray",
            "RGB" => "DeviceRGB",
            "CMYK" => "DeviceCMYK",
            "I" => "Indexed",
            other => other,
        };
        Object::Name(standard.to_owned())
    };
    match value {
        Operand::Name(name) => Ok(canonical(name)),
        Operand::Array(items) => {
            let mut out = Vec::with_capacity(items.len());
            for (index, item) in items.iter().enumerate() {
                out.push(match item {
                    Operand::Name(name) if index <= 1 => canonical(name),
                    other => inline_object(other)?,
                });
            }
            Ok(Object::Array(out))
        }
        _ => Err(RedactionError::UnsupportedImage {
            page,
            object: None,
            detail: "inline image /CS is not a name or array".to_owned(),
        }),
    }
}

/// Build a deterministic, unfiltered inline image with the redacted samples.
///
/// An unfiltered image lets a conforming reader derive the exact sample length
/// from `/W`, `/H`, `/BPC` and `/CS`, so sample bytes that happen to spell `EI`
/// cannot confuse the terminator. No `/F`, `/DP` or `/D` entry is emitted.
fn build_inline_image(width: u32, height: u32, components: usize, samples: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(samples.len() + 96);
    out.extend_from_slice(b"BI /W ");
    out.extend_from_slice(format_number(f64::from(width)).as_bytes());
    out.extend_from_slice(b" /H ");
    out.extend_from_slice(format_number(f64::from(height)).as_bytes());
    out.extend_from_slice(b" /BPC 8 /CS ");
    out.extend_from_slice(if components == 1 { b"/G" } else { b"/RGB" });
    out.extend_from_slice(b" ID\n");
    out.extend_from_slice(samples);
    out.extend_from_slice(b"\nEI");
    out
}

/// The page-space rectangle covered by the unit square under `ctm`.
fn unit_square_rect(ctm: Matrix) -> Rect {
    corners_rect(ctm, 0.0, 0.0, 1.0, 1.0)
}

/// Return the axis-aligned rectangle of a local rectangle under `matrix`.
fn corners_rect(matrix: Matrix, x0: f64, y0: f64, x1: f64, y1: f64) -> Rect {
    let corners = [
        transform(matrix, x0, y0),
        transform(matrix, x1, y0),
        transform(matrix, x1, y1),
        transform(matrix, x0, y1),
    ];
    Rect::saturating(
        corners
            .iter()
            .map(|point| point.0 as f32)
            .fold(f32::INFINITY, f32::min),
        corners
            .iter()
            .map(|point| point.1 as f32)
            .fold(f32::INFINITY, f32::min),
        corners
            .iter()
            .map(|point| point.0 as f32)
            .fold(f32::NEG_INFINITY, f32::max),
        corners
            .iter()
            .map(|point| point.1 as f32)
            .fold(f32::NEG_INFINITY, f32::max),
    )
}

/// Invert a 2D affine matrix.
fn invert(matrix: Matrix) -> Option<Matrix> {
    let [a, b, c, d, e, f] = matrix.0;
    let determinant = a * d - b * c;
    if !determinant.is_finite() || determinant.abs() < 1e-12 {
        return None;
    }
    Some(Matrix([
        d / determinant,
        -b / determinant,
        -c / determinant,
        a / determinant,
        (c * f - d * e) / determinant,
        (b * e - a * f) / determinant,
    ]))
}

/// Map one page-space mark into normalised image coordinates.
fn mark_image_region(mark: Rect, inverse: Matrix) -> Option<ImageRegion> {
    let corners = [
        transform(inverse, f64::from(mark.x0()), f64::from(mark.y0())),
        transform(inverse, f64::from(mark.x1()), f64::from(mark.y0())),
        transform(inverse, f64::from(mark.x1()), f64::from(mark.y1())),
        transform(inverse, f64::from(mark.x0()), f64::from(mark.y1())),
    ];
    let u0 = corners
        .iter()
        .map(|point| point.0)
        .fold(f64::INFINITY, f64::min)
        .clamp(0.0, 1.0);
    let u1 = corners
        .iter()
        .map(|point| point.0)
        .fold(f64::NEG_INFINITY, f64::max)
        .clamp(0.0, 1.0);
    let v0 = corners
        .iter()
        .map(|point| point.1)
        .fold(f64::INFINITY, f64::min)
        .clamp(0.0, 1.0);
    let v1 = corners
        .iter()
        .map(|point| point.1)
        .fold(f64::NEG_INFINITY, f64::max)
        .clamp(0.0, 1.0);
    (u1 > u0 && v1 > v0).then_some(ImageRegion { u0, v0, u1, v1 })
}

/// Declared image dimensions and the maximum decoded sample size.
struct ImageSupport {
    width: u32,
    height: u32,
}

/// Maximum decoded image samples accepted by the redactor.
const MAX_IMAGE_PIXELS: u64 = 100_000_000;
/// Maximum decoded sample buffer in bytes.
const MAX_IMAGE_BYTES: u64 = 64 * 1024 * 1024;

/// Reject unsupported image layouts before any pixels are touched.
fn validate_image_support(
    page_index: usize,
    object: Option<ObjectId>,
    effective: &Document,
    dictionary: &Dictionary,
    stored_len: usize,
) -> Result<ImageSupport, RedactionError> {
    let width = dictionary
        .get("Width")
        .and_then(Object::as_i64)
        .and_then(|value| u32::try_from(value).ok());
    let height = dictionary
        .get("Height")
        .and_then(Object::as_i64)
        .and_then(|value| u32::try_from(value).ok());
    let (Some(width), Some(height)) = (width, height) else {
        return Err(RedactionError::UnsupportedImage {
            page: page_index,
            object,
            detail: "image has no usable /Width or /Height".to_owned(),
        });
    };
    if width == 0 || height == 0 {
        return Err(RedactionError::UnsupportedImage {
            page: page_index,
            object,
            detail: "image has zero width or height".to_owned(),
        });
    }
    let pixels = u64::from(width) * u64::from(height);
    if pixels > MAX_IMAGE_PIXELS {
        return Err(RedactionError::ImageLimitExceeded {
            page: page_index,
            object,
            detail: format!("{width}x{height} exceeds {MAX_IMAGE_PIXELS} pixels"),
        });
    }
    if matches!(dictionary.get("ImageMask"), Some(Object::Boolean(true))) {
        return Err(RedactionError::UnsupportedImage {
            page: page_index,
            object,
            detail: "/ImageMask stencil images are not supported".to_owned(),
        });
    }
    if dictionary.get("Decode").is_some() {
        return Err(RedactionError::UnsupportedImage {
            page: page_index,
            object,
            detail: "/Decode arrays are not supported".to_owned(),
        });
    }
    let bits = dictionary
        .get("BitsPerComponent")
        .and_then(Object::as_i64)
        .unwrap_or(8);
    if bits != 8 {
        return Err(RedactionError::UnsupportedImage {
            page: page_index,
            object,
            detail: format!("{bits}-bit samples are not supported (only 8-bit)"),
        });
    }
    let filters = image_filter_names(dictionary);
    if filters.is_empty() {
        let components = color_components(effective, dictionary, page_index, object)?;
        check_sample_budget(page_index, object, pixels, components)?;
    } else if filters == ["DCTDecode"] {
        check_sample_budget(page_index, object, pixels, 3)?;
    } else if filters == ["FlateDecode"] {
        let components = color_components(effective, dictionary, page_index, object)?;
        check_sample_budget(page_index, object, pixels, components)?;
    } else {
        return Err(RedactionError::UnsupportedImage {
            page: page_index,
            object,
            detail: format!("unsupported image filter chain {filters:?}"),
        });
    }
    if stored_len == 0 {
        return Err(RedactionError::UnsupportedImage {
            page: page_index,
            object,
            detail: "image stream is empty".to_owned(),
        });
    }
    Ok(ImageSupport { width, height })
}

/// Enforce the decoded sample budget.
fn check_sample_budget(
    page: usize,
    object: Option<ObjectId>,
    pixels: u64,
    components: usize,
) -> Result<(), RedactionError> {
    let bytes = pixels.saturating_mul(components as u64);
    if bytes > MAX_IMAGE_BYTES {
        return Err(RedactionError::ImageLimitExceeded {
            page,
            object,
            detail: format!("{bytes} decoded bytes exceeds {MAX_IMAGE_BYTES}"),
        });
    }
    Ok(())
}

/// Return the image filter names from `/Filter`, in order.
fn image_filter_names(dictionary: &Dictionary) -> Vec<String> {
    match dictionary.get("Filter") {
        Some(Object::Name(name)) => vec![name.clone()],
        Some(Object::Array(items)) => items
            .iter()
            .filter_map(Object::as_name)
            .map(ToOwned::to_owned)
            .collect(),
        _ => Vec::new(),
    }
}

/// Return the number of samples per pixel for a supported colour space.
fn color_components(
    effective: &Document,
    dictionary: &Dictionary,
    page: usize,
    object: Option<ObjectId>,
) -> Result<usize, RedactionError> {
    let Some(space) = dictionary.get("ColorSpace") else {
        return Ok(1);
    };
    match space {
        Object::Name(name) if name == "DeviceGray" => Ok(1),
        Object::Name(name) if name == "DeviceRGB" => Ok(3),
        Object::Name(name) if name == "DeviceCMYK" => Err(RedactionError::UnsupportedImage {
            page,
            object,
            detail: "raw DeviceCMYK is not supported (only JPEG CMYK)".to_owned(),
        }),
        Object::Name(name) => Err(RedactionError::UnsupportedImage {
            page,
            object,
            detail: format!("unsupported colour space /{name}"),
        }),
        Object::Array(items) => {
            let Some(first) = items.first().and_then(Object::as_name) else {
                return Err(RedactionError::UnsupportedImage {
                    page,
                    object,
                    detail: "unsupported colour-space array".to_owned(),
                });
            };
            match first {
                "Indexed" => Ok(1),
                "ICCBased" => {
                    let id = items.get(1).and_then(Object::as_reference).ok_or_else(|| {
                        RedactionError::UnsupportedImage {
                            page,
                            object,
                            detail: "ICCBased colour space has no profile stream".to_owned(),
                        }
                    })?;
                    let profile = dictionary_of(effective, id)?;
                    let components = profile
                        .get("N")
                        .and_then(Object::as_i64)
                        .and_then(|value| usize::try_from(value).ok())
                        .ok_or_else(|| RedactionError::UnsupportedImage {
                            page,
                            object,
                            detail: "ICCBased profile has no /N".to_owned(),
                        })?;
                    if components > 4 {
                        return Err(RedactionError::UnsupportedImage {
                            page,
                            object,
                            detail: format!(
                                "ICCBased profile with {components} components exceeds 4"
                            ),
                        });
                    }
                    if components == 4 {
                        return Err(RedactionError::UnsupportedImage {
                            page,
                            object,
                            detail: "ICCBased CMYK cannot be filled with black samples".to_owned(),
                        });
                    }
                    if components == 0 {
                        return Err(RedactionError::UnsupportedImage {
                            page,
                            object,
                            detail: "ICCBased profile declares zero components".to_owned(),
                        });
                    }
                    Ok(components)
                }
                "Separation" | "DeviceN" => Err(RedactionError::UnsupportedImage {
                    page,
                    object,
                    detail: format!("/{first} colour spaces are not supported"),
                }),
                other => Err(RedactionError::UnsupportedImage {
                    page,
                    object,
                    detail: format!("unsupported colour space /{other}"),
                }),
            }
        }
        _ => Err(RedactionError::UnsupportedImage {
            page,
            object,
            detail: "unsupported /ColorSpace value".to_owned(),
        }),
    }
}

/// One decoded image ready to be refilled and re-encoded.
type SmaskDecoded = (ObjectId, Dictionary, u32, u32, Vec<u8>);

/// One decoded image ready to be refilled and re-encoded.
struct DecodedImage {
    width: u32,
    height: u32,
    components: usize,
    samples: Vec<u8>,
    fill: u8,
    color_space: Object,
    smask: Option<SmaskDecoded>,
    smask_edit: Option<(ObjectId, Object)>,
}

/// Decode and refill every planned image, then re-encode as Flate.
fn prepare_image_edits(
    effective: &Document,
    regions: &BTreeMap<ObjectId, (usize, Vec<ImageRegion>)>,
) -> Result<Vec<ImageEdit>, RedactionError> {
    let mut edits = Vec::new();
    for (id, (page, regions)) in regions {
        let Object::Stream { dictionary, data } = effective.object(*id)? else {
            return Err(RedactionError::MalformedStructure(format!(
                "image object {} is not a stream",
                id.number
            )));
        };
        let mut decoded = decode_image_samples(effective, *id, *page, &dictionary, &data)?;
        fill_image_regions(
            &mut decoded.samples,
            decoded.width,
            decoded.height,
            decoded.components,
            decoded.fill,
            regions,
        );
        if let Some((smask_id, mut smask_dictionary, width, height, mut alpha)) =
            decoded.smask.take()
        {
            fill_image_regions(&mut alpha, width, height, 1, 255, regions);
            let encoded_alpha = zlib_stored(&alpha);
            smask_dictionary.insert("Width".to_owned(), Object::Integer(i64::from(width)));
            smask_dictionary.insert("Height".to_owned(), Object::Integer(i64::from(height)));
            smask_dictionary.insert(
                "ColorSpace".to_owned(),
                Object::Name("DeviceGray".to_owned()),
            );
            smask_dictionary.insert("BitsPerComponent".to_owned(), Object::Integer(8));
            smask_dictionary.insert("Filter".to_owned(), Object::Name("FlateDecode".to_owned()));
            smask_dictionary.remove("DecodeParms");
            decoded.smask_edit = Some((
                smask_id,
                Object::Stream {
                    dictionary: smask_dictionary,
                    data: encoded_alpha,
                },
            ));
        }

        let mut new_dictionary = dictionary;
        new_dictionary.insert(
            "Width".to_owned(),
            Object::Integer(i64::from(decoded.width)),
        );
        new_dictionary.insert(
            "Height".to_owned(),
            Object::Integer(i64::from(decoded.height)),
        );
        new_dictionary.insert("ColorSpace".to_owned(), decoded.color_space.clone());
        new_dictionary.insert("BitsPerComponent".to_owned(), Object::Integer(8));
        new_dictionary.insert("Filter".to_owned(), Object::Name("FlateDecode".to_owned()));
        new_dictionary.remove("DecodeParms");
        new_dictionary.remove("Decode");
        new_dictionary.remove("ColorTransform");
        let encoded = zlib_stored(&decoded.samples);
        edits.push(ImageEdit {
            object: *id,
            base: Object::Stream {
                dictionary: new_dictionary,
                data: encoded,
            },
            smask: decoded.smask_edit,
        });
    }
    Ok(edits)
}

/// Decode one image stream to 8-bit samples and preserve a supported SMask.
fn decode_image_samples(
    effective: &Document,
    object: ObjectId,
    page: usize,
    dictionary: &Dictionary,
    data: &[u8],
) -> Result<DecodedImage, RedactionError> {
    let width = dictionary
        .get("Width")
        .and_then(Object::as_i64)
        .and_then(|value| u32::try_from(value).ok())
        .ok_or_else(|| RedactionError::UnsupportedImage {
            page,
            object: Some(object),
            detail: "image has no usable /Width".to_owned(),
        })?;
    let height = dictionary
        .get("Height")
        .and_then(Object::as_i64)
        .and_then(|value| u32::try_from(value).ok())
        .ok_or_else(|| RedactionError::UnsupportedImage {
            page,
            object: Some(object),
            detail: "image has no usable /Height".to_owned(),
        })?;
    let filters = image_filter_names(dictionary);
    let (components, samples, color_space) = if filters == ["DCTDecode"] {
        decode_jpeg(data, width, height, page, Some(object))?
    } else if filters.is_empty() || filters == ["FlateDecode"] {
        let raw = if filters.is_empty() {
            data.to_vec()
        } else {
            let params =
                flate_params(dictionary).ok_or_else(|| RedactionError::UnsupportedImage {
                    page,
                    object: Some(object),
                    detail: "indirect, array or TIFF-predictor /DecodeParms is not supported"
                        .to_owned(),
                })?;
            decode_stream(data, &[Filter::FlateDecode(params)]).map_err(|error| {
                RedactionError::UnsupportedImage {
                    page,
                    object: Some(object),
                    detail: format!("image Flate decode failed: {error}"),
                }
            })?
        };
        let (components, color_space, _fill) =
            raw_color_layout(effective, dictionary, page, Some(object))?;
        check_sample_budget(
            page,
            Some(object),
            u64::from(width) * u64::from(height),
            components,
        )?;
        let expected = width as usize * height as usize * components;
        if raw.len() < expected {
            return Err(RedactionError::UnsupportedImage {
                page,
                object: Some(object),
                detail: format!(
                    "decoded sample buffer is {} bytes, expected {expected}",
                    raw.len()
                ),
            });
        }
        let mut samples = raw;
        samples.truncate(expected);
        (components, samples, color_space)
    } else {
        return Err(RedactionError::UnsupportedImage {
            page,
            object: Some(object),
            detail: format!("unsupported image filter chain {filters:?}"),
        });
    };
    let fill = raw_fill_value(dictionary, page, Some(object), components)?;
    let smask = decode_smask(effective, dictionary, page, object)?;
    Ok(DecodedImage {
        width,
        height,
        components,
        samples,
        fill,
        color_space,
        smask,
        smask_edit: None,
    })
}

/// Return the byte value used to fill a pixel with opaque black.
fn raw_fill_value(
    dictionary: &Dictionary,
    page: usize,
    object: Option<ObjectId>,
    components: usize,
) -> Result<u8, RedactionError> {
    if components == 1
        && let Some(Object::Array(items)) = dictionary.get("ColorSpace")
        && items.first().and_then(Object::as_name) == Some("Indexed")
    {
        return indexed_black(items, page, object);
    }
    Ok(0)
}

/// Find the palette index whose colour is black.
fn indexed_black(
    items: &[Object],
    page: usize,
    object: Option<ObjectId>,
) -> Result<u8, RedactionError> {
    let lookup = items
        .get(3)
        .ok_or_else(|| RedactionError::UnsupportedImage {
            page,
            object,
            detail: "Indexed colour space has no lookup table".to_owned(),
        })?;
    let bytes = match lookup {
        Object::String(bytes) => bytes.clone(),
        _ => {
            return Err(RedactionError::UnsupportedImage {
                page,
                object,
                detail: "indirect Indexed lookup tables are not supported".to_owned(),
            });
        }
    };
    let base = items
        .get(1)
        .and_then(Object::as_name)
        .unwrap_or("DeviceRGB");
    let stride = if base == "DeviceGray" { 1 } else { 3 };
    for (index, pixel) in bytes.chunks_exact(stride).enumerate() {
        if pixel.iter().all(|sample| *sample == 0) {
            return u8::try_from(index).map_err(|_| RedactionError::UnsupportedImage {
                page,
                object,
                detail: "Indexed palette has more than 256 entries".to_owned(),
            });
        }
    }
    Err(RedactionError::UnsupportedImage {
        page,
        object,
        detail: "Indexed palette has no black entry".to_owned(),
    })
}

/// Return components, output colour space and fill for a raw sample layout.
fn raw_color_layout(
    effective: &Document,
    dictionary: &Dictionary,
    page: usize,
    object: Option<ObjectId>,
) -> Result<(usize, Object, u8), RedactionError> {
    let space = dictionary
        .get("ColorSpace")
        .cloned()
        .unwrap_or(Object::Name("DeviceGray".to_owned()));
    let components = color_components(effective, dictionary, page, object)?;
    Ok((components, space, 0))
}

/// Decode a DCTDecode JPEG to RGB samples.
fn decode_jpeg(
    data: &[u8],
    width: u32,
    height: u32,
    page: usize,
    object: Option<ObjectId>,
) -> Result<(usize, Vec<u8>, Object), RedactionError> {
    let image =
        image::load_from_memory_with_format(data, image::ImageFormat::Jpeg).map_err(|error| {
            RedactionError::UnsupportedImage {
                page,
                object,
                detail: format!("JPEG decode failed: {error}"),
            }
        })?;
    if image.width() != width || image.height() != height {
        return Err(RedactionError::UnsupportedImage {
            page,
            object,
            detail: format!(
                "JPEG is {}x{}, dictionary declares {width}x{height}",
                image.width(),
                image.height()
            ),
        });
    }
    let rgb = image.to_rgb8();
    check_sample_budget(page, object, u64::from(width) * u64::from(height), 3)?;
    Ok((3, rgb.into_raw(), Object::Name("DeviceRGB".to_owned())))
}

/// Decode an optional `/SMask` to 8-bit grey alpha samples.
fn decode_smask(
    effective: &Document,
    dictionary: &Dictionary,
    page: usize,
    object: ObjectId,
) -> Result<Option<SmaskDecoded>, RedactionError> {
    let Some(smask_id) = dictionary.get("SMask").and_then(Object::as_reference) else {
        return Ok(None);
    };
    let Object::Stream {
        dictionary: smask_dictionary,
        data,
    } = effective.object(smask_id)?
    else {
        return Err(RedactionError::UnsupportedImage {
            page,
            object: Some(object),
            detail: "SMask is not a stream".to_owned(),
        });
    };
    let width = smask_dictionary
        .get("Width")
        .and_then(Object::as_i64)
        .and_then(|value| u32::try_from(value).ok())
        .ok_or_else(|| RedactionError::UnsupportedImage {
            page,
            object: Some(smask_id),
            detail: "SMask has no usable /Width".to_owned(),
        })?;
    let height = smask_dictionary
        .get("Height")
        .and_then(Object::as_i64)
        .and_then(|value| u32::try_from(value).ok())
        .ok_or_else(|| RedactionError::UnsupportedImage {
            page,
            object: Some(smask_id),
            detail: "SMask has no usable /Height".to_owned(),
        })?;
    let filters = image_filter_names(&smask_dictionary);
    let samples = if filters == ["DCTDecode"] {
        let image = image::load_from_memory_with_format(&data, image::ImageFormat::Jpeg).map_err(
            |error| RedactionError::UnsupportedImage {
                page,
                object: Some(smask_id),
                detail: format!("SMask JPEG decode failed: {error}"),
            },
        )?;
        if image.width() != width || image.height() != height {
            return Err(RedactionError::UnsupportedImage {
                page,
                object: Some(smask_id),
                detail: "SMask JPEG dimensions disagree with its dictionary".to_owned(),
            });
        }
        image.to_luma8().into_raw()
    } else if filters.is_empty() || filters == ["FlateDecode"] {
        let raw = if filters.is_empty() {
            data
        } else {
            let params = flate_params(&smask_dictionary).ok_or_else(|| {
                RedactionError::UnsupportedImage {
                    page,
                    object: Some(smask_id),
                    detail: "unsupported SMask /DecodeParms".to_owned(),
                }
            })?;
            decode_stream(&data, &[Filter::FlateDecode(params)]).map_err(|error| {
                RedactionError::UnsupportedImage {
                    page,
                    object: Some(smask_id),
                    detail: format!("SMask Flate decode failed: {error}"),
                }
            })?
        };
        if raw.len() < width as usize * height as usize {
            return Err(RedactionError::UnsupportedImage {
                page,
                object: Some(smask_id),
                detail: "SMask sample buffer is too short".to_owned(),
            });
        }
        raw[..width as usize * height as usize].to_vec()
    } else {
        return Err(RedactionError::UnsupportedImage {
            page,
            object: Some(smask_id),
            detail: format!("unsupported SMask filter chain {filters:?}"),
        });
    };
    check_sample_budget(
        page,
        Some(smask_id),
        u64::from(width) * u64::from(height),
        1,
    )?;
    Ok(Some((smask_id, smask_dictionary, width, height, samples)))
}

/// Fill the planned rectangles with an opaque value.
fn fill_image_regions(
    samples: &mut [u8],
    width: u32,
    height: u32,
    components: usize,
    value: u8,
    regions: &[ImageRegion],
) {
    if components == 0 {
        return;
    }
    for region in regions {
        let x0 = (region.u0 * f64::from(width)).floor().max(0.0) as u32;
        let x1 = (region.u1 * f64::from(width))
            .ceil()
            .clamp(0.0, f64::from(width)) as u32;
        let y0 = ((1.0 - region.v1) * f64::from(height)).floor().max(0.0) as u32;
        let y1 = ((1.0 - region.v0) * f64::from(height))
            .ceil()
            .clamp(0.0, f64::from(height)) as u32;
        for y in y0..y1 {
            for x in x0..x1 {
                let offset = ((y as usize * width as usize) + x as usize) * components;
                if let Some(slice) = samples.get_mut(offset..offset + components) {
                    for sample in slice {
                        *sample = value;
                    }
                }
            }
        }
    }
}

/// Build a zlib stream using stored DEFLATE blocks.
fn zlib_stored(data: &[u8]) -> Vec<u8> {
    let mut out = vec![0x78, 0x01];
    let mut position = 0usize;
    loop {
        let chunk = (data.len() - position).min(65_535);
        let final_block = position + chunk == data.len();
        out.push(u8::from(final_block));
        let length = chunk as u16;
        out.extend_from_slice(&length.to_le_bytes());
        out.extend_from_slice(&(!length).to_le_bytes());
        out.extend_from_slice(&data[position..position + chunk]);
        position += chunk;
        if final_block {
            break;
        }
    }
    out.extend_from_slice(&adler32(data).to_be_bytes());
    out
}

/// Compute the zlib Adler-32 checksum.
fn adler32(data: &[u8]) -> u32 {
    let mut a = 1u32;
    let mut b = 0u32;
    for byte in data {
        a = (a + u32::from(*byte)) % 65_521;
        b = (b + a) % 65_521;
    }
    (b << 16) | a
}

/// Parse `/DecodeParms` into Flate parameters, or `None` when the value is
/// indirect/array and cannot be validated here.
fn flate_params(dictionary: &Dictionary) -> Option<FlateParams> {
    let params = match dictionary.get("DecodeParms") {
        None => return Some(FlateParams::default()),
        Some(Object::Dictionary(params)) => params,
        Some(Object::Null) => return Some(FlateParams::default()),
        _ => return None,
    };
    let predictor = params
        .get("Predictor")
        .and_then(Object::as_i64)
        .and_then(pdfxi_core::Predictor::from_pdf)
        .unwrap_or(pdfxi_core::Predictor::None);
    if predictor == pdfxi_core::Predictor::Tiff {
        return None;
    }
    let colors = params
        .get("Colors")
        .and_then(Object::as_i64)
        .and_then(|value| u32::try_from(value).ok())
        .unwrap_or(1);
    let bits = params
        .get("BitsPerComponent")
        .and_then(Object::as_i64)
        .and_then(|value| u32::try_from(value).ok())
        .unwrap_or(8);
    let columns = params
        .get("Columns")
        .and_then(Object::as_i64)
        .and_then(|value| u32::try_from(value).ok())
        .unwrap_or(1);
    Some(FlateParams::new(predictor, colors, bits, columns))
}

/// Remove intersecting annotations from one page.
///
/// Returns the new direct `/Annots` value when the page stored its annotations
/// as a direct array, or `None` when the array was indirect (and was staged
/// separately) or nothing changed.
fn remove_intersecting_annotations(
    document: &mut Document,
    effective: &Document,
    page_index: usize,
    marks: &[Rect],
    request: &RedactionRequest,
    report: &mut RedactionReport,
    removed_widgets: &mut BTreeSet<ObjectId>,
) -> Result<Option<Option<Object>>, RedactionError> {
    let page = effective.page(page_index)?;
    let page_dictionary = dictionary_of(effective, page.id)?;
    let Some(value) = page_dictionary.get("Annots").cloned() else {
        return Ok(None);
    };
    let (array, array_id) = match &value {
        Object::Array(items) => (items.clone(), None),
        Object::Reference(id) => match effective.object(*id)? {
            Object::Array(items) => (items, Some(*id)),
            _ => return Ok(None),
        },
        _ => return Ok(None),
    };
    let mut kept = Vec::new();
    let mut changed = false;
    for item in array {
        let (dictionary, id) = match &item {
            Object::Reference(id) => match effective.object(*id)? {
                Object::Dictionary(dictionary) => (Some(dictionary), Some(*id)),
                _ => {
                    kept.push(item);
                    continue;
                }
            },
            Object::Dictionary(dictionary) => (Some(dictionary.clone()), None),
            _ => {
                kept.push(item);
                continue;
            }
        };
        let Some(dictionary) = dictionary else {
            kept.push(item);
            continue;
        };
        let subtype = dictionary.get("Subtype").and_then(Object::as_name);
        let intersects = rect_from_dictionary(&dictionary)
            .is_some_and(|rect| marks.iter().any(|mark| rect.intersects(mark)));
        let governed = match subtype {
            Some("Link") => request.remove_links,
            Some("Widget") => request.remove_form_fields,
            _ => request.remove_annotations,
        };
        if governed && intersects {
            if let Some(id) = id {
                null_appearances(document, effective, &dictionary)?;
                document.set_object(id, Object::Null)?;
                if subtype == Some("Widget") {
                    removed_widgets.insert(id);
                }
            } else {
                null_appearances(document, effective, &dictionary)?;
            }
            report.removed_annotations = report.removed_annotations.saturating_add(1);
            if subtype == Some("Link") {
                report.removed_links = report.removed_links.saturating_add(1);
            }
            changed = true;
        } else {
            kept.push(item);
        }
    }
    if !changed {
        return Ok(None);
    }
    let new_array = Object::Array(kept);
    match array_id {
        Some(id) => {
            document.set_object(id, new_array)?;
            Ok(None)
        }
        None => Ok(Some(Some(new_array))),
    }
}

/// Replace every indirect object referenced from an annotation's appearances
/// with `null`, so a full rewrite cannot leave the old drawing behind.
fn null_appearances(
    document: &mut Document,
    effective: &Document,
    annotation: &Dictionary,
) -> Result<(), RedactionError> {
    let mut references = BTreeSet::new();
    if let Some(appearance) = annotation.get("AP") {
        collect_references(effective, appearance, 0, &mut references)?;
    }
    for id in references {
        document.set_object(id, Object::Null)?;
    }
    Ok(())
}

/// Collect indirect references reachable from one value.
fn collect_references(
    document: &Document,
    value: &Object,
    depth: usize,
    out: &mut BTreeSet<ObjectId>,
) -> Result<(), RedactionError> {
    if depth > 32 {
        return Err(RedactionError::MalformedStructure(
            "appearance dictionary nesting is too deep".to_owned(),
        ));
    }
    match value {
        Object::Reference(id) => {
            out.insert(*id);
            if let Ok(resolved) = document.object(*id) {
                collect_references(document, &resolved, depth + 1, out)?;
            }
        }
        Object::Array(items) => {
            for item in items {
                collect_references(document, item, depth + 1, out)?;
            }
        }
        Object::Dictionary(dictionary) => {
            for item in dictionary.values() {
                collect_references(document, item, depth + 1, out)?;
            }
        }
        Object::Stream { dictionary, .. } => {
            for item in dictionary.values() {
                collect_references(document, item, depth + 1, out)?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// Remove widgets from `/AcroForm /Fields` and `/AcroForm /CO`.
fn remove_form_fields(
    document: &mut Document,
    effective: &Document,
    removed_widgets: &BTreeSet<ObjectId>,
    report: &mut RedactionReport,
) -> Result<(), RedactionError> {
    let root = effective.root()?;
    let mut catalog = dictionary_of(effective, root)?;
    let Some(acroform) = catalog.get("AcroForm").cloned() else {
        return Ok(());
    };
    let (mut acroform_dictionary, acroform_id) = match &acroform {
        Object::Dictionary(dictionary) => (dictionary.clone(), None),
        Object::Reference(id) => match effective.object(*id)? {
            Object::Dictionary(dictionary) => (dictionary, Some(*id)),
            _ => return Ok(()),
        },
        _ => return Ok(()),
    };
    let Some(fields) = acroform_dictionary.get("Fields").cloned() else {
        return Ok(());
    };
    let mut removed_fields = BTreeSet::new();
    let new_fields = filter_field_level(
        document,
        effective,
        &fields,
        removed_widgets,
        &mut removed_fields,
        0,
    )?;
    acroform_dictionary.insert("Fields".to_owned(), new_fields);

    if let Some(co) = acroform_dictionary.get("CO").cloned() {
        let new_co =
            filter_reference_array(document, effective, &co, &removed_fields, removed_widgets)?;
        acroform_dictionary.insert("CO".to_owned(), new_co);
    }

    match acroform_id {
        Some(id) => document.set_object(id, Object::Dictionary(acroform_dictionary))?,
        None => {
            catalog.insert(
                "AcroForm".to_owned(),
                Object::Dictionary(acroform_dictionary),
            );
            document.set_object(root, Object::Dictionary(catalog))?;
        }
    }
    report.removed_fields = report.removed_fields.saturating_add(removed_fields.len());
    Ok(())
}

/// Filter one `/Fields` or `/Kids` level and return the surviving array object.
fn filter_field_level(
    document: &mut Document,
    effective: &Document,
    value: &Object,
    removed_widgets: &BTreeSet<ObjectId>,
    removed_fields: &mut BTreeSet<ObjectId>,
    depth: usize,
) -> Result<Object, RedactionError> {
    if depth > 64 {
        return Err(RedactionError::MalformedStructure(
            "form field tree is too deep".to_owned(),
        ));
    }
    let (array, array_id) = match value {
        Object::Array(items) => (items.clone(), None),
        Object::Reference(id) => match effective.object(*id)? {
            Object::Array(items) => (items, Some(*id)),
            _ => return Ok(value.clone()),
        },
        _ => return Ok(value.clone()),
    };
    let mut kept = Vec::new();
    for item in array {
        let Object::Reference(id) = item else {
            kept.push(item);
            continue;
        };
        if removed_widgets.contains(&id) {
            document.set_object(id, Object::Null)?;
            removed_fields.insert(id);
            continue;
        }
        let Ok(object) = effective.object(id) else {
            kept.push(Object::Reference(id));
            continue;
        };
        let Object::Dictionary(mut dictionary) = object else {
            kept.push(Object::Reference(id));
            continue;
        };
        let had_kids = dictionary.contains_key("Kids");
        if let Some(kids) = dictionary.get("Kids").cloned() {
            let new_kids = filter_field_level(
                document,
                effective,
                &kids,
                removed_widgets,
                removed_fields,
                depth + 1,
            )?;
            dictionary.insert("Kids".to_owned(), new_kids);
        }
        let empty = dictionary
            .get("Kids")
            .and_then(Object::as_array)
            .is_some_and(|kids| kids.is_empty());
        if had_kids && empty {
            document.set_object(id, Object::Null)?;
            removed_fields.insert(id);
            continue;
        }
        document.set_object(id, Object::Dictionary(dictionary))?;
        kept.push(Object::Reference(id));
    }
    let new_array = Object::Array(kept);
    if let Some(id) = array_id {
        document.set_object(id, new_array.clone())?;
    }
    Ok(new_array)
}

/// Remove references to removed fields from an `/AcroForm /CO` array.
fn filter_reference_array(
    document: &mut Document,
    effective: &Document,
    value: &Object,
    removed_fields: &BTreeSet<ObjectId>,
    removed_widgets: &BTreeSet<ObjectId>,
) -> Result<Object, RedactionError> {
    let (array, array_id) = match value {
        Object::Array(items) => (items.clone(), None),
        Object::Reference(id) => match effective.object(*id)? {
            Object::Array(items) => (items, Some(*id)),
            _ => return Ok(value.clone()),
        },
        _ => return Ok(value.clone()),
    };
    let kept: Vec<Object> = array
        .into_iter()
        .filter(|item| match item {
            Object::Reference(id) => !removed_fields.contains(id) && !removed_widgets.contains(id),
            _ => true,
        })
        .collect();
    let new_array = Object::Array(kept);
    if let Some(id) = array_id {
        document.set_object(id, new_array.clone())?;
    }
    Ok(new_array)
}

// ---------------------------------------------------------------------------
// Metadata scrub
// ---------------------------------------------------------------------------

/// Clear identifying `/Info` strings and remove the catalog XMP stream.
fn scrub_metadata(document: &mut Document, effective: &Document) -> Result<(), RedactionError> {
    if let Some(mut info) = effective.info()? {
        info.title = None;
        info.author = None;
        info.subject = None;
        info.keywords = None;
        info.creator = None;
        info.producer = None;
        document.set_info(&info)?;
    }
    let root = effective.root()?;
    let mut catalog = dictionary_of(effective, root)?;
    if let Some(metadata) = catalog.get("Metadata").cloned() {
        if let Object::Reference(id) = metadata {
            document.set_object(id, Object::Null)?;
        }
        catalog.remove("Metadata");
        document.set_object(root, Object::Dictionary(catalog))?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Shared object helpers
// ---------------------------------------------------------------------------

/// Return an object's dictionary or a typed structure error.
fn dictionary_of(document: &Document, id: ObjectId) -> Result<Dictionary, RedactionError> {
    match document.object(id)? {
        Object::Dictionary(dictionary) => Ok(dictionary),
        Object::Stream { dictionary, .. } => Ok(dictionary),
        _ => Err(RedactionError::MalformedStructure(format!(
            "object {} is not a dictionary",
            id.number
        ))),
    }
}

/// Read a numeric `/Rect` array as a rectangle.
fn rect_from_dictionary(dictionary: &Dictionary) -> Option<Rect> {
    let values: Vec<f64> = dictionary
        .get("Rect")?
        .as_array()?
        .iter()
        .map(Object::as_f64)
        .collect::<Option<Vec<_>>>()?;
    if values.len() < 4 {
        return None;
    }
    Some(Rect::saturating(
        values[0] as f32,
        values[1] as f32,
        values[2] as f32,
        values[3] as f32,
    ))
}

/// Return the indirect annotation references on a page.
fn annotation_references(
    document: &Document,
    page: &Dictionary,
) -> Result<Vec<ObjectId>, RedactionError> {
    let Some(value) = page.get("Annots") else {
        return Ok(Vec::new());
    };
    let array = match value {
        Object::Array(items) => items.clone(),
        Object::Reference(id) => match document.object(*id)? {
            Object::Array(items) => items,
            _ => return Ok(Vec::new()),
        },
        _ => return Ok(Vec::new()),
    };
    Ok(array
        .into_iter()
        .filter_map(|item| item.as_reference())
        .collect())
}

/// Build a map from page dictionary object to zero-based page index.
fn page_id_map(document: &Document) -> Result<BTreeMap<ObjectId, usize>, RedactionError> {
    let mut map = BTreeMap::new();
    for (index, page) in document.pages()?.into_iter().enumerate() {
        map.insert(page.id, index);
    }
    Ok(map)
}

/// Return the marks for one page.
fn marks_for_page(report: &RedactionReport, page: usize) -> Vec<Rect> {
    report
        .marks
        .iter()
        .filter(|mark| mark.page == page)
        .map(|mark| mark.rect)
        .collect()
}

/// Report any form field or widget that still overlaps a mark.
fn check_surviving_fields(
    document: &Document,
    page_ids: &BTreeMap<ObjectId, usize>,
    report: &RedactionReport,
    secrets: &[&str],
    survivors: &mut Vec<Survivor>,
) -> Result<(), RedactionError> {
    let root = document.root()?;
    let catalog = match document.object(root)? {
        Object::Dictionary(dictionary) => dictionary,
        Object::Stream { dictionary, .. } => dictionary,
        _ => return Ok(()),
    };
    let Some(acroform) = catalog.get("AcroForm") else {
        return Ok(());
    };
    let acroform = match acroform {
        Object::Dictionary(dictionary) => dictionary.clone(),
        Object::Reference(id) => match document.object(*id)? {
            Object::Dictionary(dictionary) => dictionary,
            _ => return Ok(()),
        },
        _ => return Ok(()),
    };
    let Some(fields) = acroform.get("Fields") else {
        return Ok(());
    };
    let fields = match fields {
        Object::Array(items) => items.clone(),
        Object::Reference(id) => match document.object(*id)? {
            Object::Array(items) => items,
            _ => return Ok(()),
        },
        _ => return Ok(()),
    };
    let mut stack: Vec<(Object, usize)> = fields.into_iter().map(|item| (item, 0)).collect();
    let mut seen = BTreeSet::new();
    while let Some((item, depth)) = stack.pop() {
        if depth > 64 {
            continue;
        }
        let (dictionary, id) = match item {
            Object::Reference(id) => {
                if !seen.insert(id) {
                    continue;
                }
                match document.object(id)? {
                    Object::Dictionary(dictionary) => (dictionary, Some(id)),
                    _ => continue,
                }
            }
            Object::Dictionary(dictionary) => (dictionary, None),
            _ => continue,
        };
        if let Some(kids) = dictionary.get("Kids") {
            match kids {
                Object::Array(items) => {
                    for kid in items {
                        stack.push((kid.clone(), depth + 1));
                    }
                }
                Object::Reference(id) => {
                    if let Ok(Object::Array(items)) = document.object(*id) {
                        for kid in items {
                            stack.push((kid.clone(), depth + 1));
                        }
                    }
                }
                _ => {}
            }
        }
        let page = dictionary
            .get("P")
            .and_then(Object::as_reference)
            .and_then(|id| page_ids.get(&id).copied());
        // A field's value can survive outside the page rectangle: an ancestor
        // field can keep `/V`, `/DV` or `/RV` after its widget was removed, and
        // a surviving field can hold the same text as a mark. Scan every value
        // that is still present; a match fails verification closed.
        for key in ["V", "DV", "RV"] {
            let Some(value) = dictionary.get(key) else {
                continue;
            };
            let value = match value {
                Object::Reference(reference) => match document.object(*reference) {
                    Ok(value) => value,
                    Err(_) => continue,
                },
                other => other.clone(),
            };
            scan_form_value(&value, secrets, survivors, page, id);
        }
        if dictionary.get("Subtype").and_then(Object::as_name) != Some("Widget") {
            continue;
        }
        let Some(rect) = rect_from_dictionary(&dictionary) else {
            continue;
        };
        let intersects = match page {
            Some(page) => marks_for_page(report, page)
                .iter()
                .any(|mark| rect.intersects(mark)),
            None => report.marks.iter().any(|mark| rect.intersects(&mark.rect)),
        };
        if intersects {
            survivors.push(Survivor {
                kind: SurvivorKind::FormField,
                page,
                object: id,
                stream_offset: None,
                detail: "a form widget still intersects a mark".to_owned(),
            });
        }
    }
    Ok(())
}

/// Report a bookmark title that still contains a redacted secret.
///
/// Titles are decoded by the outline reader, so a UTF-16BE or hex-encoded
/// title is checked as text rather than as raw bytes.
fn check_outline_titles(document: &Document, secrets: &[&str], survivors: &mut Vec<Survivor>) {
    let Ok(read) = crate::navigation::read_outline(document) else {
        return;
    };
    scan_outline_titles(&read.outline.roots, secrets, survivors);
}

fn scan_outline_titles(
    bookmarks: &[crate::navigation::Bookmark],
    secrets: &[&str],
    survivors: &mut Vec<Survivor>,
) {
    for bookmark in bookmarks {
        let title = bookmark.title.to_lowercase();
        for secret in secrets {
            if !secret.is_empty() && title.contains(&secret.to_lowercase()) {
                survivors.push(Survivor {
                    kind: SurvivorKind::ExtractedText,
                    page: None,
                    object: None,
                    stream_offset: None,
                    detail: format!("a bookmark title still contains {secret:?}"),
                });
            }
        }
        scan_outline_titles(&bookmark.children, secrets, survivors);
    }
}

/// Report a surviving form value that still contains a redacted secret.
///
/// Values are decoded with the same PDFDocEncoding/UTF-16 rule the rest of the
/// crate uses, so a UTF-16BE `/V` cannot hide behind its byte encoding.
fn scan_form_value(
    value: &Object,
    secrets: &[&str],
    survivors: &mut Vec<Survivor>,
    page: Option<usize>,
    object: Option<ObjectId>,
) {
    match value {
        Object::String(bytes) => {
            let text = pdfxi_core::decode_text_string(bytes).to_lowercase();
            for secret in secrets {
                if !secret.is_empty() && text.contains(&secret.to_lowercase()) {
                    survivors.push(Survivor {
                        kind: SurvivorKind::FormField,
                        page,
                        object,
                        stream_offset: None,
                        detail: format!("a form value still contains {secret:?}"),
                    });
                }
            }
        }
        Object::Array(items) => {
            for item in items {
                scan_form_value(item, secrets, survivors, page, object);
            }
        }
        Object::Dictionary(dictionary) => {
            for value in dictionary.values() {
                scan_form_value(value, secrets, survivors, page, object);
            }
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// Saved-byte stream scanning
// ---------------------------------------------------------------------------

/// One stream found in a saved file.
struct StreamRegion {
    object_number: u32,
    data_start: usize,
    stored: Vec<u8>,
    decoded: Option<Vec<u8>>,
    flate: bool,
    text_candidate: bool,
}

/// Find every stream in a full-rewrite output and decompress Flate streams.
fn stream_regions(bytes: &[u8]) -> Vec<StreamRegion> {
    let marker = b"\nstream\n";
    let mut regions = Vec::new();
    let mut position = 0usize;
    while let Some(relative) = find_subslice(&bytes[position..], marker) {
        let start = position + relative;
        let data_start = start + marker.len();
        let Some(dictionary_start) = bytes[..start].iter().rposition(|byte| *byte == b'\n') else {
            position = data_start;
            continue;
        };
        let dictionary = &bytes[dictionary_start + 1..start];
        let Some(length) = dictionary_integer(dictionary, b"/Length ") else {
            position = data_start;
            continue;
        };
        let Ok(length) = usize::try_from(length) else {
            position = data_start;
            continue;
        };
        let data_end = data_start.saturating_add(length);
        if data_end > bytes.len() {
            position = data_start;
            continue;
        }
        let object_number = object_number_before(bytes, dictionary_start);
        let flate = contains_subslice(dictionary, b"/FlateDecode");
        let text_candidate = stream_can_carry_text(dictionary);
        let stored = bytes[data_start..data_end].to_vec();
        let decoded = if flate {
            decode_stream(&stored, &[Filter::FlateDecode(FlateParams::default())]).ok()
        } else {
            None
        };
        if let Some(number) = object_number {
            regions.push(StreamRegion {
                object_number: number,
                data_start,
                stored,
                decoded,
                flate,
                text_candidate,
            });
        }
        position = data_end.saturating_add(1);
    }
    regions
}

/// Return `false` for binary streams that cannot carry page text.
///
/// Font programs, image data and structural streams are excluded from the
/// UTF-16LE scan: an arbitrary byte pair in a font program or image is not a
/// string a reader can extract, and treating it as a survivor makes the
/// verifier unusable. Content streams, form XObjects, annotation appearances,
/// XMP metadata and `/ToUnicode` CMaps are all scanned.
fn stream_can_carry_text(dictionary: &[u8]) -> bool {
    const BINARY_MARKERS: [&[u8]; 12] = [
        b"/Subtype /Image",
        b"/Subtype /Type1C",
        b"/Subtype /CIDFontType0C",
        b"/Subtype /OpenType",
        b"/Type /ObjStm",
        b"/Type /XRef",
        b"/Type /EmbeddedFile",
        b"/Length1 ",
        b"/Length2 ",
        b"/Length3 ",
        b"/Type /Font",
        b"/Type /FontDescriptor",
    ];
    !BINARY_MARKERS
        .iter()
        .any(|marker| contains_subslice(dictionary, marker))
}

/// Find the integer value following `key` in a serialized dictionary.
fn dictionary_integer(dictionary: &[u8], key: &[u8]) -> Option<i64> {
    let start = find_subslice(dictionary, key)? + key.len();
    let rest = &dictionary[start..];
    let text = std::str::from_utf8(rest).ok()?;
    let token = text.split_ascii_whitespace().next()?;
    token.parse::<i64>().ok()
}

/// Read the object number from the `N G obj` line before a dictionary.
fn object_number_before(bytes: &[u8], dictionary_start: usize) -> Option<u32> {
    let before = &bytes[..dictionary_start];
    let line_start = before
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map(|index| index + 1)
        .unwrap_or(0);
    let line = std::str::from_utf8(&before[line_start..]).ok()?;
    let mut parts = line.split_ascii_whitespace();
    parts.next()?.parse::<u32>().ok()
}

/// Encode one string as UTF-16LE bytes.
fn utf16le(text: &str) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(text.len() * 2);
    for unit in text.encode_utf16() {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    bytes
}

/// Encode one string as UTF-16BE bytes.
fn utf16be(text: &str) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(text.len() * 2);
    for unit in text.encode_utf16() {
        bytes.extend_from_slice(&unit.to_be_bytes());
    }
    bytes
}

/// Return `true` when `haystack` contains `needle`.
fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty() && find_subslice(haystack, needle).is_some()
}

/// Return the index of the first occurrence of `needle`.
fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || needle.len() > haystack.len() {
        return None;
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// Return `true` when `haystack` contains `needle`.
fn contains_subslice(haystack: &[u8], needle: &[u8]) -> bool {
    find_subslice(haystack, needle).is_some()
}

/// Format a number for a content-stream operator without exponent notation.
fn format_number(value: f64) -> String {
    if !value.is_finite() {
        return "0".to_owned();
    }
    let text = format!("{value:.6}");
    let trimmed = text.trim_end_matches('0').trim_end_matches('.');
    if trimmed.is_empty() || trimmed == "-" {
        "0".to_owned()
    } else {
        trimmed.to_owned()
    }
}
