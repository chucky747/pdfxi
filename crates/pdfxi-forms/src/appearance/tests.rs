use super::*;
use crate::field::{Field, FieldFlags, FieldType, FieldValue};

fn widget(width: f32, height: f32) -> WidgetAppearance {
    WidgetAppearance::new(WidgetRect::new(10.0, 20.0, width, height))
}

fn text_content(appearance: &AppearanceDictionary) -> String {
    String::from_utf8_lossy(&appearance.normal_stream().unwrap().content).into_owned()
}

#[test]
fn default_widget_is_valid() {
    assert!(widget(100.0, 40.0).validate().is_ok());
}

#[test]
fn zero_size_widget_is_rejected() {
    assert_eq!(
        widget(0.0, 40.0).validate(),
        Err(AppearanceError::ZeroSizedWidget {
            width: 0.0,
            height: 40.0
        })
    );
    assert!(matches!(
        widget(100.0, -1.0).validate(),
        Err(AppearanceError::ZeroSizedWidget { .. })
    ));
}

#[test]
fn non_finite_rect_is_rejected() {
    let mut appearance = widget(100.0, 40.0);
    appearance.rect.width = f32::NAN;
    assert_eq!(
        appearance.validate(),
        Err(AppearanceError::NonFiniteGeometry("rect"))
    );
}

#[test]
fn invalid_rotation_value_is_rejected() {
    assert_eq!(
        Rotation::try_from(45),
        Err(AppearanceError::InvalidRotation(45))
    );
    assert_eq!(Rotation::try_from(90), Ok(Rotation::Deg90));
    assert_eq!(Rotation::Deg270.degrees(), 270);
}

#[test]
fn escapes_literal_special_characters() {
    let escaped = escape_literal_string("a(b)c\\d");
    assert!(escaped.contains("\\("), "{escaped}");
    assert!(escaped.contains("\\)"), "{escaped}");
    assert!(escaped.contains("\\\\"), "{escaped}");
    assert_eq!(escape_literal_string("line\nnext"), "line\\nnext");
}

#[test]
fn escape_non_ascii_uses_utf16_bom() {
    let escaped = escape_literal_string("caf\u{e9}");
    assert!(escaped.starts_with("\\376\\377"), "{escaped}");
    assert!(escaped.contains("\\000"), "{escaped}");
}

#[test]
fn text_fits_without_clipping() {
    let appearance = widget(200.0, 40.0);
    let layout = layout_text("Hello", &appearance, false);
    assert_eq!(layout.lines.len(), 1);
    assert!(!layout.clipped);
    assert_eq!(layout.lines[0].text, "Hello");
}

#[test]
fn text_operators_are_well_formed() {
    let field = Field::new("name", FieldType::Text);
    let content = text_content(
        &build_appearance(&field, &widget(200.0, 40.0), &FieldValue::Text("Hi".into())).unwrap(),
    );
    assert!(content.contains("BT\n"), "{content}");
    assert!(content.contains("/F1 12 Tf\n"), "{content}");
    assert!(content.contains(" Td\n"), "{content}");
    assert!(content.contains("(Hi) Tj\n"), "{content}");
    assert!(content.ends_with("ET\n"), "{content}");
}

#[test]
fn text_overflow_clips_with_ellipsis() {
    let appearance = widget(60.0, 40.0);
    let layout = layout_text(
        "This is a very long single line of text that will not fit in the box",
        &appearance,
        false,
    );
    assert!(layout.clipped);
    assert_eq!(layout.lines.len(), 1);
    assert!(
        layout.lines[0].text.ends_with('\u{2026}'),
        "{:?}",
        layout.lines
    );

    let field = Field::new("long", FieldType::Text);
    let content = text_content(
        &build_appearance(
            &field,
            &appearance,
            &FieldValue::Text("This is a very long single line of text that will not fit".into()),
        )
        .unwrap(),
    );
    // The ellipsis is non-ASCII, so the whole literal is emitted as UTF-16BE
    // with a byte-order mark and octal-escaped bytes.
    assert!(content.contains("\\376\\377"), "{content}");
}

#[test]
fn multiline_wraps_into_multiple_lines() {
    let appearance = widget(80.0, 80.0);
    let layout = layout_text("alpha beta\ngamma delta epsilon zeta", &appearance, true);
    assert!(layout.lines.len() > 1, "{:?}", layout.lines);
    let first = &layout.lines[0];
    let second = &layout.lines[1];
    assert!(second.baseline_y < first.baseline_y);
}

#[test]
fn alignment_left_center_right_offsets() {
    let field = Field::new("aligned", FieldType::Text);
    let value = FieldValue::Text("Hi".into());
    let mut left = widget(120.0, 40.0);
    left.alignment = Alignment::Left;
    let mut center = left.clone();
    center.alignment = Alignment::Center;
    let mut right = left.clone();
    right.alignment = Alignment::Right;

    let left_x = layout_text("Hi", &left, false).lines[0].offset_x;
    let center_x = layout_text("Hi", &center, false).lines[0].offset_x;
    let right_x = layout_text("Hi", &right, false).lines[0].offset_x;
    assert!(
        left_x < center_x && center_x < right_x,
        "{left_x} {center_x} {right_x}"
    );

    // Alignment is also observable in the generated Td offset.
    let content = text_content(&build_appearance(&field, &left, &value).unwrap());
    let td = content
        .lines()
        .find(|line| line.ends_with(" Td"))
        .unwrap_or("");
    assert!(td.starts_with("3 "), "{td}");
    assert_eq!(Alignment::Center.quadding(), 1);
}

