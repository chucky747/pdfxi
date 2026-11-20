//! Image placement and edit model (EDIT-004).
//!
//! This module is a **model only**. It describes where caller-supplied images
//! sit on a page and produces a deterministic [`ImageEditPlan`] that a host
//! writer can serialize. It performs **no image decoding** and **never reads or
//! rewrites content streams**; intrinsic image dimensions are supplied by the
//! caller and are treated as opaque numbers. No source bytes are mutated here.
//!
//! Every geometry value is validated: rectangles come from the finite,
//! normalized [`Rect`] primitive, opacity must lie in `0.0..=1.0`, rotation must
//! be one of 0/90/180/270 degrees, and placements must stay within the owning
//! page's bounds. Determinism is guaranteed by iterating images and operations
//! in order and sorting the emitted steps by `(page, id)`.

use core::fmt;
use std::collections::BTreeMap;

use crate::geometry::{GeometryError, Rect};

/// Stable identifier used to refer to a placed image across operations.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ImageId(u64);

impl ImageId {
    /// Creates an image identifier from a `u64`.
    pub fn new(value: u64) -> Self {
        Self(value)
    }

    /// Returns the underlying `u64`.
    pub fn get(self) -> u64 {
        self.0
    }
}

/// How an image is fitted into a target rectangle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlacementMode {
    /// Uniform scale so the whole image fits inside the rectangle (letterbox),
    /// centered on the rectangle.
    Fit,
    /// Uniform scale so the image covers the entire rectangle (crop), centered.
    Fill,
    /// Independent horizontal and vertical scale to exactly fill the
    /// rectangle; this intentionally does not preserve aspect ratio.
    Stretch,
}

/// Finite, positive page dimensions used for bounds validation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PageBounds {
    /// Page width in user-space units.
    pub width: f32,
    /// Page height in user-space units.
    pub height: f32,
}

impl PageBounds {
    /// Creates page bounds, rejecting non-finite or non-positive dimensions.
    pub fn new(width: f32, height: f32) -> Result<Self, ImageEditError> {
        if !width.is_finite() || !height.is_finite() {
            return Err(ImageEditError::NonFiniteField("page dimension"));
        }
        if width <= 0.0 || height <= 0.0 {
            return Err(ImageEditError::InvalidNaturalSize { width, height });
        }
        Ok(Self { width, height })
    }
}

/// An image placed on a page, with intrinsic size supplied by the caller.
#[derive(Clone, Debug, PartialEq)]
pub struct PlacedImage {
    /// Stable identifier for this placement.
    pub id: ImageId,
    /// Zero-based page index owning this image.
    pub page: usize,
    /// Placement rectangle in page space.
    pub rect: Rect,
    /// Rotation in degrees; one of 0, 90, 180, or 270.
    pub rotation_degrees: u16,
    /// Opacity in the inclusive range `0.0..=1.0`.
    pub opacity: f32,
    /// Intrinsic image width (never decoded here).
    pub natural_width: f32,
    /// Intrinsic image height (never decoded here).
    pub natural_height: f32,
}

impl PlacedImage {
    /// Creates a placement, validating rotation, opacity, and intrinsic size.
    ///
    /// Page bounds are checked later by [`ImageEdit::build_plan`], which knows
    /// the owning page's dimensions.
    pub fn new(
        id: ImageId,
        page: usize,
        rect: Rect,
        rotation_degrees: u16,
        opacity: f32,
        natural_width: f32,
        natural_height: f32,
    ) -> Result<Self, ImageEditError> {
        let image = Self {
            id,
            page,
            rect,
            rotation_degrees,
            opacity,
            natural_width,
            natural_height,
        };
        image.validate_geometry()?;
        Ok(image)
    }

    /// Returns the effective intrinsic `(width, height)`, swapping them for
    /// 90- and 270-degree rotations.
    pub fn effective_natural_size(&self) -> (f32, f32) {
        if matches!(self.rotation_degrees, 90 | 270) {
            (self.natural_height, self.natural_width)
        } else {
            (self.natural_width, self.natural_height)
        }
    }

    /// Computes the fitted rectangle for this image inside `rect` using `mode`.
    pub fn placed_in(&self, rect: Rect, mode: PlacementMode) -> Result<Rect, ImageEditError> {
        let (width, height) = self.effective_natural_size();
        place_image(rect, width, height, mode)
    }

