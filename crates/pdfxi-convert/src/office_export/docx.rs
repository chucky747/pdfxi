//! WordprocessingML (`.docx`) writer.

use super::error::DocxExportError;
use super::model::{OfficeBlock, OfficeDocument, ParagraphStyle, TableBlock, TextBox};
use super::xml;
use super::{OfficeExport, OfficeExportError, OfficeExportReport, OfficeExporter};
use super::{OfficeFormat, ReportedUnsupported, finish, validate};

const W_NS: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";

/// Deterministic, dependency-free Word (`.docx`) adapter.
///
/// Paragraphs, headings, lists, and tables are written as real WordprocessingML
/// structures. Positioned text boxes from the extraction model are flattened
/// into paragraphs and reported, because a faithful anchored drawing is out of
/// scope for this writer.
#[derive(Debug, Clone, Copy, Default)]
pub struct DocxExporter;

impl DocxExporter {
    /// Writes `doc` to a validated `.docx` archive with the adapter-specific
    /// error type.
    ///
    /// # Errors
    ///
    /// Returns [`DocxExportError`] for invalid input or ZIP packaging failure.
    pub fn write(&self, doc: &OfficeDocument) -> Result<OfficeExport, DocxExportError> {
        let validated = validate(doc)?;
        let mut unsupported = Vec::new();

        let mut body = String::new();
        for (position, page) in validated.pages.iter().enumerate() {
            if position > 0 {
                body.push_str("<w:p><w:r><w:br w:type=\"page\"/></w:r></w:p>");
            }
            for block in &page.blocks {
                render_block(block, page.index, &mut unsupported, &mut body);
            }
        }

        let first = validated.pages[0];
        let width_twips = (f64::from(first.width) * 20.0) as i64;
        let height_twips = (f64::from(first.height) * 20.0) as i64;
        // Word needs the orientation flag in addition to swapped dimensions,
        // otherwise a wide page can be shown as a portrait page with a wide
        // media box in some viewers.
        let orientation = if width_twips > height_twips {
            " w:orient=\"landscape\""
        } else {
            ""
        };
        let document = format!(
            "{decl}<w:document xmlns:w=\"{w}\"><w:body>{body}\
             <w:sectPr><w:pgSz w:w=\"{width}\" w:h=\"{height}\"{orientation}/></w:sectPr></w:body></w:document>",
            decl = xml::declaration(),
            w = W_NS,
            width = width_twips,
            height = height_twips,
            orientation = orientation,
        );

        let parts = [
            ("[Content_Types].xml", content_types()),
            ("_rels/.rels", root_relationships()),
            ("docProps/core.xml", core_properties(validated.title)),
            ("word/_rels/document.xml.rels", document_relationships()),
            ("word/styles.xml", styles()),
            ("word/numbering.xml", numbering()),
            ("word/document.xml", document),
        ];
        let entries: Vec<super::ZipEntry<'_>> = parts
            .iter()
            .map(|(name, xml)| super::ZipEntry::new(name, xml.as_bytes()))
            .collect();
        let report = OfficeExportReport {
            format: OfficeFormat::Docx,
            part_count: 0,
            unsupported,
        };
        finish(
            &entries,
            &[
                "[Content_Types].xml",
                "_rels/.rels",
                "word/document.xml",
                "word/styles.xml",
                "word/numbering.xml",
            ],
            report,
            DocxExportError::Zip,
        )
    }
}

impl OfficeExporter for DocxExporter {
    fn format(&self) -> OfficeFormat {
        OfficeFormat::Docx
    }

    fn export(&self, doc: &OfficeDocument) -> Result<OfficeExport, OfficeExportError> {
        Ok(self.write(doc)?)
    }
}

fn content_types() -> String {
    format!(
        "{decl}<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\">\
         <Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/>\
         <Default Extension=\"xml\" ContentType=\"application/xml\"/>\
         <Override PartName=\"/word/document.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml\"/>\
         <Override PartName=\"/word/styles.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml\"/>\
         <Override PartName=\"/word/numbering.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.numbering+xml\"/>\
         <Override PartName=\"/docProps/core.xml\" ContentType=\"application/vnd.openxmlformats-package.core-properties+xml\"/>\
         </Types>",
        decl = xml::declaration(),
    )
}

fn root_relationships() -> String {
    format!(
        "{decl}<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\
         <Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument\" Target=\"word/document.xml\"/>\
         <Relationship Id=\"rId2\" Type=\"http://schemas.openxmlformats.org/package/2006/relationships/metadata/core-properties\" Target=\"docProps/core.xml\"/>\
         </Relationships>",
        decl = xml::declaration(),
    )
}

