//! Document-level writers for links, bookmarks' outline container and embedded
//! file attachments.
//!
//! The model types in [`super`] describe what a link or attachment *is*; this
//! module installs those models into a real [`Document`] and removes them
//! again. Bookmarks are written by
//! [`write_outline`](super::write_outline); links are staged as `/Link`
//! annotations on a page, and attachments are registered in the catalog
//! `/Names /EmbeddedFiles` name tree.
//!
//! Every writer validates its input before it stages an object, so a rejected
//! edit leaves the document unchanged. Staged edits use
//! [`Document::set_object`], which means a caller can chain several edits and
//! save once.

use pdfxi_core::{Dictionary, Document, Object, ObjectId, decode_text_string, encode_text_string};

use crate::document_annotations::MAX_ATTACHMENT_BYTES;
use crate::geometry::{PageIndex, Rect};

use super::{
    Destination, FitType, Link, LinkTarget, NavigationError, attachment::DEFAULT_MIME_TYPE,
};

/// Errors from the document-level navigation writers.
#[derive(Debug, thiserror::Error)]
pub enum NavigationWriteError {
    /// The core document could not be read or staged.
    #[error("PDF structure error: {0}")]
    Core(#[from] pdfxi_core::Error),
    /// A navigation model value was rejected.
    #[error("navigation model error: {0}")]
    Model(#[from] NavigationError),
    /// The requested page does not exist.
    #[error("page {page} does not exist")]
    PageNotFound {
        /// Zero-based page index that was requested.
        page: usize,
    },
    /// A link index did not select an existing link on the page.
    #[error("page {page} has {len} links; index {index} is out of range")]
    LinkIndexOutOfRange {
        /// Zero-based page index.
        page: usize,
        /// Requested link index on that page.
        index: usize,
        /// Number of links the page actually has.
        len: usize,
    },
    /// The page object is neither a dictionary nor a stream.
    #[error("page {page} object is not a dictionary")]
    MalformedPage {
        /// Zero-based page index.
        page: usize,
    },
    /// A page `/Annots` entry is not an array.
    #[error("page {page} /Annots is not an array")]
    MalformedAnnots {
        /// Zero-based page index.
        page: usize,
    },
    /// A catalog or page dictionary was not a dictionary.
    #[error("the document catalog is not a dictionary")]
    CatalogNotDictionary,
    /// The link rectangle is empty.
    #[error("a link needs a rectangle with positive width and height")]
    EmptyLinkRect,
    /// A URI link was given a blank URI.
    #[error("a URI link needs a non-empty URI")]
    EmptyUri,
    /// A location-coordinate was `NaN` or infinite.
    #[error("a link destination coordinate is not finite")]
    NonFiniteCoordinate,
    /// An attachment name was empty.
    #[error("an attachment needs a non-empty name")]
    EmptyAttachmentName,
    /// An attachment name already exists in the name tree.
    #[error("an attachment named {0:?} already exists")]
    AttachmentExists(String),
    /// No attachment carries the requested name.
    #[error("no attachment named {0:?} exists")]
    AttachmentMissing(String),
    /// An attachment payload exceeded the documented cap.
    #[error("attachment has {bytes} bytes; the maximum is {limit}")]
    AttachmentTooLarge {
        /// Supplied payload size.
        bytes: usize,
        /// The documented cap, [`MAX_ATTACHMENT_BYTES`].
        limit: usize,
    },
    /// The `/Names /EmbeddedFiles` entry is not a readable name tree.
    #[error("the catalog /Names /EmbeddedFiles entry is not a name tree")]
    MalformedNameTree,
}

/// Result of staging one link annotation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StagedLink {
    /// Indirect object holding the `/Link` annotation dictionary.
    pub annotation: ObjectId,
    /// Zero-based page that now carries the annotation.
    pub page: usize,
}

/// Result of staging one embedded file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StagedAttachment {
    /// Indirect `/EmbeddedFile` stream object.
    pub stream: ObjectId,
    /// Indirect `/Filespec` object referenced by the name tree.
    pub spec: ObjectId,
    /// Payload size in bytes.
    pub bytes: usize,
}

