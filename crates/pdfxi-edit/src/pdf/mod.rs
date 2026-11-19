//! PDF dictionary serialization and appearance geometry for annotations.
//!
//! This module keeps `pdfxi-edit` independent of the `pdfxi-core` object model:
//! [`value::Dict`] and [`value::Value`] form a small, dependency-free
//! intermediate representation that a future writer can map onto PDF objects.
//!
//! - [`serialize`] maps [`Annotation`](crate::Annotation) to and from a
//!   dictionary, validates geometry, colour, and required points, and preserves
//!   unknown subtypes through [`serialize::PdfSubtype::Opaque`].
//! - [`appearance`] derives deterministic border, icon, and markup geometry.
//! - [`error`] defines the structured errors reported by both sides.
//!
//! # Example
//!
//! ```text
//! use pdfxi_edit::annotation::{Annotation, AnnotationId, AnnotationKind, Color};
//! use pdfxi_edit::geometry::{PageIndex, Rect};
//! use pdfxi_edit::pdf::{annotation_to_dict, dict_to_annotation};
//!
//! let rect = Rect::new(10.0, 20.0, 110.0, 70.0).unwrap();
//! let original = Annotation::new(
//!     AnnotationId::new(1),
//!     AnnotationKind::Highlight,
//!     PageIndex::new(0),
//!     rect,
//!     "alice",
//!     "review",
//! ).unwrap().with_color(Color::new(255, 255, 0));
//!
//! let dict = annotation_to_dict(&original);
//! assert_eq!(dict.get("Subtype").unwrap().as_name(), Some("Highlight"));
//! let restored = dict_to_annotation(&dict).unwrap();
//! assert_eq!(restored, original);
//! ```

pub mod appearance;
pub mod error;
pub mod serialize;
pub mod value;

pub use appearance::{
    AppearanceGeometry, BorderGeometry, IconLayout, MarkLayout, MarkupKind, PathCommand,
    border_path, build_appearance, icon_layout, markup_geometry, sort_quads_in_reading_order,
};
pub use error::PdfError;
pub use serialize::{
    AppearanceNames, BorderLineStyle, BorderStyle, FreeTextData, InkData, LineData, LineEnding,
    LinkData, MarkupData, PdfAnnotation, PdfSubtype, Quad, ShapeData, TextData, TextIcon,
    annotation_to_dict, dict_to_annotation,
};
pub use value::{Dict, Value};
