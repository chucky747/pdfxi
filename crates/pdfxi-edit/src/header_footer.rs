//! Typed header, footer, and page-numbering model.
//!
//! This module is renderer-agnostic and dependency-free. It describes a
//! header/footer layout as six independent slots, substitutes tokens for each
//! page, and renders a self-contained PDF content-stream overlay. The overlay
//! is *additive*: existing page content is never parsed or mutated, and
//! [`compose_content`] returns a fresh byte vector rather than editing the
//! caller's buffer in place.
//!
//! # Coordinate model
//!
//! Header and footer positions are expressed in **display coordinates**: the
//! origin is the lower-left corner of the page as it appears to a reader, with
//! `x` growing right and `y` growing up. [`transform_for_page`] produces the
//! [`ContentMatrix`] that maps those display coordinates into the page's
//! unrotated default user space, accounting for both a non-zero MediaBox
//! origin and the page's `/Rotate` value (0, 90, 180, or 270 degrees).
//!
//! # Text encoding
//!
//! Overlay text targets the standard 14 PDF fonts, whose built-in encodings are
//! single-byte (WinAnsi, or the Symbol and ZapfDingbats encodings).
//! [`escape_literal`] turns a Rust string into a valid PDF literal string:
//! delimiters and common control characters use named escapes, and **every
//! byte outside printable ASCII is emitted as a three-digit octal escape of
//! its raw UTF-8 byte**. For example, `é` (U+00E9, UTF-8 `C3 A9`) becomes
//! `\303\251`. The result is deterministic and ASCII-only.
//!
//! This crate has no font or encoding dependency, so it does **not** translate
//! UTF-8 into WinAnsi. Consumers must either supply WinAnsi-compatible
//! single-byte text, write the overlay as a resource whose encoding is
//! UTF-8-capable (for example a composite font using `/Encoding /Identity-H`),
//! or pre-encode the text themselves. Passing arbitrary non-ASCII Unicode with
//! a standard font yields a syntactically valid stream, but the rendered
//! glyphs will not match the source Unicode.

use core::fmt;
use std::collections::BTreeMap;

use crate::geometry::{GeometryError, PageGeometry, PageIndex};

/// Errors produced while validating or rendering a [`HeaderFooterLayout`].
#[derive(Debug, Clone, PartialEq)]
pub enum HeaderFooterError {
    /// Font size was not finite, was non-positive, or exceeded 72 points.
    InvalidFontSize(f32),
    /// A margin was not finite or was negative.
    InvalidMargin {
        /// Name of the offending margin.
        field: &'static str,
        /// The rejected value.
        value: f32,
    },
    /// A page-numbering start value was less than one.
    InvalidNumberingStart(i64),
    /// A page-numbering range would overflow `i64`.
    NumberingRangeOverflow {
        /// Requested first page number.
        start: i64,
        /// Number of pages in the range.
        page_count: usize,
    },
    /// The requested page index was outside the document.
    PageOutOfRange {
        /// Rejected zero-based page index.
        index: usize,
        /// Number of pages in the document.
        page_count: usize,
    },
    /// An alternate page selection had a zero stride.
    InvalidAlternateStride,
    /// A page-set input and its overlay list had different lengths.
    PageSetLengthMismatch {
        /// Number of page content buffers supplied.
        pages: usize,
        /// Number of overlays supplied.
        overlays: usize,
    },
    /// A page index could not be represented as an `i64` offset, or adding it
    /// to the configured numbering start would overflow `i64`.
    PageIndexOutOfRange {
        /// The rejected zero-based page index.
        index: usize,
        /// Configured first page number.
        start: i64,
    },
    /// An underlying geometry value was invalid.
    Geometry(GeometryError),
}

impl fmt::Display for HeaderFooterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidFontSize(value) => {
                write!(
                    f,
                    "font size must be finite, positive, and at most 72 points, got {value}"
                )
            }
            Self::InvalidMargin { field, value } => {
                write!(
                    f,
                    "margin `{field}` must be finite and non-negative, got {value}"
                )
            }
            Self::InvalidNumberingStart(value) => {
                write!(f, "page numbering start must be at least 1, got {value}")
            }
            Self::NumberingRangeOverflow { start, page_count } => {
                write!(
                    f,
                    "page numbering range starting at {start} over {page_count} pages overflows"
                )
            }
            Self::PageOutOfRange { index, page_count } => {
                write!(
                    f,
                    "page index {index} is outside a document of {page_count} pages"
                )
            }
            Self::InvalidAlternateStride => {
                write!(f, "alternate page selection stride must be at least 1")
            }
            Self::PageSetLengthMismatch { pages, overlays } => {
                write!(
                    f,
                    "page set has {pages} pages but {overlays} overlays were supplied"
                )
            }
            Self::PageIndexOutOfRange { index, start } => {
                write!(
                    f,
                    "page index {index} cannot be numbered from start {start} without overflowing"
                )
            }
            Self::Geometry(err) => write!(f, "header/footer geometry error: {err}"),
        }
    }
}

impl std::error::Error for HeaderFooterError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Geometry(err) => Some(err),
            _ => None,
        }
    }
}

impl From<GeometryError> for HeaderFooterError {
    fn from(err: GeometryError) -> Self {
        Self::Geometry(err)
    }
}

/// One of the fourteen standard PDF base fonts.
///
/// These are the only font families permitted in an overlay: standard fonts
/// require no embedded font program and are therefore safe for a
/// dependency-free writer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum StandardFont {
    /// Courier.
    Courier,
    /// Courier Bold.
    CourierBold,
    /// Courier Oblique.
    CourierOblique,
    /// Courier Bold Oblique.
    CourierBoldOblique,
    /// Helvetica.
    Helvetica,
    /// Helvetica Bold.
    HelveticaBold,
    /// Helvetica Oblique.
    HelveticaOblique,
    /// Helvetica Bold Oblique.
    HelveticaBoldOblique,
    /// Times Roman.
    TimesRoman,
    /// Times Bold.
    TimesBold,
    /// Times Italic.
    TimesItalic,
    /// Times Bold Italic.
    TimesBoldItalic,
    /// Symbol.
    Symbol,
    /// ZapfDingbats.
    ZapfDingbats,
}

