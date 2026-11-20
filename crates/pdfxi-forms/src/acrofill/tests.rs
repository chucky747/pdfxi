use super::*;
use crate::FieldSpec;
use crate::acroform_dictionary;
use crate::appearance::WidgetRect;
use crate::field::{FieldFlags, FieldType};
use crate::rules::{CurrencyFormat, DateLayout, Format};
use std::collections::BTreeMap;

fn rect() -> WidgetRect {
    WidgetRect::new(10.0, 20.0, 100.0, 24.0)
}

fn fields(form: &PdfValue) -> &[PdfValue] {
    form.as_dict()
        .and_then(|dict| dict.get("Fields"))
        .and_then(PdfValue::as_array)
        .unwrap()
}

fn field<'a>(form: &'a PdfValue, name: &str) -> &'a BTreeMap<String, PdfValue> {
    fields(form)
        .iter()
        .find_map(|entry| {
            let dict = entry.as_dict()?;
            (dict.get("T").and_then(PdfValue::as_text) == Some(name)).then_some(dict)
        })
        .unwrap()
}

fn widget<'a>(form: &'a PdfValue, name: &str, index: usize) -> &'a BTreeMap<String, PdfValue> {
    field(form, name)
        .get("Kids")
        .and_then(PdfValue::as_array)
        .unwrap()[index]
        .as_dict()
        .unwrap()
}

fn normal(widget: &BTreeMap<String, PdfValue>) -> &PdfValue {
    widget
        .get("AP")
        .unwrap()
        .as_dict()
        .unwrap()
        .get("N")
        .unwrap()
}

fn widget_value(rect_values: [f64; 4], on_state: Option<&str>) -> PdfValue {
    let mut dict = BTreeMap::new();
    dict.insert("Type".to_owned(), PdfValue::Name("Annot".to_owned()));
    dict.insert("Subtype".to_owned(), PdfValue::Name("Widget".to_owned()));
    dict.insert(
        "Rect".to_owned(),
        PdfValue::Array(
            rect_values
                .iter()
                .map(|value| PdfValue::Number(*value))
                .collect(),
        ),
    );
    if let Some(state) = on_state {
        let mut states = BTreeMap::new();
        states.insert(STATE_OFF.to_owned(), PdfValue::Dictionary(BTreeMap::new()));
        states.insert(state.to_owned(), PdfValue::Dictionary(BTreeMap::new()));
        let mut ap = BTreeMap::new();
        ap.insert("N".to_owned(), PdfValue::Dictionary(states));
        dict.insert("AP".to_owned(), PdfValue::Dictionary(ap));
    }
    PdfValue::Dictionary(dict)
}

fn field_with_widgets(name: &str, field_type: &str, flags: u32, kids: Vec<PdfValue>) -> PdfValue {
    let mut dict = BTreeMap::new();
    dict.insert("Type".to_owned(), PdfValue::Name("Annot".to_owned()));
    dict.insert("T".to_owned(), PdfValue::Text(name.to_owned()));
    dict.insert("FT".to_owned(), PdfValue::Name(field_type.to_owned()));
    dict.insert("Ff".to_owned(), PdfValue::Number(f64::from(flags)));
    dict.insert("Kids".to_owned(), PdfValue::Array(kids));
    let mut root = BTreeMap::new();
    root.insert(
        "Fields".to_owned(),
        PdfValue::Array(vec![PdfValue::Dictionary(dict)]),
    );
    PdfValue::Dictionary(root)
}

fn radio_form() -> PdfValue {
    let mut dict = BTreeMap::new();
    dict.insert("Type".to_owned(), PdfValue::Name("Annot".to_owned()));
    dict.insert("T".to_owned(), PdfValue::Text("size".to_owned()));
    dict.insert("FT".to_owned(), PdfValue::Name("Btn".to_owned()));
    dict.insert("Ff".to_owned(), PdfValue::Number(f64::from(1u32 << 15)));
    dict.insert(
        "Opt".to_owned(),
        PdfValue::Array(vec![PdfValue::Text("S".into()), PdfValue::Text("M".into())]),
    );
    dict.insert(
        "Kids".to_owned(),
        PdfValue::Array(vec![
            widget_value([0.0, 0.0, 20.0, 20.0], Some("S")),
            widget_value([0.0, 0.0, 20.0, 20.0], Some("M")),
        ]),
    );
    let mut root = BTreeMap::new();
    root.insert(
        "Fields".to_owned(),
        PdfValue::Array(vec![PdfValue::Dictionary(dict)]),
    );
    PdfValue::Dictionary(root)
}