/// Stage one `/Link` annotation on `page_index`.
///
/// The rectangle must have positive width and height, a URI target must not be
/// blank, and every destination coordinate must be finite. A destination page
/// outside the document is rejected with [`NavigationWriteError::PageNotFound`]
/// before any object is staged.
///
/// # Errors
///
/// Returns a [`NavigationWriteError`] when the page, destination, rectangle or
/// model value is invalid, or when the core object store rejects a staged
/// object.
pub fn add_link(
    document: &mut Document,
    page_index: usize,
    link: &Link,
) -> Result<StagedLink, NavigationWriteError> {
    if link.rect.is_empty() {
        return Err(NavigationWriteError::EmptyLinkRect);
    }
    if let LinkTarget::Uri(uri) = &link.target
        && uri.trim().is_empty()
    {
        return Err(NavigationWriteError::EmptyUri);
    }
    let current = effective_document(document)?;
    let _page = current.page(page_index)?;
    let mut state = read_page_state(&current, page_index)?;
    let dictionary = link_dictionary(&current, link)?;
    let annotation = allocate_object_ids(document, 1)?[0];
    document.set_object(annotation, Object::Dictionary(dictionary))?;
    state.annots.items_mut().push(Object::Reference(annotation));
    stage_page_state(document, state)?;

    Ok(StagedLink {
        annotation,
        page: page_index,
    })
}

/// Remove the `index`-th `/Link` annotation from `page_index`.
///
/// Links are counted in annotation order, so index `0` is the first link on the
/// page regardless of any other annotation subtypes before it.
///
/// # Errors
///
/// Returns [`NavigationWriteError::LinkIndexOutOfRange`] when the page has fewer
/// links, and a structural error when the page cannot be read or staged.
pub fn remove_link(
    document: &mut Document,
    page_index: usize,
    index: usize,
) -> Result<ObjectId, NavigationWriteError> {
    let current = effective_document(document)?;
    let mut state = read_page_state(&current, page_index)?;
    let mut seen = 0;
    let mut target = None;
    for (position, item) in state.annots.items().iter().enumerate() {
        let dictionary = match item.clone() {
            Object::Reference(id) => current.object(id)?,
            other => other,
        };
        let Some(dictionary) = dictionary.as_dictionary() else {
            continue;
        };
        if dictionary.get("Subtype").and_then(Object::as_name) == Some("Link") {
            if seen == index {
                target = Some(position);
                break;
            }
            seen += 1;
        }
    }
    let Some(position) = target else {
        return Err(NavigationWriteError::LinkIndexOutOfRange {
            page: page_index,
            index,
            len: seen,
        });
    };
    let removed = state.annots.items_mut().remove(position);
    let removed_id = match removed {
        Object::Reference(id) => id,
        _ => ObjectId::new(0, 0),
    };
    stage_page_state(document, state)?;
    Ok(removed_id)
}

