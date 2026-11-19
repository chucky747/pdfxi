//! Link annotation model and PDF dictionary round-trip.
//!
//! A [`Link`] is a `/Link` annotation carrying one of three target kinds: a
//! web [`Uri`](LinkTarget::Uri), an internal [`Destination`](LinkTarget::Destination),
//! or a [`NamedAction`](LinkTarget::NamedAction). Highlight mode and border are
//! preserved so a link round-trips without losing its appearance.

use crate::annotation::Color;
use crate::geometry::Rect;
use crate::pdf::{Dict, Value};

use super::{
    Destination, NavigationError, rect_from_value, rect_to_value, required_name, rgb_from_value,
    rgb_to_value, validate_name,
};

/// How a link responds to the pointer hovering over it (`/H`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LinkHighlight {
    /// No highlight (`/N`).
    #[default]
    None,
    /// Invert the link region (`/I`).
    Invert,
    /// Draw an outline around the link region (`/O`).
    Outline,
    /// Push the link region in (`/P`).
    Push,
}

impl LinkHighlight {
    /// Returns the PDF name for this highlight mode.
    pub fn as_name(self) -> &'static str {
        match self {
            Self::None => "N",
            Self::Invert => "I",
            Self::Outline => "O",
            Self::Push => "P",
        }
    }

    /// Parses a highlight mode from a PDF name.
    pub fn from_name(name: &str) -> Result<Self, NavigationError> {
        match name {
            "N" => Ok(Self::None),
            "I" => Ok(Self::Invert),
            "O" => Ok(Self::Outline),
            "P" => Ok(Self::Push),
            other => Err(NavigationError::InvalidHighlight(other.to_string())),
        }
    }
}

/// The action performed when a link is activated.
#[derive(Clone, Debug, PartialEq)]
pub enum LinkTarget {
    /// A web URI (`/A << /S /URI /URI ... >>`).
    Uri(String),
    /// A jump to a page and fit within the same document (`/Dest`).
    Destination(Destination),
    /// A named action such as `/NextPage` (`/A << /S /Named /N ... >>`).
    NamedAction(String),
}

impl LinkTarget {
    fn to_value(&self) -> Value {
        match self {
            Self::Uri(uri) => {
                let mut action = Dict::new();
                action.insert("S".to_string(), Value::Name("URI".to_string()));
                action.insert("URI".to_string(), Value::Text(uri.clone()));
                Value::Dictionary(action)
            }
            Self::Destination(destination) => destination.to_value(),
            Self::NamedAction(name) => {
                let mut action = Dict::new();
                action.insert("S".to_string(), Value::Name("Named".to_string()));
                action.insert("N".to_string(), Value::Name(name.clone()));
                Value::Dictionary(action)
            }
        }
    }

    fn from_value(value: &Value) -> Result<Self, NavigationError> {
        if value.as_array().is_some() {
            return Ok(Self::Destination(Destination::from_value(value, "Dest")?));
        }
        let action = value.as_dict().ok_or_else(|| NavigationError::WrongType {
            key: "A".to_string(),
            expected: "an action dictionary or a destination array",
        })?;
        let subtype = action
            .get("S")
            .and_then(Value::as_name)
            .ok_or(NavigationError::MissingKey("S"))?;
        match subtype {
            "URI" => Ok(Self::Uri(required_name(action, "URI")?)),
            "GoTo" => {
                let dest = action.get("D").ok_or(NavigationError::MissingKey("D"))?;
                Ok(Self::Destination(Destination::from_value(dest, "D")?))
            }
            "Named" => {
                let name = action.get("N").and_then(Value::as_name).ok_or_else(|| {
                    NavigationError::WrongType {
                        key: "N".to_string(),
                        expected: "a name",
                    }
                })?;
                validate_name("N", name)?;
                Ok(Self::NamedAction(name.to_string()))
            }
            other => Err(NavigationError::InvalidAction(other.to_string())),
        }
    }
}

