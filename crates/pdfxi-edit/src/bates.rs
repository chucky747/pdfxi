//! Typed Bates-numbering model with deterministic, host-consumed stamps.
//!
//! This module is renderer-agnostic and dependency-free. It turns a
//! [`BatesConfig`] plus a set of caller-supplied page bounds into a
//! [`BatesPlan`]: an ordered list of [`BatesStamp`] values, each carrying the
//! page index, the full stamp text, and a validated position rectangle. A host
//! writer (outside this crate) consumes the plan and paints the stamps into a
//! document; **this module never reads, writes, or mutates PDF bytes**.
//!
//! # Coordinates
//!
//! [`BatesPageBounds`] and the resulting [`BatesStamp::position`] rectangles are
//! expressed in *display coordinates*: the origin is the lower-left of the page
//! as a reader sees it, `x` grows right and `y` grows up. Page bounds are
//! validated by [`build`] (finite, positive extent) so a hostile host cannot
//! inject `NaN` or inverted geometry into a plan.
//!
//! # Numbering
//!
//! On the *n*-th selected page (zero-based ordinal), the stamped number is
//! `start + n * increment`. It is rendered as decimal digits left-padded with
//! `'0'` to exactly `digit_width` characters. Arithmetic overflow and numbers
//! wider than `digit_width` are rejected with typed errors rather than
//! silently wrapping. Page selection is applied first, so numbers are
//! consecutive across the pages that actually receive a stamp.
//!
//! # Position model
//!
//! [`BatesAnchor`] is a nine-position, 3x3 anchor grid (top/middle/bottom by
//! left/center/right). The position rectangle is derived from the page bounds,
//! the configured [`BatesMargins`], and a deterministic text extent computed by
//! [`bates_text_extent`]. No font-metrics dependency is required.
//!
//! # Dates
//!
//! An optional [`BatesDateStamp`] appends a UTC date derived from a
//! caller-supplied Unix timestamp. The conversion is pure integer arithmetic,
//! so it is deterministic, offline, and independent of the host clock or
//! locale.

use core::fmt;

use crate::geometry::Rect;
use crate::header_footer::StandardFont;

/// Largest supported digit width (`u64::MAX` has 20 decimal digits).
pub const MAX_DIGIT_WIDTH: usize = 20;
/// Largest accepted font size, in points.
pub const MAX_FONT_SIZE: f32 = 4096.0;

/// Conservative glyph advance as a fraction of the font size.
const ADVANCE_RATIO: f32 = 0.6;
/// Conservative line height as a fraction of the font size.
const LINE_HEIGHT_RATIO: f32 = 1.2;

/// Errors produced while validating or building a [`BatesPlan`].
#[derive(Debug, Clone, PartialEq)]
pub enum BatesError {
    /// The digit width was zero or exceeded [`MAX_DIGIT_WIDTH`].
    InvalidDigitWidth(usize),
    /// The per-page increment was zero.
    InvalidIncrement(u64),
    /// Font size was not finite, was non-positive, or exceeded [`MAX_FONT_SIZE`].
    InvalidFontSize(f32),
    /// A margin was not finite or was negative.
    InvalidMargin {
        /// Name of the offending margin.
        field: &'static str,
        /// The rejected value.
        value: f32,
    },
    /// A color channel was not finite or was outside `0.0..=1.0`.
    InvalidColorChannel {
        /// Name of the offending channel.
        field: &'static str,
        /// The rejected value.
        value: f32,
    },
    /// An alternate page selection had a zero stride.
    InvalidPageStride,
    /// The page selection resolved to no pages.
    EmptyPageSet,
    /// The numbering sequence overflowed `u64`.
    NumberOverflow {
        /// Configured first number.
        start: u64,
        /// Configured increment.
        increment: u64,
        /// Zero-based ordinal that overflowed.
        ordinal: usize,
    },
    /// A number had more decimal digits than the configured width.
    NumberExceedsWidth {
        /// The number that did not fit.
        number: u64,
        /// Configured digit width.
        digit_width: usize,
    },
    /// A page-bound coordinate was infinite or `NaN`.
    NonFiniteGeometry {
        /// Name of the offending field.
        field: &'static str,
        /// The rejected value.
        value: f32,
    },
    /// A page rectangle had a non-positive extent.
    InvalidPageBounds {
        /// Index of the offending page.
        page_index: usize,
        /// Lower-left x.
        x0: f32,
        /// Lower-left y.
        y0: f32,
        /// Upper-right x.
        x1: f32,
        /// Upper-right y.
        y1: f32,
    },
    /// An underlying geometry value was invalid.
    Geometry(crate::geometry::GeometryError),
}

