//! Caller-supplied extraction model consumed by the Office exporters.
//!
//! The adapters in this module are pure writers: they never parse PDF bytes or
//! extract text. A caller maps its own extraction output into the types below
//! and hands the assembled [`OfficeDocument`] to an exporter. Keeping parsing
//! out of the write path makes the output deterministic and dependency-free.

/// Maximum number of pages a single export accepts.
pub const MAX_PAGES: usize = 10_000;
/// Maximum number of content blocks on one page.
pub const MAX_BLOCKS_PER_PAGE: usize = 100_000;
/// Maximum number of rows in one table.
pub const MAX_TABLE_ROWS: usize = 100_000;
/// Maximum number of columns in one table row.
pub const MAX_TABLE_COLS: usize = 1_024;
/// Maximum number of characters in one text field.
pub const MAX_TEXT_LEN: usize = 1_000_000;

/// Paragraph styling requested by the caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParagraphStyle {
    /// Body text with no special styling.
    Normal,
    /// A numbered heading. The level is in `1..=9`.
    Heading(u8),
    /// Document title styling.
    Title,
    /// Indented quotation styling.
    Quote,
}

/// A typed spreadsheet cell value.
#[derive(Debug, Clone, PartialEq)]
pub enum CellValue {
    /// A string written as an inline XML string.
    Text(String),
    /// A finite IEEE-754 number written as a numeric cell.
    Number(f64),
    /// A boolean written with the spreadsheet `b` cell type.
    Boolean(bool),
    /// An explicitly blank cell.
    Empty,
}

/// One spreadsheet cell.
#[derive(Debug, Clone, PartialEq)]
pub struct OfficeCell {
    /// Typed value of the cell.
    pub value: CellValue,
}

impl OfficeCell {
    /// Creates a text cell.
    #[must_use]
    pub fn text(value: impl Into<String>) -> Self {
        Self {
            value: CellValue::Text(value.into()),
        }
    }

    /// Creates a numeric cell.
    #[must_use]
    pub const fn number(value: f64) -> Self {
        Self {
            value: CellValue::Number(value),
        }
    }

    /// Creates a boolean cell.
    #[must_use]
    pub const fn boolean(value: bool) -> Self {
        Self {
            value: CellValue::Boolean(value),
        }
    }

    /// Creates an empty cell.
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            value: CellValue::Empty,
        }
    }
}

/// A table block, optionally named so the Excel adapter can use it as a sheet.
#[derive(Debug, Clone, PartialEq)]
pub struct TableBlock {
    /// Optional caller-supplied name used for per-table sheet naming.
    pub name: Option<String>,
    /// Rows in reading order; every table is rectangular.
    pub rows: Vec<Vec<OfficeCell>>,
}

impl TableBlock {
    /// Creates a table from rectangular rows.
    #[must_use]
    pub fn new(rows: Vec<Vec<OfficeCell>>) -> Self {
        Self { name: None, rows }
    }

    /// Attaches a name used by per-table sheet naming.
    #[must_use]
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }
}

/// A positioned run of text recovered from a page.
#[derive(Debug, Clone, PartialEq)]
pub struct TextBox {
    /// Recovered UTF-8 text.
    pub text: String,
    /// Left edge in PDF page coordinates.
    pub x: f32,
    /// Baseline or bottom edge in PDF page coordinates.
    pub y: f32,
    /// Width in page coordinates.
    pub width: f32,
    /// Height in page coordinates.
    pub height: f32,
    /// Font size in page coordinates.
    pub font_size: f32,
}

impl TextBox {
    /// Creates a text box anchored at `(x, y)` with zero metrics.
    #[must_use]
    pub fn new(text: impl Into<String>, x: f32, y: f32) -> Self {
        Self {
            text: text.into(),
            x,
            y,
            width: 0.0,
            height: 0.0,
            font_size: 0.0,
        }
    }

    /// Attaches size and font metrics to this text box.
    #[must_use]
    pub fn with_metrics(mut self, width: f32, height: f32, font_size: f32) -> Self {
        self.width = width;
        self.height = height;
        self.font_size = font_size;
        self
    }
}

/// Content the exporters cannot faithfully represent and must report.
///
/// Adapters record these as typed findings in the returned
/// [`crate::office_export::OfficeExportReport`] instead of silently dropping
/// them or faking a visual approximation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnsupportedContent {
    /// Raster or vector image content.
    Image {
        /// Short caller-supplied description.
        detail: String,
    },
    /// An annotation that has no editable equivalent.
    Annotation {
        /// Short caller-supplied description.
        detail: String,
    },
    /// A drawing or path with no editable equivalent.
    Drawing {
        /// Short caller-supplied description.
        detail: String,
    },
    /// A hyperlink that the target format would need to be reconstructed.
    Hyperlink {
        /// Short caller-supplied description.
        detail: String,
    },
    /// A dynamic field such as a date or page number.
    Field {
        /// Short caller-supplied description.
        detail: String,
    },
}

