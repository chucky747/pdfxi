//! Integration coverage for the byte-level preflight adapter.
//!
//! Fixtures are small, deterministic classic-xref PDFs built here so the
//! catalog facts and page boxes are explicit and no external corpus is needed.

use pdfxi_convert::preflight::{
    BYTE_PREFLIGHT_SCOPE_NOTE, BytePreflightError, PreflightOptions, build_preflight_input,
    preflight_bytes,
};
use pdfxi_core::Document;
use pdfxi_standards::PdfVersion;

/// Assemble a classic-xref PDF from numbered object bodies.
fn assemble(version: &str, bodies: &[String]) -> Vec<u8> {
    let mut out = format!("%PDF-{version}\n%PDFXI\n").into_bytes();
    let mut offsets = Vec::with_capacity(bodies.len());
    for (index, body) in bodies.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n{}\nendobj\n", index + 1, body).as_bytes());
    }
    let xref = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n", bodies.len() + 1).as_bytes());
    out.extend_from_slice(b"0000000000 65535 f \n");
    for offset in &offsets {
        out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            bodies.len() + 1
        )
        .as_bytes(),
    );
    out
}

/// A PDF/A-2b-shaped fixture: version 1.7, XMP metadata, a `GTS_PDFA1`
/// output intent carrying a profile reference, and explicit print boxes.
fn conforming_pdf() -> Vec<u8> {
    let content = "BT (Hi) Tj ET";
    let xmp = "<?xpacket begin=\"\" id=\"W5M0\"?><x:xmpmeta xmlns:x=\"adobe:ns:meta/\"/>\
               <?xpacket end=\"w\"?>";
    assemble(
        "1.7",
        &[
            "<< /Type /Catalog /Pages 2 0 R /Metadata 5 0 R /OutputIntents [6 0 R] >>".to_owned(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
             /CropBox [10 10 602 782] /BleedBox [20 20 592 772] /TrimBox [30 30 582 762] \
             /Contents 4 0 R >>"
                .to_owned(),
            format!(
                "<< /Length {} >>\nstream\n{}\nendstream",
                content.len(),
                content
            ),
            format!(
                "<< /Type /Metadata /Subtype /XML /Length {} >>\nstream\n{}\nendstream",
                xmp.len(),
                xmp
            ),
            "<< /Type /OutputIntent /S /GTS_PDFA1 /DestOutputProfile 7 0 R >>".to_owned(),
            "<< /Length 0 /N 3 >>\nstream\n\nendstream".to_owned(),
        ],
    )
}

/// A fixture with four independently fixable/reportable problems for PDF/A-2b:
/// a low version, no XMP, no output intent, and a prohibited `/JavaScript` key.
fn violating_pdf() -> Vec<u8> {
    let content = "BT (Hi) Tj ET";
    assemble(
        "1.4",
        &[
            "<< /Type /Catalog /Pages 2 0 R /JavaScript 1 >>".to_owned(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R >>".to_owned(),
            format!(
                "<< /Length {} >>\nstream\n{}\nendstream",
                content.len(),
                content
            ),
        ],
    )
}

/// A fixture with enough trim inset for every printer mark.
fn roomy_pdf() -> Vec<u8> {
    let content = "BT (Hi) Tj ET";
    assemble(
        "1.7",
        &[
            "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
             /CropBox [10 10 602 782] /BleedBox [20 20 592 772] /TrimBox [30 30 582 762] \
             /Contents 4 0 R >>"
                .to_owned(),
            format!(
                "<< /Length {} >>\nstream\n{}\nendstream",
                content.len(),
                content
            ),
        ],
    )
}

#[test]
fn conforming_fixture_has_no_error_findings() {
    let report = preflight_bytes(&conforming_pdf(), "PDF/A-2b", &PreflightOptions::default())
        .expect("preflight conforming fixture");
    assert!(report.before().is_conforming());
    assert_eq!(report.before().error_count(), 0);
    assert_eq!(report.profile_label(), "PDF/A-2b");
    assert!(report.after().is_none());
    assert!(report.output_bytes().is_none());
    assert!(
        report.enforced().is_empty(),
        "check-only runs change no bytes"
    );
    assert!(report.is_conforming());
    assert!(!report.certification_claimed());
}

#[test]
fn violating_fixture_reports_actionable_error_codes() {
    let report = preflight_bytes(&violating_pdf(), "PDF/A-2b", &PreflightOptions::default())
        .expect("preflight violating fixture");
    assert!(!report.before().is_conforming());
    let codes: Vec<&str> = report
        .before()
        .findings()
        .iter()
        .map(|finding| finding.code())
        .collect();
    for expected in [
        "PDFA2B.VERSION_TOO_LOW",
        "PDFA2B.NO_OUTPUT_INTENT",
        "PDFA2B.NO_XMP_METADATA",
        "PDFA2B.FORBIDDEN_ENTRY",
    ] {
        assert!(codes.contains(&expected), "missing {expected} in {codes:?}");
    }
    assert!(
        codes.windows(2).all(|pair| pair[0] <= pair[1]),
        "findings are canonical"
    );
}

#[test]
fn build_preflight_input_reads_catalog_and_page_boxes() {
    let doc = Document::from_bytes(conforming_pdf()).expect("fixture parses");
    let input = build_preflight_input(&doc).expect("snapshot builds");
    assert_eq!(input.version, PdfVersion::new(1, 7));
    assert!(!input.is_encrypted);
    assert_eq!(input.page_count, 1);
    assert!(input.catalog.has_xmp_metadata);
    assert_eq!(input.catalog.output_intents.len(), 1);
    assert!(input.catalog.output_intents[0].is_subtype("GTS_PDFA1"));
    assert!(input.catalog.output_intents[0].has_dest_output_profile);
    assert!(input.catalog.forbidden_entries.is_empty());
    assert_eq!(input.pages.len(), 1);
    assert!(input.pages[0].boxes.has_media_box);
    assert!(input.pages[0].boxes.has_trim_box);
    assert!(input.pages[0].boxes.has_bleed_box);
    assert!(!input.pages[0].boxes.has_art_box);
}

#[test]
fn violating_input_snapshot_exposes_the_forbidden_entry() {
    let doc = Document::from_bytes(violating_pdf()).expect("fixture parses");
    let input = build_preflight_input(&doc).expect("snapshot builds");
    assert_eq!(input.catalog.forbidden_entries, vec!["/JavaScript"]);
    assert!(!input.catalog.has_xmp_metadata);
    assert!(input.catalog.output_intents.is_empty());
}

#[test]
fn fix_changes_bytes_reparses_and_reduces_errors() {
    let source = violating_pdf();
    let check = preflight_bytes(&source, "PDF/A-2b", &PreflightOptions::default())
        .expect("check-only preflight");
    let fixed = preflight_bytes(&source, "PDF/A-2b", &PreflightOptions { fix: true })
        .expect("fix preflight");

    let output = fixed.output_bytes().expect("fix returns bytes");
    assert_ne!(
        output,
        source.as_slice(),
        "fixups must change the source bytes"
    );
    let reparsed = Document::from_bytes(output.to_vec()).expect("normalized bytes reparse");
    assert_eq!(reparsed.page_count().expect("page count"), 1);

    assert!(
        fixed.fixups().applied_count() >= 2,
        "mechanical fixups were audited"
    );
    assert!(fixed.enforced().contains(&"minimum_version"));
    assert!(fixed.enforced().contains(&"xmp_metadata_presence"));
    assert!(fixed.reported_only().contains(&"font_embedding"));
    assert!(fixed.reported_only().contains(&"icc_profile_validity"));

    let after = fixed.after().expect("fix produces an after-report");
    assert!(
        after.error_count() < check.before().error_count(),
        "fixups reduce errors: before={}, after={}",
        check.before().error_count(),
        after.error_count()
    );
    // The normalizer adds a placeholder ICC stream that the subset profile can
    // only see as "present"; it cannot validate the payload. The unfixed
    // /JavaScript entry therefore remains the decisive error, and ICC validity
    // stays reported-only. No certification is claimed.
    assert!(!fixed.is_conforming());
    assert!(
        after
            .findings()
            .iter()
            .any(|f| f.code() == "PDFA2B.FORBIDDEN_ENTRY")
    );
    assert!(fixed.reported_only().contains(&"icc_profile_validity"));
    assert!(!fixed.certification_claimed());
    assert!(fixed.scope_note().contains("no conformance certification"));
    assert_eq!(fixed.scope_note(), BYTE_PREFLIGHT_SCOPE_NOTE);
}

#[test]
fn fix_output_is_deterministic() {
    let source = violating_pdf();
    let first =
        preflight_bytes(&source, "PDF/A-2b", &PreflightOptions { fix: true }).expect("first fix");
    let second =
        preflight_bytes(&source, "PDF/A-2b", &PreflightOptions { fix: true }).expect("second fix");
    assert_eq!(first, second);
    assert_eq!(first.output_bytes(), second.output_bytes());
}

#[test]
fn case_insensitive_profile_labels_resolve_to_the_same_report() {
    let source = violating_pdf();
    let canonical = preflight_bytes(&source, "PDF/A-2b", &PreflightOptions::default())
        .expect("canonical label");
    let lower = preflight_bytes(&source, "pdf/a-2b", &PreflightOptions::default())
        .expect("lowercase label");
    assert_eq!(canonical, lower);
}

#[test]
fn unknown_profile_malformed_input_and_unavailable_fixups_are_typed() {
    assert_eq!(
        preflight_bytes(&roomy_pdf(), "PDF/A-9c", &PreflightOptions::default())
            .expect_err("unknown profile"),
        BytePreflightError::UnsupportedProfile {
            label: "PDF/A-9c".to_owned()
        }
    );
    assert_eq!(
        preflight_bytes(b"not a pdf", "PDF/A-2b", &PreflightOptions::default())
            .expect_err("malformed input"),
        BytePreflightError::MalformedPdf {
            reason: "invalid PDF header; expected %PDF-".to_owned()
        }
    );
    // PDF/UA-1 has no byte-level conformance-export target in this subset.
    assert_eq!(
        preflight_bytes(&roomy_pdf(), "PDF/UA-1", &PreflightOptions { fix: true })
            .expect_err("no UA fixups"),
        BytePreflightError::FixupsUnavailable {
            label: "PDF/UA-1".to_owned()
        }
    );
}

#[test]
fn header_only_input_returns_a_typed_catalog_error() {
    // A valid header but no xref: preflight still returns a typed error rather
    // than panicking or silently producing an empty report.
    let error = preflight_bytes(
        b"%PDF-1.7\n%%EOF\n",
        "PDF/A-2b",
        &PreflightOptions::default(),
    )
    .expect_err("no structure");
    assert!(matches!(error, BytePreflightError::Catalog { .. }));
}
