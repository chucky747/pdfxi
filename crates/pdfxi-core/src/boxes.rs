//! Typed page boxes and their validation.
//!
//! ISO 32000-1 defines five rectangles that control how a page is imposed and
//! printed. All are expressed in *default user space units* (1/72 inch, a
//! "point") with the origin at the lower-left corner of the page:
//!
//! - **MediaBox** — the boundaries of the physical medium. Required and
//!   inheritable; falls back to US Letter `[0 0 612 792]`.
//! - **CropBox** — the region to which content is clipped for display and
//!   printing. Inheritable; defaults to the MediaBox.
//! - **BleedBox** — the region for a production environment, normally larger
//!   than the TrimBox so the trim can include bleed. Defaults to the CropBox.
//! - **TrimBox** — the intended finished size of the page. Defaults to the
//!   CropBox.
//! - **ArtBox** — the extent of the page's meaningful content. Defaults to the
//!   CropBox.
//!
//! Only `/MediaBox` and `/CropBox` are inheritable through the `/Parent` chain;
//! `/BleedBox`, `/TrimBox` and `/ArtBox` are read from the page dictionary
//! itself. [`Document::page_boxes`] applies the documented fallback chain and
//! preserves which keys were explicitly present so a write can avoid inventing
//! entries that the source did not carry. [`Document::set_page_boxes`] edits the
//! page dictionary through a copy-on-write change, cloning every existing key
//! so unknown entries survive the round-trip unchanged.
//!
//! [`PageBoxes::validate`] checks the canonical containment hierarchy
//! (`CropBox ⊆ MediaBox`, `TrimBox ⊆ CropBox`, `ArtBox ⊆ CropBox`,
//! `BleedBox ⊆ MediaBox`, `TrimBox ⊆ BleedBox`). Reads are tolerant of
//! non-canonical hierarchies found in hostile input; writes are strict, so a
//! caller can never persist an inconsistent set of boxes.

use std::collections::{BTreeSet, HashSet};

use crate::object::{Object, ObjectId};
use crate::page::Rectangle;
use crate::parser::{Structure, load_object};
use crate::{Document, Error, Result};

/// One of the five standard page boxes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum BoxKind {
    /// The `/MediaBox`.
    Media,
    /// The `/CropBox`.
    Crop,
    /// The `/BleedBox`.
    Bleed,
    /// The `/TrimBox`.
    Trim,
    /// The `/ArtBox`.
    Art,
}

impl BoxKind {
    /// All five box kinds in the order they are defined by ISO 32000-1.
    pub const ALL: [Self; 5] = [Self::Media, Self::Crop, Self::Bleed, Self::Trim, Self::Art];

    /// Return the PDF dictionary key for this box, without the leading slash.
    pub fn key(self) -> &'static str {
        match self {
            Self::Media => "MediaBox",
            Self::Crop => "CropBox",
            Self::Bleed => "BleedBox",
            Self::Trim => "TrimBox",
            Self::Art => "ArtBox",
        }
    }
}

/// The five page boxes of a single page, plus which ones were explicit.
///
/// Rectangles are stored in default user space units (points), origin at the
/// lower-left corner. The value returned by [`Document::page_boxes`] already
/// has the ISO fallback chain applied, so every rectangle has a concrete value
/// even when the source dictionary omitted the corresponding key.
#[derive(Debug, Clone, PartialEq)]
pub struct PageBoxes {
    media: Rectangle,
    crop: Rectangle,
    bleed: Rectangle,
    trim: Rectangle,
    art: Rectangle,
    explicit: BTreeSet<BoxKind>,
}

impl PageBoxes {
    /// Build a canonical set of boxes from a MediaBox alone.
    ///
    /// Every other box defaults to `media`, and only `/MediaBox` is marked as
    /// explicit, so [`Document::set_page_boxes`] will not invent extra keys.
    ///
    /// # Errors
    ///
    /// Returns [`Error::NonFiniteBoxCoordinate`] or [`Error::DegenerateBox`]
    /// when `media` is not a finite, non-degenerate rectangle.
    pub fn from_media(media: Rectangle) -> Result<Self> {
        check_rectangle(BoxKind::Media, media)?;
        let mut explicit = BTreeSet::new();
        explicit.insert(BoxKind::Media);
        Ok(Self {
            media,
            crop: media,
            bleed: media,
            trim: media,
            art: media,
            explicit,
        })
    }