    /// Validates scalar fields that do not depend on page bounds.
    fn validate_geometry(&self) -> Result<(), ImageEditError> {
        if !matches!(self.rotation_degrees, 0 | 90 | 180 | 270) {
            return Err(ImageEditError::InvalidRotation(self.rotation_degrees));
        }
        if !self.opacity.is_finite() || !(0.0..=1.0).contains(&self.opacity) {
            return Err(ImageEditError::InvalidOpacity(self.opacity));
        }
        if !self.natural_width.is_finite()
            || !self.natural_height.is_finite()
            || self.natural_width <= 0.0
            || self.natural_height <= 0.0
        {
            return Err(ImageEditError::InvalidNaturalSize {
                width: self.natural_width,
                height: self.natural_height,
            });
        }
        Ok(())
    }

    /// Validates scalar fields and containment within the given page bounds.
    fn validate_against(&self, pages: &[PageBounds]) -> Result<(), ImageEditError> {
        self.validate_geometry()?;
        let page = pages.get(self.page).ok_or(ImageEditError::PageOutOfRange {
            page: self.page,
            page_count: pages.len(),
        })?;
        if self.rect.x0() < 0.0
            || self.rect.y0() < 0.0
            || self.rect.x1() > page.width
            || self.rect.y1() > page.height
        {
            return Err(ImageEditError::OutsidePageBounds {
                id: self.id,
                page: self.page,
            });
        }
        Ok(())
    }

    /// Returns a copy of this image moved by `(dx, dy)`.
    fn moved(&self, dx: f32, dy: f32) -> Result<Self, ImageEditError> {
        if !dx.is_finite() || !dy.is_finite() {
            return Err(ImageEditError::NonFiniteField("move delta"));
        }
        let moved = Rect::new(
            self.rect.x0() + dx,
            self.rect.y0() + dy,
            self.rect.x1() + dx,
            self.rect.y1() + dy,
        )
        .map_err(ImageEditError::Geometry)?;
        let mut next = self.clone();
        next.rect = moved;
        Ok(next)
    }
}

/// Computes the placement of an intrinsic `natural_width x natural_height`
/// image inside `rect` according to `mode`.
///
/// `Fit` and `Fill` preserve aspect ratio and center the result; `Stretch`
/// uses the rectangle exactly.
pub fn place_image(
    rect: Rect,
    natural_width: f32,
    natural_height: f32,
    mode: PlacementMode,
) -> Result<Rect, ImageEditError> {
    if !natural_width.is_finite()
        || !natural_height.is_finite()
        || natural_width <= 0.0
        || natural_height <= 0.0
    {
        return Err(ImageEditError::InvalidNaturalSize {
            width: natural_width,
            height: natural_height,
        });
    }
    let box_width = rect.width();
    let box_height = rect.height();
    if box_width <= 0.0 || box_height <= 0.0 {
        return Err(ImageEditError::EmptyPlacementRect);
    }
    let (width, height) = match mode {
        PlacementMode::Fit => {
            let scale = (box_width / natural_width).min(box_height / natural_height);
            (natural_width * scale, natural_height * scale)
        }
        PlacementMode::Fill => {
            let scale = (box_width / natural_width).max(box_height / natural_height);
            (natural_width * scale, natural_height * scale)
        }
        PlacementMode::Stretch => (box_width, box_height),
    };
    if !width.is_finite() || !height.is_finite() {
        return Err(ImageEditError::NonFiniteField("fitted size"));
    }
    let x0 = rect.x0() + (box_width - width) / 2.0;
    let y0 = rect.y0() + (box_height - height) / 2.0;
    if !x0.is_finite() || !y0.is_finite() {
        return Err(ImageEditError::NonFiniteField("fitted origin"));
    }
    Rect::new(x0, y0, x0 + width, y0 + height).map_err(ImageEditError::Geometry)
}

/// A single image operation applied by [`ImageEdit::build_plan`].
#[derive(Clone, Debug, PartialEq)]
pub enum ImageOperation {
    /// Replaces the placement for `id`; the page must stay the same.
    Replace {
        /// Image to replace.
        id: ImageId,
        /// New placement.
        with: PlacedImage,
    },
    /// Removes the image with the given identifier.
    Remove {
        /// Image to remove.
        id: ImageId,
    },
    /// Translates the image by a finite `(dx, dy)` delta.
    Move {
        /// Image to move.
        id: ImageId,
        /// Horizontal delta.
        dx: f32,
        /// Vertical delta.
        dy: f32,
    },
    /// Resizes the image into `rect`, optionally preserving aspect ratio.
    Resize {
        /// Image to resize.
        id: ImageId,
        /// Target rectangle.
        rect: Rect,
        /// When `true`, fit the image inside `rect` preserving aspect ratio.
        preserve_aspect: bool,
    },
}

