//! Link annotations, named destinations and embedded-file attachments.
//!
//! This module reads the interactive layer of a document: `/Link` annotations
//! with their rectangles and targets, named destinations from the catalog's
//! `/Dests` dictionary or `/Names /Dests` tree, and attachments from
//! `/Names /EmbeddedFiles` plus `/FileAttachment` annotations.
//!
//! The reader is deliberately permissive: a malformed annotation is skipped
//! instead of failing the whole document, and an unresolvable destination is
//! reported as [`LinkTarget::Unresolved`] so a viewer can still show the link.

use std::collections::BTreeMap;

use crate::object::{Dictionary, Object};
use crate::parser::{Structure, load_object};
use crate::{Document, Error, Result};

/// Maximum reference-following depth while reading the interactive layer.
const MAX_DEPTH: u8 = 32;

/// Where a link annotation points.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkTarget {
    /// An external URI.
    Uri {
        /// The URI exactly as stored in the annotation.
        uri: String,
    },
    /// A page in this document, optionally named.
    Page {
        /// Zero-based destination page.
        page: usize,
        /// Destination name when the link used a named destination.
        name: Option<String>,
    },
    /// A named destination that could not be resolved to a page.
    Unresolved {
        /// The destination name or the reason the target could not be read.
        detail: String,
    },
}

/// One link annotation on a page.
#[derive(Debug, Clone, PartialEq)]
pub struct LinkAnnotation {
    /// Zero-based page that carries the annotation.
    pub page: usize,
    /// Annotation rectangle `[left, bottom, right, top]` in points; empty when
    /// the annotation has no readable `/Rect`.
    pub rect: [f64; 4],
    /// Where the link points.
    pub target: LinkTarget,
}

/// One embedded file attached to the document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attachment {
    /// File name shown to the user (the `/UF` name when present, else `/F`).
    pub name: String,
    /// Optional description from the file specification.
    pub description: Option<String>,
    /// Embedded stream length in bytes, when readable.
    pub size: Option<u64>,
    /// Zero-based page of a `/FileAttachment` annotation, when the attachment
    /// came from an annotation instead of the document name tree.
    pub page: Option<usize>,
}

/// One file in a PDF portfolio collection.
///
/// A portfolio is a normal PDF whose catalog carries a `/Collection` dictionary
/// and whose `/Names /EmbeddedFiles` name tree holds the collection files. The
/// `/UF` value of each file specification may contain `/`-separated folders, so
/// the same entry can describe `Reports/2026/q1.pdf`.
///
/// Only metadata is exposed here; [`Document::attachment_bytes`] returns the
/// stream for an entry by its name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortfolioItem {
    /// Name-tree key of the entry, accepted by [`Document::attachment_bytes`].
    pub key: String,
    /// `/UF` path of the file, with `/` separating folders from the file name.
    pub path: String,
    /// File name shown to the user (the last component of [`Self::path`]).
    pub name: String,
    /// Optional `/Desc` description.
    pub description: Option<String>,
    /// Embedded stream length in bytes, when readable.
    pub size: Option<u64>,
    /// `/Subtype` media type of the embedded file stream, when present.
    pub mime: Option<String>,
    /// `/Params /ModDate` of the embedded file, or the file specification's
    /// `/ModDate`, exactly as stored, when present.
    pub modified: Option<String>,
}

/// Reader over one document's bytes and parsed structure.
struct Reader<'a> {
    data: &'a [u8],
    structure: &'a Structure,
}