#[test]
fn text_fill_writes_value_and_appearance() {
    let mut form = acroform_dictionary(&[FieldSpec::text("name", rect(), 0)]).unwrap();
    let report = fill_acroform(
        &mut form,
        [FieldAssignment::new("name", FieldValue::Text("Ada".into()))],
    );

    assert_eq!(report.counts(), (1, 0, 0));
    assert_eq!(
        field(&form, "name").get("V"),
        Some(&PdfValue::Text("Ada".into()))
    );
    let (dict, data) = normal(widget(&form, "name", 0)).as_stream().unwrap();
    assert_eq!(dict.get("Type"), Some(&PdfValue::Name("XObject".into())));
    assert_eq!(dict.get("Subtype"), Some(&PdfValue::Name("Form".into())));
    assert!(String::from_utf8_lossy(data).contains("(Ada) Tj"));
    assert_eq!(report.filled()[0].new, FieldValue::Text("Ada".into()));
    assert_eq!(report.filled()[0].widgets, 1);
}

#[test]
fn checkbox_on_and_off_toggle_value_as_and_states() {
    let mut form = acroform_dictionary(&[FieldSpec::checkbox("agree", rect(), 0)]).unwrap();

    let on = fill_acroform(
        &mut form,
        [FieldAssignment::new("agree", FieldValue::Bool(true))],
    );
    assert_eq!(on.counts(), (1, 0, 0));
    assert_eq!(
        field(&form, "agree").get("V"),
        Some(&PdfValue::Name("On".into()))
    );
    assert_eq!(
        widget(&form, "agree", 0).get("AS"),
        Some(&PdfValue::Name("On".into()))
    );
    let states = normal(widget(&form, "agree", 0)).as_dict().unwrap();
    assert!(states.contains_key("On") && states.contains_key("Off"));
    assert!(states["On"].as_stream().is_some());

    let off = fill_acroform(
        &mut form,
        [FieldAssignment::new("agree", FieldValue::Bool(false))],
    );
    assert_eq!(off.counts(), (1, 0, 0));
    assert_eq!(
        field(&form, "agree").get("V"),
        Some(&PdfValue::Name("Off".into()))
    );
    assert_eq!(
        widget(&form, "agree", 0).get("AS"),
        Some(&PdfValue::Name("Off".into()))
    );
}

#[test]
fn radio_selection_is_exclusive_across_widgets() {
    let mut form = radio_form();
    let report = fill_acroform(
        &mut form,
        [FieldAssignment::new(
            "size",
            FieldValue::Choice(vec!["M".into()]),
        )],
    );

    assert_eq!(report.counts(), (1, 0, 0));
    assert_eq!(
        field(&form, "size").get("V"),
        Some(&PdfValue::Name("M".into()))
    );
    assert_eq!(
        widget(&form, "size", 0).get("AS"),
        Some(&PdfValue::Name("Off".into()))
    );
    assert_eq!(
        widget(&form, "size", 1).get("AS"),
        Some(&PdfValue::Name("M".into()))
    );
    let active = [0, 1]
        .iter()
        .filter(|&&index| {
            widget(&form, "size", index).get("AS") != Some(&PdfValue::Name("Off".into()))
        })
        .count();
    assert_eq!(active, 1, "exactly one radio widget may be on");
    assert!(
        normal(widget(&form, "size", 0))
            .as_dict()
            .unwrap()
            .contains_key("M")
    );
}