/// An image edit model: page bounds, initial placements, and ordered operations.
#[derive(Clone, Debug, PartialEq)]
pub struct ImageEdit {
    /// Page dimensions, indexed by zero-based page number.
    pub pages: Vec<PageBounds>,
    /// Initial placements. Identifiers must be unique.
    pub images: Vec<PlacedImage>,
    /// Operations to apply in order.
    pub operations: Vec<ImageOperation>,
}

/// The action a host writer must perform for one image.
#[derive(Clone, Debug, PartialEq)]
pub enum ImageAction {
    /// Place the contained final image.
    Place(PlacedImage),
    /// Remove the image with the step's identifier.
    Remove,
}

/// One deterministic step in an [`ImageEditPlan`].
#[derive(Clone, Debug, PartialEq)]
pub struct ImageEditStep {
    /// Image identifier this step applies to.
    pub id: ImageId,
    /// Zero-based page index owning the image.
    pub page: usize,
    /// Action to perform.
    pub action: ImageAction,
}

/// A deterministic edit plan for a host writer.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct ImageEditPlan {
    /// Steps sorted by `(page, id)`.
    pub steps: Vec<ImageEditStep>,
}

impl ImageEditPlan {
    /// Returns the number of steps in this plan.
    pub fn step_count(&self) -> usize {
        self.steps.len()
    }
}

/// Typed errors produced while building an [`ImageEditPlan`].
#[derive(Debug, Clone, PartialEq)]
pub enum ImageEditError {
    /// A floating-point field was infinite or `NaN`.
    NonFiniteField(&'static str),
    /// Opacity was outside `0.0..=1.0` or non-finite.
    InvalidOpacity(f32),
    /// Rotation was not one of 0, 90, 180, or 270 degrees.
    InvalidRotation(u16),
    /// Intrinsic dimensions were non-finite or non-positive.
    InvalidNaturalSize {
        /// Rejected width.
        width: f32,
        /// Rejected height.
        height: f32,
    },
    /// A target rectangle had no positive area.
    EmptyPlacementRect,
    /// A page index exceeded the number of supplied page bounds.
    PageOutOfRange {
        /// Rejected page index.
        page: usize,
        /// Number of available page bounds.
        page_count: usize,
    },
    /// A placement or operation left the owning page's bounds.
    OutsidePageBounds {
        /// Image involved.
        id: ImageId,
        /// Owning page index.
        page: usize,
    },
    /// An operation referenced an unknown image identifier.
    UnknownImage(ImageId),
    /// Two initial images shared an identifier.
    DuplicateImageId(ImageId),
    /// An operation was invalid for the current image state.
    InvalidOperation(&'static str),
    /// An underlying geometry validation failed.
    Geometry(GeometryError),
}

impl fmt::Display for ImageEditError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NonFiniteField(field) => write!(f, "field `{field}` must be finite"),
            Self::InvalidOpacity(value) => {
                write!(f, "opacity {value} is outside the range 0.0..=1.0")
            }
            Self::InvalidRotation(value) => {
                write!(f, "rotation {value} is not 0, 90, 180, or 270 degrees")
            }
            Self::InvalidNaturalSize { width, height } => {
                write!(f, "invalid intrinsic image size {width}x{height}")
            }
            Self::EmptyPlacementRect => write!(f, "placement rectangle has no positive area"),
            Self::PageOutOfRange { page, page_count } => {
                write!(f, "page {page} is outside a document of {page_count} pages")
            }
            Self::OutsidePageBounds { id, page } => {
                write!(f, "image {} leaves the bounds of page {page}", id.get())
            }
            Self::UnknownImage(id) => write!(f, "unknown image id {}", id.get()),
            Self::DuplicateImageId(id) => write!(f, "duplicate image id {}", id.get()),
            Self::InvalidOperation(reason) => write!(f, "invalid image operation: {reason}"),
            Self::Geometry(error) => write!(f, "geometry error: {error}"),
        }
    }
}