impl Reader<'_> {
    /// Follow references until a direct object is reached.
    fn resolve(&self, object: &Object, depth: u8) -> Result<Object> {
        if depth > MAX_DEPTH {
            return Err(Error::CannotSerialize(
                "interactive-object graph exceeds the depth limit".to_owned(),
            ));
        }
        match object {
            Object::Reference(id) => {
                let loaded = load_object(self.data, &self.structure.xref, *id)?;
                self.resolve(&loaded, depth + 1)
            }
            other => Ok(other.clone()),
        }
    }

    /// Resolve and return a dictionary, or `None` for a non-dictionary.
    fn dictionary(&self, object: &Object, depth: u8) -> Result<Option<Dictionary>> {
        match self.resolve(object, depth)? {
            Object::Dictionary(dictionary) => Ok(Some(dictionary)),
            _ => Ok(None),
        }
    }

    /// Map an object reference to a zero-based page index.
    fn page_index(&self, object: &Object) -> Option<usize> {
        let Object::Reference(id) = object else {
            return None;
        };
        self.structure
            .pages
            .iter()
            .position(|page| page.id.number == id.number)
    }

    /// Collect every named destination from `/Dests` and `/Names /Dests`.
    fn named_destinations(&self) -> Result<BTreeMap<String, Option<usize>>> {
        let mut names = BTreeMap::new();
        let catalog = self.resolve(&Object::Reference(self.structure.root_id), 0)?;
        let Object::Dictionary(catalog) = catalog else {
            return Ok(names);
        };
        if let Some(dests) = catalog.get("Dests")
            && let Some(dictionary) = self.dictionary(dests, 0)?
        {
            for (name, value) in dictionary {
                names.insert(name, self.destination_page(&value, &BTreeMap::new(), 0)?);
            }
        }
        if let Some(tree) = catalog.get("Names")
            && let Some(tree) = self.dictionary(tree, 0)?
            && let Some(dests) = tree.get("Dests")
        {
            let mut entries = Vec::new();
            self.walk_name_tree(dests, &mut entries, 0)?;
            for (name, value) in entries {
                let under = value.get("D").cloned().unwrap_or(Object::Null);
                names.insert(
                    name.clone(),
                    self.destination_page(&under, &names.clone(), 0)?,
                );
            }
        }
        Ok(names)
    }

    /// Walk a name tree node and collect `(name, value)` pairs.
    fn walk_name_tree(
        &self,
        node: &Object,
        out: &mut Vec<(String, Dictionary)>,
        depth: u8,
    ) -> Result<()> {
        if depth > MAX_DEPTH {
            return Ok(());
        }
        let Some(dictionary) = self.dictionary(node, depth)? else {
            return Ok(());
        };
        if let Some(Object::Array(items)) = dictionary.get("Names") {
            for pair in items.chunks(2) {
                if pair.len() != 2 {
                    continue;
                }
                let name = match self.resolve(&pair[0], depth)? {
                    Object::String(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
                    Object::Name(name) => name,
                    _ => continue,
                };
                let Some(value) = self.dictionary(&pair[1], depth)? else {
                    continue;
                };
                out.push((name, value));
            }
        }
        if let Some(Object::Array(kids)) = dictionary.get("Kids") {
            for kid in kids {
                self.walk_name_tree(kid, out, depth + 1)?;
            }
        }
        Ok(())
    }

    /// Resolve a destination value to a zero-based page index.
    fn destination_page(
        &self,
        value: &Object,
        names: &BTreeMap<String, Option<usize>>,
        depth: u8,
    ) -> Result<Option<usize>> {
        if depth > MAX_DEPTH {
            return Ok(None);
        }
        match value {
            Object::Array(items) => Ok(items.first().and_then(|first| self.page_index(first))),
            Object::String(bytes) => {
                let name = String::from_utf8_lossy(bytes).into_owned();
                Ok(names.get(&name).copied().flatten())
            }
            Object::Name(name) => Ok(names.get(name).copied().flatten()),
            Object::Dictionary(dictionary) => match dictionary.get("D") {
                Some(inner) => self.destination_page(inner, names, depth + 1),
                None => Ok(None),
            },
            Object::Reference(_) => {
                let resolved = self.resolve(value, depth + 1)?;
                self.destination_page(&resolved, names, depth + 1)
            }
            _ => Ok(None),
        }
    }
}

/// Read a `[left, bottom, right, top]` rectangle; zeroes when unreadable.
fn rect_of(annotation: &Dictionary) -> [f64; 4] {
    let Some(Object::Array(items)) = annotation.get("Rect") else {
        return [0.0; 4];
    };
    let mut values = [0.0_f64; 4];
    for (slot, item) in values.iter_mut().zip(items.iter().take(4)) {
        *slot = match item {
            Object::Integer(value) => *value as f64,
            Object::Real(value) => *value,
            _ => 0.0,
        };
    }
    values
}

