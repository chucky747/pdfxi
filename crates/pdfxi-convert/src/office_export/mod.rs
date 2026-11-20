//! Deterministic, dependency-free Office export from a caller-supplied model.
//!
//! This module writes editable `.docx`, `.xlsx`, and `.pptx` files entirely from
//! ZIP and XML generated in-process. It performs no PDF parsing, no rendering,
//! no network access, and requires no Office installation. A caller maps its
//! extraction output into [`OfficeDocument`] and hands it to one of the three
//! exporters behind the [`OfficeExporter`] contract.
//!
//! # Output contract
//!
//! 1. Every adapter returns [`OfficeExport`] with the archive bytes and a typed
//!    [`OfficeExportReport`]. Content that cannot be faithfully represented is
//!    listed in the report rather than silently faked.
//! 2. Pages are processed in ascending zero-based index order. Duplicate page
//!    indices, empty documents, and oversized inputs are typed errors.
//! 3. XML parts use a fixed declaration, escape all five predefined entities,
//!    and replace XML-illegal control characters with U+FFFD.
//! 4. Archives are stored-only ZIPs with fixed DOS timestamps, so repeated
//!    exports of the same model are byte-identical. Every archive is read back
//!    and validated (signatures, local/central agreement, CRC-32, and a
//!    format-specific required-part list) before it is returned.
//! 5. [`DocxExporter`] writes paragraphs, headings, lists, and tables.
//!    [`XlsxExporter`] writes one sheet per page or per table with typed cells.
//!    [`PptxExporter`] writes one slide per page with positioned text boxes.
//! 6. Input sizes are bounded by documented constants in the `model` module so a hostile
//!    model cannot force unbounded allocation.

mod docx;
mod error;
mod model;
mod pptx;
mod xlsx;
mod xml;
mod zip;

use std::collections::HashSet;

pub use docx::DocxExporter;
pub use error::{DocxExportError, ModelError, OfficeExportError, PptxExportError, XlsxExportError};
pub use model::{
    CellValue, MAX_BLOCKS_PER_PAGE, MAX_PAGES, MAX_TABLE_COLS, MAX_TABLE_ROWS, MAX_TEXT_LEN,
    OfficeBlock, OfficeCell, OfficeDocument, OfficePage, ParagraphStyle, TableBlock, TextBox,
    UnsupportedContent,
};
pub use pptx::PptxExporter;
pub use xlsx::{SheetMode, XlsxExporter};
pub use zip::ZipError;
pub use zip::{ZipEntry, ZipEntryView};

/// The OOXML family produced by an [`OfficeExporter`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OfficeFormat {
    /// WordprocessingML (`.docx`).
    Docx,
    /// SpreadsheetML (`.xlsx`).
    Xlsx,
    /// PresentationML (`.pptx`).
    Pptx,
}

impl OfficeFormat {
    /// Returns the conventional file extension for this format.
    #[must_use]
    pub const fn extension(self) -> &'static str {
        match self {
            Self::Docx => "docx",
            Self::Xlsx => "xlsx",
            Self::Pptx => "pptx",
        }
    }

    /// Returns a short stable label used in reports.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Docx => "Word",
            Self::Xlsx => "Excel",
            Self::Pptx => "PowerPoint",
        }
    }
}

/// One typed finding of content the target format cannot represent faithfully.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportedUnsupported {
    /// The adapter that produced the finding.
    pub format: OfficeFormat,
    /// Zero-based source page index.
    pub page: usize,
    /// Stable machine-readable kind (`image`, `annotation`, ...).
    pub kind: String,
    /// Human-readable explanation of what was elided or flattened.
    pub detail: String,
}

impl ReportedUnsupported {
    /// Creates a finding.
    #[must_use]
    pub fn new(
        format: OfficeFormat,
        page: usize,
        kind: impl Into<String>,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            format,
            page,
            kind: kind.into(),
            detail: detail.into(),
        }
    }
}

/// Typed report returned alongside every export.
#[derive(Debug, Clone, PartialEq)]
pub struct OfficeExportReport {
    /// Format that produced the archive.
    pub format: OfficeFormat,
    /// Number of XML/relationship parts in the validated archive.
    pub part_count: usize,
    /// Unsupported-content findings, ordered by page then insertion order.
    pub unsupported: Vec<ReportedUnsupported>,
}

impl OfficeExportReport {
    /// Returns true when nothing had to be elided or flattened.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.unsupported.is_empty()
    }
}

/// Archive bytes plus their typed report.
#[derive(Debug, Clone, PartialEq)]
pub struct OfficeExport {
    /// Complete OOXML archive bytes.
    pub bytes: Vec<u8>,
    /// Report describing the export.
    pub report: OfficeExportReport,
}