impl UnsupportedContent {
    /// Returns a stable machine-readable kind name.
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Image { .. } => "image",
            Self::Annotation { .. } => "annotation",
            Self::Drawing { .. } => "drawing",
            Self::Hyperlink { .. } => "hyperlink",
            Self::Field { .. } => "field",
        }
    }

    /// Returns the caller-supplied detail text.
    #[must_use]
    pub fn detail(&self) -> &str {
        match self {
            Self::Image { detail }
            | Self::Annotation { detail }
            | Self::Drawing { detail }
            | Self::Hyperlink { detail }
            | Self::Field { detail } => detail,
        }
    }
}

/// One content block on a page.
#[derive(Debug, Clone, PartialEq)]
pub enum OfficeBlock {
    /// A paragraph with optional styling.
    Paragraph {
        /// Text of the paragraph.
        text: String,
        /// Requested paragraph style.
        style: ParagraphStyle,
    },
    /// A bullet or numbered list item.
    ListItem {
        /// Text of the item.
        text: String,
        /// Zero-based nesting level.
        level: u8,
        /// Whether the marker is numbered rather than bulleted.
        ordered: bool,
    },
    /// A rectangular table.
    Table(TableBlock),
    /// A positioned text run.
    TextBox(TextBox),
    /// Content that must be reported rather than written.
    Unsupported(UnsupportedContent),
}

impl OfficeBlock {
    /// Creates a normal paragraph.
    #[must_use]
    pub fn paragraph(text: impl Into<String>) -> Self {
        Self::Paragraph {
            text: text.into(),
            style: ParagraphStyle::Normal,
        }
    }

    /// Creates a styled paragraph.
    #[must_use]
    pub fn styled(text: impl Into<String>, style: ParagraphStyle) -> Self {
        Self::Paragraph {
            text: text.into(),
            style,
        }
    }

    /// Creates a list item at `level`.
    #[must_use]
    pub fn list_item(text: impl Into<String>, level: u8, ordered: bool) -> Self {
        Self::ListItem {
            text: text.into(),
            level,
            ordered,
        }
    }

    /// Creates a table block.
    #[must_use]
    pub const fn table(table: TableBlock) -> Self {
        Self::Table(table)
    }

    /// Creates a positioned text box block.
    #[must_use]
    pub const fn text_box(box_: TextBox) -> Self {
        Self::TextBox(box_)
    }

    /// Creates an unsupported-content block.
    #[must_use]
    pub const fn unsupported(content: UnsupportedContent) -> Self {
        Self::Unsupported(content)
    }
}

/// One page of extracted content.
#[derive(Debug, Clone, PartialEq)]
pub struct OfficePage {
    /// Zero-based page index.
    pub index: usize,
    /// Page width in PDF points.
    pub width: f32,
    /// Page height in PDF points.
    pub height: f32,
    /// Content blocks in reading order.
    pub blocks: Vec<OfficeBlock>,
}

impl OfficePage {
    /// Creates an empty page with the given index and geometry.
    #[must_use]
    pub fn new(index: usize, width: f32, height: f32) -> Self {
        Self {
            index,
            width,
            height,
            blocks: Vec::new(),
        }
    }

    /// Attaches content blocks to this page.
    #[must_use]
    pub fn with_blocks(mut self, blocks: Vec<OfficeBlock>) -> Self {
        self.blocks = blocks;
        self
    }
}

/// A complete document ready for Office export.
#[derive(Debug, Clone, PartialEq)]
pub struct OfficeDocument {
    /// Document title, used in core properties and default sheet/slide labels.
    pub title: String,
    /// Pages of the document.
    pub pages: Vec<OfficePage>,
}

impl OfficeDocument {
    /// Creates a document from a title and its pages.
    #[must_use]
    pub fn new(title: impl Into<String>, pages: Vec<OfficePage>) -> Self {
        Self {
            title: title.into(),
            pages,
        }
    }

    /// Returns the number of supplied pages.
    #[must_use]
    pub fn page_count(&self) -> usize {
        self.pages.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builders_produce_expected_block_shapes() {
        let block = OfficeBlock::styled("Title", ParagraphStyle::Heading(1));
        assert_eq!(
            block,
            OfficeBlock::Paragraph {
                text: "Title".to_owned(),
                style: ParagraphStyle::Heading(1),
            }
        );
        let unsupported = OfficeBlock::unsupported(UnsupportedContent::Image {
            detail: "x".to_owned(),
        });
        assert_eq!(
            unsupported,
            OfficeBlock::Unsupported(UnsupportedContent::Image {
                detail: "x".to_owned()
            })
        );
    }

    #[test]
    fn unsupported_kind_and_detail_are_stable() {
        let content = UnsupportedContent::Field {
            detail: "date".to_owned(),
        };
        assert_eq!(content.kind(), "field");
        assert_eq!(content.detail(), "date");
    }

    #[test]
    fn cell_and_page_builders_are_convenient() {
        let cell = OfficeCell::number(2.5);
        assert_eq!(cell.value, CellValue::Number(2.5));
        let page = OfficePage::new(0, 100.0, 200.0).with_blocks(vec![OfficeBlock::paragraph("hi")]);
        let doc = OfficeDocument::new("T", vec![page]);
        assert_eq!(doc.page_count(), 1);
    }
}