/// Read a text string from `/UF` or `/F` in a file specification.
fn file_name(spec: &Dictionary) -> Option<String> {
    for key in ["UF", "F", "Unix"] {
        match spec.get(key) {
            Some(Object::String(bytes)) => {
                return Some(String::from_utf8_lossy(bytes).into_owned());
            }
            Some(Object::Name(name)) => return Some(name.clone()),
            _ => {}
        }
    }
    None
}

impl Document {
    /// Return every link annotation in page order.
    ///
    /// Malformed annotations are skipped. A link whose destination cannot be
    /// resolved is still reported, with [`LinkTarget::Unresolved`].
    pub fn links(&self) -> Result<Vec<LinkAnnotation>> {
        let structure = self.supported_structure()?;
        let reader = Reader {
            data: self.bytes(),
            structure,
        };
        let names = reader.named_destinations()?;
        let mut links = Vec::new();
        for (page_index, page) in structure.pages.iter().enumerate() {
            let page_object = reader.resolve(&Object::Reference(page.id), 0)?;
            let Object::Dictionary(page_dictionary) = page_object else {
                continue;
            };
            let Some(annots) = page_dictionary.get("Annots") else {
                continue;
            };
            let Object::Array(annotations) = reader.resolve(annots, 0)? else {
                continue;
            };
            for annotation in annotations {
                let Some(dictionary) = reader.dictionary(&annotation, 0)? else {
                    continue;
                };
                if dictionary.get("Subtype").and_then(Object::as_name) != Some("Link") {
                    continue;
                }
                let target = link_target(&reader, &dictionary, &names)?;
                links.push(LinkAnnotation {
                    page: page_index,
                    rect: rect_of(&dictionary),
                    target,
                });
            }
        }
        Ok(links)
    }

    /// Return every embedded-file attachment, name-tree entries first.
    pub fn attachments(&self) -> Result<Vec<Attachment>> {
        let structure = self.supported_structure()?;
        let reader = Reader {
            data: self.bytes(),
            structure,
        };
        let mut attachments = Vec::new();

        let catalog = reader.resolve(&Object::Reference(structure.root_id), 0)?;
        if let Object::Dictionary(catalog) = catalog
            && let Some(tree) = catalog.get("Names")
            && let Some(tree) = reader.dictionary(tree, 0)?
            && let Some(files) = tree.get("EmbeddedFiles")
        {
            let mut entries = Vec::new();
            reader.walk_name_tree(files, &mut entries, 0)?;
            for (name, spec) in entries {
                attachments.push(attachment_from_spec(&reader, &name, &spec)?);
            }
        }

        for (page_index, page) in structure.pages.iter().enumerate() {
            let page_object = reader.resolve(&Object::Reference(page.id), 0)?;
            let Object::Dictionary(page_dictionary) = page_object else {
                continue;
            };
            let Some(annots) = page_dictionary.get("Annots") else {
                continue;
            };
            let Object::Array(annotations) = reader.resolve(annots, 0)? else {
                continue;
            };
            for annotation in annotations {
                let Some(dictionary) = reader.dictionary(&annotation, 0)? else {
                    continue;
                };
                if dictionary.get("Subtype").and_then(Object::as_name) != Some("FileAttachment") {
                    continue;
                }
                let Some(spec_object) = dictionary.get("FS") else {
                    continue;
                };
                let Some(spec) = reader.dictionary(spec_object, 0)? else {
                    continue;
                };
                let name = file_name(&spec).unwrap_or_else(|| "attachment".to_owned());
                let mut attachment = attachment_from_spec(&reader, &name, &spec)?;
                attachment.page = Some(page_index);
                attachments.push(attachment);
            }
        }
        Ok(attachments)
    }

    /// Whether the document catalog declares a `/Collection` dictionary.
    ///
    /// Acrobat writes `/Collection` for portfolio documents. The dictionary is
    /// not required for listing: [`Self::portfolio_items`] reports the embedded
    /// files of any document, so a plain PDF with attachments lists them too.
    pub fn is_portfolio(&self) -> Result<bool> {
        let structure = self.supported_structure()?;
        let reader = Reader {
            data: self.bytes(),
            structure,
        };
        match reader.resolve(&Object::Reference(structure.root_id), 0)? {
            Object::Dictionary(catalog) => Ok(catalog.contains_key("Collection")),
            _ => Ok(false),
        }
    }