    /// Set the CropBox and mark it explicit.
    ///
    /// # Errors
    ///
    /// Returns a rectangle error when `crop` is not finite and non-degenerate.
    pub fn with_crop(mut self, crop: Rectangle) -> Result<Self> {
        check_rectangle(BoxKind::Crop, crop)?;
        self.crop = crop;
        // Boxes that were still following the old fallback value follow the new
        // CropBox unless the caller has set them explicitly.
        for kind in [BoxKind::Bleed, BoxKind::Trim, BoxKind::Art] {
            if !self.explicit.contains(&kind) {
                match kind {
                    BoxKind::Bleed => self.bleed = crop,
                    BoxKind::Trim => self.trim = crop,
                    BoxKind::Art => self.art = crop,
                    _ => {}
                }
            }
        }
        self.explicit.insert(BoxKind::Crop);
        Ok(self)
    }

    /// Set the BleedBox and mark it explicit.
    ///
    /// # Errors
    ///
    /// Returns a rectangle error when `bleed` is not finite and non-degenerate.
    pub fn with_bleed(mut self, bleed: Rectangle) -> Result<Self> {
        check_rectangle(BoxKind::Bleed, bleed)?;
        self.bleed = bleed;
        self.explicit.insert(BoxKind::Bleed);
        Ok(self)
    }

    /// Set the TrimBox and mark it explicit.
    ///
    /// # Errors
    ///
    /// Returns a rectangle error when `trim` is not finite and non-degenerate.
    pub fn with_trim(mut self, trim: Rectangle) -> Result<Self> {
        check_rectangle(BoxKind::Trim, trim)?;
        self.trim = trim;
        self.explicit.insert(BoxKind::Trim);
        Ok(self)
    }

    /// Set the ArtBox and mark it explicit.
    ///
    /// # Errors
    ///
    /// Returns a rectangle error when `art` is not finite and non-degenerate.
    pub fn with_art(mut self, art: Rectangle) -> Result<Self> {
        check_rectangle(BoxKind::Art, art)?;
        self.art = art;
        self.explicit.insert(BoxKind::Art);
        Ok(self)
    }

    /// Return the rectangle for `kind`.
    pub fn rectangle(&self, kind: BoxKind) -> Rectangle {
        match kind {
            BoxKind::Media => self.media,
            BoxKind::Crop => self.crop,
            BoxKind::Bleed => self.bleed,
            BoxKind::Trim => self.trim,
            BoxKind::Art => self.art,
        }
    }

    /// Return the MediaBox.
    pub fn media(&self) -> Rectangle {
        self.media
    }

    /// Return the CropBox (or its MediaBox fallback).
    pub fn crop(&self) -> Rectangle {
        self.crop
    }

    /// Return the BleedBox (or its CropBox fallback).
    pub fn bleed(&self) -> Rectangle {
        self.bleed
    }

    /// Return the TrimBox (or its CropBox fallback).
    pub fn trim(&self) -> Rectangle {
        self.trim
    }

    /// Return the ArtBox (or its CropBox fallback).
    pub fn art(&self) -> Rectangle {
        self.art
    }

    /// Return `true` when `kind` was explicitly present in the source or has
    /// been set through a `with_*` builder.
    pub fn is_explicit(&self, kind: BoxKind) -> bool {
        self.explicit.contains(&kind)
    }

