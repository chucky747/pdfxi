//! Extraction-to-Office document mapping for EXPORT-001/002/003.
//!
//! `crate::office_export` writes editable DOCX/XLSX/PPTX archives from a
//! caller-supplied [`OfficeDocument`], but it does not map recovered page
//! content into that model. This module supplies the missing library-level
//! half: it takes an [`ExtractedDocument`] (runs, simple tables, positioned
//! text boxes, and typed unsupported constructs), orders it deterministically,
//! numbers paragraphs and page labels, and produces a validated
//! [`OfficeExportEnvelope`] that can be written by any of the three local
//! adapters.
//!
//! # Boundaries documented honestly
//!
//! - There is no new OOXML ZIP writer here. The local, in-process writer in
//!   `crate::office_export` is reused; it needs no Office installation.
//! - No external Office process, network access, or file-system read is used.
//! - No raw-PDF text extraction happens here: the caller supplies already
//!   extracted runs. Wiring a CLI `ExportKind` command or a UI menu to this
//!   library is host-only integration and remains outside this crate.

use crate::extract_model::{
    ExtractModelError, ExtractedDocument, ExtractedPage, ExtractedUnsupported,
};
use crate::office_export::{
    CellValue, DocxExporter, ModelError, OfficeBlock, OfficeCell, OfficeDocument, OfficeExport,
    OfficeExportError, OfficeExportReport, OfficeExporter, OfficeFormat, OfficePage, PptxExporter,
    ReportedUnsupported, TableBlock, TextBox, XlsxExporter, validate as validate_model,
};
use std::collections::HashSet;

/// Maximum number of typed unsupported findings one plan accepts.
pub const OFFICE_PLAN_MAX_UNSUPPORTED: usize = 10_000;

/// The editable Office family a plan targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OfficePlanFormat {
    /// Editable Word (`.docx`).
    Word,
    /// Editable Excel (`.xlsx`).
    Excel,
    /// Editable PowerPoint (`.pptx`).
    Presentation,
}

impl OfficePlanFormat {
    /// Every supported format, in a stable order.
    pub const ALL: &'static [Self] = &[Self::Word, Self::Excel, Self::Presentation];

    /// Return every supported format in a stable order.
    #[must_use]
    pub fn all() -> &'static [Self] {
        Self::ALL
    }

    /// Return the conventional file extension.
    #[must_use]
    pub const fn extension(self) -> &'static str {
        match self {
            Self::Word => "docx",
            Self::Excel => "xlsx",
            Self::Presentation => "pptx",
        }
    }

    /// Return the underlying local exporter's format label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Word => "Word",
            Self::Excel => "Excel",
            Self::Presentation => "PowerPoint",
        }
    }

    /// Return the singular one-based page label used for this format.
    #[must_use]
    pub const fn page_label(self) -> &'static str {
        match self {
            Self::Word => "Page",
            Self::Excel => "Sheet",
            Self::Presentation => "Slide",
        }
    }

    /// Map an `ExportKind` family to a plan format.
    #[must_use]
    pub fn from_export_kind(kind: crate::ExportKind) -> Option<Self> {
        match kind {
            crate::ExportKind::Word => Some(Self::Word),
            crate::ExportKind::Spreadsheet => Some(Self::Excel),
            crate::ExportKind::Presentation => Some(Self::Presentation),
            _ => None,
        }
    }

    /// Return the matching OOXML format used in typed reports.
    #[must_use]
    pub const fn office_format(self) -> OfficeFormat {
        match self {
            Self::Word => OfficeFormat::Docx,
            Self::Excel => OfficeFormat::Xlsx,
            Self::Presentation => OfficeFormat::Pptx,
        }
    }
}

/// A typed construct the target format cannot represent faithfully.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OfficePlanUnsupported {
    /// Zero-based source page index.
    pub page: usize,
    /// Stable machine-readable kind name.
    pub kind: &'static str,
    /// Caller-supplied explanation.
    pub detail: String,
}

