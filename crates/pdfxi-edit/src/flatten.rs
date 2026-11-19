//! Flatten annotations and form fields into page content streams.
//!
//! Flattening replaces an annotation or widget with a drawing that lives in the
//! page content stream and removes the interactive object from the page
//! dictionary. This makes the appearance permanent: it can no longer be edited
//! as an annotation, and a viewer that ignores annotations still shows it.
//!
//! # Model
//!
//! The crate deliberately has no PDF object-store dependency. Annotations live
//! in a page dictionary's `/Annots` array as [`Value::Dictionary`] entries.
//! A normal appearance cannot be stored in the small [`Value`] representation
//! (it is a stream), so appearances are supplied side-by-side in
//! [`FlattenInput::appearances`] and referenced by a name: an annotation's
//! `/AP /N` must be a name that indexes that map. The same mechanism models a
//! form widget: it is an `/Annots` entry with `/Subtype /Widget`, a `/T` field
//! name, and an `/AP /N` appearance reference. [`Widget::to_dict`] builds such
//! an entry for callers that prefer a typed widget model.
//!
//! # Placement
//!
//! For every annotated entry with a normal appearance the module appends a
//! balanced placement to the page content:
//!
//! ```text
//! q
//! a b c d e f cm
//! /XiFm<n> Do
//! Q
//! ```
//!
//! The matrix maps the appearance's `/BBox` (transformed by the appearance
//! `/Matrix` and the annotation `/Matrix`) into the annotation `/Rect`, then
//! rotates the result about the page-box centre by the page `/Rotate` value.
//! See the `placement_matrix` helper for the exact order of operations. The form
//! XObject name is installed in the page's `/Resources /XObject` dictionary and
//! points back at the appearance key, so a downstream writer can materialize
//! the referenced stream.
//!
//! # Contract
//!
//! - **Copy-on-write.** Input content and dictionaries are borrowed; results
//!   are returned in [`FlattenedDocument`]. Source bytes are never mutated.
//! - **Deterministic.** Pages and annotations are visited in ascending / array
//!   order, resource names are positional (`XiFm0`, `XiFm1`, …), and floats are
//!   formatted without negative zero or exponent notation. Equal input yields
//!   byte-identical output.
//! - **Conservative.** Entries whose appearance is missing are left in
//!   `/Annots` (and reported) unless they are widgets and
//!   [`FlattenOptions::keep_widgets_without_appearance`] is `false`. Links are
//!   preserved by default. Every other dictionary key is retained verbatim.
//! - **Cancellable.** `cancel` is checked for every page and every annotation.
//! - No unsafe code, no panics in non-test code, and no new dependencies.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use crate::geometry::Rect;
use crate::pdf::{Dict, Value};
use crate::redaction::Matrix;

/// A failure while flattening annotations or widgets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FlattenError {
    /// The caller's cancellation closure returned `true`.
    Cancelled,
    /// The page subset named a page that is not present in the input.
    UnknownPage {
        /// The offending 1-based page number.
        page: u32,
    },
    /// A page box declared a rotation other than 0, 90, 180, or 270 degrees.
    InvalidRotation {
        /// The offending 1-based page number.
        page: u32,
        /// The rejected rotation.
        degrees: u16,
    },
}

impl fmt::Display for FlattenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cancelled => write!(f, "flattening was cancelled"),
            Self::UnknownPage { page } => {
                write!(f, "page subset references unknown page {page}")
            }
            Self::InvalidRotation { page, degrees } => write!(
                f,
                "page {page} has invalid rotation {degrees}; expected 0, 90, 180, or 270"
            ),
        }
    }
}

impl std::error::Error for FlattenError {}

/// The normal appearance stream of an annotation or widget.
///
/// Appearance bytes are kept in the side table
/// [`FlattenInput::appearances`]; flattening emits a `Do` reference to a form
/// XObject resource that a downstream writer resolves back to these bytes.
#[derive(Debug, Clone, PartialEq)]
pub struct Appearance {
    /// Decoded content-stream bytes of the appearance form XObject.
    pub content: Vec<u8>,
    /// The appearance stream's `/Matrix` (identity when omitted).
    pub matrix: Matrix,
    /// The appearance stream's `/BBox`, defining its source coordinate space.
    pub bbox: Rect,
}

impl Appearance {
    /// Build an appearance from content bytes, a matrix, and a bounding box.
    pub fn new(content: Vec<u8>, matrix: Matrix, bbox: Rect) -> Self {
        Self {
            content,
            matrix,
            bbox,
        }
    }
}

/// The visible page box and rotation used when placing an appearance.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PageBox {
    /// The crop box (or media box) rectangle, in default user space.
    pub rect: Rect,
    /// The page's `/Rotate` value: 0, 90, 180, or 270 degrees.
    pub rotation_degrees: u16,
}

impl PageBox {
    /// Build a page box, validating that the rotation is a right angle.
    ///
    /// # Errors
    ///
    /// Returns [`FlattenError::InvalidRotation`] for any other rotation.
    pub fn new(rect: Rect, rotation_degrees: u16) -> Result<Self, FlattenError> {
        if !matches!(rotation_degrees, 0 | 90 | 180 | 270) {
            return Err(FlattenError::InvalidRotation {
                page: 0,
                degrees: rotation_degrees,
            });
        }
        Ok(Self {
            rect,
            rotation_degrees,
        })
    }
}