impl fmt::Display for BatesError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidDigitWidth(value) => {
                write!(
                    f,
                    "Bates digit width must be between 1 and {MAX_DIGIT_WIDTH}, got {value}"
                )
            }
            Self::InvalidIncrement(value) => {
                write!(f, "Bates increment must be at least 1, got {value}")
            }
            Self::InvalidFontSize(value) => {
                write!(
                    f,
                    "Bates font size must be finite, positive, and at most {MAX_FONT_SIZE} points, got {value}"
                )
            }
            Self::InvalidMargin { field, value } => {
                write!(
                    f,
                    "Bates margin `{field}` must be finite and non-negative, got {value}"
                )
            }
            Self::InvalidColorChannel { field, value } => {
                write!(
                    f,
                    "Bates color channel `{field}` must be finite and in 0.0..=1.0, got {value}"
                )
            }
            Self::InvalidPageStride => {
                write!(
                    f,
                    "Bates alternate page selection stride must be at least 1"
                )
            }
            Self::EmptyPageSet => {
                write!(f, "Bates page selection resolved to an empty page set")
            }
            Self::NumberOverflow {
                start,
                increment,
                ordinal,
            } => {
                write!(
                    f,
                    "Bates number overflows u64: start {start}, increment {increment}, ordinal {ordinal}"
                )
            }
            Self::NumberExceedsWidth {
                number,
                digit_width,
            } => {
                write!(
                    f,
                    "Bates number {number} does not fit in a digit width of {digit_width}"
                )
            }
            Self::NonFiniteGeometry { field, value } => {
                write!(f, "Bates page bound `{field}` must be finite, got {value}")
            }
            Self::InvalidPageBounds {
                page_index,
                x0,
                y0,
                x1,
                y1,
            } => {
                write!(
                    f,
                    "Bates page {page_index} has a non-positive extent ({x0}, {y0}) - ({x1}, {y1})"
                )
            }
            Self::Geometry(err) => write!(f, "Bates geometry error: {err}"),
        }
    }
}

impl std::error::Error for BatesError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Geometry(err) => Some(err),
            _ => None,
        }
    }
}

impl From<crate::geometry::GeometryError> for BatesError {
    fn from(err: crate::geometry::GeometryError) -> Self {
        Self::Geometry(err)
    }
}

/// An opaque RGB color for Bates stamp text.
///
/// Each channel is validated to be finite and within `0.0..=1.0`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BatesColor {
    r: f32,
    g: f32,
    b: f32,
}

impl BatesColor {
    /// Opaque black, the default stamp color.
    pub const BLACK: BatesColor = BatesColor {
        r: 0.0,
        g: 0.0,
        b: 0.0,
    };

    /// Creates a color from RGB channels in `0.0..=1.0`.
    pub fn new(r: f32, g: f32, b: f32) -> Result<Self, BatesError> {
        for (field, value) in [("r", r), ("g", g), ("b", b)] {
            if !value.is_finite() || !(0.0..=1.0).contains(&value) {
                return Err(BatesError::InvalidColorChannel { field, value });
            }
        }
        Ok(Self { r, g, b })
    }

    /// Red channel.
    pub fn r(self) -> f32 {
        self.r
    }

    /// Green channel.
    pub fn g(self) -> f32 {
        self.g
    }

    /// Blue channel.
    pub fn b(self) -> f32 {
        self.b
    }
}

/// Margins, in points, from the visible page edges to the stamp rectangle.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BatesMargins {
    left: f32,
    right: f32,
    top: f32,
    bottom: f32,
}

impl BatesMargins {
    /// Creates margins, rejecting non-finite or negative values.
    pub fn new(left: f32, right: f32, top: f32, bottom: f32) -> Result<Self, BatesError> {
        for (field, value) in [
            ("left", left),
            ("right", right),
            ("top", top),
            ("bottom", bottom),
        ] {
            if !value.is_finite() || value < 0.0 {
                return Err(BatesError::InvalidMargin { field, value });
            }
        }
        Ok(Self {
            left,
            right,
            top,
            bottom,
        })
    }

    /// Creates margins where all four edges use the same value.
    pub fn uniform(value: f32) -> Result<Self, BatesError> {
        Self::new(value, value, value, value)
    }

    /// Left margin.
    pub fn left(self) -> f32 {
        self.left
    }

    /// Right margin.
    pub fn right(self) -> f32 {
        self.right
    }

    /// Top margin.
    pub fn top(self) -> f32 {
        self.top
    }

    /// Bottom margin.
    pub fn bottom(self) -> f32 {
        self.bottom
    }
}

impl Default for BatesMargins {
    fn default() -> Self {
        Self {
            left: 36.0,
            right: 36.0,
            top: 36.0,
            bottom: 36.0,
        }
    }
}

/// Font family, size, color, and margins for a stamp.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BatesStyle {
    font: StandardFont,
    font_size: f32,
    color: BatesColor,
    margins: BatesMargins,
}