impl StandardFont {
    /// Returns the PDF base-font name for this font.
    pub fn base_name(self) -> &'static str {
        match self {
            Self::Courier => "Courier",
            Self::CourierBold => "Courier-Bold",
            Self::CourierOblique => "Courier-Oblique",
            Self::CourierBoldOblique => "Courier-BoldOblique",
            Self::Helvetica => "Helvetica",
            Self::HelveticaBold => "Helvetica-Bold",
            Self::HelveticaOblique => "Helvetica-Oblique",
            Self::HelveticaBoldOblique => "Helvetica-BoldOblique",
            Self::TimesRoman => "Times-Roman",
            Self::TimesBold => "Times-Bold",
            Self::TimesItalic => "Times-Italic",
            Self::TimesBoldItalic => "Times-BoldItalic",
            Self::Symbol => "Symbol",
            Self::ZapfDingbats => "ZapfDingbats",
        }
    }

    /// Returns a coarse average glyph advance, as a fraction of the font size.
    ///
    /// This is used only to approximate text width for centering and
    /// right-alignment; it is deterministic but is not a substitute for AFM
    /// metrics.
    pub fn average_advance(self) -> f32 {
        match self {
            Self::Courier | Self::CourierBold | Self::CourierOblique | Self::CourierBoldOblique => {
                0.6
            }
            _ => 0.5,
        }
    }
}

impl fmt::Display for StandardFont {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.base_name())
    }
}

/// An opaque RGB color for overlay text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TextColor {
    r: u8,
    g: u8,
    b: u8,
}

impl TextColor {
    /// Opaque black, the default text color.
    pub const BLACK: TextColor = TextColor { r: 0, g: 0, b: 0 };

    /// Creates an RGB color from 8-bit channels.
    pub fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }

    /// Red channel.
    pub fn r(self) -> u8 {
        self.r
    }

    /// Green channel.
    pub fn g(self) -> u8 {
        self.g
    }

    /// Blue channel.
    pub fn b(self) -> u8 {
        self.b
    }
}

/// Font family, size, and color for a slot's text.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TextStyle {
    font: StandardFont,
    font_size: f32,
    color: TextColor,
}

impl TextStyle {
    /// Creates a text style, validating the font size.
    pub fn new(
        font: StandardFont,
        font_size: f32,
        color: TextColor,
    ) -> Result<Self, HeaderFooterError> {
        if !font_size.is_finite() || font_size <= 0.0 || font_size > 72.0 {
            return Err(HeaderFooterError::InvalidFontSize(font_size));
        }
        Ok(Self {
            font,
            font_size,
            color,
        })
    }

    /// Helvetica at 10 points in black.
    pub fn default_style() -> Self {
        Self {
            font: StandardFont::Helvetica,
            font_size: 10.0,
            color: TextColor::BLACK,
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
    pub fn color(self) -> TextColor {
        self.color
    }
}

/// Margins, in points, from the visible page edges to the text.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Margins {
    left: f32,
    right: f32,
    top: f32,
    bottom: f32,
}

impl Margins {
    /// Creates margins, rejecting non-finite or negative values.
    pub fn new(left: f32, right: f32, top: f32, bottom: f32) -> Result<Self, HeaderFooterError> {
        for (field, value) in [
            ("left", left),
            ("right", right),
            ("top", top),
            ("bottom", bottom),
        ] {
            if !value.is_finite() || value < 0.0 {
                return Err(HeaderFooterError::InvalidMargin { field, value });
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
    pub fn uniform(value: f32) -> Result<Self, HeaderFooterError> {
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

impl Default for Margins {
    fn default() -> Self {
        Self {
            left: 36.0,
            right: 36.0,
            top: 36.0,
            bottom: 36.0,
        }
    }
}

/// Which pages a header/footer is drawn on, based on parity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PageSelection {
    /// Every page.
    All,
    /// Only odd-numbered pages (display positions 1, 3, 5, ...).
    Odd,
    /// Only even-numbered pages (display positions 2, 4, 6, ...).
    Even,
    /// Every `every`-th page, starting at the zero-based `offset` page.
    ///
    /// `every` must be at least 1; a zero stride is rejected by
    /// [`HeaderFooterLayout::validate`] and treated as "no pages" by the
    /// infallible [`PageFilter::includes`] so it can never divide by zero.
    Alternate {
        /// Stride between selected pages (must be at least 1).
        every: usize,
        /// Zero-based index of the first selected page.
        offset: usize,
    },
}

/// First-page and odd/even suppression rules.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PageFilter {
    /// When `true`, the first page of the document receives no overlay.
    pub suppress_first_page: bool,
    /// Parity-based page selection.
    pub selection: PageSelection,
}

impl PageFilter {
    /// Creates a filter that draws on every page.
    pub const fn all() -> Self {
        Self {
            suppress_first_page: false,
            selection: PageSelection::All,
        }
    }

    /// Creates a filter that draws on every `every`-th page starting at
    /// `offset`.
    ///
    /// A zero stride is accepted here but selects no pages (and is rejected by
    /// [`HeaderFooterLayout::validate`]); this keeps the infallible
    /// [`Self::includes`] free of a divide-by-zero panic on malformed input.
    pub const fn alternate(every: usize, offset: usize) -> Self {
        Self {
            suppress_first_page: false,
            selection: PageSelection::Alternate { every, offset },
        }
    }

    /// Returns `true` when a slot is drawn on the zero-based `page_index`.
    pub fn includes(self, page_index: usize) -> bool {
        if self.suppress_first_page && page_index == 0 {
            return false;
        }
        let one_based = page_index + 1;
        match self.selection {
            PageSelection::All => true,
            PageSelection::Odd => one_based % 2 == 1,
            PageSelection::Even => one_based.is_multiple_of(2),
            PageSelection::Alternate { every, offset } => {
                every > 0 && page_index >= offset && (page_index - offset).is_multiple_of(every)
            }
        }
    }
}

impl Default for PageFilter {
    fn default() -> Self {
        Self::all()
    }
}

/// A substitution token used inside a slot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Token {
    /// The formatted current page number (including prefix and suffix).
    Page,
    /// The total number of pages, as an Arabic decimal integer.
    Pages,
    /// A caller-supplied, preformatted date string.
    Date,
    /// A caller-supplied document title.
    Title,
    /// The caller-supplied source file name of the document.
    FileName,
    /// Verbatim literal text.
    Literal(String),
}

/// A single slot's tokens and optional style override.
#[derive(Clone, Debug, PartialEq)]
pub struct SlotContent {
    /// Tokens rendered left to right.
    pub tokens: Vec<Token>,
    /// Style override; when `None`, the layout's default style is used.
    pub style: Option<TextStyle>,
}

impl SlotContent {
    /// Creates slot content with no style override.
    pub fn new(tokens: Vec<Token>) -> Self {
        Self {
            tokens,
            style: None,
        }
    }

    /// Creates slot content with an explicit style override.
    pub fn with_style(tokens: Vec<Token>, style: TextStyle) -> Self {
        Self {
            tokens,
            style: Some(style),
        }
    }
}

/// The six independent header and footer slots.
///
/// The declaration order is also the deterministic render order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Slot {
    /// Upper-left header.
    HeaderLeft,
    /// Upper-center header.
    HeaderCenter,
    /// Upper-right header.
    HeaderRight,
    /// Lower-left footer.
    FooterLeft,
    /// Lower-center footer.
    FooterCenter,
    /// Lower-right footer.
    FooterRight,
}

impl Slot {
    /// All six slots in deterministic render order.
    pub const ALL: [Slot; 6] = [
        Self::HeaderLeft,
        Self::HeaderCenter,
        Self::HeaderRight,
        Self::FooterLeft,
        Self::FooterCenter,
        Self::FooterRight,
    ];