/// A typed form-field widget model.
///
/// This is the simple widget model promised by the task: a field name, a
/// rectangle, and a normal appearance. [`Widget::to_dict`] converts it into the
/// `/Annots` dictionary entry that [`flatten`] consumes, storing the appearance
/// under the supplied key in [`FlattenInput::appearances`].
#[derive(Debug, Clone, PartialEq)]
pub struct Widget {
    /// The widget's field name (written as `/T`).
    pub field_name: String,
    /// The widget's annotation rectangle in default user space.
    pub rect: Rect,
    /// The widget's optional annotation `/Matrix`.
    pub matrix: Matrix,
    /// Key of the widget's normal appearance in [`FlattenInput::appearances`].
    pub appearance_key: String,
}

impl Widget {
    /// Build a widget with an identity annotation matrix.
    pub fn new(
        field_name: impl Into<String>,
        rect: Rect,
        appearance_key: impl Into<String>,
    ) -> Self {
        Self {
            field_name: field_name.into(),
            rect,
            matrix: Matrix::IDENTITY,
            appearance_key: appearance_key.into(),
        }
    }

    /// Build the `/Annots` dictionary entry for this widget.
    ///
    /// The `/AP /N` value is a name equal to [`Self::appearance_key`], which the
    /// flattener resolves against [`FlattenInput::appearances`].
    pub fn to_dict(&self) -> Dict {
        let mut entry = Dict::new();
        entry.insert("Type".to_owned(), Value::Name("Annot".to_owned()));
        entry.insert("Subtype".to_owned(), Value::Name("Widget".to_owned()));
        entry.insert("T".to_owned(), Value::Text(self.field_name.clone()));
        entry.insert("Rect".to_owned(), rect_values(self.rect));
        entry.insert("Matrix".to_owned(), matrix_values(self.matrix));
        let mut appearance = Dict::new();
        appearance.insert("N".to_owned(), Value::Name(self.appearance_key.clone()));
        entry.insert("AP".to_owned(), Value::Dictionary(appearance));
        entry
    }
}

/// Everything [`flatten`] needs: pages, their dictionaries and boxes, the
/// appearance side table, and an optional document-level AcroForm dictionary.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FlattenInput {
    /// Content-stream bytes keyed by 1-based page number.
    pub pages: BTreeMap<u32, Vec<u8>>,
    /// Page dictionaries keyed by 1-based page number; `/Annots` is read here.
    pub dictionaries: BTreeMap<u32, Dict>,
    /// Page boxes keyed by 1-based page number; missing boxes default to a
    /// US-Letter rectangle with zero rotation.
    pub boxes: BTreeMap<u32, PageBox>,
    /// Normal appearances keyed by the name used in an annotation's `/AP /N`.
    pub appearances: BTreeMap<String, Appearance>,
    /// The document `/AcroForm` dictionary, removed when a widget is flattened.
    pub acroform: Option<Dict>,
}

/// Options controlling flattening.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlattenOptions {
    /// 1-based pages to flatten; empty means every page in the input.
    pub pages: BTreeSet<u32>,
    /// Preserve link annotations in `/Annots` instead of flattening them.
    pub keep_links: bool,
    /// Preserve widget annotations whose normal appearance is missing.
    ///
    /// When `false` (the default) such widgets are removed and reported, but
    /// the AcroForm is still only removed when at least one widget was actually
    /// flattened.
    pub keep_widgets_without_appearance: bool,
    /// Build the full report without modifying content or dictionaries.
    pub dry_run: bool,
}

impl Default for FlattenOptions {
    fn default() -> Self {
        Self {
            pages: BTreeSet::new(),
            keep_links: true,
            keep_widgets_without_appearance: false,
            dry_run: false,
        }
    }
}

/// Why an annotation was not flattened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    /// The annotation had no resolvable normal appearance.
    MissingAppearance,
    /// The annotation had no valid four-number `/Rect`.
    InvalidRect,
}

impl fmt::Display for SkipReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingAppearance => write!(f, "missing normal appearance"),
            Self::InvalidRect => write!(f, "missing or invalid rectangle"),
        }
    }
}

/// One recorded action during flattening.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FlattenEvent {
    /// An annotation or widget was flattened into the content stream.
    Flattened {
        /// 1-based page containing the object.
        page: u32,
        /// The object's `/Subtype`, or `Unknown` when absent.
        subtype: String,
        /// The widget's `/T` field name, when the object was a widget.
        field_name: Option<String>,
        /// The form XObject resource name used by the emitted `Do`.
        resource_name: String,
    },
    /// An annotation was kept because it had no usable appearance.
    Skipped {
        /// 1-based page containing the object.
        page: u32,
        /// The object's `/Subtype`, or `Unknown` when absent.
        subtype: String,
        /// Why the object was skipped.
        reason: SkipReason,
    },
    /// A widget with no appearance was removed per the options.
    RemovedWidget {
        /// 1-based page containing the widget.
        page: u32,
        /// The widget's `/T` field name, when present.
        field_name: Option<String>,
        /// Why the widget was not flattened.
        reason: SkipReason,
    },
    /// A link annotation was preserved.
    KeptLink {
        /// 1-based page containing the link.
        page: u32,
        /// The link's `/Subtype`.
        subtype: String,
    },
    /// The document `/AcroForm` dictionary was removed.
    AcroFormRemoved,
}