impl BatesStyle {
    /// Creates a style, validating the font size.
    ///
    /// The color and margins are validated by their own constructors.
    pub fn new(
        font: StandardFont,
        font_size: f32,
        color: BatesColor,
        margins: BatesMargins,
    ) -> Result<Self, BatesError> {
        if !font_size.is_finite() || font_size <= 0.0 || font_size > MAX_FONT_SIZE {
            return Err(BatesError::InvalidFontSize(font_size));
        }
        Ok(Self {
            font,
            font_size,
            color,
            margins,
        })
    }

    /// Helvetica at 10 points in black with 36-point margins.
    pub fn default_style() -> Self {
        Self {
            font: StandardFont::Helvetica,
            font_size: 10.0,
            color: BatesColor::BLACK,
            margins: BatesMargins::default(),
        }
    }

    /// Font family.
    pub fn font(self) -> StandardFont {
        self.font
    }

    /// Font size in points.
    pub fn font_size(self) -> f32 {
        self.font_size
    }

    /// Text color.
    pub fn color(self) -> BatesColor {
        self.color
    }

    /// Page margins.
    pub fn margins(self) -> BatesMargins {
        self.margins
    }
}

/// Vertical band of a [`BatesAnchor`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum BatesRow {
    /// Top edge of the page.
    Top,
    /// Vertical middle of the page.
    Middle,
    /// Bottom edge of the page.
    Bottom,
}

/// Horizontal band of a [`BatesAnchor`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum BatesColumn {
    /// Left edge of the page.
    Left,
    /// Horizontal center of the page.
    Center,
    /// Right edge of the page.
    Right,
}

/// One of nine positions in a 3x3 anchor grid.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum BatesAnchor {
    /// Upper-left corner.
    TopLeft,
    /// Top center.
    TopCenter,
    /// Upper-right corner.
    TopRight,
    /// Middle-left edge.
    MiddleLeft,
    /// Page center.
    #[default]
    MiddleCenter,
    /// Middle-right edge.
    MiddleRight,
    /// Lower-left corner.
    BottomLeft,
    /// Bottom center.
    BottomCenter,
    /// Lower-right corner (the conventional Bates location).
    BottomRight,
}

impl BatesAnchor {
    /// All nine anchors in deterministic declaration order.
    pub const ALL: [BatesAnchor; 9] = [
        Self::TopLeft,
        Self::TopCenter,
        Self::TopRight,
        Self::MiddleLeft,
        Self::MiddleCenter,
        Self::MiddleRight,
        Self::BottomLeft,
        Self::BottomCenter,
        Self::BottomRight,
    ];

    /// Vertical band of this anchor.
    pub const fn row(self) -> BatesRow {
        match self {
            Self::TopLeft | Self::TopCenter | Self::TopRight => BatesRow::Top,
            Self::MiddleLeft | Self::MiddleCenter | Self::MiddleRight => BatesRow::Middle,
            Self::BottomLeft | Self::BottomCenter | Self::BottomRight => BatesRow::Bottom,
        }
    }

    /// Horizontal band of this anchor.
    pub const fn column(self) -> BatesColumn {
        match self {
            Self::TopLeft | Self::MiddleLeft | Self::BottomLeft => BatesColumn::Left,
            Self::TopCenter | Self::MiddleCenter | Self::BottomCenter => BatesColumn::Center,
            Self::TopRight | Self::MiddleRight | Self::BottomRight => BatesColumn::Right,
        }
    }
}

/// Which pages of a supplied range receive a stamp.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum BatesPageSet {
    /// Every page in the range.
    #[default]
    All,
    /// Pages whose zero-based index is even (display positions 1, 3, 5, ...).
    Odd,
    /// Pages whose zero-based index is odd (display positions 2, 4, 6, ...).
    Even,
    /// Every `every`-th page starting at zero-based `offset`.
    ///
    /// A zero `every` selects no pages and is rejected by
    /// [`BatesConfig::validate`].
    Alternate {
        /// Stride between selected pages (must be at least 1).
        every: usize,
        /// Zero-based index of the first selected page.
        offset: usize,
    },
    /// Explicit zero-based page indices to select.
    ///
    /// Indices not present in the supplied range are ignored.
    Indices(Vec<usize>),
}

impl BatesPageSet {
    /// Returns `true` when the zero-based `page_index` is selected.
    ///
    /// This is infallible: a zero alternate stride selects nothing rather than
    /// dividing by zero, and is separately rejected during validation.
    pub fn includes(&self, page_index: usize) -> bool {
        match self {
            Self::All => true,
            Self::Odd => page_index.is_multiple_of(2),
            Self::Even => !page_index.is_multiple_of(2),
            Self::Alternate { every, offset } => {
                *every > 0 && page_index >= *offset && (page_index - *offset).is_multiple_of(*every)
            }
            Self::Indices(indices) => indices.contains(&page_index),
        }
    }
}

/// Format for a caller-supplied UTC date stamp.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BatesDateFormat {
    /// ISO 8601 calendar date (`2020-09-13`).
    Iso8601,
    /// Compact date (`20200913`).
    Compact,
    /// US slash date (`09/13/2020`).
    UsSlash,
    /// European slash date (`13/09/2020`).
    EuSlash,
}

