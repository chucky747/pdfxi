//! Navigation structures: destinations, outlines (bookmarks), links, and
//! attachments.
//!
//! This module extends the dependency-free PDF dictionary intermediate with the
//! navigation objects that Acrobat-style authoring exposes. Like the annotation
//! serialization in [`crate::pdf`], every type here round-trips through the
//! [`Dict`]/[`Value`] representation so a future writer in
//! `pdfxi-core` can map it onto real PDF objects.
//!
//! # Representation notes
//!
//! The dictionary intermediate has no indirect-reference type. Indirect
//! references are therefore encoded as non-negative **integer ids** that index
//! a flat table (the `/Items` array of an `/Outlines` dictionary, or the
//! alternating `/Names` array of an `/EmbeddedFiles` name tree). The writer in
//! `pdfxi-core` is responsible for allocating real object numbers for those
//! ids. Bookmark `/First`, `/Last`, `/Next`, `/Prev`, and `/Parent` values are
//! such ids, and outline item dictionaries carry an `XI_Id` extension key
//! mirroring the `XI_Id` convention used by annotations.
//!
//! Embedded-file **bytes are never stored here**. An [`Attachment`] carries an
//! opaque `bytes_ref` string (`XI_StreamRef` in the dictionary) that the caller
//! maps to the actual stream when the document is written.
//!
//! [`Attachment`]: attachment::Attachment
//! [`Dict`]: crate::pdf::Dict
//! [`Value`]: crate::pdf::Value

mod attachment;
mod error;
mod link;
mod outline;

pub use attachment::{Attachment, NameTree, dict_to_name_tree, name_tree_to_dict};
pub use error::NavigationError;
pub use link::{Link, LinkBorder, LinkHighlight, LinkTarget, dict_to_link, link_to_dict};
pub use outline::{Bookmark, Outline, dict_to_outline, outline_to_dict};

use crate::annotation::Color;
use crate::geometry::{PageIndex, Rect};
use crate::pdf::{Dict, Value};

/// Maximum permitted length, in bytes, of a bookmark title or name-tree key.
///
/// This matches the PDF limit for a name-tree key string. Longer values are
/// rejected with [`NavigationError::NameTooLong`].
pub const MAX_NAME_BYTES: usize = 32_767;

/// The fit behaviour of a destination, including any optional coordinates.
///
/// Coordinate values that the PDF specification allows to be omitted (the
/// `left`, `top`, and `zoom` of an `/XYZ` destination) are represented as
/// `None` and serialized as PDF `null`.
#[derive(Clone, Debug, PartialEq)]
pub enum FitType {
    /// `/XYZ left top zoom`; a `None` coordinate leaves that value unchanged.
    Xyz {
        /// Left edge of the view, or `null` for unchanged.
        left: Option<f32>,
        /// Top edge of the view, or `null` for unchanged.
        top: Option<f32>,
        /// Zoom factor, or `null` for unchanged.
        zoom: Option<f32>,
    },
    /// `/Fit` — fit the whole page in the window.
    Fit,
    /// `/FitH top` — fit the page width, placing `top` at the top of the window.
    FitH {
        /// Top edge of the view.
        top: f32,
    },
    /// `/FitV left` — fit the page height, placing `left` at the left of the window.
    FitV {
        /// Left edge of the view.
        left: f32,
    },
    /// `/FitR left bottom right top` — fit the given rectangle.
    FitR {
        /// Left edge of the rectangle.
        left: f32,
        /// Bottom edge of the rectangle.
        bottom: f32,
        /// Right edge of the rectangle.
        right: f32,
        /// Top edge of the rectangle.
        top: f32,
    },
    /// `/FitB` — fit the page bounding box.
    FitB,
    /// `/FitBH top` — fit the bounding box width at the given top coordinate.
    FitBH {
        /// Top edge of the view.
        top: f32,
    },
    /// `/FitBV left` — fit the bounding box height at the given left coordinate.
    FitBV {
        /// Left edge of the view.
        left: f32,
    },
}