/// Typed report of everything flattening did (or would do in a dry run).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FlattenReport {
    events: Vec<FlattenEvent>,
}

impl FlattenReport {
    /// Recorded events in application order.
    pub fn events(&self) -> &[FlattenEvent] {
        &self.events
    }

    /// Number of annotations and widgets actually placed into content.
    pub fn flattened_count(&self) -> usize {
        self.events
            .iter()
            .filter(|event| matches!(event, FlattenEvent::Flattened { .. }))
            .count()
    }

    /// Number of widgets that were flattened.
    pub fn widget_count(&self) -> usize {
        self.events
            .iter()
            .filter(|event| {
                matches!(
                    event,
                    FlattenEvent::Flattened {
                        field_name: Some(_),
                        ..
                    }
                )
            })
            .count()
    }

    /// Number of annotations skipped or widgets removed for missing data.
    pub fn skipped_count(&self) -> usize {
        self.events
            .iter()
            .filter(|event| {
                matches!(
                    event,
                    FlattenEvent::Skipped { .. } | FlattenEvent::RemovedWidget { .. }
                )
            })
            .count()
    }

    /// Whether the document `/AcroForm` was removed.
    pub fn acroform_removed(&self) -> bool {
        self.events
            .iter()
            .any(|event| matches!(event, FlattenEvent::AcroFormRemoved))
    }

    /// Distinct 1-based pages touched by an event, in ascending order.
    pub fn pages_touched(&self) -> Vec<u32> {
        let mut pages = BTreeSet::new();
        for event in &self.events {
            match event {
                FlattenEvent::Flattened { page, .. }
                | FlattenEvent::Skipped { page, .. }
                | FlattenEvent::RemovedWidget { page, .. }
                | FlattenEvent::KeptLink { page, .. } => {
                    pages.insert(*page);
                }
                FlattenEvent::AcroFormRemoved => {}
            }
        }
        pages.into_iter().collect()
    }
}

/// The flattened content and dictionary of one page.
#[derive(Debug, Clone, PartialEq)]
pub struct FlattenedPage {
    /// The (possibly extended) content-stream bytes.
    pub content: Vec<u8>,
    /// The page dictionary with flattened entries removed.
    pub dictionary: Dict,
}

/// Copy-on-write result of [`flatten`].
#[derive(Debug, Clone, PartialEq)]
pub struct FlattenedDocument {
    pages: BTreeMap<u32, FlattenedPage>,
    acroform: Option<Dict>,
    report: FlattenReport,
}

impl FlattenedDocument {
    /// Flattened content of a page, if the page was in the input.
    pub fn page_content(&self, page: u32) -> Option<&[u8]> {
        self.pages.get(&page).map(|p| p.content.as_slice())
    }

    /// Flattened dictionary of a page.
    pub fn page_dictionary(&self, page: u32) -> Option<&Dict> {
        self.pages.get(&page).map(|p| &p.dictionary)
    }

    /// All flattened pages, keyed by 1-based page number.
    pub fn pages(&self) -> &BTreeMap<u32, FlattenedPage> {
        &self.pages
    }

    /// The remaining document `/AcroForm` dictionary, if it was retained.
    pub fn acroform(&self) -> Option<&Dict> {
        self.acroform.as_ref()
    }

    /// The typed action report.
    pub fn report(&self) -> &FlattenReport {
        &self.report
    }
}

