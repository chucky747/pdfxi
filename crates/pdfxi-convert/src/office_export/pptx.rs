//! PresentationML (`.pptx`) writer.

use super::error::PptxExportError;
use super::model::{OfficeBlock, OfficeDocument, TableBlock, TextBox};
use super::xml;
use super::{OfficeExport, OfficeExportError, OfficeExportReport, OfficeExporter};
use super::{OfficeFormat, ReportedUnsupported, finish, validate};

const P_NS: &str = "http://schemas.openxmlformats.org/presentationml/2006/main";
const A_NS: &str = "http://schemas.openxmlformats.org/drawingml/2006/main";
const R_NS: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";

/// Deterministic, dependency-free PowerPoint (`.pptx`) adapter.
///
/// Every page becomes a slide. Positioned text boxes become anchored text-box
/// shapes with EMU geometry; other text is flattened into generated text boxes
/// and reported so the caller knows that exact layout was not preserved.
#[derive(Debug, Clone, Copy, Default)]
pub struct PptxExporter;

impl PptxExporter {
    /// Writes `doc` to a validated `.pptx` archive with the adapter-specific
    /// error type.
    ///
    /// # Errors
    ///
    /// Returns [`PptxExportError`] for invalid input, impossible slide geometry,
    /// or ZIP packaging failure.
    pub fn write(&self, doc: &OfficeDocument) -> Result<OfficeExport, PptxExportError> {
        let validated = validate(doc)?;
        let first = validated.pages[0];
        let slide_width = xml::points_to_emu(f64::from(first.width));
        let slide_height = xml::points_to_emu(f64::from(first.height));
        if slide_width == 0 || slide_height == 0 {
            return Err(PptxExportError::SlideGeometry);
        }

        let mut unsupported = Vec::new();
        let slide_count = validated.pages.len();
        let mut parts: Vec<(String, String)> = vec![
            ("[Content_Types].xml".to_owned(), content_types(slide_count)),
            ("_rels/.rels".to_owned(), root_relationships()),
            (
                "docProps/core.xml".to_owned(),
                core_properties(validated.title),
            ),
            (
                "ppt/presentation.xml".to_owned(),
                presentation(slide_width, slide_height, slide_count),
            ),
            (
                "ppt/_rels/presentation.xml.rels".to_owned(),
                presentation_relationships(slide_count),
            ),
            (
                "ppt/slideMasters/slideMaster1.xml".to_owned(),
                slide_master(),
            ),
            (
                "ppt/slideMasters/_rels/slideMaster1.xml.rels".to_owned(),
                slide_master_relationships(),
            ),
            (
                "ppt/slideLayouts/slideLayout1.xml".to_owned(),
                slide_layout(),
            ),
            (
                "ppt/slideLayouts/_rels/slideLayout1.xml.rels".to_owned(),
                slide_layout_relationships(),
            ),
            ("ppt/theme/theme1.xml".to_owned(), theme()),
        ];
        for (position, page) in validated.pages.iter().enumerate() {
            let number = position + 1;
            let shapes = render_page(page, &mut unsupported);
            parts.push((format!("ppt/slides/slide{number}.xml"), slide(shapes)));
            parts.push((
                format!("ppt/slides/_rels/slide{number}.xml.rels"),
                slide_relationships(),
            ));
        }

        let entries: Vec<super::ZipEntry<'_>> = parts
            .iter()
            .map(|(name, xml)| super::ZipEntry::new(name.as_str(), xml.as_bytes()))
            .collect();
        let required: [&str; 8] = [
            "[Content_Types].xml",
            "_rels/.rels",
            "ppt/presentation.xml",
            "ppt/_rels/presentation.xml.rels",
            "ppt/slides/slide1.xml",
            "ppt/slideLayouts/slideLayout1.xml",
            "ppt/slideLayouts/_rels/slideLayout1.xml.rels",
            "ppt/slideMasters/slideMaster1.xml",
        ];
        let report = OfficeExportReport {
            format: OfficeFormat::Pptx,
            part_count: 0,
            unsupported,
        };
        finish(&entries, &required, report, PptxExportError::Zip)
    }
}

