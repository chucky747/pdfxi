//! Comment and markup editing on a real [`pdfxi_core::Document`].
//!
//! Unlike the renderer-agnostic models elsewhere in this crate, this module
//! mutates the typed core document directly. A note or highlight becomes a real
//! annotation dictionary in the page's `/Annots` array, carrying a form XObject
//! appearance stream so another viewer (PDFium, Acrobat, a printer) draws it
//! without having to synthesise an appearance itself.
//!
//! The module also reads annotations back ([`list_annotations`]) and exchanges
//! them as XFDF ([`export_xfdf`] / [`import_xfdf`]) for the shared-review and
//! Comment & Review acceptance items.
//!
//! # Write model
//!
//! Every operation is copy-on-write: it stages objects through
//! [`pdfxi_core::Document::set_object`] and never mutates the source bytes.
//! Because the core document exposes only its original objects, each operation
//! first builds an in-memory view of the current state by appending the staged
//! changes with [`pdfxi_core::Document::save_incremental`] and reparsing that
//! buffer. This keeps repeated edits on one document correct without a private
//! core accessor; [`DocumentSession`](https://docs.rs/pdfxi-document) reparses
//! after every edit anyway, so the cost matches the session write path.
//!
//! # What a written annotation contains
//!
//! Both kinds are written with `/Type /Annot`, their `/Subtype`, a normalized
//! `/Rect`, `/Contents`, `/T` (author), `/NM` (a unique identifier), `/M` (the
//! PDF date), a `/C` colour array, `/CA` opacity, `/F 4` (print) and
//! `/AP << /N <form XObject> >>`. The form carries `/BBox`, `/Resources` and a
//! content stream: a vector note icon for `/Text`, a translucent fill for
//! `/Highlight`. Highlights also carry `/QuadPoints` as required by
//! ISO 32000-1 for markup annotations.
//!
//! [`DocumentSession`]: https://docs.rs/pdfxi-document

use std::time::{SystemTime, UNIX_EPOCH};

use pdfxi_core::{Dictionary, Document, Object, ObjectId, decode_text_string, encode_text_string};
use thiserror::Error;

use crate::geometry::{GeometryError, Point, Rect};

/// Side length of the sticky-note icon, in default user space units.
pub const NOTE_ICON_SIZE: f32 = 24.0;

/// Fill colour of the note icon as an RGB triple in `0.0..=1.0`.
const NOTE_COLOR: [f64; 3] = [1.0, 0.82, 0.1];

/// Fill colour of a highlight as an RGB triple in `0.0..=1.0`.
const HIGHLIGHT_COLOR: [f64; 3] = [1.0, 1.0, 0.0];

/// Annotation opacity written to `/CA` for highlights.
const HIGHLIGHT_OPACITY: f64 = 0.5;

/// One annotation subtype this module writes and reads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AnnotationSubtype {
    /// A sticky-note text comment (`/Subtype /Text`).
    Text,
    /// A text highlight markup (`/Subtype /Highlight`).
    Highlight,
    /// Any other subtype found in an existing document.
    Other(String),
}

impl AnnotationSubtype {
    /// Map a PDF `/Subtype` name to a typed subtype.
    pub fn from_pdf_name(name: &str) -> Self {
        if name.eq_ignore_ascii_case("Text") {
            Self::Text
        } else if name.eq_ignore_ascii_case("Highlight") {
            Self::Highlight
        } else {
            Self::Other(name.to_owned())
        }
    }

    /// Return the `/Subtype` name written to the annotation dictionary.
    pub fn as_pdf_name(&self) -> &str {
        match self {
            Self::Text => "Text",
            Self::Highlight => "Highlight",
            Self::Other(name) => name,
        }
    }
}

/// A review state written to `/State` with `/StateModel /Review`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AnnotationState {
    /// The comment is accepted (resolved).
    Accepted,
    /// The comment is rejected.
    Rejected,
    /// The comment is cancelled.
    Cancelled,
    /// The comment is completed.
    Completed,
    /// The comment is marked.
    Marked,
    /// The comment is unmarked.
    Unmarked,
}

impl AnnotationState {
    /// Return the PDF name written to `/State`.
    pub fn as_pdf_name(self) -> &'static str {
        match self {
            Self::Accepted => "Accepted",
            Self::Rejected => "Rejected",
            Self::Cancelled => "Cancelled",
            Self::Completed => "Completed",
            Self::Marked => "Marked",
            Self::Unmarked => "Unmarked",
        }
    }

    /// Parse a `/State` name, returning `None` for an unknown state.
    pub fn from_pdf_name(name: &str) -> Option<Self> {
        match name {
            "Accepted" => Some(Self::Accepted),
            "Rejected" => Some(Self::Rejected),
            "Cancelled" => Some(Self::Cancelled),
            "Completed" => Some(Self::Completed),
            "Marked" => Some(Self::Marked),
            "Unmarked" => Some(Self::Unmarked),
            _ => None,
        }
    }
}

/// A reference to an annotation this module created or found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AnnotationRef {
    /// The `/NM` identifier.
    pub id: String,
    /// The indirect object holding the annotation dictionary.
    pub object: ObjectId,
    /// Zero-based page that holds the annotation.
    pub page_index: usize,
    /// The annotation subtype.
    pub subtype: AnnotationSubtype,
}

/// A read-only view of one annotation on a page.
#[derive(Clone, Debug, PartialEq)]
pub struct AnnotationView {
    /// The indirect object holding the annotation dictionary.
    pub object: ObjectId,
    /// The `/NM` identifier, or an empty string when the file omits it.
    pub id: String,
    /// The annotation subtype.
    pub subtype: AnnotationSubtype,
    /// The `/Contents` text decoded from its PDF string.
    pub contents: String,
    /// The `/T` author decoded from its PDF string.
    pub author: String,
    /// `/NM` of the `/IRT` parent annotation, when the annotation is a reply.
    pub parent_id: Option<String>,
    /// The raw `/State` name, when present.
    pub state: Option<String>,
    /// The `/Rect` bounds in default user space.
    pub rect: Rect,
    /// The `/M` date string, when present.
    pub date: Option<String>,
}