    /// Every `/Names /EmbeddedFiles` entry as a portfolio item.
    ///
    /// `/UF` supplies the folder path and file name; when only `/F` exists it is
    /// used instead, and the name-tree key is the last fallback. Malformed name
    /// trees yield an empty list rather than an error, matching
    /// [`Self::attachments`]. Annotations and streams are not read here.
    pub fn portfolio_items(&self) -> Result<Vec<PortfolioItem>> {
        let structure = self.supported_structure()?;
        let reader = Reader {
            data: self.bytes(),
            structure,
        };
        let catalog = reader.resolve(&Object::Reference(structure.root_id), 0)?;
        let Object::Dictionary(catalog) = catalog else {
            return Ok(Vec::new());
        };
        let Some(tree) = catalog.get("Names") else {
            return Ok(Vec::new());
        };
        let Some(tree) = reader.dictionary(tree, 0)? else {
            return Ok(Vec::new());
        };
        let Some(files) = tree.get("EmbeddedFiles") else {
            return Ok(Vec::new());
        };
        let mut entries = Vec::new();
        reader.walk_name_tree(files, &mut entries, 0)?;
        let mut items = Vec::with_capacity(entries.len());
        for (key, spec) in entries {
            let path = file_name(&spec).unwrap_or_else(|| key.clone());
            let name = path
                .rsplit('/')
                .next()
                .filter(|name| !name.is_empty())
                .unwrap_or(&key)
                .to_owned();
            let stream = embedded_stream(&reader, &spec)?;
            items.push(PortfolioItem {
                key,
                path,
                name,
                description: spec_description(&spec),
                size: stream
                    .as_ref()
                    .and_then(|stream| match stream.get("Length") {
                        Some(Object::Integer(length)) if *length >= 0 => Some(*length as u64),
                        _ => None,
                    }),
                mime: stream.as_ref().and_then(stream_mime),
                modified: stream
                    .as_ref()
                    .and_then(stream_modified)
                    .or_else(|| spec_modified(&spec)),
            });
        }
        Ok(items)
    }
}

/// Build an [`Attachment`] from a file-specification dictionary.
fn attachment_from_spec(
    reader: &Reader<'_>,
    fallback_name: &str,
    spec: &Dictionary,
) -> Result<Attachment> {
    Ok(Attachment {
        name: file_name(spec).unwrap_or_else(|| fallback_name.to_owned()),
        description: spec_description(spec),
        size: embedded_size(reader, spec)?,
        page: None,
    })
}

/// Text of a file specification's `/Desc` entry, when present.
fn spec_description(spec: &Dictionary) -> Option<String> {
    spec.get("Desc").and_then(|value| match value {
        Object::String(bytes) => Some(String::from_utf8_lossy(bytes).into_owned()),
        _ => None,
    })
}

/// Text of a file specification's own `/ModDate`, when present.
///
/// The embedded stream's `/Params /ModDate` is authoritative; this is the
/// fallback for writers that only date the file specification.
fn spec_modified(spec: &Dictionary) -> Option<String> {
    spec.get("ModDate").and_then(|value| match value {
        Object::String(bytes) => Some(String::from_utf8_lossy(bytes).into_owned()),
        _ => None,
    })
}

/// Dictionary of the embedded file stream a file specification points at.
fn embedded_stream(reader: &Reader<'_>, spec: &Dictionary) -> Result<Option<Dictionary>> {
    let Some(embedded) = spec.get("EF") else {
        return Ok(None);
    };
    let Some(embedded_files) = reader.dictionary(embedded, 0)? else {
        return Ok(None);
    };
    let Some(Object::Reference(id)) = embedded_files.get("F") else {
        return Ok(None);
    };
    let stream = load_object(reader.data, &reader.structure.xref, *id)?;
    Ok(match stream {
        Object::Stream { dictionary, .. } => Some(dictionary),
        _ => None,
    })
}