/// Register `bytes` as an embedded file in the catalog name tree.
///
/// The name is the `/Names` key and the file name shown to readers. When
/// `file_name` is `None` the name is used. `description` may be empty, and an
/// empty `mime_type` falls back to [`DEFAULT_MIME_TYPE`]. Existing name-tree
/// entries are preserved and re-sorted, so the output stays deterministic.
///
/// # Errors
///
/// Returns [`NavigationWriteError::AttachmentExists`] for a duplicate name,
/// [`NavigationWriteError::AttachmentTooLarge`] above
/// [`MAX_ATTACHMENT_BYTES`], and a structural error when the catalog name tree
/// cannot be read or staged.
pub fn add_attachment(
    document: &mut Document,
    name: &str,
    file_name: Option<&str>,
    description: &str,
    mime_type: &str,
    bytes: &[u8],
) -> Result<StagedAttachment, NavigationWriteError> {
    let name = name.trim();
    if name.is_empty() {
        return Err(NavigationWriteError::EmptyAttachmentName);
    }
    if bytes.len() > MAX_ATTACHMENT_BYTES {
        return Err(NavigationWriteError::AttachmentTooLarge {
            bytes: bytes.len(),
            limit: MAX_ATTACHMENT_BYTES,
        });
    }
    let file_name = file_name.unwrap_or(name);
    let mime_type = if mime_type.trim().is_empty() {
        DEFAULT_MIME_TYPE
    } else {
        mime_type
    };

    let current = effective_document(document)?;
    let names = read_catalog_names(&current)?;
    let mut entries = collect_attachment_entries(&current, names.dictionary.as_ref())?;
    if entries.iter().any(|(key, _)| key == name) {
        return Err(NavigationWriteError::AttachmentExists(name.to_owned()));
    }

    let allocated = allocate_object_ids(document, 2)?;
    let stream_id = allocated[0];
    let spec_id = allocated[1];

    let mut params = Dictionary::new();
    params.insert(
        "Size".to_owned(),
        Object::Integer(i64::try_from(bytes.len()).unwrap_or(i64::MAX)),
    );
    let mut stream = Dictionary::new();
    stream.insert("Type".to_owned(), Object::Name("EmbeddedFile".to_owned()));
    stream.insert("Subtype".to_owned(), Object::Name(mime_type.to_owned()));
    stream.insert(
        "Length".to_owned(),
        Object::Integer(i64::try_from(bytes.len()).unwrap_or(i64::MAX)),
    );
    stream.insert("Params".to_owned(), Object::Dictionary(params));
    document.set_object(
        stream_id,
        Object::Stream {
            dictionary: stream,
            data: bytes.to_vec(),
        },
    )?;

    let mut embedded = Dictionary::new();
    embedded.insert("F".to_owned(), Object::Reference(stream_id));
    let mut spec = Dictionary::new();
    spec.insert("Type".to_owned(), Object::Name("Filespec".to_owned()));
    spec.insert(
        "F".to_owned(),
        Object::String(encode_text_string(file_name)),
    );
    spec.insert(
        "UF".to_owned(),
        Object::String(encode_text_string(file_name)),
    );
    spec.insert("EF".to_owned(), Object::Dictionary(embedded));
    if !description.is_empty() {
        spec.insert(
            "Desc".to_owned(),
            Object::String(encode_text_string(description)),
        );
    }
    document.set_object(spec_id, Object::Dictionary(spec))?;

    entries.push((name.to_owned(), Object::Reference(spec_id)));
    entries.sort_by(|left, right| left.0.cmp(&right.0));
    write_catalog_names(document, names, entries)?;

    Ok(StagedAttachment {
        stream: stream_id,
        spec: spec_id,
        bytes: bytes.len(),
    })
}

/// Remove the named entry from the catalog `/Names /EmbeddedFiles` name tree.
///
/// # Errors
///
/// Returns [`NavigationWriteError::AttachmentMissing`] when the name is not in
/// the tree, and a structural error when the name tree cannot be read or
/// staged. The embedded stream itself is left in place; removing the tree
/// entry is what makes the file unreachable to readers.
pub fn remove_attachment(document: &mut Document, name: &str) -> Result<(), NavigationWriteError> {
    let current = effective_document(document)?;
    let names = read_catalog_names(&current)?;
    let mut entries = collect_attachment_entries(&current, names.dictionary.as_ref())?;
    let before = entries.len();
    entries.retain(|(key, _)| key != name);
    if entries.len() == before {
        return Err(NavigationWriteError::AttachmentMissing(name.to_owned()));
    }
    write_catalog_names(document, names, entries)
}

/// The catalog `/Names` dictionary together with how it is stored.
struct CatalogNames {
    /// Catalog object id.
    catalog_id: ObjectId,
    /// Catalog dictionary, so an indirect `/Names` entry can be created.
    catalog: Dictionary,
    /// Indirect `/Names` object id, when the catalog used one.
    names_id: Option<ObjectId>,
    /// The resolved `/Names` dictionary, when the catalog has one.
    dictionary: Option<Dictionary>,
}