/// Flatten annotations and widgets into page content.
///
/// `input` and `options` are borrowed; all changes are returned in a new
/// [`FlattenedDocument`]. When [`FlattenOptions::dry_run`] is set the produced
/// content and dictionaries are byte-for-byte copies of the input while the
/// report still describes the work that would be performed.
///
/// # Errors
///
/// Returns [`FlattenError::Cancelled`] when `cancel` returns `true`,
/// [`FlattenError::UnknownPage`] when the page subset names an absent page, or
/// [`FlattenError::InvalidRotation`] for a malformed page box.
pub fn flatten(
    input: &FlattenInput,
    options: &FlattenOptions,
    cancel: &impl Fn() -> bool,
) -> Result<FlattenedDocument, FlattenError> {
    for &page in &options.pages {
        if cancel() {
            return Err(FlattenError::Cancelled);
        }
        if !input.pages.contains_key(&page) {
            return Err(FlattenError::UnknownPage { page });
        }
    }

    let mut pages = BTreeMap::new();
    let mut events = Vec::new();
    let mut total_widget_flattened = 0usize;

    for (&page, content) in &input.pages {
        if cancel() {
            return Err(FlattenError::Cancelled);
        }
        let targeted = options.pages.is_empty() || options.pages.contains(&page);
        let original_dict = input.dictionaries.get(&page).cloned().unwrap_or_default();
        if !targeted {
            pages.insert(
                page,
                FlattenedPage {
                    content: content.clone(),
                    dictionary: original_dict,
                },
            );
            continue;
        }

        let page_box = input.boxes.get(&page);
        let rotation = page_box.map_or(0, |box_| box_.rotation_degrees);
        if !matches!(rotation, 0 | 90 | 180 | 270) {
            return Err(FlattenError::InvalidRotation {
                page,
                degrees: rotation,
            });
        }
        let box_rect = page_box.map_or_else(default_box_rect, |box_| box_.rect);

        let mut new_content = content.clone();
        let mut dict = original_dict;
        let mut flattened_here = 0usize;

        let annots = dict
            .get("Annots")
            .and_then(Value::as_array)
            .map(<[Value]>::to_vec)
            .unwrap_or_default();
        let mut retained: Vec<Value> = Vec::new();

        for annot in &annots {
            if cancel() {
                return Err(FlattenError::Cancelled);
            }
            let Some(entry) = annot.as_dict() else {
                retained.push(annot.clone());
                continue;
            };
            let subtype = entry
                .get("Subtype")
                .and_then(Value::as_name)
                .unwrap_or("Unknown")
                .to_owned();
            let is_link = subtype == "Link";
            let is_widget = subtype == "Widget";

            if is_link && options.keep_links {
                events.push(FlattenEvent::KeptLink {
                    page,
                    subtype: subtype.clone(),
                });
                retained.push(annot.clone());
                continue;
            }

            let rect = rect_from_value(entry.get("Rect"));
            let appearance_key = appearance_key(entry);
            let appearance = appearance_key
                .as_deref()
                .and_then(|key| input.appearances.get(key));

            match (appearance, rect) {
                (Some(appearance), Some(rect)) => {
                    let annot_matrix = matrix_from_value(entry.get("Matrix"));
                    let placement =
                        placement_matrix(rect, annot_matrix, appearance, box_rect, rotation);
                    let resource_name = format!("XiFm{flattened_here}");
                    if !options.dry_run {
                        new_content.extend_from_slice(
                            placement_bytes(&placement, &resource_name).as_bytes(),
                        );
                        install_appearance_resource(
                            &mut dict,
                            &resource_name,
                            appearance_key.as_deref().unwrap_or(""),
                        );
                    }
                    flattened_here += 1;
                    let field_name = if is_widget {
                        entry.get("T").and_then(Value::as_text).map(str::to_owned)
                    } else {
                        None
                    };
                    if is_widget {
                        total_widget_flattened += 1;
                    }
                    events.push(FlattenEvent::Flattened {
                        page,
                        subtype: subtype.clone(),
                        field_name,
                        resource_name,
                    });
                }
                (appearance, _rect) => {
                    let reason = if appearance.is_none() {
                        SkipReason::MissingAppearance
                    } else {
                        SkipReason::InvalidRect
                    };
                    if is_widget && !options.keep_widgets_without_appearance {
                        let field_name = entry.get("T").and_then(Value::as_text).map(str::to_owned);
                        events.push(FlattenEvent::RemovedWidget {
                            page,
                            field_name,
                            reason,
                        });
                    } else {
                        events.push(FlattenEvent::Skipped {
                            page,
                            subtype: subtype.clone(),
                            reason,
                        });
                        retained.push(annot.clone());
                    }
                }
            }
        }

        if !options.dry_run {
            if retained.is_empty() {
                dict.remove("Annots");
            } else {
                dict.insert("Annots".to_owned(), Value::Array(retained));
            }
        }

        pages.insert(
            page,
            FlattenedPage {
                content: new_content,
                dictionary: dict,
            },
        );
    }

    let acroform = if !options.dry_run && total_widget_flattened > 0 {
        events.push(FlattenEvent::AcroFormRemoved);
        None
    } else {
        input.acroform.clone()
    };

    Ok(FlattenedDocument {
        pages,
        acroform,
        report: FlattenReport { events },
    })
}

/// Compute the final `cm` matrix for one appearance placement.
///
/// The order is: appearance `/Matrix` first, then the annotation `/Matrix`,
/// then an align matrix mapping the transformed `/BBox` into the annotation
/// `/Rect`, and finally a rotation about the centre of the page box by the
/// page's `/Rotate` value. The resulting matrix is applied to appearance source
/// coordinates by the emitted `Do` operator.
fn placement_matrix(
    rect: Rect,
    annot_matrix: Matrix,
    appearance: &Appearance,
    box_rect: Rect,
    rotation: u16,
) -> Matrix {
    let source_to_annot = annot_matrix.mul(appearance.matrix);
    let corners = [
        source_to_annot.transform_point(appearance.bbox.x0() as f64, appearance.bbox.y0() as f64),
        source_to_annot.transform_point(appearance.bbox.x1() as f64, appearance.bbox.y0() as f64),
        source_to_annot.transform_point(appearance.bbox.x0() as f64, appearance.bbox.y1() as f64),
        source_to_annot.transform_point(appearance.bbox.x1() as f64, appearance.bbox.y1() as f64),
    ];
    let (mut bx0, mut by0) = (f64::INFINITY, f64::INFINITY);
    let (mut bx1, mut by1) = (f64::NEG_INFINITY, f64::NEG_INFINITY);
    for (x, y) in corners {
        bx0 = bx0.min(x);
        by0 = by0.min(y);
        bx1 = bx1.max(x);
        by1 = by1.max(y);
    }
    if ![bx0, by0, bx1, by1].iter().all(|value| value.is_finite()) {
        bx0 = appearance.bbox.x0() as f64;
        by0 = appearance.bbox.y0() as f64;
        bx1 = appearance.bbox.x1() as f64;
        by1 = appearance.bbox.y1() as f64;
    }

    let source_width = bx1 - bx0;
    let source_height = by1 - by0;
    let scale_x = if source_width > 0.0 && source_width.is_finite() {
        rect.width() as f64 / source_width
    } else {
        1.0
    };
    let scale_y = if source_height > 0.0 && source_height.is_finite() {
        rect.height() as f64 / source_height
    } else {
        1.0
    };
    let align = Matrix::new(
        scale_x,
        0.0,
        0.0,
        scale_y,
        rect.x0() as f64 - bx0 * scale_x,
        rect.y0() as f64 - by0 * scale_y,
    );
    rotation_about_center(box_rect, rotation)
        .mul(align)
        .mul(source_to_annot)
}