/// The visible border of a link annotation.
#[derive(Clone, Debug, PartialEq)]
pub struct LinkBorder {
    /// Border width in page units.
    pub width: f32,
    /// Border colour (`/C`).
    pub color: Color,
}

impl Default for LinkBorder {
    fn default() -> Self {
        Self {
            width: 0.0,
            color: Color::BLACK,
        }
    }
}

impl LinkBorder {
    /// Creates a border, rejecting a non-finite or negative width.
    pub fn new(width: f32, color: Color) -> Result<Self, NavigationError> {
        if !width.is_finite() || width < 0.0 {
            return Err(NavigationError::NonFinite {
                key: "Border".to_string(),
            });
        }
        Ok(Self { width, color })
    }
}

/// A clickable link annotation on a page.
#[derive(Clone, Debug, PartialEq)]
pub struct Link {
    /// Clickable rectangle in page coordinates.
    pub rect: Rect,
    /// Action performed when the link is activated.
    pub target: LinkTarget,
    /// Pointer-hover highlight mode.
    pub highlight: LinkHighlight,
    /// Visible border.
    pub border: LinkBorder,
}

impl Link {
    /// Creates a link with no highlight and a default (invisible) border.
    pub fn new(rect: Rect, target: LinkTarget) -> Self {
        Self {
            rect,
            target,
            highlight: LinkHighlight::default(),
            border: LinkBorder::default(),
        }
    }

    /// Sets the highlight mode, returning the modified link.
    pub fn with_highlight(mut self, highlight: LinkHighlight) -> Self {
        self.highlight = highlight;
        self
    }

    /// Sets the border, returning the modified link.
    pub fn with_border(mut self, border: LinkBorder) -> Self {
        self.border = border;
        self
    }
}

/// Serializes a link into a `/Link` annotation dictionary.
pub fn link_to_dict(link: &Link) -> Dict {
    let mut dict = Dict::new();
    dict.insert("Type".to_string(), Value::Name("Annot".to_string()));
    dict.insert("Subtype".to_string(), Value::Name("Link".to_string()));
    dict.insert("Rect".to_string(), rect_to_value(link.rect));
    dict.insert(
        "H".to_string(),
        Value::Name(link.highlight.as_name().to_string()),
    );
    dict.insert(
        "Border".to_string(),
        Value::Array(vec![
            Value::Int(0),
            Value::Int(0),
            Value::Real(f64::from(link.border.width)),
        ]),
    );
    dict.insert("C".to_string(), rgb_to_value(link.border.color));

    match &link.target {
        LinkTarget::Destination(destination) => {
            dict.insert("Dest".to_string(), destination.to_value());
        }
        other => {
            dict.insert("A".to_string(), other.to_value());
        }
    }
    dict
}