/// Typed errors returned by the annotation library.
#[derive(Debug, Error)]
pub enum AnnotationEditError {
    /// The core parser or writer rejected the document.
    #[error(transparent)]
    Core(#[from] pdfxi_core::Error),
    /// A caller-supplied rectangle or point was invalid.
    #[error(transparent)]
    Geometry(#[from] GeometryError),
    /// The annotation identifier was empty.
    #[error("annotation identifier must not be empty")]
    EmptyId,
    /// The annotation identifier is already used on the page.
    #[error("annotation identifier '{id}' already exists on page {page}")]
    DuplicateId {
        /// The rejected identifier.
        id: String,
        /// The zero-based page index.
        page: usize,
    },
    /// The requested annotation does not exist on the page.
    #[error("annotation '{id}' was not found on page {page}")]
    AnnotationNotFound {
        /// The identifier that was searched for.
        id: String,
        /// The zero-based page index.
        page: usize,
    },
    /// A page's `/Annots` entry was not an array.
    #[error("page {page} /Annots is not an array")]
    MalformedAnnots {
        /// The zero-based page index.
        page: usize,
    },
    /// A page object was not a dictionary.
    #[error("page object {0} is not a dictionary")]
    MalformedPage(ObjectId),
    /// An annotation object was not a dictionary.
    #[error("annotation object {0} is not a dictionary")]
    MalformedAnnotation(ObjectId),
    /// A page lacked a rectangle that an annotation operation requires.
    #[error("page {0} has no valid /MediaBox")]
    MissingPageBounds(usize),
    /// A highlight was given no usable quads.
    #[error("a highlight requires at least one non-empty quad")]
    EmptyQuads,
    /// The PDF object-number space is exhausted.
    #[error("the PDF object number space is exhausted")]
    ObjectSpaceExhausted,
    /// XFDF text was structurally malformed.
    #[error("XFDF parse error at byte {offset}: {detail}")]
    XfdfParse {
        /// Byte offset of the problem.
        offset: usize,
        /// Human-readable explanation.
        detail: String,
    },
    /// An XFDF annotation was missing a required attribute.
    #[error("XFDF annotation at byte {offset} is missing attribute '{name}'")]
    XfdfAttribute {
        /// Byte offset of the annotation element.
        offset: usize,
        /// The missing attribute name.
        name: &'static str,
    },
    /// An XFDF annotation attribute had an invalid value.
    #[error("XFDF annotation attribute '{name}' is invalid: {value}")]
    XfdfValue {
        /// The attribute name.
        name: &'static str,
        /// The rejected value.
        value: String,
    },
    /// An XFDF annotation used a subtype this module does not create.
    #[error("unsupported XFDF annotation subtype '{0}'")]
    UnsupportedSubtype(String),
}

// ---------------------------------------------------------------------------
// Public operations
// ---------------------------------------------------------------------------

/// Add a sticky-note comment to `page_index`.
///
/// The icon is [`NOTE_ICON_SIZE`] points square with its lower-left corner at
/// `point`. `id` becomes the `/NM` identifier and must not already be used on
/// the page. The annotation gets a vector icon appearance stream, so it is
/// visible in viewers that do not synthesise appearances.
///
/// # Errors
///
/// Returns [`AnnotationEditError::EmptyId`] for an empty identifier, a duplicate or
/// missing-page error, a geometry error for a non-finite point, or a core error
/// when the document cannot be read or staged.
pub fn add_text_note(
    document: &mut Document,
    page_index: usize,
    point: Point,
    contents: &str,
    author: &str,
    id: &str,
) -> Result<AnnotationRef, AnnotationEditError> {
    add_text_note_with_date(document, page_index, point, contents, author, id, None)
}

/// Add a highlight covering every rectangle in `quads`.
///
/// The annotation `/Rect` is the union of the quads, `/QuadPoints` carries each
/// quad, and the appearance stream fills them with translucent yellow. The
/// `/NM` identifier is derived from the allocated annotation object number.
///
/// # Errors
///
/// Returns [`AnnotationEditError::EmptyQuads`] when `quads` is empty or every quad is
/// degenerate, a missing-page error, or a core error when the document cannot
/// be read or staged.
pub fn add_highlight(
    document: &mut Document,
    page_index: usize,
    quads: &[Rect],
    contents: &str,
    author: &str,
) -> Result<AnnotationRef, AnnotationEditError> {
    add_highlight_with(document, page_index, quads, contents, author, None, None)
}

/// List the annotations on `page_index`, resolving each referenced dictionary.
///
/// Entries that are not indirect references or do not resolve to a dictionary
/// are skipped, so a hostile `/Annots` array cannot make the listing fail.
///
/// # Errors
///
/// Returns a missing-page or malformed-`/Annots` error, or a core error when the
/// document cannot be read.
pub fn list_annotations(
    document: &Document,
    page_index: usize,
) -> Result<Vec<AnnotationView>, AnnotationEditError> {
    let current = effective_document(document)?;
    list_on_page(&current, page_index)
}

/// Reply to the annotation whose `/NM` is `parent` on `page_index`.
///
/// The new annotation is a `/Text` note carrying `/IRT` set to the parent's
/// indirect object and `/RT /R`, which is how viewers build a comment thread.
///
/// # Errors
///
/// Returns [`AnnotationEditError::AnnotationNotFound`] when `parent` is not on the page,
/// or any error from [`add_text_note`].
pub fn reply_to(
    document: &mut Document,
    page_index: usize,
    parent: &str,
    contents: &str,
    author: &str,
) -> Result<AnnotationRef, AnnotationEditError> {
    reply_to_with_date(document, page_index, parent, contents, author, None)
}

/// Set `/State` (and `/StateModel /Review`) on the annotation `id`.
///
/// # Errors
///
/// Returns [`AnnotationEditError::AnnotationNotFound`] when the annotation is missing, or
/// a core error when the change cannot be staged.
pub fn set_state(
    document: &mut Document,
    page_index: usize,
    id: &str,
    state: AnnotationState,
) -> Result<(), AnnotationEditError> {
    let current = effective_document(document)?;
    let (_, object, mut dictionary) = find_annotation(&current, page_index, id)?;
    dictionary.insert("State".to_owned(), pdf_name(state.as_pdf_name()));
    dictionary.insert("StateModel".to_owned(), pdf_name("Review"));
    document.set_object(object, Object::Dictionary(dictionary))?;
    Ok(())
}

/// Replace the `/Contents` text of the annotation `id`.
///
/// The annotation's appearance stream is left in place: the note icon does not
/// embed its contents, and a highlight is defined by its `/QuadPoints`.
///
/// # Errors
///
/// Returns [`AnnotationEditError::AnnotationNotFound`] when the annotation is missing, or
/// a core error when the change cannot be staged.
pub fn update_contents(
    document: &mut Document,
    page_index: usize,
    id: &str,
    contents: &str,
) -> Result<(), AnnotationEditError> {
    let current = effective_document(document)?;
    let (_, object, mut dictionary) = find_annotation(&current, page_index, id)?;
    dictionary.insert("Contents".to_owned(), pdf_text(contents));
    dictionary.insert("M".to_owned(), pdf_text(&now_pdf_date()));
    document.set_object(object, Object::Dictionary(dictionary))?;
    Ok(())
}

/// Remove the annotation `id` from `page_index` and overwrite its object.
///
/// The `/Annots` entry is removed and the annotation object (plus its `/AP /N`
/// appearance stream, when it is indirect) is replaced with `null`, so a full
/// rewrite does not retain the comment text. An incremental save keeps the
/// earlier revision by design; permanent erasure is the redaction path.
///
/// # Errors
///
/// Returns [`AnnotationEditError::AnnotationNotFound`] when the annotation is missing, or
/// a core error when the change cannot be staged.
pub fn delete_annotation(
    document: &mut Document,
    page_index: usize,
    id: &str,
) -> Result<(), AnnotationEditError> {
    let current = effective_document(document)?;
    let (index, object, dictionary) = find_annotation(&current, page_index, id)?;
    let mut page = read_page_state(&current, page_index)?;
    if index < page.annots.items().len() {
        page.annots.items_mut().remove(index);
    }
    stage_page_state(document, page)?;
    document.set_object(object, Object::Null)?;
    if let Some(appearance) = appearance_object(&dictionary) {
        document.set_object(appearance, Object::Null)?;
    }
    Ok(())
}

/// Export every annotation in the document as XFDF text.
///
/// The output is a small, deterministic XFDF document whose `<annots>` block
/// holds one `<annot>` element per annotation, in page order. Attributes are
/// `page`, `subtype`, `rect` (normalized `x0,y0,x1,y1`), `author`, `date` and
/// `id`; the element text is the `/Contents` value with XML escaping. A thread
/// reply adds `replyto`, and a resolved comment adds `state`, so the exported
/// file can carry a review thread.
///
/// # Errors
///
/// Returns a missing-page or malformed-`/Annots` error, or a core error when a
/// page cannot be read.
pub fn export_xfdf(document: &Document) -> Result<String, AnnotationEditError> {
    let current = effective_document(document)?;
    let page_count = current.page_count()?;
    let mut output = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <xfdf xmlns=\"http://ns.adobe.com/xfdf/\" xml:space=\"preserve\">\n  <annots>\n",
    );
    for page_index in 0..page_count {
        for view in list_on_page(&current, page_index)? {
            output.push_str("    <annot page=\"");
            output.push_str(&page_index.to_string());
            output.push_str("\" subtype=\"");
            output.push_str(&escape_xml_text(view.subtype.as_pdf_name()));
            output.push_str("\" rect=\"");
            output.push_str(&format_rect(view.rect));
            output.push_str("\" author=\"");
            output.push_str(&escape_xml_text(&view.author));
            output.push_str("\" date=\"");
            output.push_str(&escape_xml_text(view.date.as_deref().unwrap_or("")));
            output.push_str("\" id=\"");
            output.push_str(&escape_xml_text(&view.id));
            output.push('"');
            if let Some(parent) = &view.parent_id {
                output.push_str(" replyto=\"");
                output.push_str(&escape_xml_text(parent));
                output.push('"');
            }
            if let Some(state) = &view.state {
                output.push_str(" state=\"");
                output.push_str(&escape_xml_text(state));
                output.push('"');
            }
            output.push('>');
            output.push_str(&escape_xml_text(&view.contents));
            output.push_str("</annot>\n");
        }
    }
    output.push_str("  </annots>\n</xfdf>\n");
    Ok(output)
}

/// Import the `<annots>` block of an XFDF document into `document`.
///
/// Text annotations become [`add_text_note`] notes, highlights become
/// [`add_highlight`] calls covering the exported rectangle, and an annotation
/// with a `replyto` attribute becomes a thread reply. `author` is used when the
/// element has no `author` attribute. The returned count is the number of
/// annotations created.
///
/// # Errors
///
/// Returns an [`AnnotationEditError::XfdfParse`] or [`AnnotationEditError::XfdfValue`] error for
/// malformed markup, [`AnnotationEditError::UnsupportedSubtype`] for a subtype this module
/// does not create, or any error from the add/state operations.
pub fn import_xfdf(
    document: &mut Document,
    xml: &str,
    author: &str,
) -> Result<usize, AnnotationEditError> {
    let annotations = parse_xfdf_annots(xml)?;
    let mut imported = 0usize;
    for raw in annotations {
        let page_index = required_attribute(&raw, "page")?
            .parse::<usize>()
            .map_err(|_| AnnotationEditError::XfdfValue {
                name: "page",
                value: required_attribute(&raw, "page").unwrap_or("").to_owned(),
            })?;
        let rect = parse_xfdf_rect(required_attribute(&raw, "rect")?)?;
        let subtype = required_attribute(&raw, "subtype")?;
        let element_author = raw
            .attribute("author")
            .filter(|value| !value.is_empty())
            .unwrap_or(author);
        let date = raw.attribute("date").filter(|value| !value.is_empty());
        let id = raw
            .attribute("id")
            .filter(|value| !value.is_empty())
            .map(str::to_owned);
        let reply_to = raw.attribute("replyto");

        let created = if subtype.eq_ignore_ascii_case("Text") {
            match reply_to {
                Some(parent) => reply_to_with_date(
                    document,
                    page_index,
                    parent,
                    &raw.contents,
                    element_author,
                    date,
                )?,
                None => {
                    let generated;
                    let id = match id {
                        Some(id) => id,
                        None => {
                            generated = format!("pdfxi-import-{imported}");
                            generated
                        }
                    };
                    add_text_note_with_date(
                        document,
                        page_index,
                        Point::new(rect.x0(), rect.y0()),
                        &raw.contents,
                        element_author,
                        &id,
                        date,
                    )?
                }
            }
        } else if subtype.eq_ignore_ascii_case("Highlight") {
            add_highlight_with(
                document,
                page_index,
                std::slice::from_ref(&rect),
                &raw.contents,
                element_author,
                date,
                id.as_deref(),
            )?
        } else {
            return Err(AnnotationEditError::UnsupportedSubtype(subtype.to_owned()));
        };

        if let Some(state) = raw.attribute("state")
            && let Some(state) = AnnotationState::from_pdf_name(state)
        {
            set_state(document, page_index, &created.id, state)?;
        }
        imported += 1;
    }
    Ok(imported)
}

// ---------------------------------------------------------------------------
// Annotation construction
// ---------------------------------------------------------------------------

/// Inputs shared by a text note and a reply.
struct NoteSpec<'a> {
    rect: Rect,
    contents: &'a str,
    author: &'a str,
    id: &'a str,
    date: &'a str,
    parent: Option<ObjectId>,
}