/// An optional date appended to the Bates number text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BatesDateStamp {
    format: BatesDateFormat,
    separator: String,
}

impl BatesDateStamp {
    /// Creates a date stamp with a single-space separator.
    pub fn new(format: BatesDateFormat) -> Self {
        Self {
            format,
            separator: " ".to_owned(),
        }
    }

    /// Sets the separator inserted between the stamp text and the date.
    pub fn with_separator(mut self, separator: impl Into<String>) -> Self {
        self.separator = separator.into();
        self
    }

    /// Date format.
    pub fn format(&self) -> BatesDateFormat {
        self.format
    }

    /// Separator between the number text and the date.
    pub fn separator(&self) -> &str {
        &self.separator
    }
}

/// Complete Bates-numbering configuration.
///
/// Build one with [`BatesConfig::new`], adjust it with the `with_*` builders,
/// and pass it to [`build`]. Font size, color, and margins are validated when
/// their value objects are constructed; digit width is validated here and the
/// remaining invariants are checked by [`BatesConfig::validate`] (also called
/// by [`build`]).
#[derive(Clone, Debug, PartialEq)]
pub struct BatesConfig {
    prefix: String,
    suffix: String,
    start: u64,
    digit_width: usize,
    increment: u64,
    anchor: BatesAnchor,
    style: BatesStyle,
    page_set: BatesPageSet,
    date_stamp: Option<BatesDateStamp>,
}

impl BatesConfig {
    /// Creates a configuration, validating the digit width.
    ///
    /// Defaults: increment 1, anchor [`BatesAnchor::BottomRight`], default
    /// style, all pages, and no date stamp.
    pub fn new(
        prefix: impl Into<String>,
        suffix: impl Into<String>,
        start: u64,
        digit_width: usize,
    ) -> Result<Self, BatesError> {
        if digit_width == 0 || digit_width > MAX_DIGIT_WIDTH {
            return Err(BatesError::InvalidDigitWidth(digit_width));
        }
        Ok(Self {
            prefix: prefix.into(),
            suffix: suffix.into(),
            start,
            digit_width,
            increment: 1,
            anchor: BatesAnchor::BottomRight,
            style: BatesStyle::default_style(),
            page_set: BatesPageSet::All,
            date_stamp: None,
        })
    }

    /// Sets the per-page increment (validated by [`Self::validate`]).
    pub fn with_increment(mut self, increment: u64) -> Self {
        self.increment = increment;
        self
    }

    /// Sets the anchor position.
    pub fn with_anchor(mut self, anchor: BatesAnchor) -> Self {
        self.anchor = anchor;
        self
    }

    /// Sets the text style.
    pub fn with_style(mut self, style: BatesStyle) -> Self {
        self.style = style;
        self
    }

    /// Sets the page selection.
    pub fn with_page_set(mut self, page_set: BatesPageSet) -> Self {
        self.page_set = page_set;
        self
    }

    /// Enables or disables the date stamp.
    pub fn with_date_stamp(mut self, date_stamp: Option<BatesDateStamp>) -> Self {
        self.date_stamp = date_stamp;
        self
    }

    /// Prefix placed before the padded number.
    pub fn prefix(&self) -> &str {
        &self.prefix
    }

    /// Suffix placed after the padded number.
    pub fn suffix(&self) -> &str {
        &self.suffix
    }

    /// First stamped number.
    pub fn start(&self) -> u64 {
        self.start
    }

    /// Fixed digit width used for zero padding.
    pub fn digit_width(&self) -> usize {
        self.digit_width
    }

    /// Per-page increment.
    pub fn increment(&self) -> u64 {
        self.increment
    }

    /// Anchor position.
    pub fn anchor(&self) -> BatesAnchor {
        self.anchor
    }

    /// Text style.
    pub fn style(&self) -> BatesStyle {
        self.style
    }

    /// Page selection.
    pub fn page_set(&self) -> &BatesPageSet {
        &self.page_set
    }

    /// Optional date stamp.
    pub fn date_stamp(&self) -> Option<&BatesDateStamp> {
        self.date_stamp.as_ref()
    }

    /// Validates the configuration invariants.
    pub fn validate(&self) -> Result<(), BatesError> {
        if self.digit_width == 0 || self.digit_width > MAX_DIGIT_WIDTH {
            return Err(BatesError::InvalidDigitWidth(self.digit_width));
        }
        if self.increment == 0 {
            return Err(BatesError::InvalidIncrement(self.increment));
        }
        if matches!(self.page_set, BatesPageSet::Alternate { every: 0, .. }) {
            return Err(BatesError::InvalidPageStride);
        }
        Ok(())
    }

