use super::*;
use crate::field::{Field, FieldType, FieldValue};
use crate::fill::FormFill;
use crate::form::Form;
use crate::{export_xfdf, import_xfdf};

fn sample_form() -> Form {
    Form::from_fields([
        Field::new("name", FieldType::Text).with_value(FieldValue::Text("Ada".into())),
        Field::new("sig", FieldType::Signature).with_value(FieldValue::Text("Ada Signer".into())),
        Field::new("agree", FieldType::CheckBox).with_value(FieldValue::Bool(true)),
        Field::new("decline", FieldType::CheckBox).with_value(FieldValue::Bool(false)),
        Field::new("size", FieldType::Radio)
            .with_choices(["S", "M", "L"])
            .with_value(FieldValue::Choice(vec!["M".into()])),
        Field::new("color", FieldType::Choice)
            .with_choices(["red", "green", "blue"])
            .with_value(FieldValue::Choice(vec!["red".into(), "blue".into()])),
        Field::new("total", FieldType::Text).with_value(FieldValue::number(42.5)),
        Field::new("empty", FieldType::Text),
    ])
    .unwrap()
}

fn round_trip(form: &Form) -> FdfDocument {
    let xml = export_xfdf(form).unwrap();
    import_xfdf(&xml, form).unwrap()
}

#[test]
fn parse_xfdf_reads_fields_values_entities_and_prolog() {
    let xml = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!-- note -->\n\
               <xfdf xmlns=\"http://ns.adobe.com/xfdf/\"><fields>\
               <field name=\"a&amp;b\"><value>x&lt;y&gt;&quot;&apos;</value></field>\
               </fields></xfdf>";
    let document = parse_xfdf(xml).unwrap();
    assert_eq!(document.len(), 1);
    assert_eq!(document.fields()[0].name(), "a&b");
    assert_eq!(document.fields()[0].values(), &["x<y>\"'".to_owned()]);
}

#[test]
fn xfdf_round_trip_text_and_signature() {
    let form = sample_form();
    let document = round_trip(&form);

    assert_eq!(
        document.field("name").map(FdfField::value),
        Some(&FieldValue::Text("Ada".into()))
    );
    assert_eq!(
        document.field("sig").map(FdfField::value),
        Some(&FieldValue::Text("Ada Signer".into()))
    );
    assert_eq!(
        document.field("empty").map(FdfField::value),
        Some(&FieldValue::Empty)
    );
}

#[test]
fn xfdf_round_trip_checkbox_and_radio() {
    let form = sample_form();
    let document = round_trip(&form);

    assert_eq!(
        document.field("agree").map(FdfField::value),
        Some(&FieldValue::Bool(true))
    );
    assert_eq!(
        document.field("decline").map(FdfField::value),
        Some(&FieldValue::Bool(false))
    );
    assert_eq!(
        document.field("size").map(FdfField::value),
        Some(&FieldValue::Choice(vec!["M".into()]))
    );
}

#[test]
fn xfdf_round_trip_choice_multi_select() {
    let form = sample_form();
    let document = round_trip(&form);

    assert_eq!(
        document.field("color").map(FdfField::value),
        Some(&FieldValue::Choice(vec!["red".into(), "blue".into()]))
    );
}

#[test]
fn xfdf_preserves_off_like_option_names() {
    // "Off", "No", "0" and "False" are legitimate option names; selection must
    // be decided by declared membership, not by the literal spelling.
    let form = Form::from_fields([
        Field::new("pick", FieldType::Choice)
            .with_choices(["Off", "No", "0", "False"])
            .with_value(FieldValue::Choice(vec![
                "Off".into(),
                "No".into(),
                "0".into(),
                "False".into(),
            ])),
        Field::new("size", FieldType::Radio)
            .with_choices(["No", "Off", "0"])
            .with_value(FieldValue::Choice(vec!["No".into()])),
    ])
    .unwrap();

    let document = round_trip(&form);
    assert_eq!(
        document.field("pick").map(FdfField::value),
        Some(&FieldValue::Choice(vec![
            "Off".into(),
            "No".into(),
            "0".into(),
            "False".into(),
        ]))
    );
    assert_eq!(
        document.field("size").map(FdfField::value),
        Some(&FieldValue::Choice(vec!["No".into()]))
    );
}