impl FitType {
    /// Returns the PDF name for this fit type.
    pub fn as_name(&self) -> &'static str {
        match self {
            Self::Xyz { .. } => "XYZ",
            Self::Fit => "Fit",
            Self::FitH { .. } => "FitH",
            Self::FitV { .. } => "FitV",
            Self::FitR { .. } => "FitR",
            Self::FitB => "FitB",
            Self::FitBH { .. } => "FitBH",
            Self::FitBV { .. } => "FitBV",
        }
    }

    /// Returns the number of coordinate parameters following the fit name.
    pub fn parameter_count(&self) -> usize {
        match self {
            Self::Xyz { .. } => 3,
            Self::Fit | Self::FitB => 0,
            Self::FitH { .. } | Self::FitV { .. } | Self::FitBH { .. } | Self::FitBV { .. } => 1,
            Self::FitR { .. } => 4,
        }
    }

    /// Serializes the fit parameters (without the fit name) into a value array.
    fn parameters(&self) -> Vec<Value> {
        match *self {
            Self::Xyz { left, top, zoom } => vec![
                optional_coordinate_value(left),
                optional_coordinate_value(top),
                optional_coordinate_value(zoom),
            ],
            Self::Fit | Self::FitB => Vec::new(),
            Self::FitH { top } | Self::FitBH { top } => vec![Value::Real(f64::from(top))],
            Self::FitV { left } | Self::FitBV { left } => vec![Value::Real(f64::from(left))],
            Self::FitR {
                left,
                bottom,
                right,
                top,
            } => vec![
                Value::Real(f64::from(left)),
                Value::Real(f64::from(bottom)),
                Value::Real(f64::from(right)),
                Value::Real(f64::from(top)),
            ],
        }
    }

    /// Parses fit parameters (everything after the fit name) into a fit type.
    fn from_parameters(name: &str, params: &[Value]) -> Result<Self, NavigationError> {
        let expect = |expected: usize| -> Result<(), NavigationError> {
            if params.len() == expected {
                Ok(())
            } else {
                Err(NavigationError::InvalidFitArguments {
                    fit: name.to_string(),
                    expected,
                    got: params.len(),
                })
            }
        };
        match name {
            "XYZ" => {
                expect(3)?;
                Ok(Self::Xyz {
                    left: optional_coordinate(Some(&params[0]), "left")?,
                    top: optional_coordinate(Some(&params[1]), "top")?,
                    zoom: optional_coordinate(Some(&params[2]), "zoom")?,
                })
            }
            "Fit" => {
                expect(0)?;
                Ok(Self::Fit)
            }
            "FitH" => {
                expect(1)?;
                Ok(Self::FitH {
                    top: required_number(&params[0], "top")?,
                })
            }
            "FitV" => {
                expect(1)?;
                Ok(Self::FitV {
                    left: required_number(&params[0], "left")?,
                })
            }
            "FitR" => {
                expect(4)?;
                Ok(Self::FitR {
                    left: required_number(&params[0], "left")?,
                    bottom: required_number(&params[1], "bottom")?,
                    right: required_number(&params[2], "right")?,
                    top: required_number(&params[3], "top")?,
                })
            }
            "FitB" => {
                expect(0)?;
                Ok(Self::FitB)
            }
            "FitBH" => {
                expect(1)?;
                Ok(Self::FitBH {
                    top: required_number(&params[0], "top")?,
                })
            }
            "FitBV" => {
                expect(1)?;
                Ok(Self::FitBV {
                    left: required_number(&params[0], "left")?,
                })
            }
            other => Err(NavigationError::InvalidFit(other.to_string())),
        }
    }
}

/// A destination: a zero-based page index plus a fit behaviour.
#[derive(Clone, Debug, PartialEq)]
pub struct Destination {
    /// Zero-based index of the target page.
    pub page: PageIndex,
    /// How the page should be fitted into the window.
    pub fit: FitType,
}

impl Destination {
    /// Creates a destination from a page index and fit behaviour.
    pub fn new(page: PageIndex, fit: FitType) -> Self {
        Self { page, fit }
    }

    /// Serializes the destination as a PDF destination array.
    ///
    /// The page is written as an integer page index (the writer substitutes a
    /// real page reference). The result has the shape
    /// `[page /FitName parameters...]`.
    pub fn to_value(&self) -> Value {
        let page = i64::try_from(self.page.get()).unwrap_or(i64::MAX);
        let mut items = Vec::with_capacity(2 + self.fit.parameter_count());
        items.push(Value::Int(page));
        items.push(Value::Name(self.fit.as_name().to_string()));
        items.extend(self.fit.parameters());
        Value::Array(items)
    }