    fn format_number(&self, number: u64) -> Result<String, BatesError> {
        let digits = number.to_string();
        if digits.len() > self.digit_width {
            return Err(BatesError::NumberExceedsWidth {
                number,
                digit_width: self.digit_width,
            });
        }
        let mut out = String::with_capacity(self.digit_width);
        for _ in digits.len()..self.digit_width {
            out.push('0');
        }
        out.push_str(&digits);
        Ok(out)
    }

    fn stamp_text(&self, number: u64, now_unix_seconds: i64) -> Result<String, BatesError> {
        let mut text = String::new();
        text.push_str(&self.prefix);
        text.push_str(&self.format_number(number)?);
        text.push_str(&self.suffix);
        if let Some(stamp) = &self.date_stamp {
            text.push_str(stamp.separator());
            text.push_str(&format_unix_date(now_unix_seconds, stamp.format()));
        }
        Ok(text)
    }
}

/// Visible bounds of one page, supplied by the host in display coordinates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BatesPageBounds {
    /// Zero-based page index.
    pub page_index: usize,
    /// Lower-left x coordinate.
    pub x0: f32,
    /// Lower-left y coordinate.
    pub y0: f32,
    /// Upper-right x coordinate.
    pub x1: f32,
    /// Upper-right y coordinate.
    pub y1: f32,
}

impl BatesPageBounds {
    /// Creates raw page bounds.
    ///
    /// Values are validated later by [`build`], so untrusted coordinates can be
    /// passed here without risking a panic.
    pub fn new(page_index: usize, x0: f32, y0: f32, x1: f32, y1: f32) -> Self {
        Self {
            page_index,
            x0,
            y0,
            x1,
            y1,
        }
    }

    /// Creates page bounds from an already-validated [`Rect`].
    pub fn from_rect(page_index: usize, rect: Rect) -> Self {
        Self {
            page_index,
            x0: rect.x0(),
            y0: rect.y0(),
            x1: rect.x1(),
            y1: rect.y1(),
        }
    }
}

/// One deterministic stamp in a [`BatesPlan`].
#[derive(Clone, Debug, PartialEq)]
pub struct BatesStamp {
    page_index: usize,
    number: u64,
    text: String,
    position: Rect,
    anchor: BatesAnchor,
}

impl BatesStamp {
    /// Zero-based index of the page that receives this stamp.
    pub fn page_index(&self) -> usize {
        self.page_index
    }

    /// The unformatted Bates number for this page.
    pub fn number(&self) -> u64 {
        self.number
    }

    /// Full stamp text, including prefix, suffix, and optional date.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Validated position rectangle in display coordinates.
    pub fn position(&self) -> Rect {
        self.position
    }

    /// Anchor used to compute the position rectangle.
    pub fn anchor(&self) -> BatesAnchor {
        self.anchor
    }
}

/// A deterministic, ordered set of stamps ready for a host writer.
#[derive(Clone, Debug, PartialEq)]
pub struct BatesPlan {
    stamps: Vec<BatesStamp>,
}

impl BatesPlan {
    /// Stamps in ascending page-index order.
    pub fn stamps(&self) -> &[BatesStamp] {
        &self.stamps
    }

    /// Number of stamps in the plan.
    pub fn len(&self) -> usize {
        self.stamps.len()
    }

    /// Returns `true` when the plan has no stamps.
    pub fn is_empty(&self) -> bool {
        self.stamps.is_empty()
    }

    /// Returns the stamp for `page_index`, if that page is in the plan.
    pub fn stamp_for_page(&self, page_index: usize) -> Option<&BatesStamp> {
        self.stamps.iter().find(|s| s.page_index == page_index)
    }
}

/// Deterministic text extent used to size a stamp rectangle.
///
/// Width is `chars * font_size * 0.6` and height is `font_size * 1.2`. This is a
/// conservative approximation for standard-font metrics, not an AFM-based
/// measurement; a host writer may lay out text using real metrics while still
/// consuming the same plan text and anchor.
pub fn bates_text_extent(text: &str, font_size: f32) -> (f32, f32) {
    let chars = text.chars().count() as f32;
    (
        chars * font_size * ADVANCE_RATIO,
        font_size * LINE_HEIGHT_RATIO,
    )
}