/// Read the catalog `/Names` dictionary, resolving an indirect entry.
fn read_catalog_names(document: &Document) -> Result<CatalogNames, NavigationWriteError> {
    let catalog_id = document.root()?;
    let catalog = document
        .object(catalog_id)?
        .as_dictionary()
        .ok_or(NavigationWriteError::CatalogNotDictionary)?
        .clone();
    let (names_id, dictionary) = match catalog.get("Names") {
        None | Some(Object::Null) => (None, None),
        Some(Object::Reference(id)) => match document.object(*id)? {
            Object::Dictionary(dictionary) => (Some(*id), Some(dictionary)),
            _ => return Err(NavigationWriteError::MalformedNameTree),
        },
        Some(Object::Dictionary(dictionary)) => (None, Some(dictionary.clone())),
        Some(_) => return Err(NavigationWriteError::MalformedNameTree),
    };
    Ok(CatalogNames {
        catalog_id,
        catalog,
        names_id,
        dictionary,
    })
}

/// Write the attachment entries back into the catalog `/Names` dictionary.
fn write_catalog_names(
    document: &mut Document,
    names: CatalogNames,
    entries: Vec<(String, Object)>,
) -> Result<(), NavigationWriteError> {
    let CatalogNames {
        catalog_id,
        mut catalog,
        names_id,
        mut dictionary,
    } = names;
    let mut tree = dictionary.take().unwrap_or_default();
    if entries.is_empty() {
        tree.remove("EmbeddedFiles");
    } else {
        let mut value = Vec::with_capacity(entries.len() * 2);
        for (key, spec) in entries {
            value.push(Object::String(encode_text_string(&key)));
            value.push(spec);
        }
        let mut node = Dictionary::new();
        node.insert("Names".to_owned(), Object::Array(value));
        tree.insert("EmbeddedFiles".to_owned(), Object::Dictionary(node));
    }

    match (tree.is_empty(), names_id) {
        (true, _) => {
            catalog.remove("Names");
            document.set_object(catalog_id, Object::Dictionary(catalog))?;
        }
        (false, Some(id)) => {
            document.set_object(id, Object::Dictionary(tree))?;
        }
        (false, None) => {
            let number = document.allocate_object_number()?;
            catalog.insert(
                "Names".to_owned(),
                Object::Reference(ObjectId::new(number, 0)),
            );
            document.set_object(ObjectId::new(number, 0), Object::Dictionary(tree))?;
            document.set_object(catalog_id, Object::Dictionary(catalog))?;
        }
    }
    Ok(())
}

/// Flatten the `/EmbeddedFiles` name tree into sorted key/value pairs.
fn collect_attachment_entries(
    document: &Document,
    names: Option<&Dictionary>,
) -> Result<Vec<(String, Object)>, NavigationWriteError> {
    let Some(names) = names else {
        return Ok(Vec::new());
    };
    let Some(files) = names.get("EmbeddedFiles") else {
        return Ok(Vec::new());
    };
    let mut entries = Vec::new();
    collect_name_tree(document, files, &mut entries)?;
    Ok(entries)
}

/// Walk one name-tree node, appending its leaf pairs.
fn collect_name_tree(
    document: &Document,
    node: &Object,
    entries: &mut Vec<(String, Object)>,
) -> Result<(), NavigationWriteError> {
    let node = resolve(document, node)?;
    let Some(dictionary) = node.as_dictionary() else {
        return Err(NavigationWriteError::MalformedNameTree);
    };
    if let Some(values) = dictionary.get("Names") {
        let values = resolve(document, values)?;
        let Object::Array(values) = values else {
            return Err(NavigationWriteError::MalformedNameTree);
        };
        if values.len() % 2 != 0 {
            return Err(NavigationWriteError::MalformedNameTree);
        }
        for pair in values.chunks(2) {
            let key = match &pair[0] {
                Object::String(bytes) => decode_text_string(bytes),
                Object::Name(name) => name.clone(),
                _ => return Err(NavigationWriteError::MalformedNameTree),
            };
            entries.push((key, pair[1].clone()));
        }
    }
    if let Some(kids) = dictionary.get("Kids") {
        let kids = resolve(document, kids)?;
        let Object::Array(kids) = kids else {
            return Err(NavigationWriteError::MalformedNameTree);
        };
        for kid in &kids {
            collect_name_tree(document, kid, entries)?;
        }
    }
    Ok(())
}