fn add_text_note_with_date(
    document: &mut Document,
    page_index: usize,
    point: Point,
    contents: &str,
    author: &str,
    id: &str,
    date: Option<&str>,
) -> Result<AnnotationRef, AnnotationEditError> {
    if id.is_empty() {
        return Err(AnnotationEditError::EmptyId);
    }
    if !point.is_finite() {
        return Err(AnnotationEditError::Geometry(
            GeometryError::NonFiniteCoordinate {
                field: "point",
                value: point.x,
            },
        ));
    }
    let rect = Rect::new(
        point.x,
        point.y,
        point.x + NOTE_ICON_SIZE,
        point.y + NOTE_ICON_SIZE,
    )?;
    add_note_with(document, page_index, rect, contents, author, id, date, None)
}

fn reply_to_with_date(
    document: &mut Document,
    page_index: usize,
    parent: &str,
    contents: &str,
    author: &str,
    date: Option<&str>,
) -> Result<AnnotationRef, AnnotationEditError> {
    let current = effective_document(document)?;
    let (_, parent_object, parent_dictionary) = find_annotation(&current, page_index, parent)?;
    let parent_rect = rect_from_dictionary(&parent_dictionary);
    let id = format!("pdfxi-reply-{}", parent_object.number);
    add_note_with(
        document,
        page_index,
        parent_rect,
        contents,
        author,
        &id,
        date,
        Some(parent_object),
    )
}