#[test]
fn xfdf_ignores_radio_value_outside_declared_choices() {
    let form = Form::from_fields([Field::new("size", FieldType::Radio)
        .with_choices(["S", "M", "L"])
        .with_value(FieldValue::Choice(vec!["M".into()]))])
    .unwrap();
    let xml = "<xfdf><fields><field name=\"size\"><value>Off</value></field></fields></xfdf>";

    // Off is a valid appearance state but not a declared choice, so it deselects.
    let document = import_xfdf(xml, &form).unwrap();
    assert_eq!(
        document.field("size").map(FdfField::value),
        Some(&FieldValue::Empty)
    );
}

#[test]
fn xfdf_round_trip_preserves_numbers_via_form_type() {
    let form = sample_form();
    let document = round_trip(&form);

    assert_eq!(
        document.field("total").map(FdfField::value),
        Some(&FieldValue::number(42.5))
    );
}

#[test]
fn xfdf_escapes_and_decodes_xml_special_characters() {
    let name = "a&b<c>\"d'e";
    let value = "v&<>'\" end";
    let form = Form::from_fields([
        Field::new(name, FieldType::Text).with_value(FieldValue::Text(value.into()))
    ])
    .unwrap();

    let xml = export_xfdf(&form).unwrap();
    assert!(xml.contains("&amp;"), "{xml}");
    assert!(xml.contains("&lt;"), "{xml}");
    assert!(xml.contains("&gt;"), "{xml}");
    assert!(xml.contains("&quot;"), "{xml}");
    assert!(xml.contains("&apos;"), "{xml}");

    let document = import_xfdf(&xml, &form).unwrap();
    assert_eq!(
        document.field(name).map(FdfField::value),
        Some(&FieldValue::Text(value.into()))
    );
}

#[test]
fn xfdf_decodes_numeric_character_references() {
    let form = Form::from_fields([Field::new("n", FieldType::Text)]).unwrap();
    let xml = "<xfdf><fields><field name=\"n\"><value>&#65;&#x42;</value></field></fields></xfdf>";
    let document = import_xfdf(xml, &form).unwrap();

    assert_eq!(
        document.field("n").map(FdfField::value),
        Some(&FieldValue::Text("AB".into()))
    );
}

#[test]
fn xfdf_accepts_default_and_prefixed_namespaces() {
    let default_ns = "<xfdf xmlns=\"http://ns.adobe.com/xfdf/\"><fields/></xfdf>";
    assert!(parse_xfdf(default_ns).is_ok());

    let prefixed = "<x:xfdf xmlns:x=\"http://ns.adobe.com/xfdf/\">\
                     <x:fields><x:field name=\"a\"><x:value>1</x:value></x:field></x:fields>\
                     </x:xfdf>";
    assert_eq!(parse_xfdf(prefixed).unwrap().fields()[0].name(), "a");

    // A foreign-looking prefix is accepted when explicitly resolved to XFDF.
    let resolved = "<xfdf xmlns=\"http://ns.adobe.com/xfdf/\" \
                     xmlns:f=\"http://ns.adobe.com/xfdf/\"><fields>\
                     <f:field f:name=\"resolved\"><f:value>yes</f:value></f:field>\
                     </fields></xfdf>";
    assert_eq!(parse_xfdf(resolved).unwrap().fields()[0].name(), "resolved");
}

#[test]
fn xfdf_rejects_foreign_prefixed_elements_and_attributes() {
    // f: is never declared, so both the element and attribute prefixes fail.
    let undeclared = "<xfdf xmlns=\"http://ns.adobe.com/xfdf/\"><fields>\
                       <f:field f:name=\"x\"><f:value>v</f:value></f:field>\
                       </fields></xfdf>";
    assert!(matches!(
        parse_xfdf(undeclared),
        Err(XfdfError::UndeclaredPrefix { .. })
    ));

    // Declaring f: to a foreign namespace must be rejected too.
    let foreign = "<xfdf xmlns=\"http://ns.adobe.com/xfdf/\">\
                    <fields xmlns:f=\"http://example.com/foreign\">\
                    <f:field f:name=\"x\"/></fields></xfdf>";
    assert!(matches!(
        parse_xfdf(foreign),
        Err(XfdfError::UnexpectedNamespace { .. })
    ));
}