/// Declared length of the embedded stream a file specification points at.
fn embedded_size(reader: &Reader<'_>, spec: &Dictionary) -> Result<Option<u64>> {
    let Some(stream) = embedded_stream(reader, spec)? else {
        return Ok(None);
    };
    Ok(match stream.get("Length") {
        Some(Object::Integer(length)) if *length >= 0 => Some(*length as u64),
        _ => None,
    })
}

/// `/Subtype` media type declared by an embedded file stream.
fn stream_mime(stream: &Dictionary) -> Option<String> {
    stream
        .get("Subtype")
        .and_then(Object::as_name)
        .map(str::to_owned)
}

/// `/Params /ModDate` of an embedded file stream, exactly as stored.
fn stream_modified(stream: &Dictionary) -> Option<String> {
    let params = stream.get("Params")?;
    let Object::Dictionary(params) = params else {
        return None;
    };
    params.get("ModDate").and_then(|value| match value {
        Object::String(bytes) => Some(String::from_utf8_lossy(bytes).into_owned()),
        _ => None,
    })
}

/// Interpret a link annotation's action or destination.
fn link_target(
    reader: &Reader<'_>,
    annotation: &Dictionary,
    names: &BTreeMap<String, Option<usize>>,
) -> Result<LinkTarget> {
    if let Some(action) = annotation.get("A")
        && let Some(action) = reader.dictionary(action, 0)?
    {
        match action.get("S").and_then(Object::as_name) {
            Some("URI") => {
                if let Some(Object::String(uri)) = action.get("URI") {
                    return Ok(LinkTarget::Uri {
                        uri: String::from_utf8_lossy(uri).into_owned(),
                    });
                }
            }
            Some("GoTo") => {
                if let Some(destination) = action.get("D") {
                    return named_or_page(reader, destination, names);
                }
            }
            _ => {}
        }
    }
    if let Some(destination) = annotation.get("Dest") {
        return named_or_page(reader, destination, names);
    }
    Ok(LinkTarget::Unresolved {
        detail: "link has no /Dest or /A".to_owned(),
    })
}

/// Resolve a destination value into a page or a named target.
fn named_or_page(
    reader: &Reader<'_>,
    destination: &Object,
    names: &BTreeMap<String, Option<usize>>,
) -> Result<LinkTarget> {
    let name = match destination {
        Object::String(bytes) => Some(String::from_utf8_lossy(bytes).into_owned()),
        Object::Name(name) => Some(name.clone()),
        _ => None,
    };
    let page = reader.destination_page(destination, names, 0)?;
    match (page, name) {
        (Some(page), name) => Ok(LinkTarget::Page { page, name }),
        (None, Some(name)) => Ok(LinkTarget::Unresolved { detail: name }),
        (None, None) => Ok(LinkTarget::Unresolved {
            detail: "destination has no page reference".to_owned(),
        }),
    }
}

impl Document {
    /// Return the decoded bytes of the embedded file called `name`.
    ///
    /// The name is matched against `/Names /EmbeddedFiles` entries and
    /// `/FileAttachment` annotations. The embedded stream is decoded through its
    /// `/Filter` chain, so the caller receives the original file bytes.
    ///
    /// # Errors
    ///
    /// Returns [`Error::AttachmentNotFound`] when no attachment carries the
    /// name, and a decoding error when the stream cannot be decoded.
    pub fn attachment_bytes(&self, name: &str) -> Result<Vec<u8>> {
        let structure = self.supported_structure()?;
        let reader = Reader {
            data: self.bytes(),
            structure,
        };
        let spec = self.find_attachment_spec(&reader, name)?;
        let Some(spec) = spec else {
            return Err(Error::AttachmentNotFound(name.to_owned()));
        };
        let Some(embedded) = spec.get("EF") else {
            return Err(Error::AttachmentNotFound(name.to_owned()));
        };
        let Some(embedded) = reader.dictionary(embedded, 0)? else {
            return Err(Error::AttachmentNotFound(name.to_owned()));
        };
        let Some(stream_object) = embedded.get("F").or_else(|| embedded.get("UF")) else {
            return Err(Error::AttachmentNotFound(name.to_owned()));
        };
        let Object::Stream { dictionary, data } = reader.resolve(stream_object, 0)? else {
            return Err(Error::AttachmentNotFound(name.to_owned()));
        };
        let filters = crate::filter::filters_from_dictionary(&dictionary)?;
        crate::filter::decode_stream(&data, &filters)
    }