impl OfficeExport {
    /// Returns the file extension appropriate for these bytes.
    #[must_use]
    pub const fn extension(&self) -> &'static str {
        self.report.format.extension()
    }
}

/// Typed contract implemented by every local Office adapter.
///
/// Implementations are stateless writers; they never touch the file system.
/// Callers that want the adapter-specific error type can call the inherent
/// `write` method on [`DocxExporter`], [`XlsxExporter`], or [`PptxExporter`].
pub trait OfficeExporter {
    /// Returns the format written by this adapter.
    fn format(&self) -> OfficeFormat;

    /// Writes `doc` into a validated archive and returns its bytes and report.
    ///
    /// # Errors
    ///
    /// Returns the adapter-specific failure wrapped in [`OfficeExportError`].
    fn export(&self, doc: &OfficeDocument) -> Result<OfficeExport, OfficeExportError>;
}

/// Validated, sorted view of a document shared by all adapters.
pub(crate) struct Validated<'a> {
    pub(crate) title: &'a str,
    pub(crate) pages: Vec<&'a OfficePage>,
}

fn check_text(page: usize, field: &'static str, text: &str) -> Result<(), ModelError> {
    if text.chars().count() > MAX_TEXT_LEN {
        return Err(ModelError::TextTooLong {
            page,
            field,
            limit: MAX_TEXT_LEN,
        });
    }
    Ok(())
}

/// Validates a model and returns its pages sorted by index.
pub(crate) fn validate(doc: &OfficeDocument) -> Result<Validated<'_>, ModelError> {
    if doc.pages.is_empty() {
        return Err(ModelError::EmptyDocument);
    }
    if doc.pages.len() > MAX_PAGES {
        return Err(ModelError::TooManyPages { limit: MAX_PAGES });
    }

    let mut pages: Vec<&OfficePage> = doc.pages.iter().collect();
    pages.sort_by_key(|page| page.index);
    let mut seen: HashSet<usize> = HashSet::with_capacity(pages.len());
    for page in &pages {
        if !seen.insert(page.index) {
            return Err(ModelError::DuplicatePage { page: page.index });
        }
        if !(page.width.is_finite()
            && page.width > 0.0
            && page.height.is_finite()
            && page.height > 0.0)
        {
            return Err(ModelError::InvalidPageGeometry { page: page.index });
        }
        if page.blocks.len() > MAX_BLOCKS_PER_PAGE {
            return Err(ModelError::TooManyBlocks {
                page: page.index,
                limit: MAX_BLOCKS_PER_PAGE,
            });
        }
        check_text(page.index, "title", &doc.title)?;
        for block in &page.blocks {
            validate_block(page.index, block)?;
        }
    }
    Ok(Validated {
        title: &doc.title,
        pages,
    })
}

fn validate_block(page: usize, block: &OfficeBlock) -> Result<(), ModelError> {
    match block {
        OfficeBlock::Paragraph { text, style } => {
            check_text(page, "paragraph", text)?;
            if let ParagraphStyle::Heading(level) = style
                && !(1..=9).contains(level)
            {
                return Err(ModelError::InvalidHeadingLevel {
                    page,
                    level: *level,
                });
            }
            Ok(())
        }
        OfficeBlock::ListItem { text, level, .. } => {
            check_text(page, "list item", text)?;
            if *level > 8 {
                return Err(ModelError::InvalidListLevel {
                    page,
                    level: *level,
                });
            }
            Ok(())
        }
        OfficeBlock::Table(table) => {
            if table.rows.len() > MAX_TABLE_ROWS {
                return Err(ModelError::TooManyTableRows {
                    page,
                    limit: MAX_TABLE_ROWS,
                });
            }
            for row in &table.rows {
                if row.len() > MAX_TABLE_COLS {
                    return Err(ModelError::TooManyTableCols {
                        page,
                        limit: MAX_TABLE_COLS,
                    });
                }
                for cell in row {
                    match &cell.value {
                        CellValue::Text(text) => check_text(page, "cell text", text)?,
                        CellValue::Number(number) => {
                            if !number.is_finite() {
                                return Err(ModelError::NonFinite {
                                    page,
                                    field: "cell number",
                                });
                            }
                        }
                        CellValue::Boolean(_) | CellValue::Empty => {}
                    }
                }
            }
            if let Some(name) = &table.name {
                check_text(page, "table name", name)?;
            }
            Ok(())
        }
        OfficeBlock::TextBox(box_) => {
            check_text(page, "text box", &box_.text)?;
            for (value, field) in [
                (box_.x, "text box x"),
                (box_.y, "text box y"),
                (box_.width, "text box width"),
                (box_.height, "text box height"),
                (box_.font_size, "text box font size"),
            ] {
                if !value.is_finite() {
                    return Err(ModelError::NonFinite { page, field });
                }
                if value < 0.0 && field != "text box y" {
                    return Err(ModelError::NegativeMeasurement { page, field });
                }
            }
            Ok(())
        }
        OfficeBlock::Unsupported(content) => {
            check_text(page, "unsupported detail", content.detail())
        }
    }
}

