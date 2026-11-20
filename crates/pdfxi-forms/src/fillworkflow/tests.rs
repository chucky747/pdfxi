use super::*;
use crate::acroform::PdfValue;
use crate::appearance::WidgetRect;
use crate::field::{Field, FieldFlags, FieldType, FieldValue};
use crate::signature::{Participant, ParticipantRole};
use std::collections::BTreeMap;

fn rect() -> WidgetRect {
    WidgetRect::new(10.0, 20.0, 100.0, 24.0)
}

fn page() -> ObjectRef {
    ObjectRef::new(5, 0)
}

fn text(value: &str) -> FieldValue {
    FieldValue::Text(value.to_owned())
}

fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    needle.is_empty() || haystack.windows(needle.len()).any(|part| part == needle)
}

fn read_values(pdf: &[u8]) -> BTreeMap<String, PdfValue> {
    let document = crate::parse_pdf(pdf).unwrap();
    let catalog = document.catalog().unwrap();
    let acroform = document.resolve(catalog.get("AcroForm").unwrap()).unwrap();
    let acroform = acroform.as_dict().unwrap();
    let fields = acroform.get("Fields").unwrap().as_array().unwrap();
    fields
        .iter()
        .map(|reference| {
            let field = document.resolve(reference).unwrap();
            let field = field.as_dict().unwrap();
            let name = field.get("T").unwrap().as_text().unwrap().to_owned();
            (name, field.get("V").cloned().unwrap_or(PdfValue::Null))
        })
        .collect()
}

#[test]
fn fill_text_choice_and_checkbox_end_to_end() {
    let specs = [
        FieldSpec::text("order.name", rect(), 0).with_flags(FieldFlags::REQUIRED),
        FieldSpec::choice("order.color", rect(), 0, ["red", "green"], true),
        FieldSpec::checkbox("order.agree", rect(), 0),
    ];
    let document = FillDocument::from_specs(&specs, page()).unwrap();
    let mut flow = FillWorkflow::new(document);
    flow.set("order.name", text("Ada")).unwrap();
    flow.set("order.color", FieldValue::Choice(vec!["green".to_owned()]))
        .unwrap();
    flow.set("order.agree", FieldValue::Bool(true)).unwrap();

    let done = flow.finish().unwrap();
    assert_eq!(done.plan.fill.counts(), (3, 0, 0));
    assert!(done.verification.is_complete());
    assert_eq!(done.verification.checked, 3);
    assert_eq!(done.verification.signatures, 0);

    let values = read_values(&done.pdf);
    assert_eq!(
        values.get("order.name"),
        Some(&PdfValue::Text("Ada".into()))
    );
    assert_eq!(
        values.get("order.color"),
        Some(&PdfValue::Text("green".into()))
    );
    assert_eq!(
        values.get("order.agree"),
        Some(&PdfValue::Name("On".into()))
    );

    // Appearance updates were computed for every filled widget.
    assert_eq!(done.plan.widgets.len(), 3);
    assert!(done.plan.widgets.iter().all(|widget| {
        widget
            .as_dict()
            .and_then(|dict| dict.get("AP"))
            .and_then(PdfValue::as_dict)
            .is_some()
    }));
}

#[test]
fn validation_failure_is_typed() {
    let specs = [FieldSpec::choice(
        "color",
        rect(),
        0,
        ["red", "green"],
        true,
    )];
    let document = FillDocument::from_specs(&specs, page()).unwrap();
    let mut flow = FillWorkflow::new(document);
    flow.set("color", FieldValue::Choice(vec!["purple".to_owned()]))
        .unwrap();

    let error = flow.finish().unwrap_err();
    match error {
        FillWorkflowError::ValidationFailed { count, validation } => {
            assert_eq!(count, 1);
            assert!(matches!(
                &validation.errors()[0],
                crate::ValidationError::InvalidChoice { field, value }
                    if field == "color" && value == "purple"
            ));
        }
        other => panic!("expected a typed validation failure, got {other:?}"),
    }
}