#[allow(clippy::too_many_arguments)]
fn add_note_with(
    document: &mut Document,
    page_index: usize,
    rect: Rect,
    contents: &str,
    author: &str,
    id: &str,
    date: Option<&str>,
    parent: Option<ObjectId>,
) -> Result<AnnotationRef, AnnotationEditError> {
    let current = effective_document(document)?;
    let mut page = read_page_state(&current, page_index)?;
    if annotation_id_exists(&current, &page.annots, id)? {
        return Err(AnnotationEditError::DuplicateId {
            id: id.to_owned(),
            page: page_index,
        });
    }

    let ids = allocate_ids(document, 2)?;
    let appearance_id = ids[0];
    let annotation_id = ids[1];
    let now;
    let date = match date {
        Some(date) => date,
        None => {
            now = now_pdf_date();
            now.as_str()
        }
    };
    let spec = NoteSpec {
        rect,
        contents,
        author,
        id,
        date,
        parent,
    };
    let annotation = build_note_annotation(&spec, appearance_id);
    let appearance = note_icon_appearance(f64::from(NOTE_ICON_SIZE), NOTE_COLOR);

    document.set_object(appearance_id, appearance)?;
    document.set_object(annotation_id, Object::Dictionary(annotation))?;
    page.annots
        .items_mut()
        .push(Object::Reference(annotation_id));
    stage_page_state(document, page)?;

    Ok(AnnotationRef {
        id: id.to_owned(),
        object: annotation_id,
        page_index,
        subtype: AnnotationSubtype::Text,
    })
}

fn add_highlight_with(
    document: &mut Document,
    page_index: usize,
    quads: &[Rect],
    contents: &str,
    author: &str,
    date: Option<&str>,
    id: Option<&str>,
) -> Result<AnnotationRef, AnnotationEditError> {
    let mut bounds: Option<Rect> = None;
    let mut usable: Vec<Rect> = Vec::with_capacity(quads.len());
    for quad in quads {
        if quad.is_empty() {
            continue;
        }
        usable.push(*quad);
        bounds = Some(match bounds {
            Some(current) => current.union(quad),
            None => *quad,
        });
    }
    let Some(rect) = bounds else {
        return Err(AnnotationEditError::EmptyQuads);
    };

    let current = effective_document(document)?;
    let mut page = read_page_state(&current, page_index)?;
    let ids = allocate_ids(document, 2)?;
    let appearance_id = ids[0];
    let annotation_id = ids[1];
    let generated = format!("pdfxi-highlight-{}", annotation_id.number);
    let id = match id {
        Some(id) if !id.is_empty() => id,
        _ => generated.as_str(),
    };
    if annotation_id_exists(&current, &page.annots, id)? {
        return Err(AnnotationEditError::DuplicateId {
            id: id.to_owned(),
            page: page_index,
        });
    }
    let now;
    let date = match date {
        Some(date) => date,
        None => {
            now = now_pdf_date();
            now.as_str()
        }
    };

    let annotation =
        build_highlight_annotation(rect, &usable, contents, author, id, date, appearance_id);
    let appearance = highlight_appearance(rect, &usable);

    document.set_object(appearance_id, appearance)?;
    document.set_object(annotation_id, Object::Dictionary(annotation))?;
    page.annots
        .items_mut()
        .push(Object::Reference(annotation_id));
    stage_page_state(document, page)?;

    Ok(AnnotationRef {
        id: id.to_owned(),
        object: annotation_id,
        page_index,
        subtype: AnnotationSubtype::Highlight,
    })
}

fn build_note_annotation(spec: &NoteSpec<'_>, appearance_id: ObjectId) -> Dictionary {
    let mut dictionary = Dictionary::new();
    dictionary.insert("Type".to_owned(), pdf_name("Annot"));
    dictionary.insert("Subtype".to_owned(), pdf_name("Text"));
    dictionary.insert("Rect".to_owned(), rect_array(spec.rect));
    dictionary.insert("Contents".to_owned(), pdf_text(spec.contents));
    dictionary.insert("T".to_owned(), pdf_text(spec.author));
    dictionary.insert("NM".to_owned(), pdf_text(spec.id));
    dictionary.insert("M".to_owned(), pdf_text(spec.date));
    dictionary.insert("C".to_owned(), color_array(NOTE_COLOR));
    dictionary.insert("CA".to_owned(), Object::Real(1.0));
    dictionary.insert("F".to_owned(), Object::Integer(4));
    dictionary.insert("AP".to_owned(), appearance_dictionary(appearance_id));
    if let Some(parent) = spec.parent {
        dictionary.insert("IRT".to_owned(), Object::Reference(parent));
        dictionary.insert("RT".to_owned(), pdf_name("R"));
    }
    dictionary
}

