//! Public-API closure tests for T-0180 (CREATE-010, EXPORT-001/002/003/005/008).
//!
//! Every test uses only the crate's public surface. No network, external
//! converter, Office process, validator, or file-system write is used.

use pdfxi_convert::{
    ConformanceExportError, ConformancePlan, ConformanceRequest, ConformanceTarget, DocxExporter,
    ExportKind, ExtractedDocument, ExtractedPage, ExtractedRun, ExtractedTable, ExtractedTextBox,
    ExtractedUnsupported, ForbiddenEntryPolicy, NormalizationOptions, OfficeBlock, OfficeExporter,
    OfficePlanFormat, OfficePlanOptions, PortfolioEntry, PortfolioManifest, RtfOptions,
    TextEncoding, TextExportPlan, build_office_export_plan, build_portfolio_package,
    emit_rtf_bytes, emit_text_bytes, export_conformance_pdf,
};
use pdfxi_core::Document;

fn source_extraction() -> ExtractedDocument {
    ExtractedDocument::new(
        "Closure",
        vec![
            ExtractedPage::new(1, 200.0, 300.0)
                .with_runs(vec![ExtractedRun::new("Second", 10.0, 280.0, 24.0)]),
            ExtractedPage::new(0, 200.0, 300.0)
                .with_runs(vec![
                    ExtractedRun::new("world", 30.0, 280.0, 24.0).with_sequence(1),
                    ExtractedRun::new("Hello", 10.0, 280.0, 24.0).with_sequence(0),
                ])
                .with_table(ExtractedTable::new(vec![vec!["cell".to_owned()]]))
                .with_text_box(
                    ExtractedTextBox::new("note", 5.0, 250.0).with_metrics(20.0, 8.0, 9.0),
                )
                .with_unsupported(ExtractedUnsupported::VectorArt {
                    detail: "curve".to_owned(),
                }),
        ],
    )
}

#[test]
fn t0180_portfolio_package_parses_through_core_and_is_byte_stable() {
    let manifest = PortfolioManifest::new(
        "Closure pack",
        vec![
            PortfolioEntry::new("b.txt", "B", 2),
            PortfolioEntry::new("a.txt", "A", 1),
        ],
    );
    let first = build_portfolio_package(
        &manifest,
        &[("b.txt", b"BB".to_vec()), ("a.txt", b"A".to_vec())],
    )
    .expect("package");
    let second = build_portfolio_package(
        &manifest,
        &[("a.txt", b"A".to_vec()), ("b.txt", b"BB".to_vec())],
    )
    .expect("package again");
    assert_eq!(first.to_bytes(), second.to_bytes());
    assert_eq!(first.name_tree_names(), vec!["a.txt", "b.txt"]);

    let bytes = first.to_bytes();
    let text = String::from_utf8_lossy(&bytes).into_owned();
    assert!(!text.contains("/CreationDate"));
    assert!(!text.contains("/ModDate"));
    assert!(text.contains("/EmbeddedFiles 4 0 R"));
    assert!(text.contains("/Type /Collection"));

    let document = Document::from_bytes(bytes).expect("core parses the package");
    assert_eq!(document.page_count().expect("page count"), 1);
    assert_eq!(document.version(), "1.7");
}

#[test]
fn t0180_portfolio_package_typed_errors_and_empty_case() {
    let manifest = PortfolioManifest::new("P", vec![PortfolioEntry::new("a.txt", "", 0)]);
    assert!(build_portfolio_package(&manifest, &[]).is_err());
    assert!(build_portfolio_package(&manifest, &[("ghost.txt", b"x".to_vec())]).is_err());

    let empty = build_portfolio_package(&PortfolioManifest::new("P", Vec::new()), &[])
        .expect("empty package is valid");
    assert_eq!(empty.file_count(), 0);
    assert!(empty.name_tree_names().is_empty());
    assert_eq!(empty.to_bytes(), empty.to_bytes());
}

#[test]
fn t0180_office_envelope_writes_all_three_formats() {
    let source = source_extraction();
    for &format in OfficePlanFormat::all() {
        let envelope = build_office_export_plan(&source, format, &OfficePlanOptions::default())
            .unwrap_or_else(|error| panic!("plan {format:?}: {error}"));
        assert!(envelope.validate().is_ok());
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
                .any(|item| item.kind == "vector-art")
        );
    }
}

#[test]
fn t0180_office_envelope_reports_typed_unsupported_and_numbers_paragraphs() {
    let options = OfficePlanOptions {
        number_paragraphs: true,
        ..OfficePlanOptions::default()
    };
    let envelope = build_office_export_plan(&source_extraction(), OfficePlanFormat::Word, &options)
        .expect("word envelope");
    let manifest = String::from_utf8(envelope.to_manifest_bytes()).expect("manifest bytes");
    let numbered = envelope.document().pages[0].blocks.iter().any(
        |block| matches!(block, OfficeBlock::Paragraph { text, .. } if text == "1. Hello world"),
    );
    assert!(numbered, "first paragraph is numbered: {manifest}");
    assert!(manifest.contains("unsupported: page 1 [vector-art] curve"));
    assert!(
        envelope
            .unsupported()
            .iter()
            .any(|item| item.kind == "vector-art")
    );

    let written = envelope.write().expect("write");
    let vector_art = written
        .report
        .unsupported
        .iter()
        .find(|item| item.kind == "vector-art")
        .expect("vector-art finding survives writing");
    assert_eq!(vector_art.page, 0);
}