#[test]
fn readonly_field_rejection_is_typed() {
    let specs = [FieldSpec::text("locked", rect(), 0).with_flags(FieldFlags::READ_ONLY)];
    let document = FillDocument::from_specs(&specs, page()).unwrap();
    let mut flow = FillWorkflow::new(document);

    assert_eq!(
        flow.set("locked", text("x")),
        Err(FillWorkflowError::ReadOnlyField("locked".to_owned()))
    );
    assert_eq!(
        flow.clear("locked"),
        Err(FillWorkflowError::ReadOnlyField("locked".to_owned()))
    );
    assert_eq!(flow.plan().unwrap().fill.counts(), (0, 0, 0));
}

#[test]
fn required_missing_is_typed() {
    let specs = [FieldSpec::text("must", rect(), 0).with_flags(FieldFlags::REQUIRED)];
    let document = FillDocument::from_specs(&specs, page()).unwrap();
    let mut flow = FillWorkflow::new(document);

    assert_eq!(
        flow.clear("must"),
        Err(FillWorkflowError::RequiredField("must".to_owned()))
    );
    let error = flow.finish().unwrap_err();
    match error {
        FillWorkflowError::ValidationFailed { validation, .. } => assert!(matches!(
            &validation.errors()[0],
            crate::ValidationError::MissingRequired { field } if field == "must"
        )),
        other => panic!("expected a typed validation failure, got {other:?}"),
    }
}

#[test]
fn reset_restores_defaults_and_drops_signatures() {
    let specs = [FieldSpec::text("city", rect(), 0).with_default(text("Paris"))];
    let document = FillDocument::from_specs(&specs, page()).unwrap();
    let mut flow = FillWorkflow::new(document);
    flow.set("city", text("Rome")).unwrap();
    assert_eq!(flow.value("city"), Ok(&text("Rome")));
    flow.reset();
    assert_eq!(flow.value("city"), Ok(&text("Paris")));

    let done = flow.finish().unwrap();
    assert_eq!(
        read_values(&done.pdf).get("city"),
        Some(&PdfValue::Text("Paris".into()))
    );
}

#[test]
fn serialized_round_trip_preserves_values() {
    let specs = [
        FieldSpec::text("name", rect(), 0),
        FieldSpec::checkbox("flag", rect(), 0),
        FieldSpec::choice("pick", rect(), 0, ["a", "b"], false),
    ];
    let document = FillDocument::from_specs(&specs, page()).unwrap();
    let mut flow = FillWorkflow::new(document);
    flow.set("name", text("Zoe")).unwrap();
    flow.set("flag", FieldValue::Bool(true)).unwrap();
    flow.set("pick", FieldValue::Choice(vec!["b".to_owned()]))
        .unwrap();

    let done = flow.finish().unwrap();
    assert!(done.pdf.starts_with(b"%PDF-1.7"));
    assert!(done.pdf.ends_with(b"\n%%EOF\n"));
    let values = read_values(&done.pdf);
    assert_eq!(values.len(), 3);
    assert_eq!(values.get("name"), Some(&PdfValue::Text("Zoe".into())));
    assert_eq!(values.get("flag"), Some(&PdfValue::Name("On".into())));
    assert_eq!(values.get("pick"), Some(&PdfValue::Text("b".into())));
    assert_eq!(done.plan.expected_value("name"), Some(&text("Zoe")));
}