/// Builds a [`BatesPlan`] from a configuration, a page range, and a caller time.
///
/// `pages` is the range of pages considered; `config.page_set` selects which of
/// those receive a stamp. Numbers are assigned by selected ordinal in ascending
/// page-index order, so they are consecutive even when the input order is not.
/// `now_unix_seconds` is used only when a [`BatesDateStamp`] is configured.
pub fn build(
    config: &BatesConfig,
    pages: &[BatesPageBounds],
    now_unix_seconds: i64,
) -> Result<BatesPlan, BatesError> {
    config.validate()?;

    let mut selected: Vec<BatesPageBounds> = pages
        .iter()
        .copied()
        .filter(|page| config.page_set().includes(page.page_index))
        .collect();
    selected.sort_by_key(|page| page.page_index);
    selected.dedup_by_key(|page| page.page_index);
    if selected.is_empty() {
        return Err(BatesError::EmptyPageSet);
    }

    let mut stamps = Vec::with_capacity(selected.len());
    for (ordinal, page) in selected.iter().enumerate() {
        let overflow = || BatesError::NumberOverflow {
            start: config.start(),
            increment: config.increment(),
            ordinal,
        };
        let ordinal_u64 = u64::try_from(ordinal).map_err(|_| overflow())?;
        let step = config
            .increment()
            .checked_mul(ordinal_u64)
            .ok_or_else(overflow)?;
        let number = config.start().checked_add(step).ok_or_else(overflow)?;
        let text = config.stamp_text(number, now_unix_seconds)?;

        validate_bounds(page)?;
        let (text_width, text_height) = bates_text_extent(&text, config.style().font_size());
        let position = position_rect(
            config.anchor(),
            (page.x0, page.y0, page.x1, page.y1),
            config.style().margins(),
            text_width,
            text_height,
        )?;

        stamps.push(BatesStamp {
            page_index: page.page_index,
            number,
            text,
            position,
            anchor: config.anchor(),
        });
    }

    Ok(BatesPlan { stamps })
}

fn validate_bounds(page: &BatesPageBounds) -> Result<(), BatesError> {
    for (field, value) in [
        ("x0", page.x0),
        ("y0", page.y0),
        ("x1", page.x1),
        ("y1", page.y1),
    ] {
        if !value.is_finite() {
            return Err(BatesError::NonFiniteGeometry { field, value });
        }
    }
    if page.x1 <= page.x0 || page.y1 <= page.y0 {
        return Err(BatesError::InvalidPageBounds {
            page_index: page.page_index,
            x0: page.x0,
            y0: page.y0,
            x1: page.x1,
            y1: page.y1,
        });
    }
    Ok(())
}

fn position_rect(
    anchor: BatesAnchor,
    bounds: (f32, f32, f32, f32),
    margins: BatesMargins,
    text_width: f32,
    text_height: f32,
) -> Result<Rect, BatesError> {
    let (x0, y0, x1, y1) = bounds;
    let x = match anchor.column() {
        BatesColumn::Left => x0 + margins.left(),
        BatesColumn::Center => x0 + ((x1 - x0) - text_width) / 2.0,
        BatesColumn::Right => x1 - margins.right() - text_width,
    };
    let y = match anchor.row() {
        BatesRow::Top => y1 - margins.top() - text_height,
        BatesRow::Middle => y0 + ((y1 - y0) - text_height) / 2.0,
        BatesRow::Bottom => y0 + margins.bottom(),
    };
    Rect::new(x, y, x + text_width, y + text_height).map_err(BatesError::from)
}

/// Converts a Unix timestamp in UTC to a `(year, month, day)` calendar date.
///
/// Uses Howard Hinnant's civil-from-days algorithm with Euclidean division so
/// pre-1970 timestamps floor to the correct day.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let mut year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    if month <= 2 {
        year += 1;
    }
    (year, month as u32, day as u32)
}

fn format_year(year: i64) -> String {
    if year < 0 {
        format!("-{:04}", year.unsigned_abs())
    } else {
        format!("{year:04}")
    }
}

