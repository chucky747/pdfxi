//! End-to-end form filling on the committed real-world AcroForm fixture.
//!
//! Every mutation runs through the public library API on a real
//! `pdfxi_core::Document`, saves a full file, and reopens the saved bytes before
//! asserting. The filled output is also written to
//! `target/lanes/forms/t0234-out/filled.pdf` for an independent PDFium check.

use std::path::{Path, PathBuf};

use pdfxi_core::{Dictionary, Document, Object, ObjectId};
use pdfxi_forms::FieldValue;
use pdfxi_forms::document_form::{
    AcroFormError, FormFieldKind, export_xfdf, flatten, import_xfdf, needs_appearances,
    read_acroform, regenerate_appearances, set_checkbox, set_choice, set_radio, set_text,
};

const FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../fixtures/realworld/pdfxi-forms-selfmade.pdf"
);

fn load() -> Document {
    let bytes = std::fs::read(FIXTURE).expect("fixture bytes");
    Document::from_bytes(bytes).expect("fixture parses")
}

fn save_and_reopen(doc: &Document) -> Document {
    let bytes = doc.save().expect("full save");
    Document::from_bytes(bytes).expect("saved bytes parse")
}

fn dictionary(doc: &Document, id: ObjectId) -> Dictionary {
    doc.object(id)
        .expect("object loads")
        .as_dictionary()
        .cloned()
        .expect("object is a dictionary")
}

fn stream(doc: &Document, id: ObjectId) -> (Dictionary, Vec<u8>) {
    match doc.object(id).expect("stream object loads") {
        Object::Stream { dictionary, data } => (dictionary, data),
        other => panic!("expected a stream, found {other:?}"),
    }
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

fn bbox(dictionary: &Dictionary) -> Vec<f64> {
    dictionary
        .get("BBox")
        .and_then(Object::as_array)
        .expect("BBox array")
        .iter()
        .map(|value| value.as_f64().expect("numeric BBox"))
        .collect()
}

fn fill_fixture() -> Document {
    let mut doc = load();
    set_text(&mut doc, "text-field", "filled by test").expect("set text");
    set_checkbox(&mut doc, "checkbox-field", true).expect("set checkbox");
    set_choice(&mut doc, "choice-field", Some("three")).expect("set choice");
    doc
}

fn output_directory() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/lanes/forms/t0234-out")
        .canonicalize()
        .unwrap_or_else(|_| {
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/lanes/forms/t0234-out")
        })
}

#[test]
fn reads_three_real_fields_with_inherited_attributes() {
    let doc = load();
    let model = read_acroform(&doc).expect("read AcroForm");

    assert!(model.present);
    assert!(!model.need_appearances);
    assert!(!model.contains_xfa);
    assert!(!model.has_javascript);
    assert_eq!(model.len(), 3);

    let text = model.field("text-field").expect("text field");
    assert_eq!(text.kind, FormFieldKind::Text);
    assert_eq!(text.value, FieldValue::Text("typed value".to_owned()));
    assert_eq!(text.primary_page(), Some(0));
    assert_eq!(text.primary_rect().expect("rect").width(), 200.0);
    assert_eq!(text.primary_rect().expect("rect").height(), 20.0);
    assert!(
        text.default_appearance
            .as_deref()
            .is_some_and(|da| da.contains("Helv"))
    );

    let checkbox = model.field("checkbox-field").expect("checkbox field");
    assert_eq!(checkbox.kind, FormFieldKind::Checkbox);
    assert_eq!(checkbox.value, FieldValue::Bool(true));
    assert!(!checkbox.widgets[0].has_appearance);

    let choice = model.field("choice-field").expect("choice field");
    assert_eq!(choice.kind, FormFieldKind::Choice);
    assert_eq!(
        choice.options,
        vec!["one".to_owned(), "two".to_owned(), "three".to_owned()]
    );
    assert_eq!(choice.value, FieldValue::Choice(vec!["two".to_owned()]));
}