#[test]
fn signature_field_blocked_without_signature() {
    let specs = [FieldSpec::signature("sig", rect(), 0).with_flags(FieldFlags::REQUIRED)];
    let document = FillDocument::from_specs(&specs, page()).unwrap();
    let mut flow = FillWorkflow::new(document);

    assert_eq!(
        flow.set("sig", text("forged")),
        Err(FillWorkflowError::SignatureMustUseWorkflow(
            "sig".to_owned()
        ))
    );
    assert_eq!(
        flow.clear("sig"),
        Err(FillWorkflowError::SignatureMustUseWorkflow(
            "sig".to_owned()
        ))
    );

    let unsigned = SignatureWorkflow::new("sig").unwrap();
    assert_eq!(
        flow.attach_signature(unsigned),
        Err(FillWorkflowError::SignatureNotReady {
            field: "sig".to_owned(),
            state: SignatureStatus::Unsigned,
        })
    );

    let mut pending = SignatureWorkflow::new("sig").unwrap();
    let signer = Participant::new("Ada", ParticipantRole::Signer).unwrap();
    pending.invite(signer, "2026-01-01T00:00:00Z").unwrap();
    assert_eq!(pending.state(), SignatureStatus::Pending);
    assert_eq!(
        flow.attach_signature(pending),
        Err(FillWorkflowError::SignatureNotReady {
            field: "sig".to_owned(),
            state: SignatureStatus::Pending,
        })
    );

    assert!(matches!(
        flow.finish(),
        Err(FillWorkflowError::ValidationFailed { .. })
    ));
}

#[test]
fn signature_workflow_transition_recorded() {
    let specs = [FieldSpec::signature("sig", rect(), 0).with_flags(FieldFlags::REQUIRED)];
    let document = FillDocument::from_specs(&specs, page()).unwrap();
    let mut flow = FillWorkflow::new(document);

    let signer = Participant::new("Ada", ParticipantRole::Signer).unwrap();
    let mut workflow = SignatureWorkflow::new("sig").unwrap();
    workflow
        .invite(signer.clone(), "2026-01-01T00:00:00Z")
        .unwrap();
    workflow
        .sign(signer, "2026-01-02T00:00:00Z", Some("approval"), None, None)
        .unwrap();
    assert_eq!(workflow.state(), SignatureStatus::Signed);
    flow.attach_signature(workflow).unwrap();

    let done = flow.finish().unwrap();
    let record = done.plan.signature_record("sig").unwrap();
    assert_eq!(record.status, SignatureStatus::Signed);
    assert_eq!(record.last_kind, Some(TransitionKind::Sign));
    assert_eq!(record.last_from, Some(SignatureStatus::Pending));
    assert_eq!(record.last_to, Some(SignatureStatus::Signed));
    assert_eq!(
        record.last_timestamp.as_deref(),
        Some("2026-01-02T00:00:00Z")
    );
    assert_eq!(record.participant.as_deref(), Some("Ada"));
    assert_eq!(record.transition_count, 2);
    assert_eq!(done.verification.signatures, 1);

    // No signature bytes are written: /V stays null and /Contents never appears.
    assert_eq!(read_values(&done.pdf).get("sig"), Some(&PdfValue::Null));
    assert!(!contains_bytes(&done.pdf, b"/Contents"));
    assert!(!contains_bytes(&done.pdf, b"/ByteRange"));
}

#[test]
fn deterministic_plan_and_bytes() {
    let specs = [
        FieldSpec::text("a", rect(), 0),
        FieldSpec::checkbox("b", rect(), 0),
    ];
    let build = || {
        let document = FillDocument::from_specs(&specs, page()).unwrap();
        let mut flow = FillWorkflow::new(document);
        flow.set("a", text("same")).unwrap();
        flow.set("b", FieldValue::Bool(true)).unwrap();
        flow.finish().unwrap()
    };
    let first = build();
    let second = build();
    assert_eq!(first.pdf, second.pdf);
    assert_eq!(first.plan, second.plan);
    assert_eq!(first.verification, second.verification);
}