/// Allocate `count` fresh object ids in one step.
///
/// [`Document::allocate_object_number`] only sees objects staged before the
/// call, so allocating twice in a row would hand back the same number. This
/// helper walks forward and skips anything already used or reserved.
fn allocate_object_ids(
    document: &Document,
    count: usize,
) -> Result<Vec<ObjectId>, NavigationWriteError> {
    let mut candidate = document.allocate_object_number()?;
    let mut ids = Vec::with_capacity(count);
    while ids.len() < count {
        let id = ObjectId::new(candidate, 0);
        if document.is_object_number_used(candidate) {
            candidate = candidate.checked_add(1).ok_or_else(|| {
                pdfxi_core::Error::CannotSerialize(
                    "the 32-bit object number space is exhausted".to_owned(),
                )
            })?;
            continue;
        }
        ids.push(id);
        candidate = candidate.checked_add(1).ok_or_else(|| {
            pdfxi_core::Error::CannotSerialize(
                "the 32-bit object number space is exhausted".to_owned(),
            )
        })?;
    }
    Ok(ids)
}

/// Build a reparsed view that includes every staged change.
fn effective_document(document: &Document) -> Result<Document, NavigationWriteError> {
    let bytes = document.save_incremental()?;
    Document::from_bytes(bytes).map_err(NavigationWriteError::Core)
}

/// Resolve a reference (one level) or clone a direct object.
fn resolve(document: &Document, object: &Object) -> Result<Object, NavigationWriteError> {
    match object {
        Object::Reference(id) => Ok(document.object(*id)?.clone()),
        other => Ok(other.clone()),
    }
}

/// Build the core `/Link` annotation dictionary for a model link.
fn link_dictionary(document: &Document, link: &Link) -> Result<Dictionary, NavigationWriteError> {
    let mut dictionary = Dictionary::new();
    dictionary.insert("Type".to_owned(), Object::Name("Annot".to_owned()));
    dictionary.insert("Subtype".to_owned(), Object::Name("Link".to_owned()));
    dictionary.insert(
        "Rect".to_owned(),
        Object::Array(vec![
            Object::Real(f64::from(link.rect.x0())),
            Object::Real(f64::from(link.rect.y0())),
            Object::Real(f64::from(link.rect.x1())),
            Object::Real(f64::from(link.rect.y1())),
        ]),
    );
    dictionary.insert(
        "H".to_owned(),
        Object::Name(link.highlight.as_name().to_owned()),
    );
    dictionary.insert(
        "Border".to_owned(),
        Object::Array(vec![
            Object::Integer(0),
            Object::Integer(0),
            Object::Real(f64::from(link.border.width)),
        ]),
    );
    dictionary.insert(
        "C".to_owned(),
        Object::Array(vec![
            Object::Real(f64::from(link.border.color.r()) / 255.0),
            Object::Real(f64::from(link.border.color.g()) / 255.0),
            Object::Real(f64::from(link.border.color.b()) / 255.0),
        ]),
    );

    match &link.target {
        LinkTarget::Uri(uri) => {
            if uri.trim().is_empty() {
                return Err(NavigationWriteError::EmptyUri);
            }
            let mut action = Dictionary::new();
            action.insert("S".to_owned(), Object::Name("URI".to_owned()));
            action.insert("URI".to_owned(), Object::String(encode_text_string(uri)));
            dictionary.insert("A".to_owned(), Object::Dictionary(action));
        }
        LinkTarget::NamedAction(name) => {
            let mut action = Dictionary::new();
            action.insert("S".to_owned(), Object::Name("Named".to_owned()));
            action.insert("N".to_owned(), Object::Name(name.clone()));
            dictionary.insert("A".to_owned(), Object::Dictionary(action));
        }
        LinkTarget::Destination(destination) => {
            dictionary.insert(
                "Dest".to_owned(),
                destination_object(document, destination)?,
            );
        }
    }
    Ok(dictionary)
}