#[test]
fn xfdf_rejects_malformed_xml() {
    assert!(matches!(
        parse_xfdf("<xfdf><fields></xfdf>"),
        Err(XfdfError::MismatchedTag { .. })
    ));
    assert!(matches!(
        parse_xfdf("<xfdf><fields>"),
        Err(XfdfError::Malformed { .. })
    ));
    assert!(matches!(parse_xfdf(""), Err(XfdfError::MissingRoot)));
    assert!(matches!(
        parse_xfdf("<other><fields/></other>"),
        Err(XfdfError::UnexpectedRoot { .. })
    ));
    assert!(matches!(
        parse_xfdf("<xfdf><field name=\"x\" value=unquoted/></xfdf>"),
        Err(XfdfError::Malformed { .. })
    ));
}

#[test]
fn xfdf_rejects_unknown_entity_and_namespace() {
    let bad_entity =
        "<xfdf><fields><field name=\"x\"><value>&bogus;</value></field></fields></xfdf>";
    assert!(matches!(
        parse_xfdf(bad_entity),
        Err(XfdfError::UnknownEntity { .. })
    ));

    let bad_ns = "<xfdf xmlns=\"http://example.com/invalid\"><fields/></xfdf>";
    assert!(matches!(
        parse_xfdf(bad_ns),
        Err(XfdfError::UnexpectedNamespace { .. })
    ));

    let undeclared = "<x:xfdf><fields/></x:xfdf>";
    assert!(matches!(
        parse_xfdf(undeclared),
        Err(XfdfError::UndeclaredPrefix { .. })
    ));
}

#[test]
fn xfdf_rejects_unknown_field() {
    let xml = "<xfdf><fields><field name=\"missing\"><value>x</value></field></fields></xfdf>";
    assert_eq!(
        import_xfdf(xml, &sample_form()),
        Err(XfdfError::UnknownField {
            name: "missing".to_owned()
        })
    );
}

#[test]
fn xfdf_rejects_field_without_a_name() {
    let xml = "<xfdf><fields><field><value>x</value></field></fields></xfdf>";
    assert!(matches!(
        parse_xfdf(xml),
        Err(XfdfError::MissingFieldName { .. })
    ));
}

#[test]
fn xfdf_rejects_too_many_values_for_a_scalar_field() {
    let xml = "<xfdf><fields><field name=\"agree\">\
               <value>On</value><value>Off</value></field></fields></xfdf>";
    assert!(matches!(
        import_xfdf(xml, &sample_form()),
        Err(XfdfError::TooManyValues { .. })
    ));
}

#[test]
fn xfdf_supports_hierarchical_field_names() {
    let form = Form::from_fields([Field::new("order.address.city", FieldType::Text)]).unwrap();
    let xml = "<xfdf><fields><field name=\"order\">\
               <field name=\"address\"><field name=\"city\"><value>Paris</value></field></field>\
               </field></fields></xfdf>";
    let document = import_xfdf(xml, &form).unwrap();

    assert_eq!(
        document.field("order.address.city").map(FdfField::value),
        Some(&FieldValue::Text("Paris".into()))
    );
}

#[test]
fn xfdf_imported_document_applies_to_a_fill_session() {
    let form = Form::from_fields([Field::new("name", FieldType::Text)]).unwrap();
    let xml = "<xfdf><fields><field name=\"name\"><value>Bob</value></field></fields></xfdf>";
    let document = import_xfdf(xml, &form).unwrap();

    let mut fill = FormFill::new(form);
    fill.fill_from_fdf(&document).unwrap();
    let (filled, report) = fill.finish();
    assert_eq!(filled.value("name"), Ok(&FieldValue::Text("Bob".into())));
    assert_eq!(report.len(), 1);
}

#[test]
fn xfdf_export_is_deterministic() {
    let form = sample_form();
    let first = export_xfdf(&form).unwrap();
    let second = export_xfdf(&form).unwrap();
    assert_eq!(first, second);

    let doc = import_xfdf(&first, &form).unwrap();
    assert_eq!(doc, round_trip(&form));
}