impl std::error::Error for ImageEditError {}

impl ImageEdit {
    /// Applies the operations in order and returns a deterministic plan.
    pub fn build_plan(&self) -> Result<ImageEditPlan, ImageEditError> {
        for page in &self.pages {
            if !page.width.is_finite() || !page.height.is_finite() {
                return Err(ImageEditError::NonFiniteField("page dimension"));
            }
            if page.width <= 0.0 || page.height <= 0.0 {
                return Err(ImageEditError::InvalidNaturalSize {
                    width: page.width,
                    height: page.height,
                });
            }
        }

        let mut current: BTreeMap<ImageId, PlacedImage> = BTreeMap::new();
        for image in &self.images {
            image.validate_against(&self.pages)?;
            if current.insert(image.id, image.clone()).is_some() {
                return Err(ImageEditError::DuplicateImageId(image.id));
            }
        }

        let mut removed: Vec<(ImageId, usize)> = Vec::new();
        for operation in &self.operations {
            match operation {
                ImageOperation::Replace { id, with } => {
                    let old = current.get(id).ok_or(ImageEditError::UnknownImage(*id))?;
                    if with.page != old.page {
                        return Err(ImageEditError::InvalidOperation(
                            "replace must keep the original page",
                        ));
                    }
                    with.validate_against(&self.pages)?;
                    current.insert(*id, with.clone());
                }
                ImageOperation::Remove { id } => {
                    let image = current
                        .remove(id)
                        .ok_or(ImageEditError::UnknownImage(*id))?;
                    removed.push((*id, image.page));
                }
                ImageOperation::Move { id, dx, dy } => {
                    let image = current.get(id).ok_or(ImageEditError::UnknownImage(*id))?;
                    let moved = image.moved(*dx, *dy)?;
                    moved.validate_against(&self.pages)?;
                    current.insert(*id, moved);
                }
                ImageOperation::Resize {
                    id,
                    rect,
                    preserve_aspect,
                } => {
                    let image = current.get(id).ok_or(ImageEditError::UnknownImage(*id))?;
                    let target = if *preserve_aspect {
                        image.placed_in(*rect, PlacementMode::Fit)?
                    } else {
                        *rect
                    };
                    if target.width() <= 0.0 || target.height() <= 0.0 {
                        return Err(ImageEditError::EmptyPlacementRect);
                    }
                    let mut resized = image.clone();
                    resized.rect = target;
                    resized.validate_against(&self.pages)?;
                    current.insert(*id, resized);
                }
            }
        }

        let mut steps: Vec<ImageEditStep> = current
            .into_iter()
            .map(|(id, image)| ImageEditStep {
                page: image.page,
                id,
                action: ImageAction::Place(image),
            })
            .collect();
        for (id, page) in removed {
            steps.push(ImageEditStep {
                id,
                page,
                action: ImageAction::Remove,
            });
        }
        steps.sort_by_key(|step| (step.page, step.id));
        Ok(ImageEditPlan { steps })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x0: f32, y0: f32, x1: f32, y1: f32) -> Rect {
        Rect::new(x0, y0, x1, y1).expect("valid rect")
    }

    fn image(id: u64, page: usize, rect: Rect) -> PlacedImage {
        PlacedImage::new(ImageId::new(id), page, rect, 0, 1.0, 200.0, 100.0).expect("valid image")
    }

    fn pages(width: f32, height: f32, count: usize) -> Vec<PageBounds> {
        (0..count)
            .map(|_| PageBounds::new(width, height).expect("valid page"))
            .collect()
    }

    #[test]
    fn fit_fill_and_stretch_math_is_correct() {
        let target = rect(0.0, 0.0, 100.0, 100.0);
        let fit = place_image(target, 200.0, 100.0, PlacementMode::Fit).expect("fit");
        assert_eq!((fit.width(), fit.height()), (100.0, 50.0));
        assert_eq!((fit.x0(), fit.y0()), (0.0, 25.0));

        let fill = place_image(target, 200.0, 100.0, PlacementMode::Fill).expect("fill");
        assert_eq!((fill.width(), fill.height()), (200.0, 100.0));
        assert_eq!((fill.x0(), fill.y0()), (-50.0, 0.0));

        let stretch = place_image(target, 200.0, 100.0, PlacementMode::Stretch).expect("stretch");
        assert_eq!((stretch.width(), stretch.height()), (100.0, 100.0));
        assert_eq!((stretch.x0(), stretch.y0()), (0.0, 0.0));
    }