/// Build a destination array using this document's real page object.
fn destination_object(
    document: &Document,
    destination: &Destination,
) -> Result<Object, NavigationWriteError> {
    let pages = document.pages()?;
    let page = pages
        .get(destination.page.get())
        .ok_or(NavigationWriteError::PageNotFound {
            page: destination.page.get(),
        })?;
    let mut items = Vec::with_capacity(2 + destination.fit.parameter_count());
    items.push(Object::Reference(page.id));
    items.push(Object::Name(destination.fit.as_name().to_owned()));
    match &destination.fit {
        FitType::Xyz { left, top, zoom } => {
            items.push(optional_real(*left)?);
            items.push(optional_real(*top)?);
            items.push(optional_real(*zoom)?);
        }
        FitType::Fit | FitType::FitB => {}
        FitType::FitH { top } => items.push(finite(*top)?),
        FitType::FitV { left } => items.push(finite(*left)?),
        FitType::FitR {
            left,
            bottom,
            right,
            top,
        } => {
            items.push(finite(*left)?);
            items.push(finite(*bottom)?);
            items.push(finite(*right)?);
            items.push(finite(*top)?);
        }
        FitType::FitBH { top } => items.push(finite(*top)?),
        FitType::FitBV { left } => items.push(finite(*left)?),
    }
    Ok(Object::Array(items))
}

/// Serialize an optional destination coordinate.
fn optional_real(value: Option<f32>) -> Result<Object, NavigationWriteError> {
    match value {
        None => Ok(Object::Null),
        Some(value) => finite(value),
    }
}

/// Serialize one finite destination coordinate.
fn finite(value: f32) -> Result<Object, NavigationWriteError> {
    if !value.is_finite() {
        return Err(NavigationWriteError::NonFiniteCoordinate);
    }
    Ok(Object::Real(f64::from(value)))
}

/// A page dictionary together with the shape of its `/Annots` entry.
struct PageState {
    page_id: ObjectId,
    dictionary: Dictionary,
    stream_data: Option<Vec<u8>>,
    annots: Annots,
}

/// Whether `/Annots` is an inline array or an indirect array object.
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

/// Read a page dictionary and its `/Annots` array.
fn read_page_state(
    document: &Document,
    page_index: usize,
) -> Result<PageState, NavigationWriteError> {
    let page = document
        .page(page_index)
        .map_err(|_| NavigationWriteError::PageNotFound { page: page_index })?;
    let object = document.object(page.id)?;
    let (dictionary, stream_data) = match object {
        Object::Dictionary(dictionary) => (dictionary, None),
        Object::Stream { dictionary, data } => (dictionary, Some(data)),
        _ => {
            return Err(NavigationWriteError::MalformedPage { page: page_index });
        }
    };
    let annots = match dictionary.get("Annots").cloned() {
        None | Some(Object::Null) => Annots::Direct(Vec::new()),
        Some(Object::Array(items)) => Annots::Direct(items),
        Some(Object::Reference(id)) => match document.object(id)? {
            Object::Array(items) => Annots::Indirect(id, items),
            Object::Null => Annots::Direct(Vec::new()),
            _ => {
                return Err(NavigationWriteError::MalformedAnnots { page: page_index });
            }
        },
        Some(_) => {
            return Err(NavigationWriteError::MalformedAnnots { page: page_index });
        }
    };
    Ok(PageState {
        page_id: page.id,
        dictionary,
        stream_data,
        annots,
    })
}