    /// Find the file-specification dictionary for an attachment name.
    fn find_attachment_spec(&self, reader: &Reader<'_>, name: &str) -> Result<Option<Dictionary>> {
        let structure = reader.structure;
        let catalog = reader.resolve(&Object::Reference(structure.root_id), 0)?;
        if let Object::Dictionary(catalog) = catalog
            && let Some(tree) = catalog.get("Names")
            && let Some(tree) = reader.dictionary(tree, 0)?
            && let Some(files) = tree.get("EmbeddedFiles")
        {
            let mut entries = Vec::new();
            reader.walk_name_tree(files, &mut entries, 0)?;
            for (entry_name, spec) in entries {
                if entry_name == name || file_name(&spec).as_deref() == Some(name) {
                    return Ok(Some(spec));
                }
            }
        }
        for page in &structure.pages {
            let page_object = reader.resolve(&Object::Reference(page.id), 0)?;
            let Object::Dictionary(page_dictionary) = page_object else {
                continue;
            };
            let Some(annots) = page_dictionary.get("Annots") else {
                continue;
            };
            let Object::Array(annotations) = reader.resolve(annots, 0)? else {
                continue;
            };
            for annotation in annotations {
                let Some(dictionary) = reader.dictionary(&annotation, 0)? else {
                    continue;
                };
                if dictionary.get("Subtype").and_then(Object::as_name) != Some("FileAttachment") {
                    continue;
                }
                let Some(spec_object) = dictionary.get("FS") else {
                    continue;
                };
                let Some(spec) = reader.dictionary(spec_object, 0)? else {
                    continue;
                };
                if file_name(&spec).as_deref() == Some(name) {
                    return Ok(Some(spec));
                }
            }
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Assemble a complete classic-table PDF from a list of object bodies.
    fn assemble(objects: &[String]) -> Vec<u8> {
        let mut out = String::from("%PDF-1.7\n%PDFXI\n");
        let mut offsets = Vec::with_capacity(objects.len());
        for (index, body) in objects.iter().enumerate() {
            offsets.push(out.len());
            out.push_str(&format!("{} 0 obj\n{}\nendobj\n", index + 1, body));
        }
        let xref = out.len();
        out.push_str(&format!("xref\n0 {}\n", objects.len() + 1));
        out.push_str("0000000000 65535 f \n");
        for offset in &offsets {
            out.push_str(&format!("{offset:010} 00000 n \n"));
        }
        out.push_str(&format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            objects.len() + 1
        ));
        out.into_bytes()
    }

    /// One page with three links (explicit, URI, named) and one attachment.
    fn annotated_pdf() -> Vec<u8> {
        assemble(&[
            "<< /Type /Catalog /Pages 2 0 R /Dests << /here [3 0 R /Fit] >> /Names << /EmbeddedFiles << /Names [(notes.txt) 6 0 R] >> >> >>".to_owned(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Annots [4 0 R 5 0 R 8 0 R] >>".to_owned(),
            "<< /Type /Annot /Subtype /Link /Rect [10 10 100 100] /Dest [3 0 R /Fit] >>".to_owned(),
            "<< /Type /Annot /Subtype /Link /Rect [10 120 100 160] /A << /S /URI /URI (https://example.com) >> >>".to_owned(),
            "<< /Type /Filespec /F (notes.txt) /UF (notes.txt) /Desc (Sample notes) /EF << /F 7 0 R >> >>".to_owned(),
            "<< /Type /EmbeddedFile /Length 5 >>\nstream\nhello\nendstream".to_owned(),
            "<< /Type /Annot /Subtype /Link /Rect [10 170 100 200] /Dest (here) >>".to_owned(),
        ])
    }

    #[test]
    fn portfolio_items_report_folder_paths_mime_and_stream_metadata() {
        let pdf = assemble(&[
            "<< /Type /Catalog /Pages 2 0 R /Collection << /View /D >> /Names << /EmbeddedFiles << /Names [(brief.txt) 4 0 R (q1.pdf) 6 0 R] >> >> >>".to_owned(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>".to_owned(),
            "<< /Type /Filespec /F (brief.txt) /UF (Reports/brief.txt) /Desc (Quarterly brief) /EF << /F 5 0 R >> >>".to_owned(),
            "<< /Type /EmbeddedFile /Subtype /text#2Fplain /Length 5 /Params << /ModDate (D:20260101120000Z) /Size 5 >> >>\nstream\nhello\nendstream".to_owned(),
            "<< /Type /Filespec /F (q1.pdf) /UF (Reports/2026/q1.pdf) /EF << /F 7 0 R >> >>".to_owned(),
            "<< /Type /EmbeddedFile /Subtype /application#2Fpdf /Length 5 >>\nstream\nworld\nendstream".to_owned(),
        ]);
        let document = Document::from_bytes(pdf).expect("portfolio parses");
        assert!(document.is_portfolio().expect("read collection"));
        let items = document.portfolio_items().expect("read portfolio items");
        assert_eq!(items.len(), 2);

        assert_eq!(items[0].key, "brief.txt");
        assert_eq!(items[0].path, "Reports/brief.txt");
        assert_eq!(items[0].name, "brief.txt");
        assert_eq!(items[0].description.as_deref(), Some("Quarterly brief"));
        assert_eq!(items[0].size, Some(5));
        assert_eq!(items[0].mime.as_deref(), Some("text/plain"));
        assert_eq!(items[0].modified.as_deref(), Some("D:20260101120000Z"));

        assert_eq!(items[1].key, "q1.pdf");
        assert_eq!(items[1].path, "Reports/2026/q1.pdf");
        assert_eq!(items[1].name, "q1.pdf");
        assert_eq!(items[1].description, None);
        assert_eq!(items[1].mime.as_deref(), Some("application/pdf"));
        assert_eq!(items[1].modified, None);
    }

    #[test]
    fn a_document_without_a_collection_is_not_a_portfolio_and_lists_attachments() {
        let document = Document::from_bytes(annotated_pdf()).expect("fixture parses");
        assert!(!document.is_portfolio().expect("read collection"));
        let items = document.portfolio_items().expect("read portfolio items");
        assert_eq!(items.len(), 1, "name-tree attachment still lists");
        assert_eq!(items[0].key, "notes.txt");
        assert_eq!(items[0].path, "notes.txt");
        assert_eq!(items[0].name, "notes.txt");
    }

    #[test]
    fn links_report_explicit_uri_and_named_targets() {
        let document = Document::from_bytes(annotated_pdf()).expect("fixture parses");
        let links = document.links().expect("links read");
        assert_eq!(links.len(), 3, "{links:?}");
        assert_eq!(links[0].page, 0);
        assert_eq!(links[0].rect, [10.0, 10.0, 100.0, 100.0]);
        assert_eq!(
            links[0].target,
            LinkTarget::Page {
                page: 0,
                name: None
            }
        );
        assert_eq!(
            links[1].target,
            LinkTarget::Uri {
                uri: "https://example.com".to_owned()
            }
        );
        assert_eq!(
            links[2].target,
            LinkTarget::Page {
                page: 0,
                name: Some("here".to_owned())
            }
        );
    }

    #[test]
    fn attachment_bytes_returns_the_embedded_payload() {
        let document = Document::from_bytes(annotated_pdf()).expect("fixture parses");
        let bytes = document
            .attachment_bytes("notes.txt")
            .expect("attachment decodes");
        assert_eq!(bytes, b"hello");
        assert!(matches!(
            document.attachment_bytes("missing.txt"),
            Err(Error::AttachmentNotFound(name)) if name == "missing.txt"
        ));
    }

    #[test]
    fn attachments_report_name_description_and_size() {
        let document = Document::from_bytes(annotated_pdf()).expect("fixture parses");
        let attachments = document.attachments().expect("attachments read");
        assert_eq!(attachments.len(), 1, "{attachments:?}");
        assert_eq!(attachments[0].name, "notes.txt");
        assert_eq!(attachments[0].description.as_deref(), Some("Sample notes"));
        assert_eq!(attachments[0].size, Some(5));
        assert_eq!(attachments[0].page, None);
    }
}