    #[test]
    fn opacity_is_validated() {
        let base = rect(0.0, 0.0, 10.0, 10.0);
        assert_eq!(
            PlacedImage::new(ImageId::new(1), 0, base, 0, 1.5, 10.0, 10.0),
            Err(ImageEditError::InvalidOpacity(1.5))
        );
        assert_eq!(
            PlacedImage::new(ImageId::new(1), 0, base, 0, -0.1, 10.0, 10.0),
            Err(ImageEditError::InvalidOpacity(-0.1))
        );
        assert!(matches!(
            PlacedImage::new(ImageId::new(1), 0, base, 0, f32::NAN, 10.0, 10.0),
            Err(ImageEditError::InvalidOpacity(_))
        ));
        assert!(PlacedImage::new(ImageId::new(1), 0, base, 0, 0.0, 10.0, 10.0).is_ok());
        assert!(PlacedImage::new(ImageId::new(1), 0, base, 0, 1.0, 10.0, 10.0).is_ok());
    }

    #[test]
    fn out_of_bounds_placements_are_rejected() {
        let edit = ImageEdit {
            pages: pages(100.0, 100.0, 1),
            images: vec![image(1, 0, rect(0.0, 0.0, 150.0, 50.0))],
            operations: Vec::new(),
        };
        assert_eq!(
            edit.build_plan(),
            Err(ImageEditError::OutsidePageBounds {
                id: ImageId::new(1),
                page: 0
            })
        );
        let bad_page = ImageEdit {
            pages: pages(100.0, 100.0, 1),
            images: vec![image(1, 3, rect(0.0, 0.0, 10.0, 10.0))],
            operations: Vec::new(),
        };
        assert_eq!(
            bad_page.build_plan(),
            Err(ImageEditError::PageOutOfRange {
                page: 3,
                page_count: 1
            })
        );
    }

    #[test]
    fn resize_preserves_aspect_ratio() {
        let edit = ImageEdit {
            pages: pages(400.0, 400.0, 1),
            images: vec![image(1, 0, rect(0.0, 0.0, 200.0, 100.0))],
            operations: vec![ImageOperation::Resize {
                id: ImageId::new(1),
                rect: rect(0.0, 0.0, 400.0, 400.0),
                preserve_aspect: true,
            }],
        };
        let plan = edit.build_plan().expect("plan");
        let placed = match &plan.steps[0].action {
            ImageAction::Place(image) => image,
            ImageAction::Remove => panic!("expected placement"),
        };
        assert_eq!((placed.rect.width(), placed.rect.height()), (400.0, 200.0));
        assert!((placed.rect.width() / placed.rect.height() - 2.0).abs() < f32::EPSILON);

        let stretch = ImageEdit {
            pages: pages(400.0, 400.0, 1),
            images: vec![image(1, 0, rect(0.0, 0.0, 200.0, 100.0))],
            operations: vec![ImageOperation::Resize {
                id: ImageId::new(1),
                rect: rect(0.0, 0.0, 400.0, 400.0),
                preserve_aspect: false,
            }],
        };
        let plan = stretch.build_plan().expect("plan");
        let placed = match &plan.steps[0].action {
            ImageAction::Place(image) => image,
            ImageAction::Remove => panic!("expected placement"),
        };
        assert_eq!((placed.rect.width(), placed.rect.height()), (400.0, 400.0));
    }

    #[test]
    fn plan_is_deterministic_and_ordered_by_page_then_id() {
        let first = ImageEdit {
            pages: pages(500.0, 500.0, 2),
            images: vec![
                image(9, 1, rect(0.0, 0.0, 10.0, 10.0)),
                image(2, 0, rect(0.0, 0.0, 10.0, 10.0)),
            ],
            operations: Vec::new(),
        };
        let second = ImageEdit {
            pages: pages(500.0, 500.0, 2),
            images: vec![
                image(2, 0, rect(0.0, 0.0, 10.0, 10.0)),
                image(9, 1, rect(0.0, 0.0, 10.0, 10.0)),
            ],
            operations: Vec::new(),
        };
        let plan = first.build_plan().expect("plan");
        assert_eq!(plan, second.build_plan().expect("plan"));
        assert_eq!(plan, first.build_plan().expect("plan"));
        let keys: Vec<(usize, u64)> = plan
            .steps
            .iter()
            .map(|step| (step.page, step.id.get()))
            .collect();
        assert_eq!(keys, vec![(0, 2), (1, 9)]);
    }