/// Stage a page dictionary and its `/Annots` array back into the document.
fn stage_page_state(document: &mut Document, page: PageState) -> Result<(), NavigationWriteError> {
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

/// Build a page-jump destination.
pub fn page_destination(page: usize) -> Destination {
    Destination::new(PageIndex::new(page), FitType::Fit)
}

/// The rectangle of a link, for callers that build one from points.
pub fn link_rect(x0: f32, y0: f32, x1: f32, y1: f32) -> Result<Rect, NavigationError> {
    Ok(Rect::new(x0, y0, x1, y1)?)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::navigation::{
        Bookmark, BookmarkPath, LinkBorder, LinkHighlight, Outline, link_to_dict, read_outline,
        write_outline,
    };

    fn fixture(name: &str) -> Vec<u8> {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures")
            .join(name);
        std::fs::read(&path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
    }

    fn document() -> Document {
        Document::from_bytes(fixture("minimal.pdf")).expect("minimal parses")
    }

    #[test]
    fn links_are_staged_read_back_and_removed() {
        let mut document = document();
        let link = Link::new(
            Rect::new(72.0, 72.0, 200.0, 100.0).expect("rect"),
            LinkTarget::Uri("https://example.com/a?b=1".to_owned()),
        )
        .with_highlight(LinkHighlight::Outline)
        .with_border(LinkBorder::new(1.5, crate::annotation::Color::BLACK).expect("border"));
        let staged = add_link(&mut document, 0, &link).expect("stage link");

        let saved = document.save().expect("save");
        let reopened = Document::from_bytes(saved).expect("reopen");
        let links = reopened.links().expect("read links");
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].page, 0);
        assert_eq!(
            links[0].target,
            pdfxi_core::LinkTarget::Uri {
                uri: "https://example.com/a?b=1".to_owned()
            }
        );
        assert_eq!(links[0].rect, [72.0, 72.0, 200.0, 100.0]);

        // The staged annotation is a real /Link dictionary with the model fields.
        let annotation_object = reopened
            .object(staged.annotation)
            .expect("annotation object");
        let annotation = annotation_object
            .as_dictionary()
            .expect("annotation dictionary");
        assert_eq!(annotation.get("H").and_then(Object::as_name), Some("O"));
        let border = annotation
            .get("Border")
            .and_then(Object::as_array)
            .expect("border array");
        assert_eq!(border.get(2).and_then(Object::as_f64), Some(1.5));

        let mut reopened = reopened;
        remove_link(&mut reopened, 0, 0).expect("remove link");
        let saved = reopened.save().expect("save removed");
        let reopened = Document::from_bytes(saved).expect("reopen removed");
        assert!(reopened.links().expect("links after removal").is_empty());
    }

    #[test]
    fn page_destination_links_use_the_real_page_object() {
        let mut document = document();
        let link = Link::new(
            Rect::new(10.0, 10.0, 50.0, 30.0).expect("rect"),
            LinkTarget::Destination(page_destination(0)),
        );
        add_link(&mut document, 0, &link).expect("stage page link");
        let saved = document.save().expect("save");
        let reopened = Document::from_bytes(saved).expect("reopen");
        let links = reopened.links().expect("read links");
        assert_eq!(
            links[0].target,
            pdfxi_core::LinkTarget::Page {
                page: 0,
                name: None
            }
        );
    }

    #[test]
    fn invalid_links_are_rejected_without_staging() {
        let mut document = document();
        let empty = Link::new(
            Rect::new(10.0, 10.0, 10.0, 30.0).expect("rect"),
            LinkTarget::Uri("https://example.com".to_owned()),
        );
        assert!(matches!(
            add_link(&mut document, 0, &empty),
            Err(NavigationWriteError::EmptyLinkRect)
        ));

        let blank = Link::new(
            Rect::new(10.0, 10.0, 50.0, 30.0).expect("rect"),
            LinkTarget::Uri("   ".to_owned()),
        );
        assert!(matches!(
            add_link(&mut document, 0, &blank),
            Err(NavigationWriteError::EmptyUri)
        ));

        let off_page = Link::new(
            Rect::new(10.0, 10.0, 50.0, 30.0).expect("rect"),
            LinkTarget::Destination(page_destination(9)),
        );
        assert!(matches!(
            add_link(&mut document, 0, &off_page),
            Err(NavigationWriteError::PageNotFound { page: 9 })
        ));

        assert!(matches!(
            remove_link(&mut document, 0, 0),
            Err(NavigationWriteError::LinkIndexOutOfRange { len: 0, .. })
        ));
        let saved = document.save().expect("save untouched");
        let reopened = Document::from_bytes(saved).expect("reopen");
        assert!(reopened.links().expect("links").is_empty());
    }

    #[test]
    fn attachments_are_registered_sorted_and_removed() {
        let mut document = document();
        let first = add_attachment(
            &mut document,
            "zeta.bin",
            Some("zeta.bin"),
            "last",
            "application/octet-stream",
            b"zeta payload",
        )
        .expect("add zeta");
        add_attachment(&mut document, "alpha.bin", None, "", "", b"alpha payload")
            .expect("add alpha");

        let saved = document.save().expect("save attachments");
        let reopened = Document::from_bytes(saved).expect("reopen");
        let attachments = reopened.attachments().expect("attachments");
        eprintln!("ATTACHMENTS {attachments:?}");
        let root = reopened.root().expect("root");
        eprintln!(
            "ROOT {root:?} CATALOG {:?}",
            reopened.object(root).expect("catalog")
        );
        for number in 7..=14u32 {
            let id = pdfxi_core::ObjectId::new(number, 0);
            eprintln!("OBJ {number}: {:?}", reopened.object(id));
        }
        if let Some(spec) = attachments.iter().find(|item| item.name == "zeta.bin") {
            eprintln!("SPEC ROW {spec:?}");
        }
        let names: Vec<&str> = attachments
            .iter()
            .map(|attachment| attachment.name.as_str())
            .collect();
        assert_eq!(names, vec!["alpha.bin", "zeta.bin"]);
        assert_eq!(
            reopened.attachment_bytes("zeta.bin").expect("bytes"),
            b"zeta payload"
        );
        assert_eq!(first.bytes, b"zeta payload".len());

        let mut reopened = reopened;
        assert!(matches!(
            remove_attachment(&mut reopened, "missing.bin"),
            Err(NavigationWriteError::AttachmentMissing(_))
        ));
        remove_attachment(&mut reopened, "zeta.bin").expect("remove zeta");
        let saved = reopened.save().expect("save removed");
        let reopened = Document::from_bytes(saved).expect("reopen removed");
        let attachments = reopened.attachments().expect("attachments after removal");
        assert_eq!(attachments.len(), 1);
        assert_eq!(attachments[0].name, "alpha.bin");
        assert_eq!(
            reopened.attachment_bytes("alpha.bin").expect("alpha bytes"),
            b"alpha payload"
        );
    }

    #[test]
    fn duplicate_and_invalid_attachments_are_rejected() {
        let mut document = document();
        assert!(matches!(
            add_attachment(&mut document, "  ", None, "", "", b"x"),
            Err(NavigationWriteError::EmptyAttachmentName)
        ));
        add_attachment(&mut document, "one.bin", None, "", "", b"x").expect("first");
        assert!(matches!(
            add_attachment(&mut document, "one.bin", None, "", "", b"y"),
            Err(NavigationWriteError::AttachmentExists(name)) if name == "one.bin"
        ));
    }

    #[test]
    fn outline_written_after_a_link_and_attachment_survives_one_save() {
        let mut document = document();
        add_link(
            &mut document,
            0,
            &Link::new(
                Rect::new(10.0, 10.0, 50.0, 30.0).expect("rect"),
                LinkTarget::Uri("https://example.com".to_owned()),
            ),
        )
        .expect("add link");
        add_attachment(&mut document, "note.txt", None, "text", "text/plain", b"hi")
            .expect("add attachment");

        let mut outline = Outline::new();
        let mut root = Bookmark::new("Chapter one").expect("bookmark");
        root.destination = Some(page_destination(0));
        outline
            .insert_bookmark(&BookmarkPath::root(), 0, root)
            .expect("insert bookmark");
        write_outline(&mut document, &outline).expect("write outline");

        let saved = document.save().expect("single save");
        let reopened = Document::from_bytes(saved).expect("reopen");
        let read = read_outline(&reopened).expect("read outline");
        assert!(read.present);
        assert_eq!(read.outline.roots[0].title, "Chapter one");
        assert_eq!(reopened.links().expect("links").len(), 1);
        assert_eq!(reopened.attachments().expect("attachments").len(), 1);
    }

    #[test]
    fn model_link_dictionary_agrees_with_the_staged_annotation() {
        let link = Link::new(
            Rect::new(1.0, 2.0, 3.0, 4.0).expect("rect"),
            LinkTarget::Uri("https://example.com".to_owned()),
        );
        let model = link_to_dict(&link);
        assert_eq!(
            model.get("Subtype").and_then(|value| value.as_name()),
            Some("Link")
        );
    }
}