fn document_relationships() -> String {
    format!(
        "{decl}<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\
         <Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles\" Target=\"styles.xml\"/>\
         <Relationship Id=\"rId2\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/numbering\" Target=\"numbering.xml\"/>\
         </Relationships>",
        decl = xml::declaration(),
    )
}

fn core_properties(title: &str) -> String {
    format!(
        "{decl}<cp:coreProperties \
         xmlns:cp=\"http://schemas.openxmlformats.org/package/2006/metadata/core-properties\" \
         xmlns:dc=\"http://purl.org/dc/elements/1.1/\">\
         <dc:title>{title}</dc:title></cp:coreProperties>",
        decl = xml::declaration(),
        title = xml::escape(title),
    )
}

fn styles() -> String {
    let mut out = format!(
        "{decl}<w:styles xmlns:w=\"{w}\">\
         <w:style w:type=\"paragraph\" w:default=\"1\" w:styleId=\"Normal\"><w:name w:val=\"Normal\"/></w:style>\
         <w:style w:type=\"paragraph\" w:styleId=\"Title\"><w:name w:val=\"Title\"/><w:basedOn w:val=\"Normal\"/></w:style>\
         <w:style w:type=\"paragraph\" w:styleId=\"Quote\"><w:name w:val=\"Quote\"/><w:basedOn w:val=\"Normal\"/></w:style>\
         <w:style w:type=\"paragraph\" w:styleId=\"ListParagraph\"><w:name w:val=\"List Paragraph\"/><w:basedOn w:val=\"Normal\"/></w:style>",
        decl = xml::declaration(),
        w = W_NS,
    );
    for level in 1..=9_u8 {
        out.push_str(&format!(
            "<w:style w:type=\"paragraph\" w:styleId=\"Heading{level}\">\
             <w:name w:val=\"heading {level}\"/><w:basedOn w:val=\"Normal\"/>\
             <w:pPr><w:outlineLvl w:val=\"{outline}\"/></w:pPr></w:style>",
            outline = level - 1,
        ));
    }
    out.push_str("</w:styles>");
    out
}

fn numbering() -> String {
    let mut out = format!(
        "{decl}<w:numbering xmlns:w=\"{w}\">",
        decl = xml::declaration(),
        w = W_NS
    );
    for (abstract_id, format) in [(0_u8, "bullet"), (1_u8, "decimal")] {
        out.push_str(&format!(
            "<w:abstractNum w:abstractNumId=\"{abstract_id}\">"
        ));
        for level in 0..=8_u8 {
            let marker = if format == "bullet" {
                "\u{2022}".to_owned()
            } else {
                format!("%{}.", level + 1)
            };
            out.push_str(&format!(
                "<w:lvl w:ilvl=\"{level}\"><w:start w:val=\"1\"/>\
                 <w:numFmt w:val=\"{format}\"/><w:lvlText w:val=\"{marker}\"/>\
                 <w:lvlJc w:val=\"left\"/></w:lvl>"
            ));
        }
        out.push_str("</w:abstractNum>");
    }
    out.push_str("<w:num w:numId=\"1\"><w:abstractNumId w:val=\"0\"/></w:num>");
    out.push_str("<w:num w:numId=\"2\"><w:abstractNumId w:val=\"1\"/></w:num>");
    out.push_str("</w:numbering>");
    out
}

fn render_block(
    block: &OfficeBlock,
    page: usize,
    unsupported: &mut Vec<ReportedUnsupported>,
    out: &mut String,
) {
    match block {
        OfficeBlock::Paragraph { text, style } => {
            let style_id = match style {
                ParagraphStyle::Normal => None,
                ParagraphStyle::Heading(level) => Some(format!("Heading{level}")),
                ParagraphStyle::Title => Some("Title".to_owned()),
                ParagraphStyle::Quote => Some("Quote".to_owned()),
            };
            out.push_str("<w:p>");
            if let Some(style_id) = style_id {
                out.push_str(&format!(
                    "<w:pPr><w:pStyle w:val=\"{}\"/></w:pPr>",
                    xml::escape(&style_id)
                ));
            }
            push_run(text, out);
            out.push_str("</w:p>");
        }
        OfficeBlock::ListItem {
            text,
            level,
            ordered,
        } => {
            let num_id = if *ordered { 2 } else { 1 };
            out.push_str(&format!(
                "<w:p><w:pPr><w:pStyle w:val=\"ListParagraph\"/>\
                 <w:numPr><w:ilvl w:val=\"{level}\"/><w:numId w:val=\"{num_id}\"/></w:numPr></w:pPr>"
            ));
            push_run(text, out);
            out.push_str("</w:p>");
        }
        OfficeBlock::Table(table) => render_table(table, out),
        OfficeBlock::TextBox(box_) => {
            render_flattened_text_box(box_, out);
            unsupported.push(ReportedUnsupported::new(
                OfficeFormat::Docx,
                page,
                "text_box_position",
                "positioned text box flattened to a paragraph",
            ));
        }
        OfficeBlock::Unsupported(content) => unsupported.push(ReportedUnsupported::new(
            OfficeFormat::Docx,
            page,
            content.kind(),
            format!("omitted: {}", content.detail()),
        )),
    }
}