#[test]
fn checkbox_on_off_state_names() {
    let field = Field::new("agree", FieldType::CheckBox);
    assert_eq!(button_state_names(&field), ["Off", "On"]);

    let appearance = widget(30.0, 30.0);
    let on = build_appearance(&field, &appearance, &FieldValue::Bool(true)).unwrap();
    let off = build_appearance(&field, &appearance, &FieldValue::Bool(false)).unwrap();
    assert!(on.is_stateful());
    assert_eq!(on.active_state(), Some("On"));
    assert_eq!(off.active_state(), Some("Off"));
    let states = on.states().unwrap();
    assert_eq!(states.len(), 2);
    assert!(states.contains_key("On") && states.contains_key("Off"));

    let on_content = String::from_utf8_lossy(&states["On"].content).into_owned();
    let off_content = String::from_utf8_lossy(&states["Off"].content).into_owned();
    assert!(on_content.contains(" l\n"), "{on_content}");
    assert!(!off_content.contains(" l\n"), "{off_content}");
}

#[test]
fn radio_group_states() {
    let field = Field::new("size", FieldType::Radio).with_choices(["S", "M", "L"]);
    assert_eq!(button_state_names(&field), ["L", "M", "Off", "S"]);

    let appearance = widget(30.0, 30.0);
    let built =
        build_appearance(&field, &appearance, &FieldValue::Choice(vec!["M".into()])).unwrap();
    assert_eq!(built.active_state(), Some("M"));
    let states = built.states().unwrap();
    assert_eq!(states.len(), 4);
    assert!(states.contains_key("Off"));
    for name in ["S", "M", "L"] {
        assert!(states.contains_key(name));
    }
    let off = String::from_utf8_lossy(&states["Off"].content).into_owned();
    let on = String::from_utf8_lossy(&states["M"].content).into_owned();
    assert_ne!(off, on);
    assert!(on.contains("f\n"));
}

#[test]
fn button_state_names_empty_for_non_buttons() {
    let field = Field::new("f", FieldType::Text);
    assert!(button_state_names(&field).is_empty());
}

#[test]
fn choice_combo_renders_single_line() {
    let field = Field::new("color", FieldType::Choice)
        .with_flags(FieldFlags::COMBO)
        .with_choices(["red", "green", "blue"]);
    let built = build_appearance(
        &field,
        &widget(120.0, 40.0),
        &FieldValue::Choice(vec!["green".into()]),
    )
    .unwrap();
    assert!(!built.is_stateful());
    let content = text_content(&built);
    assert!(content.contains("(green) Tj"), "{content}");
    assert_eq!(content.matches(" Tj").count(), 1);
}

#[test]
fn choice_list_renders_highlight_and_rows() {
    let field = Field::new("color", FieldType::Choice).with_choices(["red", "green", "blue"]);
    let built = build_appearance(
        &field,
        &widget(120.0, 100.0),
        &FieldValue::Choice(vec!["green".into()]),
    )
    .unwrap();
    let content = text_content(&built);
    assert!(content.contains(" re f\n"), "{content}");
    assert!(content.contains("(green) Tj"), "{content}");
    assert!(content.matches(" Tj").count() >= 2, "{content}");
}

#[test]
fn empty_value_produces_valid_appearance() {
    let field = Field::new("empty", FieldType::Text);
    let built = build_appearance(&field, &widget(100.0, 40.0), &FieldValue::Empty).unwrap();
    let content = text_content(&built);
    assert!(content.contains("BT\n") && content.ends_with("ET\n"));
    assert!(!content.contains('\\'));

    // An unchecked checkbox with no value falls back to Off.
    let check = Field::new("check", FieldType::CheckBox);
    let built = build_appearance(&check, &widget(30.0, 30.0), &FieldValue::Empty).unwrap();
    assert_eq!(built.active_state(), Some("Off"));
}

#[test]
fn build_appearance_is_deterministic() {
    let text = Field::new("name", FieldType::Text);
    let radio = Field::new("size", FieldType::Radio).with_choices(["S", "M"]);
    let text_widget = widget(120.0, 40.0);
    let radio_widget = widget(30.0, 30.0);
    let text_value = FieldValue::Text("Ada".into());
    let radio_value = FieldValue::Choice(vec!["M".into()]);

    assert_eq!(
        build_appearance(&text, &text_widget, &text_value).unwrap(),
        build_appearance(&text, &text_widget, &text_value).unwrap()
    );
    assert_eq!(
        build_appearance(&radio, &radio_widget, &radio_value).unwrap(),
        build_appearance(&radio, &radio_widget, &radio_value).unwrap()
    );
}

#[test]
fn bbox_uses_widget_local_coordinates() {
    let field = Field::new("name", FieldType::Text);
    let built =
        build_appearance(&field, &widget(100.0, 40.0), &FieldValue::Text("x".into())).unwrap();
    assert_eq!(
        built.normal_stream().unwrap().bbox,
        WidgetRect::new(0.0, 0.0, 100.0, 40.0)
    );
}