    /// Returns `true` when this slot belongs to the header band.
    pub fn is_header(self) -> bool {
        matches!(
            self,
            Self::HeaderLeft | Self::HeaderCenter | Self::HeaderRight
        )
    }
}

/// Page-number notation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NumberFormat {
    /// Arabic decimal (`1`, `2`, `3`).
    Arabic,
    /// Lowercase Roman numerals (`i`, `ii`, `iii`).
    RomanLower,
    /// Uppercase Roman numerals (`I`, `II`, `III`).
    RomanUpper,
    /// Lowercase alphabetic (`a`, `b`, ..., `z`, `aa`).
    AlphaLower,
    /// Uppercase alphabetic (`A`, `B`, ..., `Z`, `AA`).
    AlphaUpper,
}

/// Page-numbering configuration.
///
/// The displayed number on page `i` (zero-based) is `start + i`, formatted
/// with [`NumberFormat`] and wrapped in `prefix`/`suffix`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PageNumbering {
    start: i64,
    format: NumberFormat,
    prefix: String,
    suffix: String,
}

impl PageNumbering {
    /// Creates a numbering configuration with the given start value.
    ///
    /// The start must be at least 1 so that Roman and alphabetic formats stay
    /// well-defined.
    pub fn new(start: i64) -> Result<Self, HeaderFooterError> {
        if start < 1 {
            return Err(HeaderFooterError::InvalidNumberingStart(start));
        }
        Ok(Self {
            start,
            format: NumberFormat::Arabic,
            prefix: String::new(),
            suffix: String::new(),
        })
    }

    /// Creates the default configuration: Arabic numbering starting at 1.
    pub fn arabic() -> Self {
        Self {
            start: 1,
            format: NumberFormat::Arabic,
            prefix: String::new(),
            suffix: String::new(),
        }
    }

    /// Sets the notation format.
    pub fn with_format(mut self, format: NumberFormat) -> Self {
        self.format = format;
        self
    }

    /// Sets a prefix placed before the formatted number.
    pub fn with_prefix(mut self, prefix: impl Into<String>) -> Self {
        self.prefix = prefix.into();
        self
    }

    /// Sets a suffix placed after the formatted number.
    pub fn with_suffix(mut self, suffix: impl Into<String>) -> Self {
        self.suffix = suffix.into();
        self
    }

    /// First displayed number.
    pub fn start(self) -> i64 {
        self.start
    }

    /// Notation format.
    pub fn format(self) -> NumberFormat {
        self.format
    }

    /// Prefix string.
    pub fn prefix(&self) -> &str {
        &self.prefix
    }

    /// Suffix string.
    pub fn suffix(&self) -> &str {
        &self.suffix
    }

    /// Validates that a range of `page_count` pages does not overflow.
    pub fn validate_range(&self, page_count: usize) -> Result<(), HeaderFooterError> {
        let span =
            i64::try_from(page_count).map_err(|_| HeaderFooterError::NumberingRangeOverflow {
                start: self.start,
                page_count,
            })?;
        self.start.checked_add(span.saturating_sub(1)).ok_or(
            HeaderFooterError::NumberingRangeOverflow {
                start: self.start,
                page_count,
            },
        )?;
        Ok(())
    }

    /// Returns the number displayed on the zero-based page `page_index`.
    ///
    /// This is a checked API: it returns [`HeaderFooterError::PageIndexOutOfRange`]
    /// when `page_index` cannot be represented as an `i64` offset, or when
    /// adding it to [`Self::start`] would overflow `i64`. No saturation or
    /// wrapping is performed, so an invalid index can never silently produce
    /// the number for a different page.
    pub fn number_for(&self, page_index: usize) -> Result<String, HeaderFooterError> {
        let overflow = || HeaderFooterError::PageIndexOutOfRange {
            index: page_index,
            start: self.start,
        };
        let index = i64::try_from(page_index).map_err(|_| overflow())?;
        let value = self.start.checked_add(index).ok_or_else(overflow)?;
        Ok(format!(
            "{}{}{}",
            self.prefix,
            format_number(value, self.format),
            self.suffix
        ))
    }
}

impl Default for PageNumbering {
    fn default() -> Self {
        Self::arabic()
    }
}

/// Formats a positive integer in the requested notation.
fn format_number(value: i64, format: NumberFormat) -> String {
    if value < 1 {
        return value.to_string();
    }
    match format {
        NumberFormat::Arabic => value.to_string(),
        NumberFormat::RomanLower => to_roman(value).to_lowercase(),
        NumberFormat::RomanUpper => to_roman(value),
        NumberFormat::AlphaLower => to_alpha(value).to_lowercase(),
        NumberFormat::AlphaUpper => to_alpha(value),
    }
}

/// Converts a positive integer to uppercase Roman numerals.
fn to_roman(mut value: i64) -> String {
    const TABLE: [(i64, &str); 13] = [
        (1000, "M"),
        (900, "CM"),
        (500, "D"),
        (400, "CD"),
        (100, "C"),
        (90, "XC"),
        (50, "L"),
        (40, "XL"),
        (10, "X"),
        (9, "IX"),
        (5, "V"),
        (4, "IV"),
        (1, "I"),
    ];
    let mut out = String::new();
    for (amount, glyph) in TABLE {
        while value >= amount {
            out.push_str(glyph);
            value -= amount;
        }
    }
    out
}

/// Converts a positive integer to uppercase Excel-style column letters.
fn to_alpha(mut value: i64) -> String {
    let mut digits = Vec::new();
    while value > 0 {
        let rem = (value - 1) % 26;
        if let Some(byte) = u8::try_from(rem)
            .ok()
            .and_then(|r| char::from_u32(u32::from(b'A') + u32::from(r)))
        {
            digits.push(byte);
        }
        value = (value - 1) / 26;
    }
    digits.iter().rev().collect()
}