/// Rotation by a right angle about the centre of `box`.
fn rotation_about_center(box_: Rect, degrees: u16) -> Matrix {
    let cx = f64::from(box_.x0() + box_.x1()) / 2.0;
    let cy = f64::from(box_.y0() + box_.y1()) / 2.0;
    let (cos, sin) = match degrees {
        90 => (0.0, 1.0),
        180 => (-1.0, 0.0),
        270 => (0.0, -1.0),
        _ => (1.0, 0.0),
    };
    let e = cx - (cos * cx - sin * cy);
    let f = cy - (sin * cx + cos * cy);
    Matrix::new(cos, sin, -sin, cos, e, f)
}

/// The balancing `q … cm … Do … Q` placement string.
fn placement_bytes(matrix: &Matrix, resource_name: &str) -> String {
    format!("\nq\n{} cm\n/{} Do\nQ\n", matrix.to_pdf(), resource_name)
}

/// A sensible default page box when a page supplies none.
fn default_box_rect() -> Rect {
    Rect::saturating(0.0, 0.0, 612.0, 792.0)
}

/// Parse a four-number rectangle, returning `None` for missing/invalid data.
fn rect_from_value(value: Option<&Value>) -> Option<Rect> {
    let array = value?.as_array()?;
    if array.len() < 4 {
        return None;
    }
    let mut numbers = [0.0f64; 4];
    for (slot, value) in numbers.iter_mut().zip(array.iter().take(4)) {
        *slot = value.as_number()?;
    }
    if !numbers.iter().all(|value| value.is_finite()) {
        return None;
    }
    Some(Rect::saturating(
        numbers[0] as f32,
        numbers[1] as f32,
        numbers[2] as f32,
        numbers[3] as f32,
    ))
}

/// Parse a six-number matrix, defaulting to identity for missing/invalid data.
fn matrix_from_value(value: Option<&Value>) -> Matrix {
    let Some(array) = value.and_then(Value::as_array) else {
        return Matrix::IDENTITY;
    };
    if array.len() < 6 {
        return Matrix::IDENTITY;
    }
    let mut numbers = [0.0f64; 6];
    for (slot, value) in numbers.iter_mut().zip(array.iter().take(6)) {
        match value.as_number() {
            Some(number) if number.is_finite() => *slot = number,
            _ => return Matrix::IDENTITY,
        }
    }
    Matrix::new(
        numbers[0], numbers[1], numbers[2], numbers[3], numbers[4], numbers[5],
    )
}

/// Read the appearance key stored in an annotation's `/AP /N` name.
fn appearance_key(entry: &Dict) -> Option<String> {
    entry
        .get("AP")?
        .as_dict()?
        .get("N")?
        .as_name()
        .map(str::to_owned)
}

/// Convert a rectangle to its four PDF array values.
fn rect_values(rect: Rect) -> Value {
    Value::Array(vec![
        Value::Real(f64::from(rect.x0())),
        Value::Real(f64::from(rect.y0())),
        Value::Real(f64::from(rect.x1())),
        Value::Real(f64::from(rect.y1())),
    ])
}

/// Convert a matrix to its six PDF array values.
fn matrix_values(matrix: Matrix) -> Value {
    Value::Array(matrix.coefficients().into_iter().map(Value::Real).collect())
}