fn build_highlight_annotation(
    rect: Rect,
    quads: &[Rect],
    contents: &str,
    author: &str,
    id: &str,
    date: &str,
    appearance_id: ObjectId,
) -> Dictionary {
    let mut dictionary = Dictionary::new();
    dictionary.insert("Type".to_owned(), pdf_name("Annot"));
    dictionary.insert("Subtype".to_owned(), pdf_name("Highlight"));
    dictionary.insert("Rect".to_owned(), rect_array(rect));
    dictionary.insert("QuadPoints".to_owned(), quad_points_array(quads));
    dictionary.insert("Contents".to_owned(), pdf_text(contents));
    dictionary.insert("T".to_owned(), pdf_text(author));
    dictionary.insert("NM".to_owned(), pdf_text(id));
    dictionary.insert("M".to_owned(), pdf_text(date));
    dictionary.insert("C".to_owned(), color_array(HIGHLIGHT_COLOR));
    dictionary.insert("CA".to_owned(), Object::Real(HIGHLIGHT_OPACITY));
    dictionary.insert("F".to_owned(), Object::Integer(4));
    dictionary.insert("AP".to_owned(), appearance_dictionary(appearance_id));
    dictionary
}

fn appearance_dictionary(appearance_id: ObjectId) -> Object {
    let mut appearance = Dictionary::new();
    appearance.insert("N".to_owned(), Object::Reference(appearance_id));
    Object::Dictionary(appearance)
}

/// Build a vector sticky-note icon as a form XObject.
fn note_icon_appearance(size: f64, color: [f64; 3]) -> Object {
    let [red, green, blue] = color;
    let inner = size - 0.75;
    let data = format!(
        "q\n\
         {red:.3} {green:.3} {blue:.3} rg\n\
         0 0 {size:.3} {size:.3} re f\n\
         0.25 0.25 0.25 RG 0.75 w\n\
         0.375 0.375 {inner:.3} {inner:.3} re S\n\
         Q\n\
         q\n\
         0.25 0.25 0.25 RG 0.75 w\n\
         4.000 6.000 m 20.000 6.000 l S\n\
         4.000 11.000 m 20.000 11.000 l S\n\
         4.000 16.000 m 15.000 16.000 l S\n\
         Q\n"
    );
    let mut dictionary = form_dictionary(size, size);
    dictionary.insert(
        "Resources".to_owned(),
        Object::Dictionary(Dictionary::new()),
    );
    Object::Stream {
        dictionary,
        data: data.into_bytes(),
    }
}

/// Build a translucent highlight form XObject whose coordinates are relative to
/// the annotation `/Rect` lower-left corner.
fn highlight_appearance(rect: Rect, quads: &[Rect]) -> Object {
    let width = f64::from(rect.width());
    let height = f64::from(rect.height());
    let mut data = String::from("q\n/GS0 gs\n1.000 1.000 0.000 rg\n");
    for quad in quads {
        let x = f64::from(quad.x0() - rect.x0());
        let y = f64::from(quad.y0() - rect.y0());
        let w = f64::from(quad.width());
        let h = f64::from(quad.height());
        data.push_str(&format!("{x:.3} {y:.3} {w:.3} {h:.3} re f\n"));
    }
    data.push_str("Q\n");

    let mut graphics_state = Dictionary::new();
    graphics_state.insert("Type".to_owned(), pdf_name("ExtGState"));
    graphics_state.insert("ca".to_owned(), Object::Real(HIGHLIGHT_OPACITY));
    graphics_state.insert("CA".to_owned(), Object::Real(HIGHLIGHT_OPACITY));
    let mut ext_gstates = Dictionary::new();
    ext_gstates.insert("GS0".to_owned(), Object::Dictionary(graphics_state));
    let mut resources = Dictionary::new();
    resources.insert("ExtGState".to_owned(), Object::Dictionary(ext_gstates));

    let mut dictionary = form_dictionary(width, height);
    dictionary.insert("Resources".to_owned(), Object::Dictionary(resources));
    Object::Stream {
        dictionary,
        data: data.into_bytes(),
    }
}

fn form_dictionary(width: f64, height: f64) -> Dictionary {
    let mut dictionary = Dictionary::new();
    dictionary.insert("Type".to_owned(), pdf_name("XObject"));
    dictionary.insert("Subtype".to_owned(), pdf_name("Form"));
    dictionary.insert("FormType".to_owned(), Object::Integer(1));
    dictionary.insert(
        "BBox".to_owned(),
        Object::Array(vec![
            Object::Real(0.0),
            Object::Real(0.0),
            Object::Real(width),
            Object::Real(height),
        ]),
    );
    dictionary.insert("Matrix".to_owned(), identity_matrix());
    dictionary
}

fn identity_matrix() -> Object {
    Object::Array(vec![
        Object::Real(1.0),
        Object::Real(0.0),
        Object::Real(0.0),
        Object::Real(1.0),
        Object::Real(0.0),
        Object::Real(0.0),
    ])
}

fn rect_array(rect: Rect) -> Object {
    Object::Array(vec![
        Object::Real(f64::from(rect.x0())),
        Object::Real(f64::from(rect.y0())),
        Object::Real(f64::from(rect.x1())),
        Object::Real(f64::from(rect.y1())),
    ])
}

/// ISO 32000-1 quad order: upper-left, upper-right, lower-left, lower-right.
fn quad_points_array(quads: &[Rect]) -> Object {
    let mut points = Vec::with_capacity(quads.len() * 8);
    for quad in quads {
        points.push(Object::Real(f64::from(quad.x0())));
        points.push(Object::Real(f64::from(quad.y1())));
        points.push(Object::Real(f64::from(quad.x1())));
        points.push(Object::Real(f64::from(quad.y1())));
        points.push(Object::Real(f64::from(quad.x0())));
        points.push(Object::Real(f64::from(quad.y0())));
        points.push(Object::Real(f64::from(quad.x1())));
        points.push(Object::Real(f64::from(quad.y0())));
    }
    Object::Array(points)
}