#[test]
fn t0180_office_envelope_rejects_invalid_models() {
    let empty = ExtractedDocument::new("T", Vec::new());
    assert!(
        build_office_export_plan(
            &empty,
            OfficePlanFormat::Word,
            &OfficePlanOptions::default()
        )
        .is_err()
    );
    let mut source = source_extraction();
    source.pages[0] = source.pages[0].clone().with_table(ExtractedTable::new(vec![
        vec!["a".to_owned()],
        vec!["b".to_owned(), "c".to_owned()],
    ]));
    let envelope = build_office_export_plan(
        &source,
        OfficePlanFormat::Excel,
        &OfficePlanOptions::default(),
    )
    .expect("complex table is reported, not fatal");
    assert!(
        envelope
            .unsupported()
            .iter()
            .any(|item| item.kind == "complex-table")
    );
}

#[test]
fn t0180_export_kind_to_office_plan_mapping_is_total_for_office_families() {
    assert_eq!(
        OfficePlanFormat::from_export_kind(ExportKind::Word),
        Some(OfficePlanFormat::Word)
    );
    assert_eq!(
        OfficePlanFormat::from_export_kind(ExportKind::Spreadsheet),
        Some(OfficePlanFormat::Excel)
    );
    assert_eq!(
        OfficePlanFormat::from_export_kind(ExportKind::Presentation),
        Some(OfficePlanFormat::Presentation)
    );
    assert_eq!(OfficePlanFormat::from_export_kind(ExportKind::Rtf), None);
    let docx = DocxExporter
        .export(
            build_office_export_plan(
                &source_extraction(),
                OfficePlanFormat::Word,
                &OfficePlanOptions::default(),
            )
            .expect("plan")
            .document(),
        )
        .expect("docx");
    assert_eq!(docx.extension(), "docx");
}

#[test]
fn t0180_rtf_and_plain_text_emitters_are_deterministic_from_extraction() {
    let source = source_extraction();
    let plan = TextExportPlan::new().with_page_headers(true);
    let text = emit_text_bytes(&source, &plan).expect("plain text");
    assert_eq!(
        text,
        emit_text_bytes(&source, &plan).expect("plain text again")
    );
    let rendered = String::from_utf8(text).expect("UTF-8");
    assert!(rendered.starts_with("--- Page 1 ---\nHello world"));
    assert!(rendered.contains("--- Page 2 ---\nSecond"));

    let utf16 = source
        .emit_plain_text(&plan.clone().with_encoding(TextEncoding::Utf16Be))
        .expect("UTF-16BE");
    assert_eq!(&utf16[..2], &[0xFE, 0xFF]);

    let rtf = emit_rtf_bytes(&source, &RtfOptions::new()).expect("RTF");
    assert_eq!(
        rtf,
        emit_rtf_bytes(&source, &RtfOptions::new()).expect("RTF again")
    );
    assert!(String::from_utf8_lossy(&rtf).starts_with("{\\rtf1"));
}

#[test]
fn t0180_conformance_forbidden_entries_report_and_strict_reject() {
    let source = pdfxi_convert::create_from_text(
        "Conformance closure",
        &pdfxi_convert::CreateOptions::new(),
    )
    .expect("source PDF");
    let request = ConformanceRequest::new("PDF/A-2b")
        .with_forbidden_entry_policy(ForbiddenEntryPolicy::Report);
    let result = export_conformance_pdf(&source, &request, &NormalizationOptions::all())
        .expect("report policy succeeds");
    assert!(!result.report.certification_claimed());
    assert!(result.report.present_forbidden_entries().is_empty());

    let strict = ConformanceRequest::new("PDF/A-2b")
        .with_forbidden_entry_policy(ForbiddenEntryPolicy::Reject);
    let clean = export_conformance_pdf(&source, &strict, &NormalizationOptions::all());
    assert!(clean.is_ok(), "a clean source must pass strict policy");
    assert!(
        ConformancePlan::for_request(&strict)
            .expect("plan")
            .validation_report()
            .findings()
            .iter()
            .any(|finding| finding.code() == "PLAN.FORBIDDEN_ENTRIES")
    );
}

#[test]
fn t0180_conformance_plans_cover_all_required_profiles() {
    for label in ["PDF/A-1b", "PDF/A-2b", "PDF/X-1a", "PDF/X-4", "PDF/E-1"] {
        let request = ConformanceRequest::new(label);
        let plan = ConformancePlan::for_request(&request)
            .unwrap_or_else(|error| panic!("plan {label}: {error}"));
        assert_eq!(plan.target().label(), label);
        assert!(!plan.validation_report().certification_claimed());
        assert!(
            plan.steps()
                .iter()
                .any(|step| step.code() == "PLAN.FORBIDDEN_ENTRIES")
        );
    }
    assert_eq!(
        ConformanceTarget::from_label("pdf/x-1a"),
        Some(ConformanceTarget::PdfX1a)
    );
    assert!(matches!(
        ConformancePlan::for_request(&ConformanceRequest::new("PDF/VT-1")),
        Err(ConformanceExportError::UnsupportedProfile { .. })
    ));
}