/// Parses a `/Link` annotation dictionary into a [`Link`].
///
/// A target may be supplied as a `/Dest` array or as an `/A` action dictionary
/// with subtype `URI`, `GoTo`, or `Named`.
pub fn dict_to_link(dict: &Dict) -> Result<Link, NavigationError> {
    let subtype = dict
        .get("Subtype")
        .and_then(Value::as_name)
        .ok_or(NavigationError::MissingKey("Subtype"))?;
    if subtype != "Link" {
        return Err(NavigationError::InvalidName {
            key: "Subtype",
            value: subtype.to_string(),
        });
    }

    let rect = rect_from_value(
        dict.get("Rect")
            .ok_or(NavigationError::MissingKey("Rect"))?,
        "Rect",
    )?;

    let highlight = match dict.get("H") {
        Some(value) => {
            let name = value.as_name().ok_or_else(|| NavigationError::WrongType {
                key: "H".to_string(),
                expected: "a name",
            })?;
            LinkHighlight::from_name(name)?
        }
        None => LinkHighlight::default(),
    };

    let width = match dict.get("Border") {
        Some(value) => {
            let values = value.as_array().ok_or_else(|| NavigationError::WrongType {
                key: "Border".to_string(),
                expected: "an array",
            })?;
            values.get(2).and_then(Value::as_number).unwrap_or(0.0) as f32
        }
        None => 0.0,
    };
    let color = match dict.get("C") {
        Some(value) => rgb_from_value(value)?,
        None => Color::BLACK,
    };
    let border = LinkBorder::new(width, color)?;

    let target_value = match (dict.get("Dest"), dict.get("A")) {
        (Some(dest), _) => dest.clone(),
        (None, Some(action)) => action.clone(),
        (None, None) => return Err(NavigationError::MissingKey("A")),
    };
    let target = LinkTarget::from_value(&target_value)?;

    Ok(Link {
        rect,
        target,
        highlight,
        border,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::PageIndex;
    use crate::navigation::FitType;

    fn rect() -> Rect {
        Rect::new(10.0, 20.0, 110.0, 40.0).expect("test rect")
    }

    #[test]
    fn uri_link_round_trip() {
        let link = Link::new(
            rect(),
            LinkTarget::Uri("https://example.com/docs?q=1".to_string()),
        )
        .with_highlight(LinkHighlight::Invert)
        .with_border(LinkBorder::new(1.5, Color::new(200, 100, 50)).expect("border"));
        let dict = link_to_dict(&link);
        assert_eq!(dict.get("Subtype").and_then(Value::as_name), Some("Link"));
        assert_eq!(dict.get("H").and_then(Value::as_name), Some("I"));
        assert_eq!(dict_to_link(&dict).expect("uri link"), link);
    }

    #[test]
    fn internal_destination_link_round_trip() {
        let link = Link::new(
            rect(),
            LinkTarget::Destination(Destination::new(
                PageIndex::new(2),
                FitType::Xyz {
                    left: Some(0.0),
                    top: None,
                    zoom: Some(2.0),
                },
            )),
        );
        let dict = link_to_dict(&link);
        assert!(dict.contains_key("Dest"));
        assert!(!dict.contains_key("A"));
        assert_eq!(dict_to_link(&dict).expect("dest link"), link);
    }

    #[test]
    fn named_action_link_round_trip() {
        let link = Link::new(rect(), LinkTarget::NamedAction("NextPage".to_string()));
        let dict = link_to_dict(&link);
        assert_eq!(dict_to_link(&dict).expect("named link"), link);
    }

    #[test]
    fn all_highlight_modes_round_trip() {
        for highlight in [
            LinkHighlight::None,
            LinkHighlight::Invert,
            LinkHighlight::Outline,
            LinkHighlight::Push,
        ] {
            assert_eq!(LinkHighlight::from_name(highlight.as_name()), Ok(highlight));
            let link = Link::new(rect(), LinkTarget::Uri("https://x".to_string()))
                .with_highlight(highlight);
            assert_eq!(dict_to_link(&link_to_dict(&link)).expect("link"), link);
        }
        assert!(matches!(
            LinkHighlight::from_name("Q"),
            Err(NavigationError::InvalidHighlight(_))
        ));
    }

    #[test]
    fn goto_action_destination_is_parsed() {
        let mut action = Dict::new();
        action.insert("S".to_string(), Value::Name("GoTo".to_string()));
        action.insert(
            "D".to_string(),
            Destination::new(PageIndex::new(1), FitType::Fit).to_value(),
        );
        let mut dict = link_to_dict(&Link::new(
            rect(),
            LinkTarget::Destination(Destination::new(PageIndex::new(1), FitType::Fit)),
        ));
        dict.remove("Dest");
        dict.insert("A".to_string(), Value::Dictionary(action));
        let parsed = dict_to_link(&dict).expect("goto action");
        assert_eq!(
            parsed.target,
            LinkTarget::Destination(Destination::new(PageIndex::new(1), FitType::Fit))
        );
    }

    #[test]
    fn invalid_link_dictionaries_are_rejected() {
        let mut dict = Dict::new();
        dict.insert("Subtype".to_string(), Value::Name("Text".to_string()));
        assert!(matches!(
            dict_to_link(&dict),
            Err(NavigationError::InvalidName { .. })
        ));
    }
}
