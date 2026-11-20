//! Public-API closure tests for T-0182 (PLAT-003, library-level Office integration).
//!
//! Every test uses only the crate's public surface. No network, external
//! converter, Office/LibreOffice process, COM, file-system read, or new OOXML
//! ZIP reader is involved.

use pdfxi_convert::{
    CompatibilityMatrix, ExtractedDocument, ExtractedPage, ExtractedRun, ExtractedUnsupported,
    IntegrationDescriptor, OfficeEnvelopePayload, OfficeFamily, OfficeFlow,
    OfficeIntegrationEnvelope, OfficeIntegrationError, OfficeIntegrationPlan, SupportLevel,
    integration_descriptor, plan_office_integration,
};

fn markup_envelope(name: &str, markup: &str) -> OfficeIntegrationEnvelope {
    OfficeIntegrationEnvelope::new(
        OfficeFamily::Word,
        OfficeFlow::ImportToPdf,
        name,
        OfficeEnvelopePayload::Markup(markup.to_owned()),
    )
}

fn extraction() -> ExtractedDocument {
    ExtractedDocument::new(
        "Closure",
        vec![
            ExtractedPage::new(1, 200.0, 300.0)
                .with_runs(vec![ExtractedRun::new("Second", 10.0, 280.0, 24.0)]),
            ExtractedPage::new(0, 200.0, 300.0)
                .with_runs(vec![ExtractedRun::new("Hello", 10.0, 280.0, 24.0)])
                .with_unsupported(ExtractedUnsupported::VectorArt {
                    detail: "curve".to_owned(),
                }),
        ],
    )
}

#[test]
fn t0182_public_descriptor_and_matrix_are_deterministic() {
    let first: IntegrationDescriptor = integration_descriptor();
    assert_eq!(first, integration_descriptor());
    assert_eq!(
        first.to_manifest_bytes(),
        integration_descriptor().to_manifest_bytes()
    );
    assert_eq!(first.capabilities().len(), 3);
    assert!(first.capabilities().iter().all(|c| c.is_round_trip()));
    assert_eq!(first.extensions().len(), 12);

    let matrix = CompatibilityMatrix::new();
    assert_eq!(matrix, CompatibilityMatrix::new());
    assert_eq!(matrix.signature(), CompatibilityMatrix::new().signature());
    assert!(
        matrix
            .entries()
            .iter()
            .all(|entry| entry.construct.len() > 2)
    );
    assert_eq!(
        matrix
            .get(OfficeFamily::PowerPoint, "text_box")
            .expect("text box row")
            .export,
        SupportLevel::Survives
    );
}

#[test]
fn t0182_public_entry_point_plans_and_executes_import_to_pdf() {
    let envelope = markup_envelope(
        "Quarterly Report",
        "<h1>Title</h1><p>Hello</p><!--pdfxi:page--><p>World</p>",
    );
    let plan: OfficeIntegrationPlan = plan_office_integration(&envelope).expect("plan");
    assert_eq!(plan.output_name(), "quarterly_report.pdf");
    assert_eq!(plan.page_count(), 2);
    assert_eq!(plan.steps().len(), 3);
    assert_eq!(
        plan,
        plan_office_integration(&envelope).expect("plan again")
    );

    let output = plan.execute().expect("execute");
    assert_eq!(output.extension, "pdf");
    assert!(output.bytes.starts_with(b"%PDF-"));
    assert_eq!(output.report.byte_count, output.bytes.len());
    assert_eq!(plan.execute().expect("execute again").bytes, output.bytes);
}

#[test]
fn t0182_public_entry_point_writes_all_three_office_families() {
    for family in OfficeFamily::all() {
        let envelope = OfficeIntegrationEnvelope::new(
            *family,
            OfficeFlow::PdfToOffice,
            "Deck",
            OfficeEnvelopePayload::Extracted(extraction()),
        );
        let plan = plan_office_integration(&envelope).unwrap_or_else(|e| panic!("{family:?}: {e}"));
        assert_eq!(plan.family(), *family);
        assert_eq!(
            plan.output_name(),
            format!("deck.{}", family.export_extension())
        );
        assert!(plan.capability().is_round_trip());
        let output = plan.execute().expect("execute");
        assert_eq!(output.extension, family.export_extension());
        assert_eq!(&output.bytes[..2], b"PK");
        assert!(
            output
                .report
                .unsupported
                .iter()
                .any(|item| item.kind == "vector-art")
        );
    }
}

#[test]
fn t0182_public_negative_validation_is_typed() {
    assert!(matches!(
        plan_office_integration(&markup_envelope("../escape", "<p>x</p>")),
        Err(OfficeIntegrationError::UnsafeName { .. })
    ));
    assert!(matches!(
        plan_office_integration(&markup_envelope("CON.pdf", "<p>x</p>")),
        Err(OfficeIntegrationError::UnsafeName { .. })
    ));
    assert!(matches!(
        plan_office_integration(&markup_envelope("x.docx", "<p>x</p>")),
        Err(OfficeIntegrationError::UnexpectedExtension { .. })
    ));
    assert!(matches!(
        plan_office_integration(&markup_envelope("x", "<img src=\"a.png\"/>")),
        Err(OfficeIntegrationError::NoConvertibleText)
    ));
}

#[test]
fn t0182_public_typed_unsupported_constructs_are_reported() {
    let envelope = markup_envelope("Media", "<p>kept</p><img src=\"a.png\"/><widget/>");
    let plan = plan_office_integration(&envelope).expect("plan");
    let kinds: Vec<&str> = plan
        .unsupported()
        .iter()
        .map(|item| item.kind.as_str())
        .collect();
    assert!(kinds.contains(&"media"));
    assert!(kinds.contains(&"unknown-tag"));
    let manifest = String::from_utf8(plan.to_manifest_bytes()).expect("utf8 manifest");
    assert!(manifest.contains("unsupported: import [media] img"));
}

#[test]
fn t0182_public_family_parsing_and_string_reporting_are_stable() {
    assert_eq!(
        pdfxi_convert::parse_family("docx").expect("docx"),
        OfficeFamily::Word
    );
    assert_eq!(
        pdfxi_convert::parse_family("pptx").expect("pptx"),
        OfficeFamily::PowerPoint
    );
    assert!(matches!(
        pdfxi_convert::parse_family("pdf"),
        Err(OfficeIntegrationError::UnknownFamily { .. })
    ));
    let descriptor = integration_descriptor();
    let capability = descriptor.capability(OfficeFamily::Excel).expect("excel");
    assert!(capability.summary().contains("pdf-to-office=true"));
}