/// Values supplied by the caller for a single page's token substitution.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PageContext {
    /// Zero-based index of the page being rendered.
    pub page_index: usize,
    /// Total number of pages in the document.
    pub page_count: usize,
    /// Document title for [`Token::Title`].
    pub title: String,
    /// Preformatted date string for [`Token::Date`].
    pub date: String,
    /// Source file name for [`Token::FileName`].
    pub filename: String,
}

impl PageContext {
    /// Creates a page context with an empty file name.
    pub fn new(
        page_index: usize,
        page_count: usize,
        title: impl Into<String>,
        date: impl Into<String>,
    ) -> Self {
        Self {
            page_index,
            page_count,
            title: title.into(),
            date: date.into(),
            filename: String::new(),
        }
    }

    /// Sets the source file name used by [`Token::FileName`].
    pub fn with_filename(mut self, filename: impl Into<String>) -> Self {
        self.filename = filename.into();
        self
    }
}

/// Document-scoped values shared by every page of a [`render_page_set`] call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PageSetContext {
    /// Document title substituted for [`Token::Title`].
    pub title: String,
    /// Preformatted date string substituted for [`Token::Date`].
    pub date: String,
    /// Source file name substituted for [`Token::FileName`].
    pub filename: String,
}

impl PageSetContext {
    /// Creates a page-set context.
    pub fn new(
        title: impl Into<String>,
        date: impl Into<String>,
        filename: impl Into<String>,
    ) -> Self {
        Self {
            title: title.into(),
            date: date.into(),
            filename: filename.into(),
        }
    }
}

/// Complete header/footer layout.
#[derive(Clone, Debug, PartialEq)]
pub struct HeaderFooterLayout {
    slots: BTreeMap<Slot, SlotContent>,
    style: TextStyle,
    margins: Margins,
    filter: PageFilter,
    numbering: PageNumbering,
}

impl HeaderFooterLayout {
    /// Creates an empty layout with default style, margins, filter, and
    /// numbering.
    pub fn new() -> Self {
        Self {
            slots: BTreeMap::new(),
            style: TextStyle::default_style(),
            margins: Margins::default(),
            filter: PageFilter::default(),
            numbering: PageNumbering::default(),
        }
    }

    /// Assigns content to a slot.
    pub fn with_slot(mut self, slot: Slot, content: SlotContent) -> Self {
        self.slots.insert(slot, content);
        self
    }

    /// Sets the default text style used by slots without an override.
    pub fn with_style(mut self, style: TextStyle) -> Self {
        self.style = style;
        self
    }

    /// Sets the margins.
    pub fn with_margins(mut self, margins: Margins) -> Self {
        self.margins = margins;
        self
    }

    /// Sets the suppression filter.
    pub fn with_filter(mut self, filter: PageFilter) -> Self {
        self.filter = filter;
        self
    }

    /// Sets the page-numbering configuration.
    pub fn with_numbering(mut self, numbering: PageNumbering) -> Self {
        self.numbering = numbering;
        self
    }

    /// Content assigned to `slot`, if any.
    pub fn slot(&self, slot: Slot) -> Option<&SlotContent> {
        self.slots.get(&slot)
    }

    /// Default text style.
    pub fn style(&self) -> TextStyle {
        self.style
    }

    /// Margins.
    pub fn margins(&self) -> Margins {
        self.margins
    }

    /// Suppression filter.
    pub fn filter(&self) -> PageFilter {
        self.filter
    }

    /// Page-numbering configuration.
    pub fn numbering(&self) -> &PageNumbering {
        &self.numbering
    }

    /// Validates every configurable value in the layout.
    pub fn validate(&self) -> Result<(), HeaderFooterError> {
        validate_style(self.style)?;
        for content in self.slots.values() {
            if let Some(style) = content.style {
                validate_style(style)?;
            }
        }
        for (field, value) in [
            ("left", self.margins.left),
            ("right", self.margins.right),
            ("top", self.margins.top),
            ("bottom", self.margins.bottom),
        ] {
            if !value.is_finite() || value < 0.0 {
                return Err(HeaderFooterError::InvalidMargin { field, value });
            }
        }
        if self.numbering.start < 1 {
            return Err(HeaderFooterError::InvalidNumberingStart(
                self.numbering.start,
            ));
        }
        if let PageSelection::Alternate { every, .. } = self.filter.selection
            && every == 0
        {
            return Err(HeaderFooterError::InvalidAlternateStride);
        }
        Ok(())
    }
}

impl Default for HeaderFooterLayout {
    fn default() -> Self {
        Self::new()
    }
}

fn validate_style(style: TextStyle) -> Result<(), HeaderFooterError> {
    if !style.font_size.is_finite() || style.font_size <= 0.0 || style.font_size > 72.0 {
        return Err(HeaderFooterError::InvalidFontSize(style.font_size));
    }
    Ok(())
}

/// A PDF content-stream transformation matrix.
///
/// The six coefficients match the order of the PDF `cm` operator: a point
/// `(x, y)` maps to `(a*x + c*y + e, b*x + d*y + f)`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ContentMatrix {
    a: f32,
    b: f32,
    c: f32,
    d: f32,
    e: f32,
    f: f32,
}

impl ContentMatrix {
    /// Creates a matrix from its six coefficients.
    pub fn new(a: f32, b: f32, c: f32, d: f32, e: f32, f: f32) -> Self {
        Self { a, b, c, d, e, f }
    }

    /// Returns the coefficients in `cm` order.
    pub fn values(self) -> [f32; 6] {
        [self.a, self.b, self.c, self.d, self.e, self.f]
    }

    /// Formats the matrix as the operand string for a PDF `cm` operator.
    pub fn to_pdf(self) -> String {
        let [a, b, c, d, e, f] = self.values();
        format!("{} {} {} {} {} {}", a, b, c, d, e, f)
    }
}

/// Returns the displayed width and height of a page after `/Rotate`.
///
/// Rotation by 90 or 270 degrees swaps the width and height of the MediaBox.
pub fn display_dimensions(page: &PageGeometry) -> (f32, f32) {
    let box_rect = page.media_box();
    let width = box_rect.width();
    let height = box_rect.height();
    match page.rotation_degrees() {
        90 | 270 => (height, width),
        _ => (width, height),
    }
}