    /// Validate the canonical containment hierarchy.
    ///
    /// Every box must be finite and non-degenerate, and the following
    /// relationships must hold: CropBox within MediaBox, TrimBox within
    /// CropBox, ArtBox within CropBox, BleedBox within MediaBox, and TrimBox
    /// within BleedBox. Containment is inclusive (equal edges are allowed).
    ///
    /// # Errors
    ///
    /// Returns [`Error::BoxNotContained`] when a box lies outside the box that
    /// must contain it, or a rectangle error for a malformed box.
    pub fn validate(&self) -> Result<()> {
        for kind in BoxKind::ALL {
            check_rectangle(kind, self.rectangle(kind))?;
        }
        require_contained(BoxKind::Crop, BoxKind::Media, self.crop, self.media)?;
        require_contained(BoxKind::Trim, BoxKind::Crop, self.trim, self.crop)?;
        require_contained(BoxKind::Art, BoxKind::Crop, self.art, self.crop)?;
        require_contained(BoxKind::Bleed, BoxKind::Media, self.bleed, self.media)?;
        require_contained(BoxKind::Trim, BoxKind::Bleed, self.trim, self.bleed)?;
        Ok(())
    }

    /// Assemble a fully explicit set from parsed rectangles.
    fn from_parts(parts: [(BoxKind, Rectangle); 5], explicit: BTreeSet<BoxKind>) -> Self {
        let get = |kind: BoxKind| -> Rectangle {
            parts
                .iter()
                .find(|(k, _)| *k == kind)
                .map(|(_, rectangle)| *rectangle)
                .unwrap_or_default()
        };
        Self {
            media: get(BoxKind::Media),
            crop: get(BoxKind::Crop),
            bleed: get(BoxKind::Bleed),
            trim: get(BoxKind::Trim),
            art: get(BoxKind::Art),
            explicit,
        }
    }

    /// Serialize one rectangle as a four-element PDF array of numbers.
    fn to_array(&self, kind: BoxKind) -> Object {
        rectangle_to_object(self.rectangle(kind))
    }
}

/// Reject a rectangle with non-finite coordinates or zero/negative extent.
fn check_rectangle(kind: BoxKind, rectangle: Rectangle) -> Result<()> {
    if ![
        rectangle.left,
        rectangle.bottom,
        rectangle.right,
        rectangle.top,
    ]
    .iter()
    .all(|value| value.is_finite())
    {
        return Err(Error::NonFiniteBoxCoordinate { name: kind.key() });
    }
    if rectangle.right <= rectangle.left || rectangle.top <= rectangle.bottom {
        return Err(Error::DegenerateBox { name: kind.key() });
    }
    Ok(())
}

/// Return `true` when `inner` lies within `outer`, inclusive of edges.
fn contains(outer: Rectangle, inner: Rectangle) -> bool {
    outer.left <= inner.left
        && outer.bottom <= inner.bottom
        && inner.right <= outer.right
        && inner.top <= outer.top
}

/// Reject `inner` when it is not contained by `outer`.
fn require_contained(
    inner: BoxKind,
    outer: BoxKind,
    inner_rect: Rectangle,
    outer_rect: Rectangle,
) -> Result<()> {
    if contains(outer_rect, inner_rect) {
        Ok(())
    } else {
        Err(Error::BoxNotContained {
            inner: inner.key(),
            outer: outer.key(),
        })
    }
}

/// Convert a rectangle into an `Object::Array` of integers or reals.
fn rectangle_to_object(rectangle: Rectangle) -> Object {
    let number = |value: f64| {
        if value.fract() == 0.0 {
            Object::Integer(value as i64)
        } else {
            Object::Real(value)
        }
    };
    Object::Array(vec![
        number(rectangle.left),
        number(rectangle.bottom),
        number(rectangle.right),
        number(rectangle.top),
    ])
}

/// Parse a four-number box array from a page dictionary entry.
fn parse_box(value: &Object, kind: BoxKind, page: u32) -> Result<Rectangle> {
    let items = value.as_array().ok_or(Error::InvalidBoxArray {
        page,
        name: kind.key(),
    })?;
    if items.len() != 4 {
        return Err(Error::InvalidBoxArray {
            page,
            name: kind.key(),
        });
    }
    let mut coordinates = [0.0f64; 4];
    for (slot, item) in coordinates.iter_mut().zip(items.iter()) {
        *slot = item.as_f64().ok_or(Error::InvalidBoxArray {
            page,
            name: kind.key(),
        })?;
    }
    let rectangle = Rectangle::new(
        coordinates[0],
        coordinates[1],
        coordinates[2],
        coordinates[3],
    );
    check_rectangle(kind, rectangle)?;
    Ok(rectangle)
}