#[test]
fn fills_saves_reopens_and_writes_appearances() {
    let filled = fill_fixture();
    let saved = save_and_reopen(&filled);

    let directory = output_directory();
    std::fs::create_dir_all(&directory).expect("create output directory");
    std::fs::write(
        directory.join("filled.pdf"),
        filled.save().expect("serialize filled file"),
    )
    .expect("write filled.pdf");

    let model = read_acroform(&saved).expect("read filled AcroForm");
    assert_eq!(model.len(), 3);
    assert_eq!(
        model.field("text-field").expect("text").value,
        FieldValue::Text("filled by test".to_owned())
    );
    assert_eq!(
        model.field("checkbox-field").expect("checkbox").value,
        FieldValue::Bool(true)
    );
    assert_eq!(
        model.field("choice-field").expect("choice").value,
        FieldValue::Choice(vec!["three".to_owned()])
    );

    let text_widget = model.field("text-field").expect("text").widgets[0]
        .object
        .expect("widget id");
    let text_dictionary = dictionary(&saved, text_widget);
    let text_ap = text_dictionary
        .get("AP")
        .and_then(Object::as_dictionary)
        .expect("text AP");
    let text_stream_id = text_ap
        .get("N")
        .and_then(Object::as_reference)
        .expect("text AP/N reference");
    let (text_stream_dictionary, text_data) = stream(&saved, text_stream_id);
    assert_eq!(bbox(&text_stream_dictionary), vec![0.0, 0.0, 200.0, 20.0]);
    assert!(!text_data.is_empty());
    assert!(contains(&text_data, b"filled by test"));
    assert!(contains(&text_data, b"/Helv"));

    let check_widget = model.field("checkbox-field").expect("checkbox").widgets[0]
        .object
        .expect("widget id");
    let check_dictionary = dictionary(&saved, check_widget);
    assert_eq!(
        check_dictionary.get("V").and_then(Object::as_name),
        Some("Yes")
    );
    assert_eq!(
        check_dictionary.get("AS").and_then(Object::as_name),
        Some("Yes")
    );
    let check_ap = check_dictionary
        .get("AP")
        .and_then(Object::as_dictionary)
        .expect("checkbox AP");
    let states = check_ap
        .get("N")
        .and_then(Object::as_dictionary)
        .expect("checkbox states");
    assert!(states.contains_key("Yes"));
    assert!(states.contains_key("Off"));
    let on_id = states
        .get("Yes")
        .and_then(Object::as_reference)
        .expect("on state reference");
    let (on_dictionary, on_data) = stream(&saved, on_id);
    assert_eq!(bbox(&on_dictionary), vec![0.0, 0.0, 20.0, 20.0]);
    assert!(!on_data.is_empty());

    let choice_widget = model.field("choice-field").expect("choice").widgets[0]
        .object
        .expect("widget id");
    let choice_dictionary = dictionary(&saved, choice_widget);
    let choice_ap = choice_dictionary
        .get("AP")
        .and_then(Object::as_dictionary)
        .expect("choice AP");
    let choice_stream_id = choice_ap
        .get("N")
        .and_then(Object::as_reference)
        .expect("choice AP/N reference");
    let (choice_stream_dictionary, choice_data) = stream(&saved, choice_stream_id);
    assert_eq!(bbox(&choice_stream_dictionary), vec![0.0, 0.0, 200.0, 20.0]);
    assert!(contains(&choice_data, b"three"));
}

#[test]
fn xfdf_round_trips_values_into_a_fresh_copy() {
    let saved = save_and_reopen(&fill_fixture());
    let model = read_acroform(&saved).expect("read filled AcroForm");
    let xfdf = export_xfdf(&model);
    assert!(xfdf.contains("name=\"text-field\""));
    assert!(xfdf.contains("<value>filled by test</value>"));
    assert!(xfdf.contains("<value>On</value>"));
    assert!(xfdf.contains("<value>three</value>"));

    let mut fresh = load();
    let applied = import_xfdf(&mut fresh, &xfdf).expect("import XFDF");
    assert_eq!(applied, 3);
    let reopened = save_and_reopen(&fresh);
    let imported = read_acroform(&reopened).expect("read imported AcroForm");

    assert_eq!(
        imported.field("text-field").expect("text").value,
        FieldValue::Text("filled by test".to_owned())
    );
    assert_eq!(
        imported.field("checkbox-field").expect("checkbox").value,
        FieldValue::Bool(true)
    );
    assert_eq!(
        imported.field("choice-field").expect("choice").value,
        FieldValue::Choice(vec!["three".to_owned()])
    );
}

