//! SpreadsheetML (`.xlsx`) writer.

use super::error::XlsxExportError;
use super::model::{CellValue, OfficeBlock, OfficeCell, OfficeDocument};
use super::xml;
use super::{OfficeExport, OfficeExportError, OfficeExportReport, OfficeExporter};
use super::{OfficeFormat, ReportedUnsupported, finish, validate};

const S_NS: &str = "http://schemas.openxmlformats.org/spreadsheetml/2006/main";
const R_NS: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";

/// Selects how an [`XlsxExporter`] maps source content to worksheets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SheetMode {
    /// One worksheet per source page.
    #[default]
    PerPage,
    /// One worksheet per table; pages with no tables fall back to one sheet
    /// per page.
    PerTable,
}

/// Deterministic, dependency-free Excel (`.xlsx`) adapter.
///
/// Cells carry typed values (text, number, boolean, empty) and are addressed
/// with A1 references. Non-tabular blocks are flattened into a single text
/// column and reported; positioned text boxes lose their coordinates and are
/// reported as well.
#[derive(Debug, Clone, Copy, Default)]
pub struct XlsxExporter {
    sheet_mode: SheetMode,
}

impl OfficeExporter for XlsxExporter {
    fn format(&self) -> OfficeFormat {
        OfficeFormat::Xlsx
    }

    fn export(&self, doc: &OfficeDocument) -> Result<OfficeExport, OfficeExportError> {
        Ok(self.write(doc)?)
    }
}

impl XlsxExporter {
    /// Creates an exporter with the default [`SheetMode::PerPage`] layout.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Selects per-page or per-table worksheets.
    #[must_use]
    pub const fn with_sheet_mode(mut self, mode: SheetMode) -> Self {
        self.sheet_mode = mode;
        self
    }

    /// Returns the configured sheet layout.
    #[must_use]
    pub const fn sheet_mode(&self) -> SheetMode {
        self.sheet_mode
    }

    /// Writes `doc` to a validated `.xlsx` archive with the adapter-specific
    /// error type.
    ///
    /// # Errors
    ///
    /// Returns [`XlsxExportError`] for invalid input or ZIP packaging failure.
    pub fn write(&self, doc: &OfficeDocument) -> Result<OfficeExport, XlsxExportError> {
        let validated = validate(doc)?;
        let mut unsupported = Vec::new();
        let sheets = self.build_sheets(&validated, &mut unsupported);

        let mut parts: Vec<(String, String)> = vec![
            (
                "[Content_Types].xml".to_owned(),
                content_types(sheets.len()),
            ),
            ("_rels/.rels".to_owned(), root_relationships()),
            ("xl/workbook.xml".to_owned(), workbook(&sheets)),
            (
                "xl/_rels/workbook.xml.rels".to_owned(),
                workbook_relationships(sheets.len()),
            ),
            ("xl/styles.xml".to_owned(), styles()),
        ];
        for (index, sheet) in sheets.iter().enumerate() {
            parts.push((
                format!("xl/worksheets/sheet{}.xml", index + 1),
                sheet_xml(sheet),
            ));
        }
        let entries: Vec<super::ZipEntry<'_>> = parts
            .iter()
            .map(|(name, xml)| super::ZipEntry::new(name.as_str(), xml.as_bytes()))
            .collect();
        let required: [&str; 6] = [
            "[Content_Types].xml",
            "_rels/.rels",
            "xl/workbook.xml",
            "xl/_rels/workbook.xml.rels",
            "xl/styles.xml",
            "xl/worksheets/sheet1.xml",
        ];
        let report = OfficeExportReport {
            format: OfficeFormat::Xlsx,
            part_count: 0,
            unsupported,
        };
        finish(&entries, &required, report, XlsxExportError::Zip)
    }