#[test]
fn choice_accepts_index_and_export_value() {
    let spec = FieldSpec::choice("color", rect(), 0, ["red", "green"], true);
    let mut form = acroform_dictionary(&[spec]).unwrap();

    let indexed = fill_acroform(
        &mut form,
        [FieldAssignment::new("color", FieldValue::Number(1.0))],
    );
    assert_eq!(indexed.counts(), (1, 0, 0));
    assert_eq!(
        field(&form, "color").get("V"),
        Some(&PdfValue::Text("green".into()))
    );

    let exported = fill_acroform(
        &mut form,
        [FieldAssignment::new(
            "color",
            FieldValue::Choice(vec!["red".into()]),
        )],
    );
    assert_eq!(exported.counts(), (1, 0, 0));
    assert_eq!(
        field(&form, "color").get("V"),
        Some(&PdfValue::Text("red".into()))
    );

    let mut pairs = BTreeMap::new();
    pairs.insert(
        "Opt".to_owned(),
        PdfValue::Array(vec![PdfValue::Array(vec![
            PdfValue::Text("e1".into()),
            PdfValue::Text("Display One".into()),
        ])]),
    );
    let mut dict = field(&form, "color").clone();
    dict.extend(pairs);
    let mut root = BTreeMap::new();
    root.insert(
        "Fields".to_owned(),
        PdfValue::Array(vec![PdfValue::Dictionary(dict)]),
    );
    let mut pair_form = PdfValue::Dictionary(root);
    let mapped = fill_acroform(
        &mut pair_form,
        [FieldAssignment::new(
            "color",
            FieldValue::Choice(vec!["Display One".into()]),
        )],
    );
    assert_eq!(mapped.counts(), (1, 0, 0));
    let entry = fields(&pair_form)[0].as_dict().unwrap();
    assert_eq!(entry.get("V"), Some(&PdfValue::Text("e1".into())));
}

#[test]
fn push_button_rejects_value_and_skips_empty() {
    let mut form = acroform_dictionary(&[FieldSpec::push_button("go", rect(), 0)]).unwrap();
    let before = form.clone();

    let rejected = fill_acroform(
        &mut form,
        [FieldAssignment::new("go", FieldValue::Text("x".into()))],
    );
    assert_eq!(rejected.counts(), (0, 0, 1));
    assert!(matches!(
        &rejected.rejected()[0].error,
        AcroFillError::TypeMismatch { found, .. } if *found == ValueKind::Text
    ));
    assert_eq!(form, before);

    let skipped = fill_acroform(&mut form, [FieldAssignment::new("go", FieldValue::Empty)]);
    assert_eq!(skipped.counts(), (0, 1, 0));
    assert_eq!(skipped.skipped()[0].reason, SkipReason::ButtonNoValue);
}

#[test]
fn read_only_field_is_rejected() {
    let spec = FieldSpec::text("locked", rect(), 0).with_flags(FieldFlags::READ_ONLY);
    let mut form = acroform_dictionary(&[spec]).unwrap();
    let before = form.clone();

    let report = fill_acroform(
        &mut form,
        [FieldAssignment::new("locked", FieldValue::Text("x".into()))],
    );

    assert_eq!(
        report.rejected(),
        &[RejectedField {
            name: "locked".to_owned(),
            error: AcroFillError::ReadOnlyField("locked".to_owned()),
        }]
    );
    assert_eq!(form, before);
}

#[test]
fn unknown_field_is_reported_deterministically() {
    let mut form = acroform_dictionary(&[FieldSpec::text("known", rect(), 0)]).unwrap();
    let report = fill_acroform(
        &mut form,
        [
            FieldAssignment::new("zeta", FieldValue::Text("x".into())),
            FieldAssignment::new("alpha", FieldValue::Text("y".into())),
        ],
    );

    let names: Vec<&str> = report
        .rejected()
        .iter()
        .map(|entry| entry.name.as_str())
        .collect();
    assert_eq!(names, ["alpha", "zeta"]);
    assert_eq!(
        report.rejected()[0].error,
        AcroFillError::UnknownField("alpha".to_owned())
    );
    assert_eq!(report.counts(), (0, 0, 2));
}

#[test]
fn multi_widget_field_updates_every_widget_identically() {
    let mut form = field_with_widgets(
        "notes",
        "Tx",
        0,
        vec![
            widget_value([0.0, 0.0, 100.0, 20.0], None),
            widget_value([0.0, 100.0, 100.0, 120.0], None),
        ],
    );
    let report = fill_acroform(
        &mut form,
        [FieldAssignment::new(
            "notes",
            FieldValue::Text("both".into()),
        )],
    );

    assert_eq!(report.filled()[0].widgets, 2);
    let first = normal(widget(&form, "notes", 0)).as_stream().unwrap().1;
    let second = normal(widget(&form, "notes", 1)).as_stream().unwrap().1;
    assert_eq!(first, second);
    assert!(String::from_utf8_lossy(first).contains("(both) Tj"));
}

