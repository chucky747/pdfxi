use super::*;
use crate::appearance::{AppearanceStream, WidgetRect};
use crate::field::{Field, FieldType, FieldValue};
use crate::form::Form;
use std::collections::BTreeMap;

fn sample_form() -> Form {
    Form::from_fields([
        Field::new("name", FieldType::Text).with_value(FieldValue::Text("Café".into())),
        Field::new("agree", FieldType::CheckBox).with_value(FieldValue::Bool(true)),
        Field::new("color", FieldType::Choice)
            .with_choices(["red", "green"])
            .with_value(FieldValue::Choice(vec!["green".into()])),
    ])
    .unwrap()
}

fn widget(name: &str, x: f32, y: f32) -> PageAnnotation {
    PageAnnotation::Widget {
        field: name.to_owned(),
        appearance: WidgetAppearance::new(WidgetRect::new(x, y, 120.0, 80.0)),
        stored: None,
    }
}

fn link_annotation() -> PageAnnotation {
    PageAnnotation::Other {
        subtype: "Link".to_owned(),
        data: b"link-bytes".to_vec(),
    }
}

fn stored_stream(marker: &[u8]) -> StoredAppearance {
    StoredAppearance::new(
        NormalAppearance::Stream(AppearanceStream::new(
            WidgetRect::new(0.0, 0.0, 120.0, 80.0),
            marker.to_vec(),
        )),
        None,
    )
}

#[test]
fn flatten_text_appearance_is_present_and_positioned() {
    let page = FlatPage {
        content: b"BT\n(page) Tj\nET\n".to_vec(),
        annotations: vec![widget("name", 10.0, 20.0)],
    };
    let out = flatten_page(&page, &sample_form(), AppearancePolicy::Generate).unwrap();
    let text = String::from_utf8_lossy(&out.content);

    assert_eq!(out.flattened_widgets, 1);
    assert!(out.content.starts_with(b"BT\n(page) Tj\nET\n"));
    assert!(text.contains("1 0 0 1 10 20 cm"), "{text}");
    assert!(text.contains("Tj"), "{text}");
    assert!(text.trim_end().ends_with('Q'), "{text}");
}

#[test]
fn flatten_checkbox_active_state_changes_the_output() {
    let on_form = sample_form();
    let off_form = Form::from_fields([
        Field::new("name", FieldType::Text).with_value(FieldValue::Text("Café".into())),
        Field::new("agree", FieldType::CheckBox).with_value(FieldValue::Bool(false)),
        Field::new("color", FieldType::Choice).with_choices(["red", "green"]),
    ])
    .unwrap();
    let page = FlatPage {
        content: Vec::new(),
        annotations: vec![widget("agree", 0.0, 0.0)],
    };

    let on = flatten_page(&page, &on_form, AppearancePolicy::Generate).unwrap();
    let off = flatten_page(&page, &off_form, AppearancePolicy::Generate).unwrap();
    assert_ne!(on.content, off.content);
    assert!(String::from_utf8_lossy(&on.content).contains(" m\n"));
}

#[test]
fn flatten_stored_checkbox_uses_the_active_state() {
    let mut states = BTreeMap::new();
    states.insert(
        "Off".to_owned(),
        AppearanceStream::new(
            WidgetRect::new(0.0, 0.0, 120.0, 80.0),
            b"OFF_MARK\n".to_vec(),
        ),
    );
    states.insert(
        "On".to_owned(),
        AppearanceStream::new(
            WidgetRect::new(0.0, 0.0, 120.0, 80.0),
            b"ON_MARK\n".to_vec(),
        ),
    );
    let annotation = PageAnnotation::Widget {
        field: "agree".to_owned(),
        appearance: WidgetAppearance::new(WidgetRect::new(1.0, 2.0, 120.0, 80.0)),
        stored: Some(StoredAppearance::new(
            NormalAppearance::States(states),
            Some("On".to_owned()),
        )),
    };
    let page = FlatPage {
        content: Vec::new(),
        annotations: vec![annotation],
    };

    let out = flatten_page(&page, &sample_form(), AppearancePolicy::PreferStored).unwrap();
    let text = String::from_utf8_lossy(&out.content);
    assert!(text.contains("ON_MARK"), "{text}");
    assert!(!text.contains("OFF_MARK"), "{text}");
}

#[test]
fn flatten_choice_lists_the_selected_values() {
    let page = FlatPage {
        content: Vec::new(),
        annotations: vec![widget("color", 5.0, 5.0)],
    };
    let out = flatten_page(&page, &sample_form(), AppearancePolicy::Generate).unwrap();
    let text = String::from_utf8_lossy(&out.content);

    assert!(text.contains("red"), "{text}");
    assert!(text.contains("green"), "{text}");
}