/// Builds the matrix that maps display coordinates into unrotated user space.
///
/// The matrix accounts for the MediaBox origin `(x0, y0)` and the page's
/// rotation. Its translation places display `(0, 0)` at the correct corner of
/// the rotated MediaBox, and its linear part keeps text horizontal on screen.
pub fn transform_for_page(page: &PageGeometry) -> ContentMatrix {
    let m = page.media_box();
    let (x0, y0, x1, y1) = (m.x0(), m.y0(), m.x1(), m.y1());
    match page.rotation_degrees() {
        0 => ContentMatrix::new(1.0, 0.0, 0.0, 1.0, x0, y0),
        90 => ContentMatrix::new(0.0, 1.0, -1.0, 0.0, x1, y0),
        180 => ContentMatrix::new(-1.0, 0.0, 0.0, -1.0, x1, y1),
        270 => ContentMatrix::new(0.0, -1.0, 1.0, 0.0, x0, y1),
        // `PageGeometry` validates rotation, so this arm is unreachable for a
        // correctly constructed page; it is a safe fallback rather than a panic.
        _ => ContentMatrix::new(1.0, 0.0, 0.0, 1.0, x0, y0),
    }
}

/// Escapes a Rust string into a PDF literal string body (without delimiters).
///
/// Backslashes, parentheses, and common control characters use PDF named
/// escape sequences. Every other byte outside printable ASCII is emitted as a
/// three-digit octal escape of its **raw UTF-8 byte**: `é` (UTF-8 `C3 A9`)
/// becomes `\303\251`. The result is deterministic and ASCII-only.
///
/// This function does not translate UTF-8 to WinAnsi. With the standard 14
/// fonts, callers must supply WinAnsi-compatible text, use a UTF-8-capable
/// encoding, or pre-encode the text; otherwise the glyphs will not match the
/// source Unicode even though the stream is valid PDF. See the [module docs]
/// for the full encoding contract.
///
/// [module docs]: crate::header_footer#text-encoding
pub fn escape_literal(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for byte in input.as_bytes() {
        match *byte {
            b'\\' => out.push_str("\\\\"),
            b'(' => out.push_str("\\("),
            b')' => out.push_str("\\)"),
            b'\n' => out.push_str("\\n"),
            b'\r' => out.push_str("\\r"),
            b'\t' => out.push_str("\\t"),
            0x08 => out.push_str("\\b"),
            0x0c => out.push_str("\\f"),
            0x20..=0x7e => out.push(*byte as char),
            other => {
                // Formatting an octal escape with `{:03o}` yields three digits,
                // keeping the output unambiguous for the following character.
                out.push_str(&format!("\\{:03o}", other));
            }
        }
    }
    out
}

/// Approximates the width of rendered text in points.
fn approximate_width(text: &str, style: TextStyle) -> f32 {
    let chars = text.chars().count() as f32;
    chars * style.font_size * style.font.average_advance()
}

/// Substitutes all tokens in a slot into a single display string.
///
/// Propagates the checked numbering error from [`PageNumbering::number_for`]
/// rather than saturating an invalid page index.
fn substitute(
    tokens: &[Token],
    context: &PageContext,
    numbering: &PageNumbering,
) -> Result<String, HeaderFooterError> {
    let mut out = String::new();
    for token in tokens {
        match token {
            Token::Page => out.push_str(&numbering.number_for(context.page_index)?),
            Token::Pages => out.push_str(&context.page_count.to_string()),
            Token::Date => out.push_str(&context.date),
            Token::Title => out.push_str(&context.title),
            Token::FileName => out.push_str(&context.filename),
            Token::Literal(text) => out.push_str(text),
        }
    }
    Ok(out)
}

/// A generated overlay stream and the font resources it references.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Overlay {
    content: String,
    fonts: Vec<(String, StandardFont)>,
}

impl Overlay {
    /// Returns the content-stream bytes.
    pub fn content_bytes(&self) -> &[u8] {
        self.content.as_bytes()
    }

    /// Returns the content stream as text (it is always ASCII).
    pub fn content_str(&self) -> &str {
        &self.content
    }

    /// Returns `(resource name, base font)` pairs in deterministic order.
    pub fn fonts(&self) -> &[(String, StandardFont)] {
        &self.fonts
    }

    /// Returns `true` when the overlay draws nothing.
    pub fn is_empty(&self) -> bool {
        self.content.is_empty()
    }

    /// Returns an owned copy of the content-stream bytes.
    ///
    /// The crate emits PDF *content-stream* bytes (not complete PDF files): a
    /// writer in `pdfxi-core` wraps these in a stream object and merges
    /// [`Self::fonts`] into the page resource dictionary. Copying here keeps the
    /// overlay copy-on-write by contract.
    pub fn into_bytes(&self) -> Vec<u8> {
        self.content.as_bytes().to_vec()
    }
}

/// Renders the overlay for one page.
///
/// The function validates the layout, returns an empty overlay when the page is
/// suppressed by [`PageFilter`], and otherwise emits a self-contained content
/// stream wrapped in `q`/`Q` with a single `cm` transform. Page indices are
/// checked both against the document's page count and through the checked
/// [`PageNumbering::number_for`] path used by token substitution, so an invalid
/// or overflowing index is reported as an error rather than silently
/// saturating. Existing page content is never read or modified.
pub fn render_overlay(
    layout: &HeaderFooterLayout,
    page: &PageGeometry,
    context: &PageContext,
) -> Result<Overlay, HeaderFooterError> {
    layout.validate()?;
    layout.numbering.validate_range(context.page_count)?;
    PageIndex::new(context.page_index).checked_for(context.page_count)?;

    if !layout.filter.includes(context.page_index) {
        return Ok(Overlay {
            content: String::new(),
            fonts: Vec::new(),
        });
    }

    let (display_width, display_height) = display_dimensions(page);
    let margin = layout.margins;
    let matrix = transform_for_page(page);

    let mut lines: Vec<String> = Vec::new();
    let mut fonts: Vec<(String, StandardFont)> = Vec::new();

    for slot in Slot::ALL {
        let Some(content) = layout.slot(slot) else {
            continue;
        };
        let text = substitute(&content.tokens, context, &layout.numbering)?;
        if text.is_empty() {
            continue;
        }
        let style = content.style.unwrap_or(layout.style);

        let resource_name = match fonts.iter().find(|(_, font)| *font == style.font) {
            Some((name, _)) => name.clone(),
            None => {
                let name = format!("F{}", fonts.len() + 1);
                fonts.push((name.clone(), style.font));
                name
            }
        };

        let width = approximate_width(&text, style);
        let size = style.font_size;
        let x = match slot {
            Slot::HeaderLeft | Slot::FooterLeft => margin.left(),
            Slot::HeaderCenter | Slot::FooterCenter => {
                let center = margin.left() + (display_width - margin.left() - margin.right()) / 2.0;
                (center - width / 2.0).max(margin.left())
            }
            Slot::HeaderRight | Slot::FooterRight => {
                (display_width - margin.right() - width).max(margin.left())
            }
        };
        let y = if slot.is_header() {
            display_height - margin.top() - size
        } else {
            margin.bottom()
        };

        let color = style.color;
        // `escape_literal` guarantees an ASCII-only string, so the emitted
        // content stream is valid UTF-8 and deterministic.
        let escaped = escape_literal(&text);
        lines.push("BT".to_string());
        lines.push(format!("/{} {} Tf", resource_name, format_num(size)));
        lines.push(format!(
            "{} {} {} rg",
            format_num(f32::from(color.r()) / 255.0),
            format_num(f32::from(color.g()) / 255.0),
            format_num(f32::from(color.b()) / 255.0)
        ));
        lines.push(format!("{} {} Td", format_num(x), format_num(y)));
        lines.push(format!("({}) Tj", escaped));
        lines.push("ET".to_string());
    }

    let mut content = String::new();
    if lines.is_empty() {
        return Ok(Overlay { content, fonts });
    }
    content.push_str("q\n");
    content.push_str(&matrix.to_pdf());
    content.push_str(" cm\n");
    for line in lines {
        content.push_str(&line);
        content.push('\n');
    }
    content.push_str("Q\n");

    Ok(Overlay { content, fonts })
}