    fn build_sheets(
        &self,
        validated: &super::Validated<'_>,
        unsupported: &mut Vec<ReportedUnsupported>,
    ) -> Vec<Sheet> {
        let mut sheets: Vec<Sheet> = Vec::new();
        let mut used_names: Vec<String> = Vec::new();

        match self.sheet_mode {
            SheetMode::PerPage => {
                for page in &validated.pages {
                    let mut rows: Vec<Vec<OfficeCell>> = Vec::new();
                    for block in &page.blocks {
                        collect_block(block, page.index, unsupported, &mut rows);
                    }
                    add_sheet(
                        &mut sheets,
                        &mut used_names,
                        format!("Page {}", page.index + 1),
                        rows,
                    );
                }
            }
            SheetMode::PerTable => {
                for page in &validated.pages {
                    for block in &page.blocks {
                        if let OfficeBlock::Table(table) = block {
                            let name = table.name.clone().unwrap_or_else(|| {
                                format!("Table {}_{}", page.index + 1, used_names.len() + 1)
                            });
                            add_sheet(&mut sheets, &mut used_names, name, table.rows.clone());
                        }
                    }
                }
                if sheets.is_empty() {
                    for page in &validated.pages {
                        let mut rows: Vec<Vec<OfficeCell>> = Vec::new();
                        for block in &page.blocks {
                            collect_block(block, page.index, unsupported, &mut rows);
                        }
                        add_sheet(
                            &mut sheets,
                            &mut used_names,
                            format!("Page {}", page.index + 1),
                            rows,
                        );
                    }
                } else {
                    // Report non-tabular content that a table-only sheet does
                    // not carry, so nothing is silently discarded.
                    for page in &validated.pages {
                        for block in &page.blocks {
                            match block {
                                OfficeBlock::Table(_) => {}
                                OfficeBlock::TextBox(box_) => {
                                    unsupported.push(ReportedUnsupported::new(
                                        OfficeFormat::Xlsx,
                                        page.index,
                                        "text_box_position",
                                        format!(
                                            "text box '{}' not represented in table sheets",
                                            box_.text
                                        ),
                                    ))
                                }
                                OfficeBlock::Unsupported(content) => {
                                    unsupported.push(ReportedUnsupported::new(
                                        OfficeFormat::Xlsx,
                                        page.index,
                                        content.kind(),
                                        format!("omitted: {}", content.detail()),
                                    ))
                                }
                                _ => unsupported.push(ReportedUnsupported::new(
                                    OfficeFormat::Xlsx,
                                    page.index,
                                    "non_tabular_text",
                                    "text block not represented in table sheets",
                                )),
                            }
                        }
                    }
                }
            }
        }
        sheets
    }
}

struct Sheet {
    name: String,
    rows: Vec<Vec<OfficeCell>>,
}

fn add_sheet(
    sheets: &mut Vec<Sheet>,
    used: &mut Vec<String>,
    name: String,
    rows: Vec<Vec<OfficeCell>>,
) {
    let unique = unique_sheet_name(name, used);
    used.push(unique.clone());
    sheets.push(Sheet { name: unique, rows });
}