impl OfficePlanUnsupported {
    /// Create a typed unsupported finding.
    #[must_use]
    pub fn new(page: usize, kind: &'static str, detail: impl Into<String>) -> Self {
        Self {
            page,
            kind,
            detail: detail.into(),
        }
    }
}

/// A typed failure while mapping extracted content into an Office envelope.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum OfficePlanError {
    /// The caller-supplied extraction bundle failed validation.
    #[error(transparent)]
    Model(#[from] ExtractModelError),
    /// The mapped `OfficeDocument` failed the shared model validation.
    #[error(transparent)]
    Document(#[from] ModelError),
    /// Too many typed unsupported constructs were supplied.
    #[error("office plan has more than {limit} unsupported constructs")]
    TooManyUnsupported {
        /// The enforced finding limit.
        limit: usize,
    },
    /// A local adapter refused the validated envelope.
    #[error(transparent)]
    Export(#[from] OfficeExportError),
}

/// Deterministic mapping options shared by all three formats.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OfficePlanOptions {
    /// Runs at or above this font size become first-level headings.
    pub heading_font_size: f32,
    /// Prefix each generated paragraph with a one-based sequence number.
    pub number_paragraphs: bool,
}

impl Default for OfficePlanOptions {
    fn default() -> Self {
        Self {
            heading_font_size: 18.0,
            number_paragraphs: false,
        }
    }
}

/// A validated document model plus an explicit, typed report and labels.
#[derive(Debug, Clone, PartialEq)]
pub struct OfficeExportEnvelope {
    format: OfficePlanFormat,
    title: String,
    document: OfficeDocument,
    unsupported: Vec<OfficePlanUnsupported>,
    page_labels: Vec<String>,
}

impl OfficeExportEnvelope {
    /// Return the target format.
    #[must_use]
    pub const fn format(&self) -> OfficePlanFormat {
        self.format
    }

    /// Return the document title.
    #[must_use]
    pub fn title(&self) -> &str {
        &self.title
    }

    /// Return the validated document model ready for an `OfficeExporter`.
    #[must_use]
    pub fn document(&self) -> &OfficeDocument {
        &self.document
    }

    /// Return typed unsupported findings in canonical order (page, kind, detail).
    #[must_use]
    pub fn unsupported(&self) -> &[OfficePlanUnsupported] {
        &self.unsupported
    }

    /// Return deterministic one-based labels for the pages in output order.
    #[must_use]
    pub fn page_labels(&self) -> &[String] {
        &self.page_labels
    }

    /// Re-validate this envelope without writing any bytes.
    ///
    /// # Errors
    ///
    /// Returns a typed [`OfficePlanError`] if the held document fails the
    /// shared Office model validation.
    pub fn validate(&self) -> Result<(), OfficePlanError> {
        if self.page_labels.len() != self.document.pages.len() {
            return Err(OfficePlanError::Document(ModelError::EmptyDocument));
        }
        validate_model(&self.document)?;
        Ok(())
    }

    /// Return deterministic text lines describing the envelope.
    #[must_use]
    pub fn to_lines(&self) -> Vec<String> {
        let mut lines = vec![
            format!("format: {}", self.format.extension()),
            format!("title: {}", self.title),
            format!("pages: {}", self.document.pages.len()),
        ];
        for (index, label) in self.page_labels.iter().enumerate() {
            lines.push(format!(
                "page {}: {} [{} blocks]",
                index + 1,
                label,
                self.document.pages[index].blocks.len()
            ));
        }
        for item in &self.unsupported {
            lines.push(format!(
                "unsupported: page {} [{}] {}",
                item.page + 1,
                item.kind,
                item.detail
            ));
        }
        lines
    }

    /// Serialize the envelope description to deterministic bytes.
    #[must_use]
    pub fn to_manifest_bytes(&self) -> Vec<u8> {
        let mut lines = self.to_lines();
        lines.push(String::new());
        crate::LineEnding::Lf.join(lines).into_bytes()
    }

    /// Write the envelope with the matching local Office adapter.
    ///
    /// The ZIP is produced in-process by `crate::office_export`; unsupported
    /// findings from this mapping layer are merged into the returned report.
    ///
    /// # Errors
    ///
    /// Returns a typed [`OfficePlanError`] when validation or the adapter fails.
    pub fn write(&self) -> Result<OfficeExport, OfficePlanError> {
        self.validate()?;
        let mut result = match self.format {
            OfficePlanFormat::Word => DocxExporter.export(&self.document)?,
            OfficePlanFormat::Excel => XlsxExporter::new().export(&self.document)?,
            OfficePlanFormat::Presentation => PptxExporter.export(&self.document)?,
        };
        let mut merged: Vec<ReportedUnsupported> = result.report.unsupported.clone();
        for item in &self.unsupported {
            merged.push(ReportedUnsupported::new(
                self.format.office_format(),
                item.page,
                item.kind,
                item.detail.clone(),
            ));
        }
        merged.sort_by(|left, right| {
            left.page
                .cmp(&right.page)
                .then_with(|| left.kind.cmp(&right.kind))
                .then_with(|| left.detail.cmp(&right.detail))
        });
        result.report = OfficeExportReport {
            format: result.report.format,
            part_count: result.report.part_count,
            unsupported: merged,
        };
        Ok(result)
    }
}

/// Map an extracted document into a validated Office export envelope.
///
/// Pages are processed in ascending zero-based index order. Runs are ordered
/// top-to-bottom then left-to-right and grouped into paragraphs; same-line runs
/// are joined with single spaces. Text boxes become positioned text boxes, and
/// rectangular tables become typed table blocks. Non-rectangular tables and
/// every declared unsupported construct are reported with typed kinds rather
/// than silently flattened.
///
/// # Errors
///
/// Returns a typed [`OfficePlanError`] for an invalid extraction bundle, an
/// invalid mapped document, or too many unsupported findings.
pub fn build_office_export_plan(
    source: &ExtractedDocument,
    format: OfficePlanFormat,
    options: &OfficePlanOptions,
) -> Result<OfficeExportEnvelope, OfficePlanError> {
    source.validate()?;
    if !options.heading_font_size.is_finite() || options.heading_font_size < 0.0 {
        return Err(OfficePlanError::Model(ExtractModelError::NonFinite {
            page: 0,
            field: "heading font size",
        }));
    }

    let mut unsupported: Vec<OfficePlanUnsupported> = Vec::new();
    let mut paragraph_number = 1usize;
    let mut office_pages = Vec::with_capacity(source.pages.len());
    let mut page_labels = Vec::with_capacity(source.pages.len());

    for (position, page) in source.sorted_pages().into_iter().enumerate() {
        page_labels.push(format!("{} {}", format.page_label(), position + 1));
        collect_unsupported(page, &mut unsupported)?;
        let blocks = map_blocks(page, format, options, &mut paragraph_number);
        office_pages.push(OfficePage::new(page.index, page.width, page.height).with_blocks(blocks));
    }

    let document = OfficeDocument::new(source.title.clone(), office_pages);
    let envelope = OfficeExportEnvelope {
        format,
        title: source.title.clone(),
        document,
        unsupported,
        page_labels,
    };
    envelope.validate()?;
    Ok(envelope)
}

/// Record typed unsupported constructs for one page in deterministic order.
fn collect_unsupported(
    page: &ExtractedPage,
    out: &mut Vec<OfficePlanUnsupported>,
) -> Result<(), OfficePlanError> {
    for table in &page.tables {
        if !table.is_rectangular() {
            let detail = table
                .name
                .clone()
                .unwrap_or_else(|| "unnamed table".to_owned());
            out.push(OfficePlanUnsupported::new(
                page.index,
                "complex-table",
                format!("{detail}: non-rectangular table is not editable"),
            ));
        }
    }
    for item in &page.unsupported {
        let (kind, detail) = match item {
            ExtractedUnsupported::VectorArt { detail } => ("vector-art", detail.as_str()),
            ExtractedUnsupported::ComplexTable { detail } => ("complex-table", detail.as_str()),
            ExtractedUnsupported::EmbeddedMedia { detail } => ("embedded-media", detail.as_str()),
        };
        out.push(OfficePlanUnsupported::new(page.index, kind, detail));
    }
    out.sort_by(|left, right| {
        left.page
            .cmp(&right.page)
            .then_with(|| left.kind.cmp(right.kind))
            .then_with(|| left.detail.cmp(&right.detail))
    });
    let mut seen = HashSet::with_capacity(out.len());
    out.retain(|item| seen.insert((item.page, item.kind, item.detail.clone())));
    if out.len() > OFFICE_PLAN_MAX_UNSUPPORTED {
        return Err(OfficePlanError::TooManyUnsupported {
            limit: OFFICE_PLAN_MAX_UNSUPPORTED,
        });
    }
    Ok(())
}

/// Map one page's supported constructs into ordered Office blocks.
fn map_blocks(
    page: &ExtractedPage,
    format: OfficePlanFormat,
    options: &OfficePlanOptions,
    paragraph_number: &mut usize,
) -> Vec<OfficeBlock> {
    let mut blocks = Vec::new();
    let mut line_key = i64::MIN;
    let mut line = String::new();
    let mut line_font = 0.0f32;
    let ordered = ExtractedDocument::ordered_runs(page);
    for run in ordered {
        let key = (f64::from(run.y) * 100.0).round() as i64;
        if !run.text.is_empty() && line_key != key {
            if !line.is_empty() {
                blocks.push(paragraph_block(
                    &line,
                    line_font,
                    format,
                    options,
                    paragraph_number,
                ));
                line.clear();
            }
            line_key = key;
            line_font = run.font_size;
        }
        if !run.text.is_empty() {
            if !line.is_empty() {
                line.push(' ');
            }
            line.push_str(&run.text);
            if run.font_size > line_font {
                line_font = run.font_size;
            }
        }
    }
    if !line.is_empty() {
        blocks.push(paragraph_block(
            &line,
            line_font,
            format,
            options,
            paragraph_number,
        ));
    }

    for table in &page.tables {
        if !table.is_rectangular() {
            continue;
        }
        let rows = table
            .rows
            .iter()
            .map(|row| {
                row.iter()
                    .map(|cell| OfficeCell {
                        value: CellValue::Text(cell.clone()),
                    })
                    .collect()
            })
            .collect();
        let mut block = TableBlock::new(rows);
        if let Some(name) = &table.name {
            block = block.with_name(name.clone());
        }
        blocks.push(OfficeBlock::table(block));
    }

    let mut boxes: Vec<&crate::extract_model::ExtractedTextBox> = page.text_boxes.iter().collect();
    boxes.sort_by(|left, right| {
        let left_y = (f64::from(left.y) * 100.0).round() as i64;
        let right_y = (f64::from(right.y) * 100.0).round() as i64;
        let left_x = (f64::from(left.x) * 100.0).round() as i64;
        let right_x = (f64::from(right.x) * 100.0).round() as i64;
        right_y
            .cmp(&left_y)
            .then_with(|| left_x.cmp(&right_x))
            .then_with(|| left.text.cmp(&right.text))
    });
    for text_box in boxes {
        let box_ = TextBox::new(text_box.text.clone(), text_box.x, text_box.y).with_metrics(
            text_box.width,
            text_box.height,
            text_box.font_size,
        );
        blocks.push(OfficeBlock::text_box(box_));
    }

    // Keep the format parameter meaningful for future per-format tuning; today
    // only the page label differs, which is emitted by the envelope.
    let _ = format;
    blocks
}

/// Build one paragraph, styling it as a heading when its font is large enough.
fn paragraph_block(
    text: &str,
    font_size: f32,
    _format: OfficePlanFormat,
    options: &OfficePlanOptions,
    paragraph_number: &mut usize,
) -> OfficeBlock {
    let numbered = if options.number_paragraphs {
        let prefix = *paragraph_number;
        *paragraph_number = paragraph_number.saturating_add(1);
        format!("{prefix}. {text}")
    } else {
        text.to_owned()
    };
    if font_size >= options.heading_font_size {
        OfficeBlock::styled(numbered, crate::office_export::ParagraphStyle::Heading(1))
    } else {
        OfficeBlock::paragraph(numbered)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract_model::{ExtractedRun, ExtractedTable, ExtractedTextBox};

    fn source() -> ExtractedDocument {
        ExtractedDocument::new(
            "Deck",
            vec![
                ExtractedPage::new(1, 200.0, 300.0)
                    .with_runs(vec![ExtractedRun::new("B", 10.0, 280.0, 24.0)]),
                ExtractedPage::new(0, 200.0, 300.0)
                    .with_runs(vec![
                        ExtractedRun::new("A", 10.0, 280.0, 24.0),
                        ExtractedRun::new("body", 10.0, 260.0, 11.0),
                    ])
                    .with_table(ExtractedTable::new(vec![vec!["x".to_owned()]]))
                    .with_text_box(
                        ExtractedTextBox::new("note", 5.0, 250.0).with_metrics(20.0, 8.0, 9.0),
                    )
                    .with_unsupported(ExtractedUnsupported::VectorArt {
                        detail: "bezier".to_owned(),
                    })
                    .with_unsupported(ExtractedUnsupported::EmbeddedMedia {
                        detail: "clip".to_owned(),
                    }),
            ],
        )
    }

    #[test]
    fn plan_orders_pages_and_labels_each_format() {
        for format in OfficePlanFormat::all() {
            let envelope =
                build_office_export_plan(&source(), *format, &OfficePlanOptions::default())
                    .unwrap_or_else(|error| panic!("plan: {error}"));
            assert_eq!(
                envelope.page_labels(),
                &[
                    format!("{} 1", format.page_label()),
                    format!("{} 2", format.page_label())
                ]
            );
            assert_eq!(envelope.document().pages[0].index, 0);
            assert_eq!(envelope.document().pages[1].index, 1);
            assert!(envelope.validate().is_ok());
        }
    }

    #[test]
    fn plan_maps_runs_tables_and_text_boxes_into_blocks() {
        let envelope = build_office_export_plan(
            &source(),
            OfficePlanFormat::Word,
            &OfficePlanOptions::default(),
        )
        .expect("word plan");
        let blocks = &envelope.document().pages[0].blocks;
        assert!(matches!(
            blocks[0],
            OfficeBlock::Paragraph {
                style: crate::office_export::ParagraphStyle::Heading(1),
                ..
            }
        ));
        assert!(matches!(blocks[1], OfficeBlock::Paragraph { .. }));
        assert!(matches!(blocks[2], OfficeBlock::Table(_)));
        assert!(matches!(blocks[3], OfficeBlock::TextBox(_)));
    }

    #[test]
    fn numbering_is_deterministic_across_shuffled_pages() {
        let options = OfficePlanOptions {
            number_paragraphs: true,
            ..OfficePlanOptions::default()
        };
        let first = build_office_export_plan(&source(), OfficePlanFormat::Word, &options)
            .expect("first plan");
        let mut shuffled = source();
        shuffled.pages.reverse();
        let second = build_office_export_plan(&shuffled, OfficePlanFormat::Word, &options)
            .expect("second plan");
        assert_eq!(first.document(), second.document());
        assert_eq!(first.to_manifest_bytes(), second.to_manifest_bytes());
        let blocks = &first.document().pages[0].blocks;
        let numbered: Vec<&str> = blocks
            .iter()
            .filter_map(|block| match block {
                OfficeBlock::Paragraph { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(numbered, vec!["1. A", "2. body"]);
    }

    #[test]
    fn unsupported_constructs_are_typed_and_ordered() {
        let envelope = build_office_export_plan(
            &source(),
            OfficePlanFormat::Presentation,
            &OfficePlanOptions::default(),
        )
        .expect("pptx plan");
        let kinds: Vec<&str> = envelope
            .unsupported()
            .iter()
            .map(|item| item.kind)
            .collect();
        assert_eq!(kinds, vec!["embedded-media", "vector-art"]);
        let text = String::from_utf8(envelope.to_manifest_bytes()).expect("manifest");
        assert!(text.contains("unsupported: page 1 [embedded-media] clip"));
    }

    #[test]
    fn non_rectangular_tables_are_reported_not_faked() {
        let source = ExtractedDocument::new(
            "T",
            vec![
                ExtractedPage::new(0, 100.0, 100.0)
                    .with_table(ExtractedTable::new(vec![
                        vec!["a".to_owned()],
                        vec!["b".to_owned(), "c".to_owned()],
                    ]))
                    .with_table(ExtractedTable::new(vec![vec!["ok".to_owned()]])),
            ],
        );
        let envelope = build_office_export_plan(
            &source,
            OfficePlanFormat::Excel,
            &OfficePlanOptions::default(),
        )
        .expect("xlsx plan");
        assert_eq!(envelope.unsupported()[0].kind, "complex-table");
        let table_blocks = envelope.document().pages[0]
            .blocks
            .iter()
            .filter(|block| matches!(block, OfficeBlock::Table(_)))
            .count();
        assert_eq!(table_blocks, 1);
    }

    #[test]
    fn invalid_models_and_options_are_typed_errors() {
        let empty = ExtractedDocument::new("T", Vec::new());
        assert!(matches!(
            build_office_export_plan(
                &empty,
                OfficePlanFormat::Word,
                &OfficePlanOptions::default()
            ),
            Err(OfficePlanError::Model(ExtractModelError::EmptyDocument))
        ));
        let options = OfficePlanOptions {
            heading_font_size: f32::NAN,
            ..OfficePlanOptions::default()
        };
        assert!(matches!(
            build_office_export_plan(&source(), OfficePlanFormat::Word, &options),
            Err(OfficePlanError::Model(_))
        ));
    }

    #[test]
    fn envelope_writes_all_three_formats_deterministically() {
        for format in OfficePlanFormat::all() {
            let envelope =
                build_office_export_plan(&source(), *format, &OfficePlanOptions::default())
                    .unwrap_or_else(|error| panic!("plan: {error}"));
            let first = envelope.write().expect("write");
            let second = envelope.write().expect("write again");
            assert_eq!(first.bytes, second.bytes);
            assert_eq!(first.extension(), format.extension());
            assert!(first.report.part_count >= 3);
            assert!(
                first
                    .report
                    .unsupported
                    .iter()
                    .any(|item| item.kind == "vector-art" && item.page == 0)
            );
        }
    }

    #[test]
    fn export_kind_mapping_and_format_helpers_are_stable() {
        assert_eq!(
            OfficePlanFormat::from_export_kind(crate::ExportKind::Word),
            Some(OfficePlanFormat::Word)
        );
        assert_eq!(
            OfficePlanFormat::from_export_kind(crate::ExportKind::Spreadsheet),
            Some(OfficePlanFormat::Excel)
        );
        assert_eq!(
            OfficePlanFormat::from_export_kind(crate::ExportKind::Presentation),
            Some(OfficePlanFormat::Presentation)
        );
        assert_eq!(
            OfficePlanFormat::from_export_kind(crate::ExportKind::Text),
            None
        );
        assert_eq!(OfficePlanFormat::all().len(), 3);
        assert_eq!(OfficePlanFormat::Word.office_format(), OfficeFormat::Docx);
    }

    #[test]
    fn unsupported_budget_is_enforced() {
        let mut page = ExtractedPage::new(0, 100.0, 100.0);
        for index in 0..OFFICE_PLAN_MAX_UNSUPPORTED + 1 {
            page = page.with_unsupported(ExtractedUnsupported::VectorArt {
                detail: format!("curve-{index}"),
            });
        }
        let source = ExtractedDocument::new("T", vec![page]);
        assert!(matches!(
            build_office_export_plan(
                &source,
                OfficePlanFormat::Word,
                &OfficePlanOptions::default()
            ),
            Err(OfficePlanError::TooManyUnsupported { .. })
        ));
    }
}