    /// Parses a destination from a PDF destination array.
    pub fn from_value(value: &Value, key: &'static str) -> Result<Self, NavigationError> {
        let items = value.as_array().ok_or_else(|| NavigationError::WrongType {
            key: key.to_string(),
            expected: "an array",
        })?;
        if items.len() < 2 {
            return Err(NavigationError::InvalidDestination {
                key,
                got: items.len(),
            });
        }
        let page_int = items[0]
            .as_int()
            .ok_or_else(|| NavigationError::WrongType {
                key: key.to_string(),
                expected: "an integer page index",
            })?;
        let page_u64 =
            u64::try_from(page_int).map_err(|_| NavigationError::NegativeId(page_int))?;
        let page = usize::try_from(page_u64)
            .map_err(|_| NavigationError::PageIndexTooLarge(page_u64 as usize))?;
        let fit_name = items[1]
            .as_name()
            .ok_or_else(|| NavigationError::WrongType {
                key: key.to_string(),
                expected: "a fit name",
            })?;
        let fit = FitType::from_parameters(fit_name, &items[2..])?;
        Ok(Self::new(PageIndex::new(page), fit))
    }
}

fn optional_coordinate_value(value: Option<f32>) -> Value {
    match value {
        Some(number) => Value::Real(f64::from(number)),
        None => Value::Null,
    }
}

fn optional_coordinate(
    value: Option<&Value>,
    key: &'static str,
) -> Result<Option<f32>, NavigationError> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(v) => required_number(v, key).map(Some),
    }
}

fn required_number(value: &Value, key: &'static str) -> Result<f32, NavigationError> {
    let number = value
        .as_number()
        .ok_or_else(|| NavigationError::WrongType {
            key: key.to_string(),
            expected: "a number",
        })?;
    if number.is_finite() {
        Ok(number as f32)
    } else {
        Err(NavigationError::NonFinite {
            key: key.to_string(),
        })
    }
}

/// Serializes a rectangle as a four-element PDF array.
pub(crate) fn rect_to_value(rect: Rect) -> Value {
    Value::Array(vec![
        Value::Real(f64::from(rect.x0())),
        Value::Real(f64::from(rect.y0())),
        Value::Real(f64::from(rect.x1())),
        Value::Real(f64::from(rect.y1())),
    ])
}

/// Parses a rectangle from a four-element PDF array.
pub(crate) fn rect_from_value(value: &Value, key: &'static str) -> Result<Rect, NavigationError> {
    let values = value.as_array().ok_or_else(|| NavigationError::WrongType {
        key: key.to_string(),
        expected: "an array",
    })?;
    if values.len() != 4 {
        return Err(NavigationError::WrongType {
            key: key.to_string(),
            expected: "an array of four numbers",
        });
    }
    let coords: Vec<f32> = values
        .iter()
        .map(|value| required_number(value, key))
        .collect::<Result<_, _>>()?;
    Rect::new(coords[0], coords[1], coords[2], coords[3]).map_err(NavigationError::from)
}

/// Reads an optional id (non-negative integer) from `dict` under `key`.
pub(crate) fn optional_id(dict: &Dict, key: &'static str) -> Result<Option<u64>, NavigationError> {
    let Some(value) = dict.get(key) else {
        return Ok(None);
    };
    let number = value.as_int().ok_or_else(|| NavigationError::WrongType {
        key: key.to_string(),
        expected: "an integer id",
    })?;
    u64::try_from(number)
        .map(Some)
        .map_err(|_| NavigationError::NegativeId(number))
}

/// Reads a required id (non-negative integer) from `dict` under `key`.
pub(crate) fn required_id(dict: &Dict, key: &'static str) -> Result<u64, NavigationError> {
    optional_id(dict, key)?.ok_or(NavigationError::MissingKey(key))
}

/// Serializes an id as a dictionary integer value.
pub(crate) fn id_value(id: u64) -> Value {
    Value::Int(i64::try_from(id).unwrap_or(i64::MAX))
}

/// Validates that a title or name-tree key is non-empty and within the limit.
pub(crate) fn validate_name(key: &'static str, text: &str) -> Result<(), NavigationError> {
    if text.is_empty() {
        return Err(NavigationError::EmptyName { key });
    }
    let len = text.len();
    if len > MAX_NAME_BYTES {
        return Err(NavigationError::NameTooLong {
            key,
            max: MAX_NAME_BYTES,
            got: len,
        });
    }
    Ok(())
}