fn color_array(color: [f64; 3]) -> Object {
    Object::Array(color.into_iter().map(Object::Real).collect())
}

fn pdf_name(name: &str) -> Object {
    Object::Name(name.to_owned())
}

fn pdf_text(text: &str) -> Object {
    Object::String(encode_text_string(text))
}

// ---------------------------------------------------------------------------
// Document access helpers
// ---------------------------------------------------------------------------

/// Build a reparsed view that includes every staged change.
fn effective_document(document: &Document) -> Result<Document, AnnotationEditError> {
    let bytes = document.save_incremental()?;
    Document::from_bytes(bytes).map_err(AnnotationEditError::Core)
}

/// A page dictionary together with the shape of its `/Annots` entry.
struct PageState {
    page_id: ObjectId,
    dictionary: Dictionary,
    stream_data: Option<Vec<u8>>,
    annots: Annots,
}

/// The `/Annots` value: either an inline array or an indirect array object.
enum Annots {
    Direct(Vec<Object>),
    Indirect(ObjectId, Vec<Object>),
}

impl Annots {
    fn items(&self) -> &[Object] {
        match self {
            Self::Direct(items) | Self::Indirect(_, items) => items,
        }
    }

    fn items_mut(&mut self) -> &mut Vec<Object> {
        match self {
            Self::Direct(items) | Self::Indirect(_, items) => items,
        }
    }
}

fn read_page_state(
    document: &Document,
    page_index: usize,
) -> Result<PageState, AnnotationEditError> {
    let page = document.page(page_index)?;
    let object = document.object(page.id)?;
    let (dictionary, stream_data) = match object {
        Object::Dictionary(dictionary) => (dictionary, None),
        Object::Stream { dictionary, data } => (dictionary, Some(data)),
        _ => return Err(AnnotationEditError::MalformedPage(page.id)),
    };
    let annots = match dictionary.get("Annots").cloned() {
        None | Some(Object::Null) => Annots::Direct(Vec::new()),
        Some(Object::Array(items)) => Annots::Direct(items),
        Some(Object::Reference(id)) => match document.object(id)? {
            Object::Array(items) => Annots::Indirect(id, items),
            Object::Null => Annots::Direct(Vec::new()),
            _ => {
                return Err(AnnotationEditError::MalformedAnnots { page: page_index });
            }
        },
        Some(_) => {
            return Err(AnnotationEditError::MalformedAnnots { page: page_index });
        }
    };
    Ok(PageState {
        page_id: page.id,
        dictionary,
        stream_data,
        annots,
    })
}

fn stage_page_state(document: &mut Document, page: PageState) -> Result<(), AnnotationEditError> {
    let PageState {
        page_id,
        mut dictionary,
        stream_data,
        annots,
    } = page;
    match annots {
        Annots::Direct(items) => {
            dictionary.insert("Annots".to_owned(), Object::Array(items));
        }
        Annots::Indirect(id, items) => {
            document.set_object(id, Object::Array(items))?;
        }
    }
    let object = match stream_data {
        Some(data) => Object::Stream { dictionary, data },
        None => Object::Dictionary(dictionary),
    };
    document.set_object(page_id, object)?;
    Ok(())
}

fn list_on_page(
    document: &Document,
    page_index: usize,
) -> Result<Vec<AnnotationView>, AnnotationEditError> {
    let page = read_page_state(document, page_index)?;
    let mut views = Vec::new();
    for item in page.annots.items() {
        let Some(id) = item.as_reference() else {
            continue;
        };
        let Ok(Object::Dictionary(dictionary)) = document.object(id) else {
            continue;
        };
        views.push(view_from_dictionary(document, id, &dictionary));
    }
    Ok(views)
}

fn view_from_dictionary(
    document: &Document,
    object: ObjectId,
    dictionary: &Dictionary,
) -> AnnotationView {
    let subtype = dictionary
        .get("Subtype")
        .and_then(Object::as_name)
        .map(AnnotationSubtype::from_pdf_name)
        .unwrap_or_else(|| AnnotationSubtype::Other(String::new()));
    let parent_id = dictionary
        .get("IRT")
        .and_then(Object::as_reference)
        .map(|parent| {
            document
                .object(parent)
                .ok()
                .and_then(|value| value.as_dictionary().cloned())
                .and_then(|dictionary| string_from_dictionary(&dictionary, "NM"))
                .unwrap_or_else(|| format!("{} {} R", parent.number, parent.generation))
        });
    AnnotationView {
        object,
        id: string_from_dictionary(dictionary, "NM").unwrap_or_default(),
        subtype,
        contents: string_from_dictionary(dictionary, "Contents").unwrap_or_default(),
        author: string_from_dictionary(dictionary, "T").unwrap_or_default(),
        parent_id,
        state: dictionary
            .get("State")
            .and_then(Object::as_name)
            .map(str::to_owned),
        rect: rect_from_dictionary(dictionary),
        date: string_from_dictionary(dictionary, "M"),
    }
}

fn find_annotation(
    document: &Document,
    page_index: usize,
    id: &str,
) -> Result<(usize, ObjectId, Dictionary), AnnotationEditError> {
    let page = read_page_state(document, page_index)?;
    for (index, item) in page.annots.items().iter().enumerate() {
        let Some(object_id) = item.as_reference() else {
            continue;
        };
        let Ok(Object::Dictionary(dictionary)) = document.object(object_id) else {
            continue;
        };
        if string_from_dictionary(&dictionary, "NM").as_deref() == Some(id) {
            return Ok((index, object_id, dictionary));
        }
    }
    Err(AnnotationEditError::AnnotationNotFound {
        id: id.to_owned(),
        page: page_index,
    })
}

fn annotation_id_exists(
    document: &Document,
    annots: &Annots,
    id: &str,
) -> Result<bool, AnnotationEditError> {
    for item in annots.items() {
        let Some(object_id) = item.as_reference() else {
            continue;
        };
        if let Object::Dictionary(dictionary) = document.object(object_id)?
            && string_from_dictionary(&dictionary, "NM").as_deref() == Some(id)
        {
            return Ok(true);
        }
    }
    Ok(false)
}