fn sanitize_sheet_name(name: &str) -> String {
    let mut out: String = name
        .chars()
        .map(|ch| match ch {
            ':' | '\\' | '/' | '?' | '*' | '[' | ']' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();
    if out.is_empty() {
        out.push('S');
    }
    // Spreadsheet sheet names are limited to 31 characters.
    if out.chars().count() > 31 {
        out = out.chars().take(31).collect();
    }
    out
}

fn unique_sheet_name(name: String, used: &[String]) -> String {
    let base = sanitize_sheet_name(&name);
    if !used.iter().any(|existing| existing == &base) {
        return base;
    }
    for suffix in 2..=usize::MAX {
        let tail = format!("_{suffix}");
        let keep = 31usize.saturating_sub(tail.len());
        let truncated: String = base.chars().take(keep).collect();
        let candidate = format!("{truncated}{tail}");
        if !used.iter().any(|existing| existing == &candidate) {
            return candidate;
        }
    }
    base
}

fn collect_block(
    block: &OfficeBlock,
    page: usize,
    unsupported: &mut Vec<ReportedUnsupported>,
    rows: &mut Vec<Vec<OfficeCell>>,
) {
    match block {
        OfficeBlock::Paragraph { text, .. } | OfficeBlock::ListItem { text, .. } => {
            rows.push(vec![OfficeCell::text(text.clone())]);
        }
        OfficeBlock::Table(table) => {
            if !rows.is_empty() {
                rows.push(Vec::new());
            }
            rows.extend(table.rows.iter().cloned());
            if !rows.is_empty() {
                rows.push(Vec::new());
            }
        }
        OfficeBlock::TextBox(box_) => {
            rows.push(vec![OfficeCell::text(box_.text.clone())]);
            unsupported.push(ReportedUnsupported::new(
                OfficeFormat::Xlsx,
                page,
                "text_box_position",
                "positioned text box flattened into a single text cell",
            ));
        }
        OfficeBlock::Unsupported(content) => unsupported.push(ReportedUnsupported::new(
            OfficeFormat::Xlsx,
            page,
            content.kind(),
            format!("omitted: {}", content.detail()),
        )),
    }
}

fn content_types(sheet_count: usize) -> String {
    let mut out = format!(
        "{decl}<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\">\
         <Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/>\
         <Default Extension=\"xml\" ContentType=\"application/xml\"/>\
         <Override PartName=\"/xl/workbook.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml\"/>\
         <Override PartName=\"/xl/styles.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.styles+xml\"/>",
        decl = xml::declaration(),
    );
    for index in 1..=sheet_count {
        out.push_str(&format!(
            "<Override PartName=\"/xl/worksheets/sheet{index}.xml\" \
             ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml\"/>"
        ));
    }
    out.push_str("</Types>");
    out
}

fn root_relationships() -> String {
    format!(
        "{decl}<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\
         <Relationship Id=\"rId1\" Type=\"{r}/officeDocument\" Target=\"xl/workbook.xml\"/>\
         </Relationships>",
        decl = xml::declaration(),
        r = R_NS,
    )
}

fn workbook(sheets: &[Sheet]) -> String {
    let mut out = format!(
        "{decl}<workbook xmlns=\"{s}\" xmlns:r=\"{r}\"><sheets>",
        decl = xml::declaration(),
        s = S_NS,
        r = R_NS,
    );
    for (index, sheet) in sheets.iter().enumerate() {
        out.push_str(&format!(
            "<sheet name=\"{name}\" sheetId=\"{id}\" r:id=\"rId{id}\"/>",
            name = xml::escape(&sheet.name),
            id = index + 1,
        ));
    }
    out.push_str("</sheets></workbook>");
    out
}

fn workbook_relationships(sheet_count: usize) -> String {
    let mut out = format!(
        "{decl}<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">",
        decl = xml::declaration(),
    );
    for index in 1..=sheet_count {
        out.push_str(&format!(
            "<Relationship Id=\"rId{index}\" Type=\"{r}/worksheet\" Target=\"worksheets/sheet{index}.xml\"/>",
            r = R_NS,
        ));
    }
    let styles_id = sheet_count + 1;
    out.push_str(&format!(
        "<Relationship Id=\"rId{styles_id}\" Type=\"{r}/styles\" Target=\"styles.xml\"/>",
        r = R_NS,
    ));
    out.push_str("</Relationships>");
    out
}

fn styles() -> String {
    format!(
        "{decl}<styleSheet xmlns=\"{s}\">\
         <fonts count=\"1\"><font/></fonts>\
         <fills count=\"1\"><fill><patternFill patternType=\"none\"/></fill></fills>\
         <borders count=\"1\"><border/></borders>\
         <cellStyleXfs count=\"1\"><xf/></cellStyleXfs>\
         <cellXfs count=\"1\"><xf/></cellXfs></styleSheet>",
        decl = xml::declaration(),
        s = S_NS,
    )
}

fn sheet_xml(sheet: &Sheet) -> String {
    let mut out = format!(
        "{decl}<worksheet xmlns=\"{s}\"><sheetData>",
        decl = xml::declaration(),
        s = S_NS,
    );
    for (row_index, cells) in sheet.rows.iter().enumerate() {
        out.push_str(&format!("<row r=\"{}\">", row_index + 1));
        for (column_index, cell) in cells.iter().enumerate() {
            let reference = xml::cell_ref(column_index, row_index);
            match &cell.value {
                CellValue::Text(text) => out.push_str(&format!(
                    "<c r=\"{reference}\" t=\"inlineStr\"><is><t xml:space=\"preserve\">{text}</t></is></c>",
                    text = xml::escape(text),
                )),
                CellValue::Number(number) => out.push_str(&format!(
                    "<c r=\"{reference}\"><v>{}</v></c>",
                    xml::number(*number),
                )),
                CellValue::Boolean(value) => out.push_str(&format!(
                    "<c r=\"{reference}\" t=\"b\"><v>{}</v></c>",
                    u8::from(*value),
                )),
                CellValue::Empty => out.push_str(&format!("<c r=\"{reference}\"/>")),
            }
        }
        out.push_str("</row>");
    }
    out.push_str("</sheetData></worksheet>");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::office_export::zip;
    use crate::office_export::{OfficePage, TableBlock, TextBox, UnsupportedContent};

    fn doc_with(blocks: Vec<OfficeBlock>) -> OfficeDocument {
        OfficeDocument::new(
            "Doc",
            vec![OfficePage::new(0, 612.0, 792.0).with_blocks(blocks)],
        )
    }

    fn part<'a>(export: &'a OfficeExport, name: &str) -> &'a str {
        let views = zip::read_zip(&export.bytes).expect("valid archive");
        let view = views
            .iter()
            .find(|view| view.name == name)
            .unwrap_or_else(|| panic!("missing part {name}"));
        std::str::from_utf8(view.data).expect("utf8 part")
    }

    #[test]
    fn xlsx_has_required_parts_and_valid_zip() {
        let export = XlsxExporter::new().write(&doc_with(vec![])).expect("write");
        let views = zip::read_zip(&export.bytes).expect("valid zip");
        zip::require_parts(
            &views,
            &[
                "[Content_Types].xml",
                "_rels/.rels",
                "xl/workbook.xml",
                "xl/_rels/workbook.xml.rels",
                "xl/styles.xml",
                "xl/worksheets/sheet1.xml",
            ],
        )
        .expect("required parts");
        assert!(part(&export, "xl/workbook.xml").contains("<sheet name=\"Page 1\""));
    }

    #[test]
    fn xlsx_sheet_per_page_and_per_table() {
        let doc = OfficeDocument::new(
            "Doc",
            vec![
                OfficePage::new(0, 612.0, 792.0).with_blocks(vec![
                    OfficeBlock::paragraph("page text"),
                    OfficeBlock::table(
                        TableBlock::new(vec![vec![OfficeCell::text("A")]]).with_name("First"),
                    ),
                ]),
                OfficePage::new(1, 612.0, 792.0).with_blocks(vec![OfficeBlock::table(
                    TableBlock::new(vec![vec![OfficeCell::text("B")]]).with_name("Second"),
                )]),
            ],
        );
        let per_page = XlsxExporter::new().write(&doc).expect("per page");
        let workbook = part(&per_page, "xl/workbook.xml");
        assert!(workbook.contains("name=\"Page 1\""));
        assert!(workbook.contains("name=\"Page 2\""));
        assert!(part(&per_page, "xl/worksheets/sheet1.xml").contains("page text"));

        let per_table = XlsxExporter::new()
            .with_sheet_mode(SheetMode::PerTable)
            .write(&doc)
            .expect("per table");
        assert_eq!(XlsxExporter::new().sheet_mode(), SheetMode::PerPage);
        assert_eq!(
            XlsxExporter::new()
                .with_sheet_mode(SheetMode::PerTable)
                .sheet_mode(),
            SheetMode::PerTable
        );
        let workbook = part(&per_table, "xl/workbook.xml");
        assert!(workbook.contains("name=\"First\""));
        assert!(workbook.contains("name=\"Second\""));
    }

    #[test]
    fn xlsx_cell_types_and_references_are_correct() {
        let doc = doc_with(vec![OfficeBlock::table(TableBlock::new(vec![
            vec![
                OfficeCell::text("A1"),
                OfficeCell::number(12.5),
                OfficeCell::boolean(true),
                OfficeCell::empty(),
            ],
            vec![OfficeCell::text("A2")],
        ]))]);
        let export = XlsxExporter::new().write(&doc).expect("write");
        let xml = part(&export, "xl/worksheets/sheet1.xml");
        assert!(xml.contains("r=\"A1\" t=\"inlineStr\""));
        assert!(xml.contains("r=\"B1\"><v>12.5</v>"));
        assert!(xml.contains("r=\"C1\" t=\"b\"><v>1</v>"));
        assert!(xml.contains("r=\"D1\"/>"));
        assert!(xml.contains("r=\"A2\" t=\"inlineStr\""));
        assert!(!xml.contains("&lt;"));
    }

    #[test]
    fn xlsx_escapes_xml_in_text_and_sheet_names() {
        let doc = OfficeDocument::new(
            "T",
            vec![
                OfficePage::new(0, 100.0, 100.0).with_blocks(vec![OfficeBlock::table(
                    TableBlock::new(vec![vec![OfficeCell::text("<&>\"'\0")]]).with_name("a/b:c"),
                )]),
            ],
        );
        let export = XlsxExporter::new()
            .with_sheet_mode(SheetMode::PerTable)
            .write(&doc)
            .expect("write");
        let workbook = part(&export, "xl/workbook.xml");
        assert!(workbook.contains("name=\"a_b_c\""));
        let xml = part(&export, "xl/worksheets/sheet1.xml");
        assert!(xml.contains("&lt;&amp;&gt;&quot;&apos;\u{fffd}"));
    }

    #[test]
    fn xlsx_reports_unsupported_content() {
        let doc = doc_with(vec![
            OfficeBlock::text_box(TextBox::new("pos", 1.0, 2.0)),
            OfficeBlock::unsupported(UnsupportedContent::Annotation {
                detail: "sticky note".to_owned(),
            }),
        ]);
        let export = XlsxExporter::new().write(&doc).expect("write");
        assert_eq!(export.report.unsupported.len(), 2);
        assert_eq!(export.report.unsupported[0].kind, "text_box_position");
        assert_eq!(export.report.unsupported[1].kind, "annotation");
        assert!(part(&export, "xl/worksheets/sheet1.xml").contains("pos"));
    }

    #[test]
    fn xlsx_is_deterministic() {
        let doc = doc_with(vec![OfficeBlock::paragraph("stable")]);
        assert_eq!(
            XlsxExporter::new().write(&doc).expect("a").bytes,
            XlsxExporter::new().write(&doc).expect("b").bytes
        );
    }

    #[test]
    fn xlsx_sheet_names_are_bounded_and_unique() {
        let long = "x".repeat(50);
        let used = vec![sanitize_sheet_name(&long)];
        let unique = unique_sheet_name(long.clone(), &used);
        assert!(unique.chars().count() <= 31);
        assert_ne!(unique, used[0]);
        assert_eq!(sanitize_sheet_name(""), "S");
    }

    #[test]
    fn xlsx_rejects_invalid_models_with_typed_errors() {
        let bad_number = doc_with(vec![OfficeBlock::table(TableBlock::new(vec![vec![
            OfficeCell::number(f64::INFINITY),
        ]]))]);
        assert!(matches!(
            XlsxExporter::new().write(&bad_number),
            Err(XlsxExportError::Invalid(
                crate::office_export::ModelError::NonFinite { .. }
            ))
        ));
        let dup = OfficeDocument::new(
            "T",
            vec![
                OfficePage::new(0, 10.0, 10.0),
                OfficePage::new(0, 10.0, 10.0),
            ],
        );
        assert!(matches!(
            XlsxExporter::new().write(&dup),
            Err(XlsxExportError::Invalid(
                crate::office_export::ModelError::DuplicatePage { .. }
            ))
        ));
    }
}