fn format_unix_date(seconds: i64, format: BatesDateFormat) -> String {
    let days = seconds.div_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let y = format_year(year);
    match format {
        BatesDateFormat::Iso8601 => format!("{y}-{month:02}-{day:02}"),
        BatesDateFormat::Compact => format!("{y}{month:02}{day:02}"),
        BatesDateFormat::UsSlash => format!("{month:02}/{day:02}/{y}"),
        BatesDateFormat::EuSlash => format!("{day:02}/{month:02}/{y}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(prefix: &str, suffix: &str, start: u64, width: usize) -> BatesConfig {
        BatesConfig::new(prefix, suffix, start, width).expect("valid test config")
    }

    fn page(index: usize) -> BatesPageBounds {
        BatesPageBounds::new(index, 0.0, 0.0, 612.0, 792.0)
    }

    fn pages(count: usize) -> Vec<BatesPageBounds> {
        (0..count).map(page).collect()
    }

    #[test]
    fn zero_padding_produces_exact_digit_width() {
        let plan = build(&config("", "", 1, 7), &pages(1), 0).expect("build");
        assert_eq!(plan.stamps()[0].text(), "0000001");

        let plan = build(&config("", "", 12_345, 5), &pages(1), 0).expect("build");
        assert_eq!(plan.stamps()[0].text(), "12345");
    }

    #[test]
    fn prefix_and_suffix_are_concatenated() {
        let plan = build(&config("ABC-", "-XYZ", 7, 4), &pages(1), 0).expect("build");
        assert_eq!(plan.stamps()[0].text(), "ABC-0007-XYZ");
    }

    #[test]
    fn start_and_increment_math() {
        let cfg = config("", "", 100, 4).with_increment(5);
        let plan = build(&cfg, &pages(3), 0).expect("build");
        let numbers: Vec<u64> = plan.stamps().iter().map(|s| s.number()).collect();
        assert_eq!(numbers, vec![100, 105, 110]);
    }

    #[test]
    fn number_overflow_is_rejected() {
        let cfg = config("", "", u64::MAX - 1, 20).with_increment(10);
        let err = build(&cfg, &pages(3), 0).expect_err("overflow must fail");
        assert!(matches!(err, BatesError::NumberOverflow { .. }), "{err:?}");
    }

    #[test]
    fn number_exceeding_digit_width_is_rejected() {
        let err = build(&config("", "", 1000, 3), &pages(1), 0).expect_err("width overflow");
        assert_eq!(
            err,
            BatesError::NumberExceedsWidth {
                number: 1000,
                digit_width: 3
            }
        );
    }

    #[test]
    fn invalid_digit_width_and_increment_are_rejected() {
        assert_eq!(
            BatesConfig::new("", "", 1, 0),
            Err(BatesError::InvalidDigitWidth(0))
        );
        assert_eq!(
            BatesConfig::new("", "", 1, MAX_DIGIT_WIDTH + 1),
            Err(BatesError::InvalidDigitWidth(MAX_DIGIT_WIDTH + 1))
        );
        let cfg = config("", "", 1, 4).with_increment(0);
        assert_eq!(cfg.validate(), Err(BatesError::InvalidIncrement(0)));
    }

    #[test]
    fn invalid_margin_font_size_and_color_are_rejected() {
        assert!(matches!(
            BatesColor::new(0.0, -0.1, 0.0),
            Err(BatesError::InvalidColorChannel { field: "g", .. })
        ));
        assert!(matches!(
            BatesColor::new(f32::NAN, 0.0, 0.0),
            Err(BatesError::InvalidColorChannel { field: "r", .. })
        ));
        assert!(matches!(
            BatesMargins::new(-1.0, 0.0, 0.0, 0.0),
            Err(BatesError::InvalidMargin { field: "left", .. })
        ));
        assert!(matches!(
            BatesStyle::new(
                StandardFont::Helvetica,
                0.0,
                BatesColor::BLACK,
                BatesMargins::default()
            ),
            Err(BatesError::InvalidFontSize(_))
        ));
        assert!(matches!(
            BatesStyle::new(
                StandardFont::Helvetica,
                f32::INFINITY,
                BatesColor::BLACK,
                BatesMargins::default()
            ),
            Err(BatesError::InvalidFontSize(_))
        ));
    }

    #[test]
    fn all_nine_anchors_produce_expected_positions() {
        assert_eq!(BatesAnchor::ALL.len(), 9);
        let bounds = page(0);
        for anchor in BatesAnchor::ALL {
            let cfg = config("", "", 1, 3).with_anchor(anchor);
            let plan = build(&cfg, &[bounds], 0).expect("build");
            let rect = plan.stamps()[0].position();
            let margins = cfg.style().margins();

            match anchor.column() {
                BatesColumn::Left => assert_eq!(rect.x0(), margins.left(), "{anchor:?}"),
                BatesColumn::Center => {
                    assert!(
                        ((rect.x0() + rect.x1()) / 2.0 - 306.0).abs() < 0.001,
                        "{anchor:?}"
                    )
                }
                BatesColumn::Right => {
                    assert_eq!(rect.x1(), 612.0 - margins.right(), "{anchor:?}")
                }
            }
            match anchor.row() {
                BatesRow::Top => assert_eq!(rect.y1(), 792.0 - margins.top(), "{anchor:?}"),
                BatesRow::Middle => {
                    assert!(
                        ((rect.y0() + rect.y1()) / 2.0 - 396.0).abs() < 0.001,
                        "{anchor:?}"
                    )
                }
                BatesRow::Bottom => assert_eq!(rect.y0(), margins.bottom(), "{anchor:?}"),
            }
        }
    }

    #[test]
    fn stamps_are_deterministically_ordered_by_page_index() {
        let cfg = config("", "", 1, 3);
        let mut unordered = pages(3);
        unordered.reverse();
        let plan = build(&cfg, &unordered, 0).expect("build");
        let indices: Vec<usize> = plan.stamps().iter().map(|s| s.page_index()).collect();
        assert_eq!(indices, vec![0, 1, 2]);
        let numbers: Vec<u64> = plan.stamps().iter().map(|s| s.number()).collect();
        assert_eq!(numbers, vec![1, 2, 3]);
        assert_eq!(plan, build(&cfg, &unordered, 0).expect("rebuild"));
    }

    #[test]
    fn empty_page_set_is_rejected() {
        let cfg = config("", "", 1, 3);
        assert_eq!(
            build(&cfg, &[], 0).expect_err("empty range"),
            BatesError::EmptyPageSet
        );

        let missing = config("", "", 1, 3).with_page_set(BatesPageSet::Indices(vec![9]));
        assert_eq!(
            build(&missing, &pages(2), 0).expect_err("no match"),
            BatesError::EmptyPageSet
        );

        let even = config("", "", 1, 3).with_page_set(BatesPageSet::Even);
        assert_eq!(
            build(&even, &pages(1), 0).expect_err("no even page"),
            BatesError::EmptyPageSet
        );
    }

    #[test]
    fn date_stamp_formats_caller_time() {
        let cases = [
            (BatesDateFormat::Iso8601, "2020-09-13"),
            (BatesDateFormat::Compact, "20200913"),
            (BatesDateFormat::UsSlash, "09/13/2020"),
            (BatesDateFormat::EuSlash, "13/09/2020"),
        ];
        for (format, expected) in cases {
            let cfg = config("", "", 1, 3).with_date_stamp(Some(BatesDateStamp::new(format)));
            let plan = build(&cfg, &pages(1), 1_600_000_000).expect("build");
            assert!(plan.stamps()[0].text().ends_with(expected), "{format:?}");
        }

        let cfg = config("", "", 1, 3)
            .with_date_stamp(Some(BatesDateStamp::new(BatesDateFormat::Iso8601)));
        assert!(
            build(&cfg, &pages(1), 0).expect("epoch").stamps()[0]
                .text()
                .ends_with("1970-01-01")
        );
        assert!(
            build(&cfg, &pages(1), -1).expect("pre-epoch").stamps()[0]
                .text()
                .ends_with("1969-12-31")
        );
    }

    #[test]
    fn date_stamp_is_appended_after_suffix_with_separator() {
        let cfg = config("P", "S", 1, 4).with_date_stamp(Some(
            BatesDateStamp::new(BatesDateFormat::Iso8601).with_separator(" @ "),
        ));
        let plan = build(&cfg, &pages(1), 1_600_000_000).expect("build");
        assert_eq!(plan.stamps()[0].text(), "P0001S @ 2020-09-13");
    }

    #[test]
    fn non_finite_and_inverted_geometry_is_rejected() {
        let cfg = config("", "", 1, 3);
        let nan = BatesPageBounds::new(0, f32::NAN, 0.0, 612.0, 792.0);
        assert!(matches!(
            build(&cfg, &[nan], 0),
            Err(BatesError::NonFiniteGeometry { field: "x0", .. })
        ));
        let inf = BatesPageBounds::new(0, 0.0, 0.0, f32::INFINITY, 792.0);
        assert!(matches!(
            build(&cfg, &[inf], 0),
            Err(BatesError::NonFiniteGeometry { field: "x1", .. })
        ));
        let inverted = BatesPageBounds::new(0, 10.0, 0.0, 5.0, 792.0);
        assert!(matches!(
            build(&cfg, &[inverted], 0),
            Err(BatesError::InvalidPageBounds { page_index: 0, .. })
        ));
    }

    #[test]
    fn page_set_selectors_pick_expected_pages() {
        let numbers = |set: BatesPageSet| -> Vec<usize> {
            let cfg = config("", "", 1, 3).with_page_set(set);
            build(&cfg, &pages(6), 0)
                .expect("build")
                .stamps()
                .iter()
                .map(|s| s.page_index())
                .collect()
        };
        assert_eq!(numbers(BatesPageSet::All), vec![0, 1, 2, 3, 4, 5]);
        assert_eq!(numbers(BatesPageSet::Odd), vec![0, 2, 4]);
        assert_eq!(numbers(BatesPageSet::Even), vec![1, 3, 5]);
        assert_eq!(
            numbers(BatesPageSet::Alternate {
                every: 2,
                offset: 1
            }),
            vec![1, 3, 5]
        );
        assert_eq!(numbers(BatesPageSet::Indices(vec![4, 2])), vec![2, 4]);

        let bad_stride = config("", "", 1, 3).with_page_set(BatesPageSet::Alternate {
            every: 0,
            offset: 0,
        });
        assert_eq!(
            build(&bad_stride, &pages(2), 0).expect_err("zero stride"),
            BatesError::InvalidPageStride
        );
    }

    #[test]
    fn plan_and_stamp_accessors_expose_model_data() {
        let cfg = config("DOC-", "", 1, 6);
        let plan = build(&cfg, &pages(3), 0).expect("build");
        assert_eq!(plan.len(), 3);
        assert!(!plan.is_empty());
        assert!(plan.stamp_for_page(1).is_some());
        assert!(plan.stamp_for_page(99).is_none());

        let stamp = &plan.stamps()[0];
        assert_eq!(stamp.page_index(), 0);
        assert_eq!(stamp.number(), 1);
        assert_eq!(stamp.text(), "DOC-000001");
        assert_eq!(stamp.anchor(), cfg.anchor());
        assert!(stamp.position().width() > 0.0);
    }
}