/// Install a form XObject resource pointing back at the appearance key.
///
/// Unexpected non-dictionary `/Resources` or `/XObject` values are preserved
/// under side keys instead of being silently dropped.
fn install_appearance_resource(dict: &mut Dict, resource_name: &str, appearance_key: &str) {
    let mut resources = match dict.remove("Resources") {
        Some(Value::Dictionary(existing)) => existing,
        Some(other) => {
            dict.insert("XI_OriginalResources".to_owned(), other);
            Dict::new()
        }
        None => Dict::new(),
    };
    let mut xobjects = match resources.remove("XObject") {
        Some(Value::Dictionary(existing)) => existing,
        Some(other) => {
            resources.insert("XI_OriginalXObject".to_owned(), other);
            Dict::new()
        }
        None => Dict::new(),
    };
    let mut reference = Dict::new();
    reference.insert("Type".to_owned(), Value::Name("XObject".to_owned()));
    reference.insert("Subtype".to_owned(), Value::Name("Form".to_owned()));
    reference.insert(
        "XI_AppearanceKey".to_owned(),
        Value::Name(appearance_key.to_owned()),
    );
    xobjects.insert(resource_name.to_owned(), Value::Dictionary(reference));
    resources.insert("XObject".to_owned(), Value::Dictionary(xobjects));
    dict.insert("Resources".to_owned(), Value::Dictionary(resources));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_cancel() -> bool {
        false
    }

    fn rect(x0: f32, y0: f32, x1: f32, y1: f32) -> Rect {
        Rect::new(x0, y0, x1, y1).expect("test rect must be valid")
    }

    fn annotate(subtype: &str, rect: Rect, appearance_key: Option<&str>) -> Value {
        let mut entry = Dict::new();
        entry.insert("Subtype".to_owned(), Value::Name(subtype.to_owned()));
        entry.insert("Rect".to_owned(), rect_values(rect));
        if let Some(key) = appearance_key {
            let mut appearance = Dict::new();
            appearance.insert("N".to_owned(), Value::Name(key.to_owned()));
            entry.insert("AP".to_owned(), Value::Dictionary(appearance));
        }
        Value::Dictionary(entry)
    }

    fn page_input(content: &[u8], annots: Vec<Value>) -> FlattenInput {
        let mut dict = Dict::new();
        if !annots.is_empty() {
            dict.insert("Annots".to_owned(), Value::Array(annots));
        }
        let mut input = FlattenInput::default();
        input.pages.insert(1, content.to_vec());
        input.dictionaries.insert(1, dict);
        input
    }

    fn add_appearance(input: &mut FlattenInput, key: &str, rect: Rect, matrix: Matrix) {
        input.appearances.insert(
            key.to_owned(),
            Appearance::new(b"appearance bytes".to_vec(), matrix, rect),
        );
    }

    #[test]
    fn text_annotation_flatten_appends_placement_and_removes_entry() {
        let rect = rect(10.0, 20.0, 110.0, 70.0);
        let mut input = page_input(b"original\n", vec![annotate("Text", rect, Some("ap1"))]);
        add_appearance(&mut input, "ap1", rect, Matrix::IDENTITY);
        let mut acroform = Dict::new();
        acroform.insert("Fields".to_owned(), Value::Array(Vec::new()));
        input.acroform = Some(acroform);

        let result = flatten(&input, &FlattenOptions::default(), &no_cancel).expect("flatten");
        let content = String::from_utf8(result.page_content(1).unwrap().to_vec()).unwrap();
        assert!(content.starts_with("original\n"));
        assert!(content.contains("cm\n/XiFm0 Do\nQ"));
        let dict = result.page_dictionary(1).unwrap();
        assert!(!dict.contains_key("Annots"));
        let xobjects = dict
            .get("Resources")
            .and_then(Value::as_dict)
            .and_then(|r| r.get("XObject"))
            .and_then(Value::as_dict)
            .unwrap();
        assert!(xobjects.contains_key("XiFm0"));
        assert_eq!(result.report().flattened_count(), 1);
        assert!(!result.report().acroform_removed());
    }

    #[test]
    fn multiple_annotations_are_placed_in_array_order() {
        let a = rect(0.0, 0.0, 10.0, 10.0);
        let b = rect(20.0, 20.0, 30.0, 30.0);
        let mut input = page_input(
            b"",
            vec![
                annotate("Square", a, Some("apA")),
                annotate("Circle", b, Some("apB")),
            ],
        );
        add_appearance(&mut input, "apA", a, Matrix::IDENTITY);
        add_appearance(&mut input, "apB", b, Matrix::IDENTITY);

        let result = flatten(&input, &FlattenOptions::default(), &no_cancel).expect("flatten");
        let content = String::from_utf8(result.page_content(1).unwrap().to_vec()).unwrap();
        let first = content.find("/XiFm0 Do").expect("first placement");
        let second = content.find("/XiFm1 Do").expect("second placement");
        assert!(first < second);
        match result.report().events() {
            [
                FlattenEvent::Flattened {
                    resource_name: first,
                    ..
                },
                FlattenEvent::Flattened {
                    resource_name: second,
                    ..
                },
            ] => {
                assert_eq!(first, "XiFm0");
                assert_eq!(second, "XiFm1");
            }
            other => panic!("unexpected events: {other:?}"),
        }
    }

    #[test]
    fn missing_appearance_is_skipped_and_entry_preserved() {
        let rect = rect(0.0, 0.0, 10.0, 10.0);
        let input = page_input(b"base", vec![annotate("Text", rect, None)]);
        let result = flatten(&input, &FlattenOptions::default(), &no_cancel).expect("flatten");
        assert_eq!(result.page_content(1).unwrap(), b"base");
        let annots = result.page_dictionary(1).unwrap().get("Annots").unwrap();
        assert_eq!(annots.as_array().map(<[_]>::len), Some(1));
        assert_eq!(result.report().skipped_count(), 1);
        assert!(result.report().events().iter().any(|event| matches!(
            event,
            FlattenEvent::Skipped {
                reason: SkipReason::MissingAppearance,
                ..
            }
        )));
    }

    #[test]
    fn invalid_rect_is_skipped() {
        let mut entry = Dict::new();
        entry.insert("Subtype".to_owned(), Value::Name("Text".to_owned()));
        entry.insert(
            "Rect".to_owned(),
            Value::Array(vec![Value::Real(0.0), Value::Real(0.0)]),
        );
        let mut appearance = Dict::new();
        appearance.insert("N".to_owned(), Value::Name("ap".to_owned()));
        entry.insert("AP".to_owned(), Value::Dictionary(appearance));
        let mut input = page_input(b"", vec![Value::Dictionary(entry)]);
        add_appearance(&mut input, "ap", rect(0.0, 0.0, 1.0, 1.0), Matrix::IDENTITY);
        let result = flatten(&input, &FlattenOptions::default(), &no_cancel).expect("flatten");
        assert!(result.report().events().iter().any(|event| matches!(
            event,
            FlattenEvent::Skipped {
                reason: SkipReason::InvalidRect,
                ..
            }
        )));
    }

    #[test]
    fn rotation_and_matrix_are_folded_into_placement() {
        let annot_rect = rect(0.0, 0.0, 100.0, 100.0);
        let box_rect = rect(0.0, 0.0, 612.0, 792.0);
        let mut input = page_input(b"", vec![annotate("Text", annot_rect, Some("ap"))]);
        input
            .boxes
            .insert(1, PageBox::new(box_rect, 90).expect("valid page box"));
        // Appearance bbox matches the annotation rect and matrices are identity,
        // so the placement reduces to a 90-degree rotation about (306, 396).
        add_appearance(&mut input, "ap", annot_rect, Matrix::IDENTITY);
        let result = flatten(&input, &FlattenOptions::default(), &no_cancel).expect("flatten");
        let content = String::from_utf8(result.page_content(1).unwrap().to_vec()).unwrap();
        assert!(
            content.contains("0 1 -1 0 702 90 cm"),
            "content was: {content}"
        );

        let placement = placement_matrix(
            annot_rect,
            Matrix::IDENTITY,
            input.appearances.get("ap").unwrap(),
            box_rect,
            90,
        );
        assert_eq!(placement.coefficients(), [0.0, 1.0, -1.0, 0.0, 702.0, 90.0]);
    }

    #[test]
    fn link_preservation_option_controls_retention() {
        let rect = rect(0.0, 0.0, 10.0, 10.0);
        let mut keep = page_input(b"", vec![annotate("Link", rect, Some("ap"))]);
        add_appearance(&mut keep, "ap", rect, Matrix::IDENTITY);
        let options = FlattenOptions {
            keep_links: true,
            ..FlattenOptions::default()
        };
        let kept = flatten(&keep, &options, &no_cancel).expect("flatten");
        assert!(kept.page_dictionary(1).unwrap().contains_key("Annots"));
        assert_eq!(kept.report().flattened_count(), 0);
        assert!(
            kept.report()
                .events()
                .iter()
                .any(|e| matches!(e, FlattenEvent::KeptLink { .. }))
        );

        let options = FlattenOptions {
            keep_links: false,
            ..FlattenOptions::default()
        };
        let flattened = flatten(&keep, &options, &no_cancel).expect("flatten");
        assert_eq!(flattened.report().flattened_count(), 1);
    }

    #[test]
    fn widget_is_flattened_and_removed_from_annots() {
        let rect = rect(0.0, 0.0, 50.0, 20.0);
        let widget = Widget::new("name", rect, "w1");
        let mut input = page_input(b"", vec![Value::Dictionary(widget.to_dict())]);
        add_appearance(&mut input, "w1", rect, Matrix::IDENTITY);
        let result = flatten(&input, &FlattenOptions::default(), &no_cancel).expect("flatten");
        assert!(!result.page_dictionary(1).unwrap().contains_key("Annots"));
        assert_eq!(result.report().widget_count(), 1);
        assert!(matches!(
            result.report().events().first(),
            Some(FlattenEvent::Flattened { field_name: Some(name), .. }) if name == "name"
        ));
    }

    #[test]
    fn acroform_removal_is_gated_on_a_flattened_widget() {
        let rect = rect(0.0, 0.0, 10.0, 10.0);
        let mut acroform = Dict::new();
        acroform.insert("Fields".to_owned(), Value::Array(Vec::new()));

        let mut with_widget = page_input(b"", vec![annotate("Text", rect, Some("ap"))]);
        add_appearance(&mut with_widget, "ap", rect, Matrix::IDENTITY);
        with_widget.acroform = Some(acroform.clone());
        let kept = flatten(&with_widget, &FlattenOptions::default(), &no_cancel).expect("flatten");
        assert!(kept.acroform().is_some());
        assert!(!kept.report().acroform_removed());

        let widget = Widget::new("field", rect, "w");
        let mut with_widget = page_input(b"", vec![Value::Dictionary(widget.to_dict())]);
        add_appearance(&mut with_widget, "w", rect, Matrix::IDENTITY);
        with_widget.acroform = Some(acroform);
        let removed =
            flatten(&with_widget, &FlattenOptions::default(), &no_cancel).expect("flatten");
        assert!(removed.acroform().is_none());
        assert!(removed.report().acroform_removed());
    }

    #[test]
    fn keep_widgets_without_appearance_option() {
        let rect = rect(0.0, 0.0, 10.0, 10.0);
        let widget = Widget::new("ghost", rect, "missing");
        let input = page_input(b"", vec![Value::Dictionary(widget.to_dict())]);

        let removed = flatten(&input, &FlattenOptions::default(), &no_cancel).expect("flatten");
        assert!(!removed.page_dictionary(1).unwrap().contains_key("Annots"));
        assert!(
            removed
                .report()
                .events()
                .iter()
                .any(|e| matches!(e, FlattenEvent::RemovedWidget { .. }))
        );

        let options = FlattenOptions {
            keep_widgets_without_appearance: true,
            ..FlattenOptions::default()
        };
        let kept = flatten(&input, &options, &no_cancel).expect("flatten");
        assert!(kept.page_dictionary(1).unwrap().contains_key("Annots"));
        assert!(
            kept.report()
                .events()
                .iter()
                .any(|e| matches!(e, FlattenEvent::Skipped { .. }))
        );
    }

    #[test]
    fn dry_run_reports_without_changing_bytes() {
        let rect = rect(0.0, 0.0, 10.0, 10.0);
        let widget = Widget::new("field", rect, "w");
        let mut input = page_input(b"original", vec![Value::Dictionary(widget.to_dict())]);
        add_appearance(&mut input, "w", rect, Matrix::IDENTITY);
        let mut acroform = Dict::new();
        acroform.insert("Fields".to_owned(), Value::Array(Vec::new()));
        input.acroform = Some(acroform);

        let options = FlattenOptions {
            dry_run: true,
            ..FlattenOptions::default()
        };
        let result = flatten(&input, &options, &no_cancel).expect("flatten");
        assert_eq!(result.page_content(1).unwrap(), b"original");
        assert_eq!(
            result.page_dictionary(1).unwrap(),
            input.dictionaries.get(&1).unwrap()
        );
        assert!(result.acroform().is_some());
        assert_eq!(result.report().flattened_count(), 1);
        assert_eq!(result.report().widget_count(), 1);
        assert!(!result.report().acroform_removed());
    }

    #[test]
    fn unchanged_pages_are_byte_stable() {
        let rect = rect(0.0, 0.0, 10.0, 10.0);
        let mut input = page_input(b"page one", vec![annotate("Text", rect, Some("ap"))]);
        add_appearance(&mut input, "ap", rect, Matrix::IDENTITY);
        input.pages.insert(2, b"page two unchanged".to_vec());
        input.dictionaries.insert(2, Dict::new());

        let options = FlattenOptions {
            pages: BTreeSet::from([1]),
            ..FlattenOptions::default()
        };
        let result = flatten(&input, &options, &no_cancel).expect("flatten");
        assert_eq!(result.page_content(2).unwrap(), b"page two unchanged");
        assert_eq!(result.page_dictionary(2).unwrap(), &Dict::new());
        assert!(result.report().pages_touched().contains(&1));
        assert!(!result.report().pages_touched().contains(&2));
    }

    #[test]
    fn output_is_deterministic_across_runs() {
        let rect = rect(1.25, 2.5, 40.75, 60.5);
        let mut input = page_input(
            b"seed",
            vec![
                annotate("Square", rect, Some("ap1")),
                annotate("Circle", rect, Some("ap2")),
            ],
        );
        add_appearance(
            &mut input,
            "ap1",
            rect,
            Matrix::new(1.0, 0.0, 0.0, 1.0, 3.0, 4.0),
        );
        add_appearance(&mut input, "ap2", rect, Matrix::IDENTITY);
        let options = FlattenOptions::default();

        let first = flatten(&input, &options, &no_cancel).expect("flatten");
        let second = flatten(&input, &options, &no_cancel).expect("flatten");
        assert_eq!(first, second);
        assert_eq!(
            first.page_content(1).unwrap(),
            second.page_content(1).unwrap()
        );
    }

    #[test]
    fn cancellation_is_honored() {
        let rect = rect(0.0, 0.0, 10.0, 10.0);
        let mut input = page_input(b"", vec![annotate("Text", rect, Some("ap"))]);
        add_appearance(&mut input, "ap", rect, Matrix::IDENTITY);
        assert_eq!(
            flatten(&input, &FlattenOptions::default(), &|| true).unwrap_err(),
            FlattenError::Cancelled
        );

        input.pages.insert(2, b"".to_vec());
        let options = FlattenOptions {
            pages: BTreeSet::from([9]),
            ..FlattenOptions::default()
        };
        assert_eq!(
            flatten(&input, &options, &no_cancel).unwrap_err(),
            FlattenError::UnknownPage { page: 9 }
        );
    }

    #[test]
    fn invalid_rotation_is_an_error() {
        let mut input = page_input(b"", Vec::new());
        let mut box_ = PageBox::new(rect(0.0, 0.0, 10.0, 10.0), 0).expect("valid");
        box_.rotation_degrees = 45;
        input.boxes.insert(1, box_);
        assert_eq!(
            flatten(&input, &FlattenOptions::default(), &no_cancel).unwrap_err(),
            FlattenError::InvalidRotation {
                page: 1,
                degrees: 45
            }
        );
        assert!(PageBox::new(rect(0.0, 0.0, 1.0, 1.0), 45).is_err());
    }

    #[test]
    fn appearance_matrix_scales_bbox_into_rect() {
        let annot_rect = rect(0.0, 0.0, 200.0, 100.0);
        let box_rect = rect(0.0, 0.0, 200.0, 100.0);
        let appearance = Appearance::new(Vec::new(), Matrix::IDENTITY, rect(0.0, 0.0, 100.0, 50.0));
        let placement = placement_matrix(annot_rect, Matrix::IDENTITY, &appearance, box_rect, 0);
        assert_eq!(placement.coefficients(), [2.0, 0.0, 0.0, 2.0, 0.0, 0.0]);
    }

    #[test]
    fn error_messages_are_descriptive() {
        assert!(!FlattenError::Cancelled.to_string().is_empty());
        assert!(!FlattenError::UnknownPage { page: 3 }.to_string().is_empty());
        assert!(
            !FlattenError::InvalidRotation {
                page: 1,
                degrees: 45
            }
            .to_string()
            .is_empty()
        );
        assert!(!SkipReason::MissingAppearance.to_string().is_empty());
        assert!(!SkipReason::InvalidRect.to_string().is_empty());
    }
}