#[test]
fn appearance_stream_is_emitted_for_each_widget() {
    let mut form = acroform_dictionary(&[FieldSpec::text("field", rect(), 0)]).unwrap();
    fill_acroform(
        &mut form,
        [FieldAssignment::new(
            "field",
            FieldValue::Text("Abc".into()),
        )],
    );
    let (dict, data) = normal(widget(&form, "field", 0)).as_stream().unwrap();
    assert_eq!(
        dict.get("BBox"),
        Some(&PdfValue::Array(vec![
            PdfValue::Number(0.0),
            PdfValue::Number(0.0),
            PdfValue::Number(100.0),
            PdfValue::Number(24.0),
        ]))
    );
    assert_eq!(
        dict.get("Length"),
        Some(&PdfValue::Number(data.len() as f64))
    );
    assert!(!data.is_empty());
}

#[test]
fn value_normalization_is_deterministic_for_dates_and_numbers() {
    let specs = [
        FieldSpec::text("when", rect(), 0),
        FieldSpec::text("amount", rect(), 0),
    ];
    let mut first = acroform_dictionary(&specs).unwrap();
    let mut second = acroform_dictionary(&specs).unwrap();
    let assignments = [
        FieldAssignment::new("when", FieldValue::Text("03/05/2024".into()))
            .with_format(Format::Date(DateLayout::Us)),
        FieldAssignment::new("amount", FieldValue::Text("$1,234.50".into()))
            .with_format(Format::Currency(CurrencyFormat::usd())),
    ];

    let first_report = fill_acroform(&mut first, assignments.clone());
    let second_report = fill_acroform(&mut second, assignments);
    assert_eq!(first, second);
    assert_eq!(first_report, second_report);
    assert_eq!(
        field(&first, "when").get("V"),
        Some(&PdfValue::Text("2024-03-05".into()))
    );
    assert_eq!(
        field(&first, "amount").get("V"),
        Some(&PdfValue::Number(1234.5))
    );
}

#[test]
fn fill_report_is_deterministic_and_collects_all_outcomes() {
    let specs = [
        FieldSpec::text("alpha", rect(), 0),
        FieldSpec::text("locked", rect(), 0).with_flags(FieldFlags::READ_ONLY),
    ];
    let mut first = acroform_dictionary(&specs).unwrap();
    let mut second = acroform_dictionary(&specs).unwrap();
    let assignments = [
        FieldAssignment::new("zeta", FieldValue::Text("z".into())),
        FieldAssignment::new("locked", FieldValue::Text("no".into())),
        FieldAssignment::new("alpha", FieldValue::Text("a".into())),
        FieldAssignment::new("alpha", FieldValue::Text("a".into())),
    ];

    let first_report = fill_acroform(&mut first, assignments.clone());
    let second_report = fill_acroform(&mut second, assignments);
    assert_eq!(first_report, second_report);
    assert_eq!(first_report.counts(), (1, 1, 2));
    assert_eq!(first_report.filled()[0].name, "alpha");
    assert_eq!(first_report.skipped()[0].reason, SkipReason::Unchanged);
    let rejected: Vec<&str> = first_report
        .rejected()
        .iter()
        .map(|entry| entry.name.as_str())
        .collect();
    assert_eq!(rejected, ["locked", "zeta"]);
}

#[test]
fn untouched_widgets_remain_byte_stable() {
    let specs = [
        FieldSpec::text("changed", rect(), 0),
        FieldSpec::text("untouched", rect(), 0),
    ];
    let mut form = acroform_dictionary(&specs).unwrap();
    let before = form.clone();

    fill_acroform(
        &mut form,
        [FieldAssignment::new(
            "changed",
            FieldValue::Text("new".into()),
        )],
    );

    assert_eq!(field(&form, "untouched"), field(&before, "untouched"));
    assert_eq!(
        widget(&form, "untouched", 0),
        widget(&before, "untouched", 0)
    );
    assert_ne!(widget(&form, "changed", 0), widget(&before, "changed", 0));
}