impl OfficeExporter for PptxExporter {
    fn format(&self) -> OfficeFormat {
        OfficeFormat::Pptx
    }

    fn export(&self, doc: &OfficeDocument) -> Result<OfficeExport, OfficeExportError> {
        Ok(self.write(doc)?)
    }
}

fn content_types(slide_count: usize) -> String {
    let mut out = format!(
        "{decl}<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\">\
         <Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/>\
         <Default Extension=\"xml\" ContentType=\"application/xml\"/>\
         <Override PartName=\"/ppt/presentation.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.presentationml.presentation.main+xml\"/>\
         <Override PartName=\"/ppt/slideMasters/slideMaster1.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.presentationml.slideMaster+xml\"/>\
         <Override PartName=\"/ppt/slideLayouts/slideLayout1.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.presentationml.slideLayout+xml\"/>\
         <Override PartName=\"/ppt/theme/theme1.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.theme+xml\"/>\
         <Override PartName=\"/docProps/core.xml\" ContentType=\"application/vnd.openxmlformats-package.core-properties+xml\"/>",
        decl = xml::declaration(),
    );
    for index in 1..=slide_count {
        out.push_str(&format!(
            "<Override PartName=\"/ppt/slides/slide{index}.xml\" \
             ContentType=\"application/vnd.openxmlformats-officedocument.presentationml.slide+xml\"/>"
        ));
    }
    out.push_str("</Types>");
    out
}