#[test]
fn flatten_removes_widget_annotations() {
    let page = FlatPage {
        content: Vec::new(),
        annotations: vec![widget("name", 0.0, 0.0), link_annotation()],
    };
    let out = flatten_page(&page, &sample_form(), AppearancePolicy::Generate).unwrap();

    assert_eq!(out.flattened_widgets, 1);
    assert_eq!(out.annotations.len(), 1);
    assert!(matches!(
        &out.annotations[0],
        PageAnnotation::Other { subtype, .. } if subtype == "Link"
    ));
}

#[test]
fn flatten_preserves_non_widget_annotations_verbatim() {
    let link = link_annotation();
    let page = FlatPage {
        content: b"content".to_vec(),
        annotations: vec![link.clone()],
    };
    let out = flatten_page(&page, &Form::new(), AppearancePolicy::Generate).unwrap();

    assert_eq!(out.annotations, vec![link]);
    assert_eq!(out.content, b"content");
    assert!(!out.changed());
}

#[test]
fn flatten_is_deterministic() {
    let page = FlatPage {
        content: b"seed\n".to_vec(),
        annotations: vec![
            widget("name", 1.0, 2.0),
            widget("agree", 3.0, 4.0),
            link_annotation(),
        ],
    };
    let first = flatten_page(&page, &sample_form(), AppearancePolicy::Generate).unwrap();
    let second = flatten_page(&page, &sample_form(), AppearancePolicy::Generate).unwrap();

    assert_eq!(first, second);
    assert_eq!(first.content, second.content);
}

#[test]
fn flatten_unchanged_document_is_a_noop() {
    let link = link_annotation();
    let document = FlatDocument {
        pages: vec![FlatPage {
            content: b"only content".to_vec(),
            annotations: vec![link.clone()],
        }],
        has_acroform: false,
    };
    let out = flatten_document(&document, &Form::new(), AppearancePolicy::Generate).unwrap();

    assert!(!out.changed());
    assert!(!out.acroform_removed);
    assert_eq!(out.flattened_widgets, 0);
    assert_eq!(out.pages[0].content, b"only content");
    assert_eq!(out.pages[0].annotations, vec![link]);
}

#[test]
fn flatten_empty_acroform_is_not_removed() {
    let document = FlatDocument {
        pages: Vec::new(),
        has_acroform: true,
    };
    let out = flatten_document(&document, &Form::new(), AppearancePolicy::Generate).unwrap();
    assert!(!out.acroform_removed);
    assert!(!out.changed());
}

#[test]
fn flatten_keeps_acroform_when_no_widgets_were_flattened() {
    // The form is non-empty, but none of its widgets appear on the supplied
    // pages. Nothing may be flattened or removed, otherwise widgets that live
    // on other pages would be orphaned when /AcroForm disappears.
    let link = link_annotation();
    let document = FlatDocument {
        pages: vec![FlatPage {
            content: b"unchanged page content".to_vec(),
            annotations: vec![link.clone()],
        }],
        has_acroform: true,
    };

    let out = flatten_document(&document, &sample_form(), AppearancePolicy::Generate).unwrap();

    assert_eq!(out.flattened_widgets, 0);
    assert!(!out.acroform_removed);
    assert!(!out.changed());
    assert_eq!(out.pages.len(), 1);
    assert_eq!(out.pages[0].content, b"unchanged page content");
    assert_eq!(out.pages[0].annotations, vec![link]);
    assert_eq!(out.pages[0].flattened_widgets, 0);
}

#[test]
fn flatten_prefers_stored_and_falls_back_when_absent() {
    let mut with_stored = widget("name", 0.0, 0.0);
    if let PageAnnotation::Widget { stored, .. } = &mut with_stored {
        *stored = Some(stored_stream(b"STORED_MARK\n"));
    }
    let page = FlatPage {
        content: Vec::new(),
        annotations: vec![with_stored, widget("color", 0.0, 0.0)],
    };

    let out = flatten_page(&page, &sample_form(), AppearancePolicy::PreferStored).unwrap();
    let text = String::from_utf8_lossy(&out.content);
    assert_eq!(out.flattened_widgets, 2);
    assert!(text.contains("STORED_MARK"), "{text}");
    assert!(text.contains("red"), "{text}");
}