fn push_run(text: &str, out: &mut String) {
    out.push_str("<w:r><w:t xml:space=\"preserve\">");
    out.push_str(&xml::escape(text));
    out.push_str("</w:t></w:r>");
}

fn render_flattened_text_box(box_: &TextBox, out: &mut String) {
    out.push_str("<w:p><w:pPr><w:pStyle w:val=\"Quote\"/></w:pPr>");
    push_run(&box_.text, out);
    out.push_str("</w:p>");
}

fn cell_text(table: &TableBlock, row: usize, column: usize) -> String {
    table
        .rows
        .get(row)
        .and_then(|cells| cells.get(column))
        .map(|cell| match &cell.value {
            super::CellValue::Text(text) => text.clone(),
            super::CellValue::Number(number) => xml::number(*number),
            super::CellValue::Boolean(value) => if *value { "TRUE" } else { "FALSE" }.to_owned(),
            super::CellValue::Empty => String::new(),
        })
        .unwrap_or_default()
}

fn render_table(table: &TableBlock, out: &mut String) {
    if table.rows.is_empty() {
        return;
    }
    let columns = table.rows[0].len();
    out.push_str("<w:tbl><w:tblPr><w:tblW w:w=\"0\" w:type=\"auto\"/></w:tblPr><w:tblGrid>");
    for _ in 0..columns {
        out.push_str("<w:gridCol/>");
    }
    out.push_str("</w:tblGrid>");
    for row in 0..table.rows.len() {
        out.push_str("<w:tr>");
        for column in 0..columns {
            out.push_str("<w:tc><w:tcPr><w:tcW w:w=\"2400\" w:type=\"dxa\"/></w:tcPr>");
            out.push_str("<w:p>");
            push_run(&cell_text(table, row, column), out);
            out.push_str("</w:p></w:tc>");
        }
        out.push_str("</w:tr>");
    }
    out.push_str("</w:tbl>");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::office_export::zip;
    use crate::office_export::{OfficeCell, OfficePage, TableBlock, UnsupportedContent, ZipError};

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

    /// Extracts the literal text of every `<w:t>` element.
    ///
    /// The tag-name check matters: `<w:tbl>`, `<w:tc>` and `<w:tr>` also start
    /// with the bytes `<w:t` and must not be mistaken for text runs.
    fn texts(xml: &str) -> Vec<String> {
        let bytes = xml.as_bytes();
        let mut result = Vec::new();
        let mut search_from = 0;
        while let Some(relative) = xml[search_from..].find("<w:t") {
            let start = search_from + relative;
            let after_name = start + 4;
            let Some(next) = bytes.get(after_name) else {
                break;
            };
            if *next != b'>' && *next != b' ' && *next != b'\t' {
                search_from = after_name;
                continue;
            }
            let Some(tag_end) = xml[after_name..].find('>') else {
                break;
            };
            let content_start = after_name + tag_end + 1;
            let Some(end) = xml[content_start..].find("</w:t>") else {
                break;
            };
            result.push(xml[content_start..content_start + end].to_owned());
            search_from = content_start + end + 6;
        }
        result
    }

    #[test]
    fn docx_has_required_parts_and_valid_zip() {
        let export = DocxExporter.write(&doc_with(vec![])).expect("write");
        let views = zip::read_zip(&export.bytes).expect("valid zip");
        zip::require_parts(
            &views,
            &[
                "[Content_Types].xml",
                "_rels/.rels",
                "word/document.xml",
                "word/styles.xml",
                "word/numbering.xml",
            ],
        )
        .expect("required parts");
        let xml = part(&export, "word/document.xml");
        assert!(xml.contains("<w:sectPr>"));
        assert!(!xml.contains("w:orient=\"landscape\""));
        assert!(
            part(&export, "[Content_Types].xml").contains("wordprocessingml.document.main+xml")
        );
        assert!(export.report.part_count >= 7);
    }

    #[test]
    fn docx_wide_page_declares_landscape_orientation() {
        let doc = OfficeDocument::new(
            "Wide",
            vec![
                OfficePage::new(0, 720.0, 405.0).with_blocks(vec![OfficeBlock::paragraph("slide")]),
            ],
        );
        let export = DocxExporter.write(&doc).expect("write");
        let xml = part(&export, "word/document.xml");
        assert!(xml.contains("w:orient=\"landscape\""));
        assert!(xml.contains("<w:pgSz w:w=\"14400\" w:h=\"8100\""));
    }

    #[test]
    fn docx_page_text_round_trips() {
        let export = DocxExporter
            .write(&doc_with(vec![OfficeBlock::paragraph("Hello DOCX")]))
            .expect("write");
        assert!(texts(part(&export, "word/document.xml")).contains(&"Hello DOCX".to_owned()));
    }

    #[test]
    fn docx_renders_headings_lists_and_tables() {
        let doc = doc_with(vec![
            OfficeBlock::styled("Heading", ParagraphStyle::Heading(2)),
            OfficeBlock::list_item("bullet", 0, false),
            OfficeBlock::list_item("numbered", 1, true),
            OfficeBlock::table(TableBlock::new(vec![
                vec![OfficeCell::text("A"), OfficeCell::text("B")],
                vec![OfficeCell::text("C"), OfficeCell::text("D")],
            ])),
        ]);
        let export = DocxExporter.write(&doc).expect("write");
        let xml = part(&export, "word/document.xml");
        assert!(xml.contains("<w:pStyle w:val=\"Heading2\"/>"));
        assert!(xml.contains("<w:numId w:val=\"1\"/>"));
        assert!(xml.contains("<w:numId w:val=\"2\"/>"));
        assert!(xml.contains("<w:tbl>"));
        assert_eq!(xml.matches("<w:tr>").count(), 2);
        assert!(texts(xml).contains(&"D".to_owned()));
    }

    #[test]
    fn docx_escapes_text_and_controls() {
        let export = DocxExporter
            .write(&doc_with(vec![OfficeBlock::paragraph("<a>&\"'\0")]))
            .expect("write");
        let xml = part(&export, "word/document.xml");
        assert!(xml.contains("&lt;a&gt;&amp;&quot;&apos;\u{fffd}"));
        assert!(!xml.contains("<a>"));
    }

    #[test]
    fn docx_reports_unsupported_content() {
        let doc = doc_with(vec![
            OfficeBlock::text_box(TextBox::new("pos", 1.0, 2.0)),
            OfficeBlock::unsupported(UnsupportedContent::Image {
                detail: "photo".to_owned(),
            }),
        ]);
        let export = DocxExporter.write(&doc).expect("write");
        assert_eq!(export.report.unsupported.len(), 2);
        assert_eq!(export.report.unsupported[0].kind, "text_box_position");
        assert_eq!(export.report.unsupported[1].kind, "image");
        assert!(texts(part(&export, "word/document.xml")).contains(&"pos".to_owned()));
    }

    #[test]
    fn docx_is_deterministic() {
        let doc = doc_with(vec![OfficeBlock::paragraph("stable")]);
        assert_eq!(
            DocxExporter.write(&doc).expect("a").bytes,
            DocxExporter.write(&doc).expect("b").bytes
        );
    }

    #[test]
    fn docx_rejects_invalid_models_with_typed_errors() {
        let empty = OfficeDocument::new("T", Vec::new());
        assert_eq!(
            DocxExporter.write(&empty).map(|_| ()),
            Err(DocxExportError::Invalid(
                crate::office_export::ModelError::EmptyDocument
            ))
        );
        let bad = OfficeDocument::new(
            "T",
            vec![OfficePage::new(0, -1.0, 10.0).with_blocks(vec![OfficeBlock::paragraph("x")])],
        );
        assert!(matches!(
            DocxExporter.write(&bad),
            Err(DocxExportError::Invalid(
                crate::office_export::ModelError::InvalidPageGeometry { .. }
            ))
        ));
    }

    #[test]
    fn docx_error_type_is_distinct_and_transparent() {
        let err = DocxExportError::Zip(ZipError::MissingPart("word/document.xml".to_owned()));
        assert_eq!(
            err,
            DocxExportError::Zip(ZipError::MissingPart("word/document.xml".to_owned()))
        );
        let wrapped = OfficeExportError::from(err);
        assert!(matches!(wrapped, OfficeExportError::Docx(_)));
    }
}