fn rect_from_dictionary(dictionary: &Dictionary) -> Rect {
    let Some(Object::Array(items)) = dictionary.get("Rect") else {
        return Rect::saturating(0.0, 0.0, 0.0, 0.0);
    };
    if items.len() != 4 {
        return Rect::saturating(0.0, 0.0, 0.0, 0.0);
    }
    let values: Option<Vec<f64>> = items.iter().map(Object::as_f64).collect();
    let Some(values) = values else {
        return Rect::saturating(0.0, 0.0, 0.0, 0.0);
    };
    Rect::saturating(
        values[0] as f32,
        values[1] as f32,
        values[2] as f32,
        values[3] as f32,
    )
}

fn string_from_dictionary(dictionary: &Dictionary, key: &str) -> Option<String> {
    match dictionary.get(key) {
        Some(Object::String(bytes)) => Some(decode_text_string(bytes)),
        _ => None,
    }
}

fn appearance_object(dictionary: &Dictionary) -> Option<ObjectId> {
    dictionary
        .get("AP")
        .and_then(Object::as_dictionary)
        .and_then(|appearance| appearance.get("N"))
        .and_then(Object::as_reference)
}

fn allocate_ids(document: &Document, count: usize) -> Result<Vec<ObjectId>, AnnotationEditError> {
    let mut candidate = document.allocate_object_number()?;
    let mut ids = Vec::with_capacity(count);
    while ids.len() < count {
        if candidate == 0 || document.is_object_number_used(candidate) {
            candidate = candidate
                .checked_add(1)
                .ok_or(AnnotationEditError::ObjectSpaceExhausted)?;
            continue;
        }
        ids.push(ObjectId::new(candidate, 0));
        candidate = candidate
            .checked_add(1)
            .ok_or(AnnotationEditError::ObjectSpaceExhausted)?;
    }
    Ok(ids)
}

// ---------------------------------------------------------------------------
// Date helpers
// ---------------------------------------------------------------------------

/// Return the current UTC time as a PDF date string (`D:YYYYMMDDHHmmSSZ`).
fn now_pdf_date() -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    pdf_date_from_unix(seconds)
}

fn pdf_date_from_unix(seconds: u64) -> String {
    let days = i64::try_from(seconds / 86_400).unwrap_or(i64::MAX);
    let remainder = seconds % 86_400;
    let (year, month, day) = civil_from_days(days);
    let hour = remainder / 3_600;
    let minute = (remainder % 3_600) / 60;
    let second = remainder % 60;
    format!("D:{year:04}{month:02}{day:02}{hour:02}{minute:02}{second:02}Z")
}

/// Convert days since the Unix epoch to a proleptic Gregorian date.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let shifted = days + 719_468;
    let era = if shifted >= 0 {
        shifted
    } else {
        shifted - 146_096
    } / 146_097;
    let day_of_era = shifted - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = if month_prime < 10 {
        month_prime + 3
    } else {
        month_prime - 9
    };
    if month <= 2 {
        year += 1;
    }
    (year, month as u32, day as u32)
}

// ---------------------------------------------------------------------------
// XFDF parsing
// ---------------------------------------------------------------------------

/// One `<annot>` element decoded from XFDF.
struct RawXfdfAnnot {
    offset: usize,
    attributes: Vec<(String, String)>,
    contents: String,
}

impl RawXfdfAnnot {
    fn attribute(&self, name: &str) -> Option<&str> {
        self.attributes
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }
}

fn required_attribute<'a>(
    annotation: &'a RawXfdfAnnot,
    name: &'static str,
) -> Result<&'a str, AnnotationEditError> {
    annotation
        .attribute(name)
        .ok_or(AnnotationEditError::XfdfAttribute {
            offset: annotation.offset,
            name,
        })
}

fn parse_xfdf_annots(xml: &str) -> Result<Vec<RawXfdfAnnot>, AnnotationEditError> {
    let Some(open) = find_element_open(xml, "annots", 0) else {
        return Ok(Vec::new());
    };
    let open_end = find_tag_end(xml, open)?;
    let body_start = open_end + 1;
    let Some(close_rel) = xml[body_start..].find("</annots>") else {
        return Err(AnnotationEditError::XfdfParse {
            offset: open,
            detail: "missing </annots> close tag".to_owned(),
        });
    };
    let body_end = body_start + close_rel;

    let mut annotations = Vec::new();
    let mut search = body_start;
    while search < body_end {
        let Some(relative) = xml[search..body_end].find("<annot") else {
            break;
        };
        let tag_start = search + relative;
        let after_name = tag_start + "<annot".len();
        let next = xml[after_name..].chars().next();
        let is_annot = matches!(
            next,
            Some(character)
                if character.is_ascii_whitespace() || character == '>' || character == '/'
        );
        if !is_annot {
            search = after_name;
            continue;
        }
        let tag_end = find_tag_end(xml, tag_start)?;
        let attributes = parse_attributes(&xml[after_name..tag_end], after_name)?;
        let self_closing = xml[tag_start..tag_end].trim_end().ends_with('/');
        let (contents, next_search) = if self_closing {
            (String::new(), tag_end + 1)
        } else {
            let Some(close) = xml[tag_end + 1..body_end].find("</annot>") else {
                return Err(AnnotationEditError::XfdfParse {
                    offset: tag_start,
                    detail: "missing </annot> close tag".to_owned(),
                });
            };
            let text_start = tag_end + 1;
            let text_end = text_start + close;
            (
                decode_entities(xml[text_start..text_end].trim()),
                text_end + "</annot>".len(),
            )
        };
        annotations.push(RawXfdfAnnot {
            offset: tag_start,
            attributes,
            contents,
        });
        search = next_search;
    }
    Ok(annotations)
}

/// Find the byte offset of `<name` followed by whitespace, `>` or `/`.
fn find_element_open(xml: &str, name: &str, from: usize) -> Option<usize> {
    let mut search = from;
    while search < xml.len() {
        let relative = xml[search..].find('<')?;
        let at = search + relative;
        let after = at + 1;
        if xml[after..].starts_with(name) {
            let next = xml[after + name.len()..].chars().next();
            if matches!(
                next,
                Some(character)
                    if character.is_ascii_whitespace() || character == '>' || character == '/'
            ) {
                return Some(at);
            }
        }
        search = at + 1;
    }
    None
}