#[test]
fn flatten_generate_policy_ignores_stored_appearance() {
    let mut annotation = widget("name", 0.0, 0.0);
    if let PageAnnotation::Widget { stored, .. } = &mut annotation {
        *stored = Some(stored_stream(b"NEVER_DRAWN\n"));
    }
    let page = FlatPage {
        content: Vec::new(),
        annotations: vec![annotation],
    };
    let out = flatten_page(&page, &sample_form(), AppearancePolicy::Generate).unwrap();
    assert!(!String::from_utf8_lossy(&out.content).contains("NEVER_DRAWN"));
}

#[test]
fn flatten_stored_only_policy_uses_stored_appearance() {
    let mut annotation = widget("name", 7.0, 9.0);
    if let PageAnnotation::Widget { stored, .. } = &mut annotation {
        *stored = Some(stored_stream(b"STORED_ONLY\n"));
    }
    let page = FlatPage {
        content: Vec::new(),
        annotations: vec![annotation],
    };
    let out = flatten_page(&page, &sample_form(), AppearancePolicy::StoredOnly).unwrap();
    let text = String::from_utf8_lossy(&out.content);

    assert!(text.contains("STORED_ONLY"), "{text}");
    assert!(text.contains("1 0 0 1 7 9 cm"), "{text}");
}

#[test]
fn flatten_stored_only_policy_errors_without_an_appearance() {
    let page = FlatPage {
        content: Vec::new(),
        annotations: vec![widget("name", 0.0, 0.0)],
    };
    assert_eq!(
        flatten_page(&page, &sample_form(), AppearancePolicy::StoredOnly),
        Err(FlattenError::MissingStoredAppearance {
            field: "name".to_owned()
        })
    );
}

#[test]
fn flatten_rejects_unknown_fields() {
    let page = FlatPage {
        content: Vec::new(),
        annotations: vec![widget("missing", 0.0, 0.0)],
    };
    assert_eq!(
        flatten_page(&page, &sample_form(), AppearancePolicy::Generate),
        Err(FlattenError::UnknownField {
            name: "missing".to_owned()
        })
    );
}

#[test]
fn flatten_rejects_an_unresolvable_appearance_state() {
    let mut states = BTreeMap::new();
    states.insert(
        "A".to_owned(),
        AppearanceStream::new(WidgetRect::new(0.0, 0.0, 1.0, 1.0), b"A\n".to_vec()),
    );
    states.insert(
        "B".to_owned(),
        AppearanceStream::new(WidgetRect::new(0.0, 0.0, 1.0, 1.0), b"B\n".to_vec()),
    );
    let annotation = PageAnnotation::Widget {
        field: "name".to_owned(),
        appearance: WidgetAppearance::new(WidgetRect::new(0.0, 0.0, 120.0, 80.0)),
        stored: Some(StoredAppearance::new(
            NormalAppearance::States(states),
            Some("Z".to_owned()),
        )),
    };
    let page = FlatPage {
        content: Vec::new(),
        annotations: vec![annotation],
    };
    assert_eq!(
        flatten_page(&page, &sample_form(), AppearancePolicy::PreferStored),
        Err(FlattenError::MissingAppearanceState {
            field: "name".to_owned(),
            state: "Z".to_owned(),
        })
    );
}

#[test]
fn flatten_propagates_appearance_generation_errors() {
    let annotation = PageAnnotation::Widget {
        field: "name".to_owned(),
        appearance: WidgetAppearance::new(WidgetRect::new(0.0, 0.0, 0.0, 0.0)),
        stored: None,
    };
    let page = FlatPage {
        content: Vec::new(),
        annotations: vec![annotation],
    };
    assert!(matches!(
        flatten_page(&page, &sample_form(), AppearancePolicy::Generate),
        Err(FlattenError::Appearance { .. })
    ));
}

#[test]
fn flatten_document_counts_widgets_and_removes_the_acroform() {
    let document = FlatDocument {
        pages: vec![
            FlatPage {
                content: Vec::new(),
                annotations: vec![widget("name", 0.0, 0.0)],
            },
            FlatPage {
                content: Vec::new(),
                annotations: vec![widget("agree", 1.0, 1.0), link_annotation()],
            },
        ],
        has_acroform: true,
    };
    let out = flatten_document(&document, &sample_form(), AppearancePolicy::Generate).unwrap();

    assert!(out.changed());
    assert!(out.acroform_removed);
    assert_eq!(out.flattened_widgets, 2);
    assert_eq!(out.pages[0].flattened_widgets, 1);
    assert_eq!(out.pages[1].annotations.len(), 1);
}