#[test]
fn default_value_can_be_written_when_requested() {
    let mut form = acroform_dictionary(&[FieldSpec::checkbox("agree", rect(), 0)]).unwrap();
    let report = fill_acroform(
        &mut form,
        [FieldAssignment::new("agree", FieldValue::Bool(true)).also_set_default()],
    );

    assert_eq!(report.counts(), (1, 0, 0));
    let field = field(&form, "agree");
    assert_eq!(field.get("V"), Some(&PdfValue::Name("On".into())));
    assert_eq!(field.get("DV"), Some(&PdfValue::Name("On".into())));
}

#[test]
fn hierarchical_field_names_are_supported() {
    let mut inner = BTreeMap::new();
    inner.insert("T".to_owned(), PdfValue::Text("name".into()));
    inner.insert("FT".to_owned(), PdfValue::Name("Tx".into()));
    inner.insert(
        "Kids".to_owned(),
        PdfValue::Array(vec![widget_value([0.0, 0.0, 100.0, 20.0], None)]),
    );
    let mut outer = BTreeMap::new();
    outer.insert("T".to_owned(), PdfValue::Text("order".into()));
    outer.insert(
        "Kids".to_owned(),
        PdfValue::Array(vec![PdfValue::Dictionary(inner)]),
    );
    let mut root = BTreeMap::new();
    root.insert(
        "Fields".to_owned(),
        PdfValue::Array(vec![PdfValue::Dictionary(outer)]),
    );
    let mut form = PdfValue::Dictionary(root);

    let report = fill_acroform(
        &mut form,
        [FieldAssignment::new(
            "order.name",
            FieldValue::Text("Ada".into()),
        )],
    );

    assert_eq!(report.counts(), (1, 0, 0));
    let outer = fields(&form)[0].as_dict().unwrap();
    let inner = outer["Kids"].as_array().unwrap()[0].as_dict().unwrap();
    assert_eq!(inner.get("V"), Some(&PdfValue::Text("Ada".into())));
    let widget = inner["Kids"].as_array().unwrap()[0].as_dict().unwrap();
    assert!(normal(widget).as_stream().is_some());
}

#[test]
fn invalid_geometry_is_rejected_without_mutating() {
    let mut form = field_with_widgets(
        "bad",
        "Tx",
        0,
        vec![widget_value([0.0, 0.0, 0.0, 0.0], None)],
    );
    let before = form.clone();

    let report = fill_acroform(
        &mut form,
        [FieldAssignment::new("bad", FieldValue::Text("x".into()))],
    );

    assert_eq!(report.counts(), (0, 0, 1));
    assert!(matches!(
        &report.rejected()[0].error,
        AcroFillError::Appearance { field, source }
            if field == "bad" && matches!(source, crate::AppearanceError::ZeroSizedWidget { .. })
    ));
    assert_eq!(form, before);
}

#[test]
fn date_normalization_rejects_invalid_calendar_date() {
    let mut form = acroform_dictionary(&[FieldSpec::text("when", rect(), 0)]).unwrap();
    let before = form.clone();
    let report = fill_acroform(
        &mut form,
        [
            FieldAssignment::new("when", FieldValue::Text("02/29/2023".into()))
                .with_format(Format::Date(DateLayout::Us)),
        ],
    );

    assert_eq!(report.counts(), (0, 0, 1));
    assert_eq!(
        report.rejected()[0].error,
        AcroFillError::InvalidDate {
            field: "when".to_owned(),
            value: "02/29/2023".to_owned(),
        }
    );
    assert_eq!(form, before);
}

#[test]
fn radio_type_and_flag_handling_preserves_unknown_bits() {
    let mut form = radio_form();
    fill_acroform(
        &mut form,
        [FieldAssignment::new(
            "size",
            FieldValue::Choice(vec!["S".into()]),
        )],
    );
    let field = field(&form, "size");
    assert_eq!(field.get("FT"), Some(&PdfValue::Name("Btn".into())));
    assert_eq!(
        field.get("Ff"),
        Some(&PdfValue::Number(f64::from(1u32 << 15)))
    );
    assert_eq!(field.get("V"), Some(&PdfValue::Name("S".into())));
    assert_eq!(
        field.get("Opt").and_then(PdfValue::as_array).unwrap().len(),
        2
    );
    assert_eq!(FieldType::Radio, FieldType::Radio);
}