fn root_relationships() -> String {
    format!(
        "{decl}<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\
         <Relationship Id=\"rId1\" Type=\"{r}/officeDocument\" Target=\"ppt/presentation.xml\"/>\
         <Relationship Id=\"rId2\" Type=\"http://schemas.openxmlformats.org/package/2006/relationships/metadata/core-properties\" Target=\"docProps/core.xml\"/>\
         </Relationships>",
        decl = xml::declaration(),
        r = R_NS,
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

fn presentation(width: i64, height: i64, slide_count: usize) -> String {
    let mut out = format!(
        "{decl}<p:presentation xmlns:a=\"{a}\" xmlns:r=\"{r}\" xmlns:p=\"{p}\">\
         <p:sldMasterIdLst><p:sldMasterId id=\"2147483648\" r:id=\"rIdMaster\"/></p:sldMasterIdLst>\
         <p:sldIdLst>",
        decl = xml::declaration(),
        a = A_NS,
        r = R_NS,
        p = P_NS,
    );
    for index in 0..slide_count {
        let id = 256usize + index;
        let relationship = index + 1;
        out.push_str(&format!(
            "<p:sldId id=\"{id}\" r:id=\"rId{relationship}\"/>"
        ));
    }
    out.push_str(&format!(
        "</p:sldIdLst><p:sldSz cx=\"{width}\" cy=\"{height}\"/>\
         <p:notesSz cx=\"{width}\" cy=\"{height}\"/></p:presentation>"
    ));
    out
}

fn presentation_relationships(slide_count: usize) -> String {
    let mut out = format!(
        "{decl}<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">",
        decl = xml::declaration(),
    );
    for index in 1..=slide_count {
        out.push_str(&format!(
            "<Relationship Id=\"rId{index}\" Type=\"{r}/slide\" Target=\"slides/slide{index}.xml\"/>",
            r = R_NS,
        ));
    }
    out.push_str(&format!(
        "<Relationship Id=\"rIdMaster\" Type=\"{r}/slideMaster\" Target=\"slideMasters/slideMaster1.xml\"/>",
        r = R_NS,
    ));
    out.push_str("</Relationships>");
    out
}

fn empty_shape_tree() -> String {
    "<p:nvGrpSpPr><p:cNvPr id=\"1\" name=\"\"/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr>\
     <p:grpSpPr><a:xfrm><a:off x=\"0\" y=\"0\"/><a:ext cx=\"0\" cy=\"0\"/>\
     <a:chOff x=\"0\" y=\"0\"/><a:chExt cx=\"0\" cy=\"0\"/></a:xfrm></p:grpSpPr>"
        .to_owned()
}

fn slide(shapes: Vec<String>) -> String {
    format!(
        "{decl}<p:sld xmlns:a=\"{a}\" xmlns:r=\"{r}\" xmlns:p=\"{p}\"><p:cSld>\
         <p:spTree>{tree}{shapes}</p:spTree></p:cSld>\
         <p:clrMapOvr><a:masterClrMapping/></p:clrMapOvr></p:sld>",
        decl = xml::declaration(),
        a = A_NS,
        r = R_NS,
        p = P_NS,
        tree = empty_shape_tree(),
        shapes = shapes.concat(),
    )
}

fn slide_relationships() -> String {
    format!(
        "{decl}<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\
         <Relationship Id=\"rId1\" Type=\"{r}/slideLayout\" Target=\"../slideLayouts/slideLayout1.xml\"/>\
         </Relationships>",
        decl = xml::declaration(),
        r = R_NS,
    )
}

fn slide_layout() -> String {
    format!(
        "{decl}<p:sldLayout xmlns:a=\"{a}\" xmlns:r=\"{r}\" xmlns:p=\"{p}\" \
         type=\"blank\" preserve=\"1\"><p:cSld name=\"Blank\">\
         <p:spTree>{tree}</p:spTree></p:cSld>\
         <p:clrMapOvr><a:masterClrMapping/></p:clrMapOvr></p:sldLayout>",
        decl = xml::declaration(),
        a = A_NS,
        r = R_NS,
        p = P_NS,
        tree = empty_shape_tree(),
    )
}

fn slide_layout_relationships() -> String {
    format!(
        "{decl}<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\
         <Relationship Id=\"rId1\" Type=\"{r}/slideMaster\" Target=\"../slideMasters/slideMaster1.xml\"/>\
         </Relationships>",
        decl = xml::declaration(),
        r = R_NS,
    )
}

fn slide_master() -> String {
    let colors = [
        "lt1", "dk1", "lt2", "dk2", "accent1", "accent2", "accent3", "accent4", "accent5",
        "accent6", "hlink", "folHlink",
    ];
    let clr_map = format!(
        "<p:clrMap bg1=\"{c0}\" tx1=\"{c1}\" bg2=\"{c2}\" tx2=\"{c3}\" \
         accent1=\"{c4}\" accent2=\"{c5}\" accent3=\"{c6}\" accent4=\"{c7}\" \
         accent5=\"{c8}\" accent6=\"{c9}\" hlink=\"{c10}\" folHlink=\"{c11}\"/>",
        c0 = colors[0],
        c1 = colors[1],
        c2 = colors[2],
        c3 = colors[3],
        c4 = colors[4],
        c5 = colors[5],
        c6 = colors[6],
        c7 = colors[7],
        c8 = colors[8],
        c9 = colors[9],
        c10 = colors[10],
        c11 = colors[11],
    );
    format!(
        "{decl}<p:sldMaster xmlns:a=\"{a}\" xmlns:r=\"{r}\" xmlns:p=\"{p}\">\
         <p:cSld><p:spTree>{tree}</p:spTree></p:cSld>{clr_map}\
         <p:sldLayoutIdLst><p:sldLayoutId id=\"2147483649\" r:id=\"rId1\"/></p:sldLayoutIdLst>\
         <p:txStyles><p:titleStyle/><p:bodyStyle/><p:otherStyle/></p:txStyles></p:sldMaster>",
        decl = xml::declaration(),
        a = A_NS,
        r = R_NS,
        p = P_NS,
        tree = empty_shape_tree(),
        clr_map = clr_map,
    )
}

fn slide_master_relationships() -> String {
    format!(
        "{decl}<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\
         <Relationship Id=\"rId1\" Type=\"{r}/slideLayout\" Target=\"../slideLayouts/slideLayout1.xml\"/>\
         <Relationship Id=\"rId2\" Type=\"{r}/theme\" Target=\"../theme/theme1.xml\"/>\
         </Relationships>",
        decl = xml::declaration(),
        r = R_NS,
    )
}

fn theme() -> String {
    format!(
        "{decl}<a:theme xmlns:a=\"{a}\" name=\"PDFXI\">\
         <a:themeElements>\
         <a:clrScheme name=\"PDFXI\">\
         <a:dk1><a:sysClr val=\"windowText\" lastClr=\"000000\"/></a:dk1>\
         <a:lt1><a:sysClr val=\"window\" lastClr=\"FFFFFF\"/></a:lt1>\
         <a:dk2><a:srgbClr val=\"44546A\"/></a:dk2>\
         <a:lt2><a:srgbClr val=\"E7E6E6\"/></a:lt2>\
         <a:accent1><a:srgbClr val=\"4472C4\"/></a:accent1>\
         <a:accent2><a:srgbClr val=\"ED7D31\"/></a:accent2>\
         <a:accent3><a:srgbClr val=\"A5A5A5\"/></a:accent3>\
         <a:accent4><a:srgbClr val=\"FFC000\"/></a:accent4>\
         <a:accent5><a:srgbClr val=\"5B9BD5\"/></a:accent5>\
         <a:accent6><a:srgbClr val=\"70AD47\"/></a:accent6>\
         <a:hlink><a:srgbClr val=\"0563C1\"/></a:hlink>\
         <a:folHlink><a:srgbClr val=\"954F72\"/></a:folHlink>\
         </a:clrScheme>\
         <a:fontScheme name=\"PDFXI\">\
         <a:majorFont><a:latin typeface=\"Arial\"/><a:ea typeface=\"\"/><a:cs typeface=\"\"/></a:majorFont>\
         <a:minorFont><a:latin typeface=\"Arial\"/><a:ea typeface=\"\"/><a:cs typeface=\"\"/></a:minorFont>\
         </a:fontScheme>\
         <a:fmtScheme name=\"PDFXI\">\
         <a:fillStyleLst><a:solidFill><a:schemeClr val=\"phClr\"/></a:solidFill>\
         <a:solidFill><a:schemeClr val=\"phClr\"/></a:solidFill>\
         <a:solidFill><a:schemeClr val=\"phClr\"/></a:solidFill></a:fillStyleLst>\
         <a:lnStyleLst><a:ln><a:solidFill><a:schemeClr val=\"phClr\"/></a:solidFill></a:ln>\
         <a:ln><a:solidFill><a:schemeClr val=\"phClr\"/></a:solidFill></a:ln>\
         <a:ln><a:solidFill><a:schemeClr val=\"phClr\"/></a:solidFill></a:ln></a:lnStyleLst>\
         <a:effectStyleLst><a:effectStyle><a:effectLst/></a:effectStyle>\
         <a:effectStyle><a:effectLst/></a:effectStyle>\
         <a:effectStyle><a:effectLst/></a:effectStyle></a:effectStyleLst>\
         <a:bgFillStyleLst><a:solidFill><a:schemeClr val=\"phClr\"/></a:solidFill>\
         <a:solidFill><a:schemeClr val=\"phClr\"/></a:solidFill>\
         <a:solidFill><a:schemeClr val=\"phClr\"/></a:solidFill></a:bgFillStyleLst>\
         </a:fmtScheme></a:themeElements></a:theme>",
        decl = xml::declaration(),
        a = A_NS,
    )
}

fn render_page(
    page: &super::OfficePage,
    unsupported: &mut Vec<ReportedUnsupported>,
) -> Vec<String> {
    let mut shapes = Vec::new();
    let mut shape_id = 2u32;
    let mut flattened_index = 0u32;
    let width = f64::from(page.width);
    let height = f64::from(page.height);

    for block in &page.blocks {
        match block {
            OfficeBlock::TextBox(box_) => {
                shapes.push(text_box_shape(shape_id, box_, height));
                shape_id += 1;
            }
            OfficeBlock::Paragraph { text, .. } | OfficeBlock::ListItem { text, .. } => {
                let top = (height - 60.0 - f64::from(flattened_index) * 30.0).max(0.0) as f32;
                let box_ = TextBox::new(text.clone(), 40.0, top).with_metrics(
                    (width - 80.0).max(1.0) as f32,
                    24.0,
                    18.0,
                );
                shapes.push(text_box_shape(shape_id, &box_, height));
                shape_id += 1;
                flattened_index += 1;
                unsupported.push(ReportedUnsupported::new(
                    OfficeFormat::Pptx,
                    page.index,
                    "text_layout_flattened",
                    "text block converted to a generated text box",
                ));
            }
            OfficeBlock::Table(table) => {
                let text = flatten_table(table);
                let box_ = TextBox::new(text, 40.0, 60.0).with_metrics(
                    (width - 80.0).max(1.0) as f32,
                    60.0,
                    12.0,
                );
                shapes.push(text_box_shape(shape_id, &box_, height));
                shape_id += 1;
                unsupported.push(ReportedUnsupported::new(
                    OfficeFormat::Pptx,
                    page.index,
                    "table_flattened",
                    "table converted to plain text",
                ));
            }
            OfficeBlock::Unsupported(content) => unsupported.push(ReportedUnsupported::new(
                OfficeFormat::Pptx,
                page.index,
                content.kind(),
                format!("omitted: {}", content.detail()),
            )),
        }
    }
    shapes
}

fn flatten_table(table: &TableBlock) -> String {
    let mut lines = Vec::new();
    for row in &table.rows {
        let cells: Vec<String> = row
            .iter()
            .map(|cell| match &cell.value {
                super::CellValue::Text(text) => text.clone(),
                super::CellValue::Number(number) => xml::number(*number),
                super::CellValue::Boolean(value) => value.to_string(),
                super::CellValue::Empty => String::new(),
            })
            .collect();
        lines.push(cells.join(" | "));
    }
    lines.join("\n")
}

fn text_box_shape(id: u32, box_: &TextBox, page_height: f64) -> String {
    let x = xml::points_to_emu(f64::from(box_.x));
    let top = page_height - f64::from(box_.y) - f64::from(box_.height);
    let y = xml::points_to_emu(top);
    let cx = xml::points_to_emu(f64::from(box_.width));
    let cy = xml::points_to_emu(f64::from(box_.height));
    let cx = if cx == 0 { 914_400 } else { cx };
    let cy = if cy == 0 { 274_320 } else { cy };
    let size = if box_.font_size > 0.0 {
        (f64::from(box_.font_size) * 100.0) as i64
    } else {
        1800
    };
    format!(
        "<p:sp><p:nvSpPr><p:cNvPr id=\"{id}\" name=\"TextBox {id}\"/>\
         <p:cNvSpPr txBox=\"1\"/><p:nvPr/></p:nvSpPr>\
         <p:spPr><a:xfrm><a:off x=\"{x}\" y=\"{y}\"/><a:ext cx=\"{cx}\" cy=\"{cy}\"/></a:xfrm>\
         <a:prstGeom prst=\"rect\"><a:avLst/></a:prstGeom><a:noFill/><a:ln><a:noFill/></a:ln></p:spPr>\
         <p:txBody><a:bodyPr wrap=\"square\" lIns=\"0\" tIns=\"0\" rIns=\"0\" bIns=\"0\"/>\
         <a:lstStyle/><a:p><a:r><a:rPr lang=\"en-US\" sz=\"{size}\"/>\
         <a:t>{text}</a:t></a:r><a:endParaRPr lang=\"en-US\"/></a:p></p:txBody></p:sp>",
        text = xml::escape(&box_.text),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::office_export::zip;
    use crate::office_export::{OfficeCell, OfficePage, TableBlock, UnsupportedContent};

    fn doc_with(blocks: Vec<OfficeBlock>) -> OfficeDocument {
        OfficeDocument::new(
            "Doc",
            vec![OfficePage::new(0, 200.0, 300.0).with_blocks(blocks)],
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
    fn pptx_has_required_parts_and_valid_zip() {
        let export = PptxExporter.write(&doc_with(vec![])).expect("write");
        let views = zip::read_zip(&export.bytes).expect("valid zip");
        zip::require_parts(
            &views,
            &[
                "[Content_Types].xml",
                "_rels/.rels",
                "ppt/presentation.xml",
                "ppt/_rels/presentation.xml.rels",
                "ppt/slides/slide1.xml",
                "ppt/slideLayouts/slideLayout1.xml",
                "ppt/slideMasters/slideMaster1.xml",
                "ppt/theme/theme1.xml",
            ],
        )
        .expect("required parts");
        let presentation = part(&export, "ppt/presentation.xml");
        assert!(presentation.contains("cx=\"2540000\""));
        assert!(presentation.contains("cy=\"3810000\""));
        assert!(export.report.part_count >= 12);
    }

    #[test]
    fn pptx_slide_per_page_with_positioned_text_box() {
        let doc = OfficeDocument::new(
            "Doc",
            vec![
                OfficePage::new(0, 200.0, 300.0).with_blocks(vec![OfficeBlock::text_box(
                    TextBox::new("Hi", 10.0, 280.0).with_metrics(40.0, 12.0, 12.0),
                )]),
                OfficePage::new(1, 200.0, 300.0).with_blocks(vec![OfficeBlock::text_box(
                    TextBox::new("Bye", 20.0, 270.0).with_metrics(30.0, 10.0, 14.0),
                )]),
            ],
        );
        let export = PptxExporter.write(&doc).expect("write");
        let views = zip::read_zip(&export.bytes).expect("valid zip");
        assert!(views.iter().any(|v| v.name == "ppt/slides/slide1.xml"));
        assert!(views.iter().any(|v| v.name == "ppt/slides/slide2.xml"));
        let slide1 = part(&export, "ppt/slides/slide1.xml");
        // x = 10pt -> 127000 EMU; top = 300-(280+12)=8pt -> 101600 EMU.
        assert!(slide1.contains("<a:off x=\"127000\" y=\"101600\"/>"));
        assert!(slide1.contains("<a:ext cx=\"508000\" cy=\"152400\"/>"));
        assert!(slide1.contains("sz=\"1200\""));
        assert!(slide1.contains("<a:t>Hi</a:t>"));
        assert!(part(&export, "ppt/slides/slide2.xml").contains("<a:t>Bye</a:t>"));
    }

    #[test]
    fn pptx_escapes_text_and_reports_flattened_content() {
        let doc = doc_with(vec![
            OfficeBlock::text_box(TextBox::new("<&>\"'\0", 1.0, 2.0)),
            OfficeBlock::paragraph("body"),
            OfficeBlock::table(TableBlock::new(vec![vec![OfficeCell::text("cell")]])),
            OfficeBlock::unsupported(UnsupportedContent::Drawing {
                detail: "vector".to_owned(),
            }),
        ]);
        let export = PptxExporter.write(&doc).expect("write");
        let xml = part(&export, "ppt/slides/slide1.xml");
        assert!(xml.contains("&lt;&amp;&gt;&quot;&apos;\u{fffd}"));
        assert_eq!(export.report.unsupported.len(), 3);
        assert_eq!(export.report.unsupported[0].kind, "text_layout_flattened");
        assert_eq!(export.report.unsupported[1].kind, "table_flattened");
        assert_eq!(export.report.unsupported[2].kind, "drawing");
    }

    #[test]
    fn pptx_is_deterministic() {
        let doc = doc_with(vec![OfficeBlock::text_box(TextBox::new(
            "stable", 1.0, 2.0,
        ))]);
        assert_eq!(
            PptxExporter.write(&doc).expect("a").bytes,
            PptxExporter.write(&doc).expect("b").bytes
        );
    }

    #[test]
    fn pptx_rejects_invalid_models_with_typed_errors() {
        let nan = doc_with(vec![OfficeBlock::text_box(TextBox::new(
            "x",
            f32::NAN,
            0.0,
        ))]);
        assert!(matches!(
            PptxExporter.write(&nan),
            Err(PptxExportError::Invalid(
                crate::office_export::ModelError::NonFinite { .. }
            ))
        ));
        let negative = doc_with(vec![OfficeBlock::text_box(TextBox::new("x", -1.0, 0.0))]);
        assert!(matches!(
            PptxExporter.write(&negative),
            Err(PptxExportError::Invalid(
                crate::office_export::ModelError::NegativeMeasurement { .. }
            ))
        ));
    }

    #[test]
    fn pptx_error_type_is_distinct_and_transparent() {
        let err = PptxExportError::Zip(crate::office_export::ZipError::MissingPart(
            "ppt/presentation.xml".to_owned(),
        ));
        let wrapped = OfficeExportError::from(err);
        assert!(matches!(wrapped, OfficeExportError::Pptx(_)));
    }
}
