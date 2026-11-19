//! Page-level metadata resolved from a document's page tree.
//!
//! The types here are value objects: they are produced once when a document is
//! parsed and can be cloned cheaply by callers.

use crate::object::ObjectId;

/// Default page width in default user space units (US Letter).
const DEFAULT_WIDTH: f64 = 612.0;
/// Default page height in default user space units (US Letter).
const DEFAULT_HEIGHT: f64 = 792.0;

/// A rectangle in default user space.
///
/// Coordinates follow the PDF convention: the origin is the bottom-left
/// corner, `left`/`bottom` give the lower-left point and `right`/`top` the
/// upper-right point.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rectangle {
    /// The x-coordinate of the left edge.
    pub left: f64,
    /// The y-coordinate of the bottom edge.
    pub bottom: f64,
    /// The x-coordinate of the right edge.
    pub right: f64,
    /// The y-coordinate of the top edge.
    pub top: f64,
}

impl Rectangle {
    /// Construct a rectangle from its four edges.
    pub fn new(left: f64, bottom: f64, right: f64, top: f64) -> Self {
        Self {
            left,
            bottom,
            right,
            top,
        }
    }

    /// Return the absolute width of the rectangle.
    pub fn width(&self) -> f64 {
        (self.right - self.left).abs()
    }

    /// Return the absolute height of the rectangle.
    pub fn height(&self) -> f64 {
        (self.top - self.bottom).abs()
    }
}

impl Default for Rectangle {
    /// Return the US Letter fallback box `[0 0 612 792]`.
    fn default() -> Self {
        Self::new(0.0, 0.0, DEFAULT_WIDTH, DEFAULT_HEIGHT)
    }
}

/// Metadata for one leaf page of a document.
#[derive(Debug, Clone)]
pub struct Page {
    /// The zero-based position of this page in document order.
    pub index: usize,
    /// The indirect object identifier of this page's dictionary.
    pub id: ObjectId,
    /// The page's media box in default user space units.
    pub media_box: Rectangle,
    /// Indirect references to the page's content streams, in draw order.
    pub contents: Vec<ObjectId>,
    /// An optional inherited or direct reference to the resource dictionary.
    pub resources: Option<ObjectId>,
}

impl Page {
    /// Return the media box width in default user space units.
    pub fn width(&self) -> f64 {
        self.media_box.width()
    }

    /// Return the media box height in default user space units.
    pub fn height(&self) -> f64 {
        self.media_box.height()
    }
}