/// Find the `>` that closes the tag starting at `tag_start`, ignoring quoted `>`.
fn find_tag_end(xml: &str, tag_start: usize) -> Result<usize, AnnotationEditError> {
    let bytes = xml.as_bytes();
    let mut quote: Option<u8> = None;
    let mut position = tag_start + 1;
    while position < bytes.len() {
        let byte = bytes[position];
        match quote {
            Some(active) => {
                if byte == active {
                    quote = None;
                }
            }
            None => match byte {
                b'"' | b'\'' => quote = Some(byte),
                b'>' => return Ok(position),
                _ => {}
            },
        }
        position += 1;
    }
    Err(AnnotationEditError::XfdfParse {
        offset: tag_start,
        detail: "unterminated tag".to_owned(),
    })
}

/// Parse `name="value"` pairs from the inside of an element's open tag.
fn parse_attributes(
    text: &str,
    offset: usize,
) -> Result<Vec<(String, String)>, AnnotationEditError> {
    let bytes = text.as_bytes();
    let mut attributes = Vec::new();
    let mut position = 0usize;
    while position < bytes.len() {
        while position < bytes.len() && is_xml_space(bytes[position]) {
            position += 1;
        }
        if position >= bytes.len() || bytes[position] == b'/' {
            break;
        }
        let name_start = position;
        while position < bytes.len() && bytes[position] != b'=' && !is_xml_space(bytes[position]) {
            position += 1;
        }
        if name_start == position {
            position += 1;
            continue;
        }
        let name = &text[name_start..position];
        while position < bytes.len() && is_xml_space(bytes[position]) {
            position += 1;
        }
        if position >= bytes.len() || bytes[position] != b'=' {
            return Err(AnnotationEditError::XfdfParse {
                offset: offset + name_start,
                detail: format!("attribute '{name}' has no value"),
            });
        }
        position += 1;
        while position < bytes.len() && is_xml_space(bytes[position]) {
            position += 1;
        }
        if position >= bytes.len() || !matches!(bytes[position], b'"' | b'\'') {
            return Err(AnnotationEditError::XfdfParse {
                offset: offset + position,
                detail: format!("attribute '{name}' value must be quoted"),
            });
        }
        let quote = bytes[position];
        position += 1;
        let value_start = position;
        while position < bytes.len() && bytes[position] != quote {
            position += 1;
        }
        if position >= bytes.len() {
            return Err(AnnotationEditError::XfdfParse {
                offset: offset + value_start,
                detail: format!("attribute '{name}' value is unterminated"),
            });
        }
        let value = decode_entities(&text[value_start..position]);
        position += 1;
        attributes.push((name.to_owned(), value));
    }
    Ok(attributes)
}

fn is_xml_space(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\r' | b'\n')
}

fn parse_xfdf_rect(value: &str) -> Result<Rect, AnnotationEditError> {
    let parts: Vec<&str> = value.split(',').collect();
    if parts.len() != 4 {
        return Err(AnnotationEditError::XfdfValue {
            name: "rect",
            value: value.to_owned(),
        });
    }
    let mut numbers = [0.0f32; 4];
    for (slot, part) in numbers.iter_mut().zip(parts.iter()) {
        *slot = part
            .trim()
            .parse::<f32>()
            .map_err(|_| AnnotationEditError::XfdfValue {
                name: "rect",
                value: value.to_owned(),
            })?;
    }
    Rect::new(numbers[0], numbers[1], numbers[2], numbers[3]).map_err(|error| match error {
        GeometryError::InvertedRect { .. } => AnnotationEditError::XfdfValue {
            name: "rect",
            value: value.to_owned(),
        },
        other => AnnotationEditError::Geometry(other),
    })
}

fn format_rect(rect: Rect) -> String {
    format!("{},{},{},{}", rect.x0(), rect.y0(), rect.x1(), rect.y1())
}

fn escape_xml_text(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&apos;"),
            other => escaped.push(other),
        }
    }
    escaped
}

fn decode_entities(text: &str) -> String {
    let mut decoded = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(ampersand) = rest.find('&') {
        decoded.push_str(&rest[..ampersand]);
        let after = &rest[ampersand + 1..];
        let Some(semicolon) = after.find(';').filter(|index| *index <= 12) else {
            decoded.push('&');
            rest = after;
            continue;
        };
        let entity = &after[..semicolon];
        let replacement = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            _ => entity
                .strip_prefix("#x")
                .or_else(|| entity.strip_prefix("#X"))
                .and_then(|hex| u32::from_str_radix(hex, 16).ok())
                .or_else(|| {
                    entity
                        .strip_prefix('#')
                        .and_then(|decimal| decimal.parse::<u32>().ok())
                })
                .and_then(char::from_u32),
        };
        match replacement {
            Some(character) => decoded.push(character),
            None => {
                decoded.push('&');
                decoded.push_str(entity);
                decoded.push(';');
            }
        }
        rest = &after[semicolon + 1..];
    }
    decoded.push_str(rest);
    decoded
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x0: f32, y0: f32, x1: f32, y1: f32) -> Rect {
        Rect::new(x0, y0, x1, y1).expect("test rectangle")
    }

    #[test]
    fn pdf_date_is_utc_and_zero_padded() {
        assert_eq!(pdf_date_from_unix(0), "D:19700101000000Z");
        assert_eq!(pdf_date_from_unix(1_700_000_000), "D:20231114221320Z");
    }

    #[test]
    fn xml_entities_round_trip() {
        let original = "a & b < c > d \"e\" 'f'";
        assert_eq!(decode_entities(&escape_xml_text(original)), original);
        assert_eq!(decode_entities("&#65;&#x42;"), "AB");
        assert_eq!(decode_entities("broken & entity"), "broken & entity");
    }

    #[test]
    fn quad_points_follow_pdf_order() {
        let points = quad_points_array(&[rect(10.0, 20.0, 30.0, 40.0)]);
        let values: Vec<f64> = points
            .as_array()
            .expect("array")
            .iter()
            .filter_map(Object::as_f64)
            .collect();
        assert_eq!(values, vec![10.0, 40.0, 30.0, 40.0, 10.0, 20.0, 30.0, 20.0]);
    }

    #[test]
    fn attributes_are_parsed_with_entities_and_quotes() {
        let attributes =
            parse_attributes(" page=\"1\" author=\"A &amp; B\" id='x'", 0).expect("attributes");
        assert_eq!(attributes[0], ("page".to_owned(), "1".to_owned()));
        assert_eq!(attributes[1], ("author".to_owned(), "A & B".to_owned()));
        assert_eq!(attributes[2], ("id".to_owned(), "x".to_owned()));
    }
}