    #[test]
    fn move_replace_and_remove_operations_apply_in_order() {
        let edit = ImageEdit {
            pages: pages(500.0, 500.0, 1),
            images: vec![image(1, 0, rect(10.0, 10.0, 110.0, 60.0))],
            operations: vec![
                ImageOperation::Move {
                    id: ImageId::new(1),
                    dx: 5.0,
                    dy: -5.0,
                },
                ImageOperation::Replace {
                    id: ImageId::new(1),
                    with: image(1, 0, rect(0.0, 0.0, 80.0, 40.0)),
                },
                ImageOperation::Remove {
                    id: ImageId::new(1),
                },
            ],
        };
        let plan = edit.build_plan().expect("plan");
        assert_eq!(plan.step_count(), 1);
        assert_eq!(plan.steps[0].action, ImageAction::Remove);
        assert_eq!(plan.steps[0].page, 0);
    }

    #[test]
    fn move_outside_bounds_and_non_finite_deltas_are_rejected() {
        let edit = ImageEdit {
            pages: pages(100.0, 100.0, 1),
            images: vec![image(1, 0, rect(0.0, 0.0, 50.0, 50.0))],
            operations: vec![ImageOperation::Move {
                id: ImageId::new(1),
                dx: 80.0,
                dy: 0.0,
            }],
        };
        assert_eq!(
            edit.build_plan(),
            Err(ImageEditError::OutsidePageBounds {
                id: ImageId::new(1),
                page: 0
            })
        );
        let non_finite = ImageEdit {
            pages: pages(100.0, 100.0, 1),
            images: vec![image(1, 0, rect(0.0, 0.0, 50.0, 50.0))],
            operations: vec![ImageOperation::Move {
                id: ImageId::new(1),
                dx: f32::INFINITY,
                dy: 0.0,
            }],
        };
        assert_eq!(
            non_finite.build_plan(),
            Err(ImageEditError::NonFiniteField("move delta"))
        );
    }

    #[test]
    fn invalid_rotation_natural_size_and_unknown_id_are_rejected() {
        let base = rect(0.0, 0.0, 10.0, 10.0);
        assert_eq!(
            PlacedImage::new(ImageId::new(1), 0, base, 45, 1.0, 10.0, 10.0),
            Err(ImageEditError::InvalidRotation(45))
        );
        assert_eq!(
            PlacedImage::new(ImageId::new(1), 0, base, 0, 1.0, 0.0, 10.0),
            Err(ImageEditError::InvalidNaturalSize {
                width: 0.0,
                height: 10.0
            })
        );
        let unknown = ImageEdit {
            pages: pages(100.0, 100.0, 1),
            images: Vec::new(),
            operations: vec![ImageOperation::Remove {
                id: ImageId::new(7),
            }],
        };
        assert_eq!(
            unknown.build_plan(),
            Err(ImageEditError::UnknownImage(ImageId::new(7)))
        );
    }

    #[test]
    fn rotation_swaps_effective_natural_size() {
        let mut image = image(1, 0, rect(0.0, 0.0, 200.0, 100.0));
        assert_eq!(image.effective_natural_size(), (200.0, 100.0));
        image.rotation_degrees = 90;
        assert_eq!(image.effective_natural_size(), (100.0, 200.0));
        let fitted = image
            .placed_in(rect(0.0, 0.0, 100.0, 100.0), PlacementMode::Fit)
            .expect("fit");
        assert_eq!((fitted.width(), fitted.height()), (50.0, 100.0));
    }

    #[test]
    fn duplicate_ids_are_rejected() {
        let edit = ImageEdit {
            pages: pages(100.0, 100.0, 1),
            images: vec![
                image(1, 0, rect(0.0, 0.0, 10.0, 10.0)),
                image(1, 0, rect(20.0, 20.0, 30.0, 30.0)),
            ],
            operations: Vec::new(),
        };
        assert_eq!(
            edit.build_plan(),
            Err(ImageEditError::DuplicateImageId(ImageId::new(1)))
        );
    }
}