/// Load `id` from a staged change first, then from the cross-reference table.
fn effective_object(document: &Document, structure: &Structure, id: ObjectId) -> Result<Object> {
    match document.changes.get(&id) {
        Some(object) => Ok(object.clone()),
        None => load_object(document.bytes(), &structure.xref, id),
    }
}

impl Document {
    /// Read the five page boxes of page `index` with ISO fallbacks applied.
    ///
    /// `/MediaBox` and `/CropBox` are inherited through the `/Parent` chain
    /// (the nearest value wins); `/BleedBox`, `/TrimBox` and `/ArtBox` are read
    /// only from the page itself. Missing values fall back in this order:
    /// CropBox → MediaBox, and BleedBox/TrimBox/ArtBox → CropBox. A page with
    /// no MediaBox anywhere in its ancestry uses US Letter.
    ///
    /// Reads are tolerant: a non-canonical containment hierarchy in hostile
    /// input is returned as-is rather than rejected. Call
    /// [`PageBoxes::validate`] to enforce the canonical hierarchy, which
    /// [`Document::set_page_boxes`] does before writing.
    ///
    /// # Errors
    ///
    /// Returns [`Error::PageNotFound`] when `index` is out of range,
    /// [`Error::MalformedPageTree`] for a cycle or a non-dictionary node, and a
    /// box error when a present entry is malformed, non-finite or degenerate.
    pub fn page_boxes(&self, index: usize) -> Result<PageBoxes> {
        let structure = self.structure.as_ref().map_err(Clone::clone)?;
        let page = structure
            .pages
            .get(index)
            .ok_or(Error::PageNotFound(index))?;

        let mut media = None;
        let mut crop = None;
        let mut bleed = None;
        let mut trim = None;
        let mut art = None;
        let mut explicit = BTreeSet::new();
        let mut seen = HashSet::new();
        let mut current = page.id;
        let mut page_level = true;

        loop {
            if !seen.insert(current.number) {
                return Err(Error::MalformedPageTree(
                    "cycle in page /Parent chain while reading boxes".to_owned(),
                ));
            }
            let object = effective_object(self, structure, current)?;
            let dictionary = object.as_dictionary().ok_or_else(|| {
                Error::MalformedPageTree("page tree node is not a dictionary".to_owned())
            })?;

            if media.is_none()
                && let Some(value) = dictionary.get(BoxKind::Media.key())
            {
                media = Some(parse_box(value, BoxKind::Media, current.number)?);
                if page_level {
                    explicit.insert(BoxKind::Media);
                }
            }
            if crop.is_none()
                && let Some(value) = dictionary.get(BoxKind::Crop.key())
            {
                crop = Some(parse_box(value, BoxKind::Crop, current.number)?);
                if page_level {
                    explicit.insert(BoxKind::Crop);
                }
            }
            if page_level {
                for (kind, slot) in [
                    (BoxKind::Bleed, &mut bleed),
                    (BoxKind::Trim, &mut trim),
                    (BoxKind::Art, &mut art),
                ] {
                    if let Some(value) = dictionary.get(kind.key()) {
                        *slot = Some(parse_box(value, kind, current.number)?);
                        explicit.insert(kind);
                    }
                }
                page_level = false;
            }

            match dictionary.get("Parent").and_then(Object::as_reference) {
                Some(parent) => current = parent,
                None => break,
            }
        }

        let media = media.unwrap_or_default();
        let crop = crop.unwrap_or(media);
        let bleed = bleed.unwrap_or(crop);
        let trim = trim.unwrap_or(crop);
        let art = art.unwrap_or(crop);
        Ok(PageBoxes::from_parts(
            [
                (BoxKind::Media, media),
                (BoxKind::Crop, crop),
                (BoxKind::Bleed, bleed),
                (BoxKind::Trim, trim),
                (BoxKind::Art, art),
            ],
            explicit,
        ))
    }