#[test]
fn flatten_paints_widgets_removes_acroform_and_keeps_page_text() {
    let mut doc = save_and_reopen(&fill_fixture());
    let before = doc.page_text(0).expect("page text before flatten");
    assert!(before.contains("Form page"));

    let report = flatten(&mut doc).expect("flatten");
    assert_eq!(report.fields, 3);
    assert_eq!(report.widgets, 3);
    assert_eq!(report.pages, 1);

    let flattened = save_and_reopen(&doc);
    let model = read_acroform(&flattened).expect("read flattened AcroForm");
    assert!(!model.present);
    assert!(model.fields.is_empty());

    let after = flattened.page_text(0).expect("page text after flatten");
    assert!(after.contains("Form page"));

    let page = flattened.page(0).expect("page");
    let page_dictionary = dictionary(&flattened, page.id);
    let annots = page_dictionary
        .get("Annots")
        .and_then(Object::as_array)
        .expect("Annots array");
    assert!(annots.is_empty(), "widget annotations must be removed");

    let contents = page_dictionary
        .get("Contents")
        .and_then(Object::as_array)
        .expect("appended contents array");
    assert_eq!(contents.len(), 2);
    let paint_id = contents[1]
        .as_reference()
        .expect("new content stream reference");
    let (_, paint) = stream(&flattened, paint_id);
    assert!(contains(&paint, b"/Fm0 Do"));
    assert!(contains(&paint, b"/Fm2 Do"));
}

#[test]
fn radio_group_fills_widget_states_on_a_derived_fixture() {
    let mut doc = load();
    add_radio_group(&mut doc);
    let mut doc = save_and_reopen(&doc);

    let model = read_acroform(&doc).expect("read radio variant");
    let radio = model.field("radio-field").expect("radio field");
    assert_eq!(radio.kind, FormFieldKind::Radio);
    assert_eq!(radio.widgets.len(), 2);
    assert_eq!(radio.widgets[0].on_states, vec!["A".to_owned()]);
    assert_eq!(radio.widgets[1].on_states, vec!["B".to_owned()]);
    assert_eq!(radio.value, FieldValue::Choice(vec!["A".to_owned()]));

    set_radio(&mut doc, "radio-field", "B").expect("select B");
    let saved = save_and_reopen(&doc);
    let model = read_acroform(&saved).expect("read filled radio variant");
    let radio = model.field("radio-field").expect("radio field");
    assert_eq!(radio.value, FieldValue::Choice(vec!["B".to_owned()]));

    let widget_a = radio.widgets[0].object.expect("widget A");
    let widget_b = radio.widgets[1].object.expect("widget B");
    let dictionary_a = dictionary(&saved, widget_a);
    let dictionary_b = dictionary(&saved, widget_b);
    assert_eq!(
        dictionary_a.get("AS").and_then(Object::as_name),
        Some("Off")
    );
    assert_eq!(dictionary_b.get("AS").and_then(Object::as_name), Some("B"));

    for widget in [widget_a, widget_b] {
        let dictionary = dictionary(&saved, widget);
        let states = dictionary
            .get("AP")
            .and_then(Object::as_dictionary)
            .and_then(|ap| ap.get("N"))
            .and_then(Object::as_dictionary)
            .expect("radio state dictionary");
        assert!(states.contains_key("Off"));
        for state in states.values() {
            let id = state.as_reference().expect("state stream reference");
            let (stream_dictionary, data) = stream(&saved, id);
            assert_eq!(bbox(&stream_dictionary), vec![0.0, 0.0, 20.0, 20.0]);
            assert!(!data.is_empty());
        }
    }
}

#[test]
fn need_appearances_is_detected_and_repair_clears_it() {
    let mut doc = load();
    set_need_appearances(&mut doc, true);
    let mut doc = save_and_reopen(&doc);
    assert!(needs_appearances(&doc).expect("read flag"));

    let regenerated = regenerate_appearances(&mut doc).expect("regenerate");
    assert_eq!(regenerated, 3);
    let saved = save_and_reopen(&doc);
    assert!(!needs_appearances(&saved).expect("read flag"));

    let model = read_acroform(&saved).expect("read regenerated form");
    for field in model.fields() {
        let widget = field.widgets[0].object.expect("widget id");
        let dictionary = dictionary(&saved, widget);
        assert!(
            dictionary.contains_key("AP"),
            "field '{}' must have /AP",
            field.name
        );
    }
}

#[test]
fn invalid_requests_return_typed_errors() {
    let mut doc = load();
    assert!(matches!(
        set_text(&mut doc, "missing-field", "x"),
        Err(AcroFormError::UnknownField(_))
    ));
    assert!(matches!(
        set_text(&mut doc, "checkbox-field", "x"),
        Err(AcroFormError::WrongFieldKind { .. })
    ));
    assert!(matches!(
        set_choice(&mut doc, "choice-field", Some("not-an-option")),
        Err(AcroFormError::ValueNotAllowed { .. })
    ));
}