/// Formats a number for a content stream without locale or exponent surprises.
fn format_num(value: f32) -> String {
    if value == 0.0 {
        return "0".to_string();
    }
    format!("{value}")
}

/// Returns a fresh byte vector with `overlay` appended after `existing`.
///
/// The input slice is copied, never mutated. The overlay is already wrapped in
/// a balanced `q`/`Q`, so appending it after a newline is safe for a standard
/// PDF content stream.
pub fn compose_content(existing: &[u8], overlay: &Overlay) -> Vec<u8> {
    let mut combined = Vec::with_capacity(existing.len() + 1 + overlay.content.len());
    combined.extend_from_slice(existing);
    combined.push(b'\n');
    combined.extend_from_slice(overlay.content_bytes());
    combined
}

/// Renders one overlay per page of a page set in input order.
///
/// The layout is validated once and the number of pages is taken from
/// `pages.len()`, so a single call applies headers, footers, and page numbers
/// over the whole set with a shared [`PageSetContext`]. The returned vector has
/// exactly one entry per input page; suppressed pages receive an empty
/// [`Overlay`]. Output is deterministic for identical inputs.
pub fn render_page_set(
    layout: &HeaderFooterLayout,
    pages: &[PageGeometry],
    context: &PageSetContext,
) -> Result<Vec<Overlay>, HeaderFooterError> {
    layout.validate()?;
    let page_count = pages.len();
    layout.numbering.validate_range(page_count)?;
    let mut overlays = Vec::with_capacity(page_count);
    for (page_index, page) in pages.iter().enumerate() {
        let page_context = PageContext::new(page_index, page_count, &context.title, &context.date)
            .with_filename(&context.filename);
        overlays.push(render_overlay(layout, page, &page_context)?);
    }
    Ok(overlays)
}