/// Reads a required text value from `dict` under `key`, validating its length.
pub(crate) fn required_name(dict: &Dict, key: &'static str) -> Result<String, NavigationError> {
    let value = dict.get(key).ok_or(NavigationError::MissingKey(key))?;
    let text = value.as_text().ok_or_else(|| NavigationError::WrongType {
        key: key.to_string(),
        expected: "a string",
    })?;
    validate_name(key, text)?;
    Ok(text.to_string())
}

/// Parses a three-component `/C` colour array into an [`Color`].
pub(crate) fn rgb_from_value(value: &Value) -> Result<Color, NavigationError> {
    let values = value.as_array().ok_or_else(|| NavigationError::WrongType {
        key: "C".to_string(),
        expected: "an array of three numbers",
    })?;
    if values.len() != 3 {
        return Err(NavigationError::WrongType {
            key: "C".to_string(),
            expected: "an array of three numbers",
        });
    }
    let mut components = [0u8; 3];
    for (index, value) in values.iter().enumerate() {
        let number = value
            .as_number()
            .ok_or_else(|| NavigationError::WrongType {
                key: "C".to_string(),
                expected: "a number",
            })?;
        if !number.is_finite() || !(0.0..=1.0).contains(&number) {
            return Err(NavigationError::InvalidColorComponent {
                index,
                value: number,
            });
        }
        components[index] = (number * 255.0).round().clamp(0.0, 255.0) as u8;
    }
    Ok(Color::new(components[0], components[1], components[2]))
}

/// Serializes a [`Color`] into a three-component `/C` array.
pub(crate) fn rgb_to_value(color: Color) -> Value {
    Value::Array(vec![
        Value::Real(f64::from(color.r()) / 255.0),
        Value::Real(f64::from(color.g()) / 255.0),
        Value::Real(f64::from(color.b()) / 255.0),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dest(page: usize, fit: FitType) -> Destination {
        Destination::new(PageIndex::new(page), fit)
    }

    #[test]
    fn destination_fit_variants_round_trip() {
        let fits = [
            FitType::Xyz {
                left: Some(10.0),
                top: None,
                zoom: Some(1.5),
            },
            FitType::Fit,
            FitType::FitH { top: 700.0 },
            FitType::FitV { left: 20.0 },
            FitType::FitR {
                left: 1.0,
                bottom: 2.0,
                right: 3.0,
                top: 4.0,
            },
            FitType::FitB,
            FitType::FitBH { top: 100.0 },
            FitType::FitBV { left: 50.0 },
        ];
        for fit in fits {
            let original = dest(3, fit);
            let value = original.to_value();
            let restored = Destination::from_value(&value, "Dest").expect("fit must round-trip");
            assert_eq!(restored, original);
        }
    }

    #[test]
    fn xyz_all_null_coordinates_round_trip() {
        let original = dest(
            0,
            FitType::Xyz {
                left: None,
                top: None,
                zoom: None,
            },
        );
        let value = original.to_value();
        assert_eq!(
            Destination::from_value(&value, "Dest").expect("round-trip"),
            original
        );
    }

    #[test]
    fn invalid_fit_name_and_arguments_are_rejected() {
        let bad_name = Value::Array(vec![Value::Int(0), Value::Name("Bogus".into())]);
        assert!(matches!(
            Destination::from_value(&bad_name, "Dest"),
            Err(NavigationError::InvalidFit(_))
        ));
        let bad_args = Value::Array(vec![
            Value::Int(0),
            Value::Name("FitH".into()),
            Value::Real(1.0),
            Value::Real(2.0),
        ]);
        assert!(matches!(
            Destination::from_value(&bad_args, "Dest"),
            Err(NavigationError::InvalidFitArguments { .. })
        ));
        let too_short = Value::Array(vec![Value::Int(0)]);
        assert!(matches!(
            Destination::from_value(&too_short, "Dest"),
            Err(NavigationError::InvalidDestination { .. })
        ));
    }

    #[test]
    fn name_validation_enforces_empty_and_length_limits() {
        assert_eq!(
            validate_name("Title", ""),
            Err(NavigationError::EmptyName { key: "Title" })
        );
        let long = "x".repeat(MAX_NAME_BYTES + 1);
        assert_eq!(
            validate_name("Title", &long),
            Err(NavigationError::NameTooLong {
                key: "Title",
                max: MAX_NAME_BYTES,
                got: long.len(),
            })
        );
        assert!(validate_name("Title", "ok").is_ok());
    }
}