/// Builds an archive from parts, validates it, and attaches a report.
///
/// Each adapter declares its own required-part list and error wrapper, so this
/// helper takes closures for finalisation to keep the error types distinct.
pub(crate) fn finish<E>(
    entries: &[ZipEntry<'_>],
    required: &[&str],
    report: OfficeExportReport,
    wrap_zip: impl Fn(ZipError) -> E,
) -> Result<OfficeExport, E> {
    let bytes = zip::write_zip(entries).map_err(&wrap_zip)?;
    let views = zip::read_zip(&bytes).map_err(&wrap_zip)?;
    zip::require_parts(&views, required).map_err(&wrap_zip)?;
    let mut report = report;
    report.part_count = views.len();
    Ok(OfficeExport { bytes, report })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_document() -> OfficeDocument {
        OfficeDocument::new(
            "Sample",
            vec![OfficePage::new(0, 200.0, 300.0).with_blocks(vec![OfficeBlock::paragraph("hi")])],
        )
    }

    #[test]
    fn contract_dispatches_to_all_three_formats() {
        let doc = sample_document();
        let docx = DocxExporter.export(&doc).expect("docx");
        let xlsx = XlsxExporter::new().export(&doc).expect("xlsx");
        let pptx = PptxExporter.export(&doc).expect("pptx");
        assert_eq!(docx.extension(), "docx");
        assert_eq!(xlsx.extension(), "xlsx");
        assert_eq!(pptx.extension(), "pptx");
        assert_eq!(DocxExporter.format(), OfficeFormat::Docx);
        assert_eq!(XlsxExporter::new().format(), OfficeFormat::Xlsx);
        assert_eq!(PptxExporter.format(), OfficeFormat::Pptx);
        assert_eq!(OfficeFormat::Pptx.label(), "PowerPoint");
    }

    #[test]
    fn validation_rejects_empty_duplicate_and_bad_geometry() {
        assert_eq!(
            validate(&OfficeDocument::new("T", Vec::new())).map(|_| ()),
            Err(ModelError::EmptyDocument)
        );
        let page = OfficePage::new(0, 200.0, 300.0);
        let duplicate = OfficeDocument::new("T", vec![page.clone(), page]);
        assert_eq!(
            validate(&duplicate).map(|_| ()),
            Err(ModelError::DuplicatePage { page: 0 })
        );
        let bad = OfficeDocument::new("T", vec![OfficePage::new(0, f32::NAN, 10.0)]);
        assert_eq!(
            validate(&bad).map(|_| ()),
            Err(ModelError::InvalidPageGeometry { page: 0 })
        );
    }

    #[test]
    fn validation_checks_numeric_and_structural_fields() {
        let number = OfficeDocument::new(
            "T",
            vec![
                OfficePage::new(0, 100.0, 100.0).with_blocks(vec![OfficeBlock::table(
                    TableBlock::new(vec![vec![OfficeCell::number(f64::NAN)]]),
                )]),
            ],
        );
        assert_eq!(
            validate(&number).map(|_| ()),
            Err(ModelError::NonFinite {
                page: 0,
                field: "cell number"
            })
        );

        let heading = OfficeDocument::new(
            "T",
            vec![
                OfficePage::new(0, 100.0, 100.0)
                    .with_blocks(vec![OfficeBlock::styled("bad", ParagraphStyle::Heading(0))]),
            ],
        );
        assert_eq!(
            validate(&heading).map(|_| ()),
            Err(ModelError::InvalidHeadingLevel { page: 0, level: 0 })
        );
    }

    #[test]
    fn archive_reports_are_typed_and_populated() {
        let doc = OfficeDocument::new(
            "T",
            vec![
                OfficePage::new(0, 200.0, 300.0).with_blocks(vec![OfficeBlock::unsupported(
                    UnsupportedContent::Image {
                        detail: "photo".to_owned(),
                    },
                )]),
            ],
        );
        let exporters: [&dyn OfficeExporter; 3] =
            [&DocxExporter, &XlsxExporter::new(), &PptxExporter];
        for exporter in exporters {
            let result = exporter.export(&doc).expect("export");
            assert!(!result.report.is_clean());
            assert_eq!(result.report.unsupported[0].format, exporter.format());
            assert_eq!(result.report.unsupported[0].kind, "image");
            assert!(result.report.part_count >= 3);
        }
    }
}