/// Composes each page's original content bytes with its generated overlay.
///
/// Returns one fresh byte vector per page in the same order as the inputs. The
/// source buffers are copied and never mutated. A length mismatch between
/// `pages` and `overlays` is reported as
/// [`HeaderFooterError::PageSetLengthMismatch`] rather than truncating either
/// list.
pub fn compose_page_set(
    pages: &[Vec<u8>],
    overlays: &[Overlay],
) -> Result<Vec<Vec<u8>>, HeaderFooterError> {
    if pages.len() != overlays.len() {
        return Err(HeaderFooterError::PageSetLengthMismatch {
            pages: pages.len(),
            overlays: overlays.len(),
        });
    }
    Ok(pages
        .iter()
        .zip(overlays)
        .map(|(content, overlay)| compose_content(content, overlay))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::{PageIndex, Rect};

    fn rect(x0: f32, y0: f32, x1: f32, y1: f32) -> Rect {
        Rect::new(x0, y0, x1, y1).expect("test rect must be valid")
    }

    fn letter_page() -> PageGeometry {
        PageGeometry::new(PageIndex::new(0), rect(0.0, 0.0, 612.0, 792.0))
            .expect("test page must be valid")
    }

    #[test]
    fn token_substitution_renders_all_token_kinds() {
        let numbering = PageNumbering::arabic()
            .with_prefix("Page ")
            .with_suffix(" of");
        let tokens = [
            Token::Literal("Doc ".to_string()),
            Token::Title,
            Token::Literal(" | ".to_string()),
            Token::Date,
            Token::Literal(" | ".to_string()),
            Token::Page,
            Token::Literal(" ".to_string()),
            Token::Pages,
        ];
        let context = PageContext::new(2, 5, "Report", "2026-11-19");
        assert_eq!(
            substitute(&tokens, &context, &numbering).expect("valid substitution"),
            "Doc Report | 2026-11-19 | Page 3 of 5"
        );
    }

    #[test]
    fn page_number_formats_and_start_offset() {
        let cases = [
            (NumberFormat::Arabic, "4"),
            (NumberFormat::RomanLower, "iv"),
            (NumberFormat::RomanUpper, "IV"),
            (NumberFormat::AlphaLower, "d"),
            (NumberFormat::AlphaUpper, "D"),
        ];
        for (format, expected) in cases {
            let numbering = PageNumbering::new(2)
                .expect("valid start")
                .with_format(format);
            assert_eq!(
                numbering.number_for(2).expect("valid page index"),
                expected,
                "format {format:?}"
            );
        }
        let alpha = PageNumbering::new(25)
            .expect("valid start")
            .with_format(NumberFormat::AlphaUpper);
        assert_eq!(alpha.number_for(0).expect("valid page index"), "Y");
        assert_eq!(alpha.number_for(1).expect("valid page index"), "Z");
        assert_eq!(alpha.number_for(2).expect("valid page index"), "AA");
    }

    #[test]
    fn number_for_rejects_unrepresentable_and_overflowing_indices() {
        let arabic = PageNumbering::arabic();
        assert!(matches!(
            arabic.number_for(usize::MAX),
            Err(HeaderFooterError::PageIndexOutOfRange {
                index: usize::MAX,
                start: 1
            })
        ));

        let near_max = PageNumbering::new(i64::MAX - 1).expect("valid start");
        assert!(matches!(
            near_max.number_for(5),
            Err(HeaderFooterError::PageIndexOutOfRange { index: 5, .. })
        ));
        assert_eq!(
            near_max.number_for(1).expect("last representable page"),
            i64::MAX.to_string()
        );
    }

    #[test]
    fn substitute_propagates_numbering_overflow() {
        let numbering = PageNumbering::new(i64::MAX).expect("valid start");
        let context = PageContext::new(usize::MAX, usize::MAX, "", "");
        assert!(matches!(
            substitute(&[Token::Page], &context, &numbering),
            Err(HeaderFooterError::PageIndexOutOfRange { .. })
        ));
    }

    #[test]
    fn odd_even_and_first_page_suppression() {
        let all = PageFilter::all();
        assert!(all.includes(0) && all.includes(1) && all.includes(2));

        let odd = PageFilter {
            suppress_first_page: false,
            selection: PageSelection::Odd,
        };
        assert!(odd.includes(0));
        assert!(!odd.includes(1));
        assert!(odd.includes(2));

        let even_no_first = PageFilter {
            suppress_first_page: true,
            selection: PageSelection::Even,
        };
        assert!(!even_no_first.includes(0));
        assert!(even_no_first.includes(1));
        assert!(!even_no_first.includes(2));
    }

    #[test]
    fn all_six_slots_render_into_one_stream() {
        let layout = HeaderFooterLayout::new()
            .with_slot(Slot::HeaderLeft, SlotContent::new(vec![Token::Title]))
            .with_slot(
                Slot::HeaderCenter,
                SlotContent::new(vec![Token::Literal("C".into())]),
            )
            .with_slot(Slot::HeaderRight, SlotContent::new(vec![Token::Date]))
            .with_slot(
                Slot::FooterLeft,
                SlotContent::new(vec![Token::Literal("FL".into())]),
            )
            .with_slot(
                Slot::FooterCenter,
                SlotContent::new(vec![Token::Literal("FC".into())]),
            )
            .with_slot(Slot::FooterRight, SlotContent::new(vec![Token::Page]));
        let context = PageContext::new(0, 1, "Title", "DATE");
        let overlay = render_overlay(&layout, &letter_page(), &context).expect("render");
        let text = overlay.content_str();
        for needle in ["(Title)", "(C)", "(DATE)", "(FL)", "(FC)", "(1)"] {
            assert!(text.contains(needle), "missing {needle} in {text}");
        }
        assert_eq!(text.matches("BT\n").count(), 6);
    }

    #[test]
    fn escaped_literal_strings_are_valid_and_deterministic() {
        assert_eq!(escape_literal(r"a\(b)\c"), r"a\\\(b\)\\c");
        assert_eq!(escape_literal("line\nnext"), r"line\nnext");
        assert_eq!(escape_literal("tab\there"), r"tab\there");
        assert_eq!(escape_literal("é"), r"\303\251");
        assert!(escape_literal("(x)").is_ascii());
    }

    #[test]
    fn rotation_transform_accounts_for_media_box_origin() {
        let rotated = PageGeometry::with_crop_box(
            PageIndex::new(0),
            rect(100.0, 200.0, 712.0, 992.0),
            rect(100.0, 200.0, 712.0, 992.0),
            90,
        )
        .expect("valid page");
        assert_eq!(
            transform_for_page(&rotated).values(),
            [0.0, 1.0, -1.0, 0.0, 712.0, 200.0]
        );
        assert_eq!(display_dimensions(&rotated), (792.0, 612.0));

        let flat = PageGeometry::new(PageIndex::new(0), rect(100.0, 200.0, 712.0, 992.0))
            .expect("valid page");
        assert_eq!(
            transform_for_page(&flat).values(),
            [1.0, 0.0, 0.0, 1.0, 100.0, 200.0]
        );
        let half = PageGeometry::with_crop_box(
            PageIndex::new(0),
            rect(0.0, 0.0, 612.0, 792.0),
            rect(0.0, 0.0, 612.0, 792.0),
            180,
        )
        .expect("valid page");
        assert_eq!(
            transform_for_page(&half).values(),
            [-1.0, 0.0, 0.0, -1.0, 612.0, 792.0]
        );
    }

    #[test]
    fn overlay_bytes_are_deterministic() {
        let layout = HeaderFooterLayout::new().with_slot(
            Slot::FooterRight,
            SlotContent::new(vec![Token::Page, Token::Literal("/".into()), Token::Pages]),
        );
        let context = PageContext::new(0, 3, "T", "D");
        let page = letter_page();
        let a = render_overlay(&layout, &page, &context).expect("render a");
        let b = render_overlay(&layout, &page, &context).expect("render b");
        assert_eq!(a, b);
        assert_eq!(a.content_bytes(), b.content_bytes());
        assert_eq!(a.fonts(), &[("F1".to_string(), StandardFont::Helvetica)]);
    }

    #[test]
    fn invalid_configuration_is_rejected() {
        assert!(matches!(
            TextStyle::new(StandardFont::Helvetica, 0.0, TextColor::BLACK),
            Err(HeaderFooterError::InvalidFontSize(_))
        ));
        assert!(matches!(
            TextStyle::new(StandardFont::Helvetica, 73.0, TextColor::BLACK),
            Err(HeaderFooterError::InvalidFontSize(_))
        ));
        assert!(matches!(
            Margins::new(-1.0, 0.0, 0.0, 0.0),
            Err(HeaderFooterError::InvalidMargin { field: "left", .. })
        ));
        assert!(matches!(
            PageNumbering::new(0),
            Err(HeaderFooterError::InvalidNumberingStart(0))
        ));
        let bad = PageNumbering::arabic();
        assert!(matches!(
            bad.validate_range(usize::MAX),
            Err(HeaderFooterError::NumberingRangeOverflow { .. })
        ));
    }

    #[test]
    fn compose_content_preserves_original_bytes() {
        let original = b"q 1 0 0 1 0 0 cm /GS0 gs Q".to_vec();
        let before = original.clone();
        let layout = HeaderFooterLayout::new().with_slot(
            Slot::FooterCenter,
            SlotContent::new(vec![Token::Literal("X".into())]),
        );
        let overlay = render_overlay(&layout, &letter_page(), &PageContext::new(0, 1, "", ""))
            .expect("render");
        let combined = compose_content(&original, &overlay);
        assert_eq!(original, before, "source buffer was mutated");
        assert!(combined.starts_with(&before));
        assert!(combined[before.len()..].starts_with(b"\nq\n"));
    }

    #[test]
    fn suppressed_page_renders_empty_overlay() {
        let layout = HeaderFooterLayout::new()
            .with_slot(Slot::FooterCenter, SlotContent::new(vec![Token::Page]))
            .with_filter(PageFilter {
                suppress_first_page: true,
                selection: PageSelection::All,
            });
        let first = render_overlay(&layout, &letter_page(), &PageContext::new(0, 2, "", ""))
            .expect("render first");
        assert!(first.is_empty());
        let second = render_overlay(&layout, &letter_page(), &PageContext::new(1, 2, "", ""))
            .expect("render second");
        assert!(!second.is_empty());
    }

    #[test]
    fn per_slot_style_produces_multiple_font_resources() {
        let bold = TextStyle::new(StandardFont::HelveticaBold, 12.0, TextColor::new(255, 0, 0))
            .expect("valid style");
        let layout = HeaderFooterLayout::new()
            .with_slot(
                Slot::HeaderLeft,
                SlotContent::with_style(vec![Token::Literal("A".into())], bold),
            )
            .with_slot(
                Slot::FooterRight,
                SlotContent::new(vec![Token::Literal("B".into())]),
            );
        let overlay = render_overlay(&layout, &letter_page(), &PageContext::new(0, 1, "", ""))
            .expect("render");
        assert_eq!(
            overlay.fonts(),
            &[
                ("F1".to_string(), StandardFont::HelveticaBold),
                ("F2".to_string(), StandardFont::Helvetica),
            ]
        );
        assert!(overlay.content_str().contains("1 0 0 rg"));
    }

    #[test]
    fn page_index_out_of_range_is_rejected() {
        let layout = HeaderFooterLayout::new()
            .with_slot(Slot::FooterCenter, SlotContent::new(vec![Token::Page]));
        let result = render_overlay(&layout, &letter_page(), &PageContext::new(3, 3, "", ""));
        assert!(matches!(
            result,
            Err(HeaderFooterError::Geometry(GeometryError::PageOutOfRange {
                index: 3,
                page_count: 3
            }))
        ));
    }

    #[test]
    fn token_substitution_includes_filename_token() {
        let context = PageContext::new(0, 1, "Title", "DATE").with_filename("report.pdf");
        let tokens = [
            Token::Literal("File: ".into()),
            Token::FileName,
            Token::Literal(" | ".into()),
            Token::Title,
            Token::Literal(" | ".into()),
            Token::Date,
        ];
        assert_eq!(
            substitute(&tokens, &context, &PageNumbering::arabic()).expect("substitution"),
            "File: report.pdf | Title | DATE"
        );
    }

    #[test]
    fn alternate_selection_stride_and_offset() {
        let every_three = PageFilter::alternate(3, 1);
        let selected: Vec<usize> = (0..10).filter(|&i| every_three.includes(i)).collect();
        assert_eq!(selected, vec![1, 4, 7]);

        let every_two = PageFilter::alternate(2, 0);
        let selected: Vec<usize> = (0..8).filter(|&i| every_two.includes(i)).collect();
        assert_eq!(selected, vec![0, 2, 4, 6]);

        // A zero stride selects nothing and never divides by zero.
        assert!(!PageFilter::alternate(0, 0).includes(0));
    }

    #[test]
    fn zero_alternate_stride_is_rejected() {
        let layout = HeaderFooterLayout::new().with_filter(PageFilter::alternate(0, 0));
        assert!(matches!(
            layout.validate(),
            Err(HeaderFooterError::InvalidAlternateStride)
        ));
        let result = render_page_set(&layout, &[letter_page()], &PageSetContext::new("", "", ""));
        assert!(matches!(
            result,
            Err(HeaderFooterError::InvalidAlternateStride)
        ));
    }

    #[test]
    fn render_page_set_applies_layout_over_all_pages() {
        let layout = HeaderFooterLayout::new().with_slot(
            Slot::FooterCenter,
            SlotContent::new(vec![
                Token::FileName,
                Token::Literal(" ".into()),
                Token::Page,
                Token::Literal("/".into()),
                Token::Pages,
            ]),
        );
        let pages = [letter_page(), letter_page(), letter_page()];
        let overlays = render_page_set(&layout, &pages, &PageSetContext::new("T", "D", "doc.pdf"))
            .expect("page set");
        assert_eq!(overlays.len(), 3);
        for (index, overlay) in overlays.iter().enumerate() {
            let text = overlay.content_str();
            assert!(
                text.contains(&format!("(doc.pdf {}/3)", index + 1)),
                "page {index}: {text}"
            );
        }
    }

    #[test]
    fn render_page_set_respects_suppression_and_numbering() {
        let layout = HeaderFooterLayout::new()
            .with_slot(Slot::FooterRight, SlotContent::new(vec![Token::Page]))
            .with_filter(PageFilter::alternate(2, 1))
            .with_numbering(PageNumbering::new(5).expect("start"));
        let pages = [letter_page(), letter_page(), letter_page(), letter_page()];
        let overlays =
            render_page_set(&layout, &pages, &PageSetContext::new("", "", "")).expect("page set");
        assert!(overlays[0].is_empty(), "offset page is not selected");
        assert!(!overlays[1].is_empty());
        assert!(overlays[2].is_empty(), "off-stride page is not selected");
        assert!(!overlays[3].is_empty());
        assert!(overlays[1].content_str().contains("(6)"));
        assert!(overlays[3].content_str().contains("(8)"));
    }

    #[test]
    fn compose_page_set_is_copy_on_write_and_deterministic() {
        let layout = HeaderFooterLayout::new()
            .with_slot(Slot::FooterCenter, SlotContent::new(vec![Token::Page]));
        let pages = vec![b"q Q".to_vec(), b"0 0 1 rg".to_vec()];
        let geoms = [letter_page(), letter_page()];
        let overlays =
            render_page_set(&layout, &geoms, &PageSetContext::new("", "", "")).expect("render");
        let before = pages.clone();
        let first = compose_page_set(&pages, &overlays).expect("compose a");
        let second = compose_page_set(&pages, &overlays).expect("compose b");
        assert_eq!(pages, before, "source pages must not be mutated");
        assert_eq!(first, second);
        assert_eq!(first.len(), 2);
        assert!(first[0].starts_with(b"q Q\n"));
    }

    #[test]
    fn compose_page_set_length_mismatch_is_typed() {
        let overlays = render_page_set(
            &HeaderFooterLayout::new(),
            &[letter_page()],
            &PageSetContext::new("", "", ""),
        )
        .expect("render");
        let pages = vec![Vec::new(), Vec::new()];
        assert!(matches!(
            compose_page_set(&pages, &overlays),
            Err(HeaderFooterError::PageSetLengthMismatch {
                pages: 2,
                overlays: 1
            })
        ));
    }

    #[test]
    fn filename_token_renders_into_overlay_stream() {
        let layout = HeaderFooterLayout::new().with_slot(
            Slot::HeaderLeft,
            SlotContent::new(vec![Token::Literal("File ".into()), Token::FileName]),
        );
        let context = PageContext::new(0, 1, "", "").with_filename("a(b).pdf");
        let overlay = render_overlay(&layout, &letter_page(), &context).expect("render");
        assert!(overlay.content_str().contains(r"(File a\(b\).pdf)"));
    }
}