    /// Write the five page boxes of page `index` through a copy-on-write change.
    ///
    /// Every existing key of the page dictionary is preserved; only the box
    /// entries are touched. Explicit boxes are written as four-number arrays;
    /// non-explicit boxes have their key removed so the ISO fallback applies on
    /// the next read. The change is visible after both [`Document::save`] and
    /// [`Document::save_incremental`].
    ///
    /// # Errors
    ///
    /// Returns [`Error::PageNotFound`] when `index` is out of range, a
    /// validation error when `boxes` is inconsistent, and any structural or
    /// object-identification error from the underlying document.
    pub fn set_page_boxes(&mut self, index: usize, boxes: &PageBoxes) -> Result<()> {
        boxes.validate()?;
        let (id, object) = {
            let structure = self.structure.as_ref().map_err(Clone::clone)?;
            let page = structure
                .pages
                .get(index)
                .ok_or(Error::PageNotFound(index))?;
            (page.id, effective_object(self, structure, page.id)?)
        };
        let stream_data = match &object {
            Object::Stream { data, .. } => Some(data.clone()),
            _ => None,
        };
        let mut dictionary = match object {
            Object::Dictionary(dictionary) => dictionary,
            Object::Stream { dictionary, .. } => dictionary,
            _ => {
                return Err(Error::MalformedPageTree(
                    "page object is not a dictionary".to_owned(),
                ));
            }
        };

        dictionary.insert(
            BoxKind::Media.key().to_owned(),
            boxes.to_array(BoxKind::Media),
        );
        for kind in [BoxKind::Crop, BoxKind::Bleed, BoxKind::Trim, BoxKind::Art] {
            if boxes.is_explicit(kind) {
                dictionary.insert(kind.key().to_owned(), boxes.to_array(kind));
            } else {
                dictionary.remove(kind.key());
            }
        }

        let replacement = match stream_data {
            // A /Page is normally a dictionary, but preserve the object kind if
            // a malformed source stored a stream so unknown bytes survive.
            Some(data) => Object::Stream { dictionary, data },
            None => Object::Dictionary(dictionary),
        };
        self.set_object(id, replacement)?;
        Ok(())
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

    /// A two-page PDF with custom boxes on page 1 and an unknown key on page 2.
    fn boxed_pdf() -> Vec<u8> {
        let objects = vec![
            "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
            "<< /Type /Pages /Kids [3 0 R 5 0 R] /Count 2 >>".to_owned(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
             /TrimBox [20 20 592 772] /BleedBox [10 10 602 782] \
             /CustomKey (keep me) >>"
                .to_owned(),
            "<< /Length 13 >>\nstream\nBT (One) Tj ET\nendstream".to_owned(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 400] \
             /CustomKey (untouched) >>"
                .to_owned(),
            "<< /Length 13 >>\nstream\nBT (Two) Tj ET\nendstream".to_owned(),
        ];
        assemble(&objects)
    }

    #[test]
    fn defaults_fall_back_to_media_box() {
        let media = Rectangle::new(0.0, 0.0, 612.0, 792.0);
        let boxes = PageBoxes::from_media(media).expect("valid media");
        assert_eq!(boxes.media(), media);
        assert_eq!(boxes.crop(), media);
        assert_eq!(boxes.bleed(), media);
        assert_eq!(boxes.trim(), media);
        assert_eq!(boxes.art(), media);
        assert!(boxes.is_explicit(BoxKind::Media));
        assert!(!boxes.is_explicit(BoxKind::Crop));
        assert!(boxes.validate().is_ok());
    }

    #[test]
    fn containment_validation_rejects_out_of_order_boxes() {
        let media = Rectangle::new(0.0, 0.0, 612.0, 792.0);
        let bad_crop = Rectangle::new(-10.0, 0.0, 612.0, 792.0);
        let boxes = PageBoxes::from_media(media)
            .expect("valid media")
            .with_crop(bad_crop)
            .expect("finite crop");
        assert!(matches!(
            boxes.validate(),
            Err(Error::BoxNotContained {
                inner: "CropBox",
                outer: "MediaBox"
            })
        ));

        let trim_outside_bleed = Rectangle::new(20.0, 20.0, 592.0, 772.0);
        let boxes = PageBoxes::from_media(media)
            .expect("valid media")
            .with_crop(media)
            .expect("valid crop")
            .with_bleed(Rectangle::new(30.0, 30.0, 582.0, 762.0))
            .expect("valid bleed")
            .with_trim(trim_outside_bleed)
            .expect("valid trim");
        assert!(matches!(
            boxes.validate(),
            Err(Error::BoxNotContained {
                inner: "TrimBox",
                outer: "BleedBox"
            })
        ));
    }

    #[test]
    fn read_write_round_trip_preserves_boxes() {
        let source = boxed_pdf();
        let document = Document::from_bytes(source.clone()).expect("valid fixture");
        let boxes = document.page_boxes(0).expect("page boxes");
        assert_eq!(boxes.bleed(), Rectangle::new(10.0, 10.0, 602.0, 782.0));
        assert_eq!(boxes.trim(), Rectangle::new(20.0, 20.0, 592.0, 772.0));
        assert_eq!(boxes.crop(), boxes.media(), "crop falls back to media");
        assert_eq!(boxes.art(), boxes.crop(), "art falls back to crop");
        assert!(boxes.is_explicit(BoxKind::Bleed));
        assert!(!boxes.is_explicit(BoxKind::Crop));

        let mut document = document;
        document.set_page_boxes(0, &boxes).expect("write boxes");
        let saved = document.save().expect("save");
        let reparsed = Document::from_bytes(saved).expect("saved parses");
        assert_eq!(reparsed.page_boxes(0).expect("reread"), boxes);
    }

    #[test]
    fn writing_boxes_preserves_unknown_dictionary_keys() {
        let source = boxed_pdf();
        let mut document = Document::from_bytes(source).expect("valid fixture");
        let boxes = document.page_boxes(0).expect("page boxes");
        document.set_page_boxes(0, &boxes).expect("write");
        let saved = document.save().expect("save");
        let text = String::from_utf8_lossy(&saved);
        assert!(
            text.contains("/CustomKey <6B656570206D65>"),
            "page 1 key lost"
        );
        // Page 2 was not rewritten, so its literal string stays verbatim.
        assert!(text.contains("/CustomKey (untouched)"), "page 2 key lost");
    }

    #[test]
    fn unchanged_page_object_bytes_are_stable_after_saving() {
        let source = boxed_pdf();
        let second_page_start = source
            .windows(b"5 0 obj".len())
            .position(|window| window == b"5 0 obj")
            .expect("page 2 object");
        let second_page_end = source[second_page_start..]
            .windows(b"endobj".len())
            .position(|window| window == b"endobj")
            .expect("page 2 end")
            + second_page_start
            + b"endobj".len();
        let original = source[second_page_start..second_page_end].to_vec();

        let mut document = Document::from_bytes(source).expect("valid fixture");
        let boxes = document.page_boxes(0).expect("page boxes");
        document.set_page_boxes(0, &boxes).expect("write");
        let saved = document.save().expect("save");
        assert!(
            saved
                .windows(original.len())
                .any(|window| window == original.as_slice()),
            "page 2 object body changed across save"
        );
    }

    #[test]
    fn inherited_media_box_is_used_when_page_has_none() {
        let objects = vec![
            "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 400 500] >>".to_owned(),
            "<< /Type /Page /Parent 2 0 R /Contents 4 0 R >>".to_owned(),
            "<< /Length 13 >>\nstream\nBT (One) Tj ET\nendstream".to_owned(),
        ];
        let document = Document::from_bytes(assemble(&objects)).expect("valid fixture");
        let boxes = document.page_boxes(0).expect("boxes");
        assert_eq!(boxes.media(), Rectangle::new(0.0, 0.0, 400.0, 500.0));
        assert!(!boxes.is_explicit(BoxKind::Media), "inherited, not direct");
    }
}