fn set_need_appearances(doc: &mut Document, value: bool) {
    let acroform_id = ObjectId::new(6, 0);
    let mut acroform = dictionary(doc, acroform_id);
    acroform.insert("NeedAppearances".to_owned(), Object::Boolean(value));
    doc.set_object(acroform_id, Object::Dictionary(acroform))
        .expect("stage AcroForm");
}

fn empty_appearance_stream() -> Object {
    let mut dictionary = Dictionary::new();
    dictionary.insert("Type".to_owned(), Object::Name("XObject".to_owned()));
    dictionary.insert("Subtype".to_owned(), Object::Name("Form".to_owned()));
    dictionary.insert("FormType".to_owned(), Object::Integer(1));
    dictionary.insert(
        "BBox".to_owned(),
        Object::Array(vec![
            Object::Integer(0),
            Object::Integer(0),
            Object::Integer(20),
            Object::Integer(20),
        ]),
    );
    Object::Stream {
        dictionary,
        data: Vec::new(),
    }
}

fn radio_widget(parent: ObjectId, rect: [i64; 4], on_state: &str, on: u32, off: u32) -> Object {
    let mut states = Dictionary::new();
    states.insert("Off".to_owned(), Object::Reference(ObjectId::new(off, 0)));
    states.insert(on_state.to_owned(), Object::Reference(ObjectId::new(on, 0)));
    let mut appearance = Dictionary::new();
    appearance.insert("N".to_owned(), Object::Dictionary(states));
    let mut widget = Dictionary::new();
    widget.insert("Type".to_owned(), Object::Name("Annot".to_owned()));
    widget.insert("Subtype".to_owned(), Object::Name("Widget".to_owned()));
    widget.insert("Parent".to_owned(), Object::Reference(parent));
    widget.insert(
        "Rect".to_owned(),
        Object::Array(rect.iter().copied().map(Object::Integer).collect()),
    );
    widget.insert("P".to_owned(), Object::Reference(ObjectId::new(3, 0)));
    widget.insert("F".to_owned(), Object::Integer(4));
    widget.insert("AP".to_owned(), Object::Dictionary(appearance));
    widget.insert(
        "AS".to_owned(),
        Object::Name(if on_state == "A" { "A" } else { "Off" }.to_owned()),
    );
    Object::Dictionary(widget)
}

fn add_radio_group(doc: &mut Document) {
    let field_id = ObjectId::new(10, 0);
    let widget_a = ObjectId::new(11, 0);
    let widget_b = ObjectId::new(12, 0);

    doc.set_object(
        widget_a,
        radio_widget(field_id, [300, 660, 320, 680], "A", 13, 14),
    )
    .expect("stage widget A");
    doc.set_object(
        widget_b,
        radio_widget(field_id, [330, 660, 350, 680], "B", 15, 16),
    )
    .expect("stage widget B");
    for id in [13_u32, 14, 15, 16] {
        doc.set_object(ObjectId::new(id, 0), empty_appearance_stream())
            .expect("stage empty appearance");
    }

    let mut field = Dictionary::new();
    field.insert("FT".to_owned(), Object::Name("Btn".to_owned()));
    field.insert("T".to_owned(), Object::String(b"radio-field".to_vec()));
    field.insert("Ff".to_owned(), Object::Integer(1 << 15));
    field.insert("V".to_owned(), Object::Name("A".to_owned()));
    field.insert("DA".to_owned(), Object::String(b"/Helv 0 Tf 0 g".to_vec()));
    field.insert(
        "Kids".to_owned(),
        Object::Array(vec![
            Object::Reference(widget_a),
            Object::Reference(widget_b),
        ]),
    );
    doc.set_object(field_id, Object::Dictionary(field))
        .expect("stage radio field");

    let page_id = ObjectId::new(3, 0);
    let mut page = dictionary(doc, page_id);
    let mut annots = page
        .get("Annots")
        .and_then(Object::as_array)
        .expect("page Annots")
        .to_vec();
    annots.push(Object::Reference(widget_a));
    annots.push(Object::Reference(widget_b));
    page.insert("Annots".to_owned(), Object::Array(annots));
    doc.set_object(page_id, Object::Dictionary(page))
        .expect("stage page");

    let acroform_id = ObjectId::new(6, 0);
    let mut acroform = dictionary(doc, acroform_id);
    let mut fields = acroform
        .get("Fields")
        .and_then(Object::as_array)
        .expect("AcroForm Fields")
        .to_vec();
    fields.push(Object::Reference(field_id));
    acroform.insert("Fields".to_owned(), Object::Array(fields));
    doc.set_object(acroform_id, Object::Dictionary(acroform))
        .expect("stage AcroForm");
}