#[test]
fn malformed_input_is_typed() {
    let form = Form::from_fields([Field::new("x", FieldType::Text)]).unwrap();
    assert_eq!(
        FillDocument::from_parts(form.clone(), PdfValue::Null, page()).unwrap_err(),
        FillWorkflowError::Malformed("AcroForm is not a dictionary".to_owned())
    );

    let other_dict = crate::acroform_dictionary(&[FieldSpec::text("other", rect(), 0)]).unwrap();
    assert!(matches!(
        FillDocument::from_parts(form.clone(), other_dict, page()),
        Err(FillWorkflowError::Malformed(_))
    ));

    let dict = crate::acroform_dictionary(&[FieldSpec::text("x", rect(), 0)]).unwrap();
    assert!(matches!(
        FillDocument::from_parts(form, dict, ObjectRef::new(0, 0)),
        Err(FillWorkflowError::Malformed(_))
    ));

    // Unknown field writes and signature mismatches are typed as well.
    let document = FillDocument::from_specs(&[FieldSpec::text("x", rect(), 0)], page()).unwrap();
    let mut flow = FillWorkflow::new(document);
    assert_eq!(
        flow.set("missing", text("v")),
        Err(FillWorkflowError::FieldNotFound("missing".to_owned()))
    );
    assert_eq!(
        flow.attach_signature(SignatureWorkflow::new("x").unwrap()),
        Err(FillWorkflowError::UnknownSignatureField("x".to_owned()))
    );
}

#[test]
fn verification_rejects_non_pdf_bytes() {
    let document = FillDocument::from_specs(&[FieldSpec::text("x", rect(), 0)], page()).unwrap();
    let plan = FillWorkflow::new(document).plan().unwrap();
    assert!(matches!(
        plan.verify(b"not a pdf"),
        Err(FillWorkflowError::Parse(_))
    ));
}

#[test]
fn clear_and_number_values_round_trip() {
    let specs = [FieldSpec::text("amount", rect(), 0)];
    let document = FillDocument::from_specs(&specs, page()).unwrap();
    let mut flow = FillWorkflow::new(document);
    flow.set("amount", FieldValue::number(42.5)).unwrap();
    let done = flow.finish().unwrap();
    assert_eq!(
        read_values(&done.pdf).get("amount"),
        Some(&PdfValue::Number(42.5))
    );

    let document = FillDocument::from_specs(&specs, page()).unwrap();
    let mut flow = FillWorkflow::new(document);
    flow.set("amount", text("temp")).unwrap();
    flow.clear("amount").unwrap();
    assert_eq!(flow.value("amount"), Ok(&FieldValue::Empty));
    let done = flow.finish().unwrap();
    assert_eq!(read_values(&done.pdf).get("amount"), Some(&PdfValue::Null));
}

#[test]
fn valid_signature_and_reset_field_record() {
    let specs = [FieldSpec::signature("sig", rect(), 0)];
    let document = FillDocument::from_specs(&specs, page()).unwrap();
    let mut flow = FillWorkflow::new(document);
    let signer = Participant::new("Grace", ParticipantRole::Approver).unwrap();
    let mut workflow = SignatureWorkflow::new("sig").unwrap();
    workflow
        .sign(signer, "2026-03-01T00:00:00Z", None, None, None)
        .unwrap();
    workflow.mark_valid("2026-03-02T00:00:00Z").unwrap();
    assert_eq!(workflow.state(), SignatureStatus::Valid);
    flow.attach_signature(workflow.clone()).unwrap();
    assert!(flow.signature_workflow("sig").is_some());

    // A second workflow for the same field is rejected.
    assert_eq!(
        flow.attach_signature(workflow),
        Err(FillWorkflowError::DuplicateSignatureWorkflow(
            "sig".to_owned()
        ))
    );

    flow.reset_field("sig").unwrap();
    assert!(flow.signature_workflow("sig").is_none());
    assert!(flow.plan().unwrap().signatures.is_empty());

    let done = flow.finish().unwrap();
    assert_eq!(done.verification.signatures, 1);
    assert!(done.plan.signatures.is_empty());
}
