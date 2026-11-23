//! Unit tests for the `/AcroForm` loader.
//!
//! The [`TestDocument`] builder is intentionally defined here, inside the
//! crate's test tree: it constructs dictionaries directly and depends on no
//! external fixture crate, proving that the loader works against the
//! dependency-free [`FormSource`] boundary.

use super::*;

fn dict(entries: &[(&str, PdfValue)]) -> PdfValue {
    PdfValue::Dictionary(
        entries
            .iter()
            .map(|(key, value)| ((*key).to_owned(), value.clone()))
            .collect(),
    )
}

fn array(items: Vec<PdfValue>) -> PdfValue {
    PdfValue::Array(items)
}

fn name(value: &str) -> PdfValue {
    PdfValue::Name(value.to_owned())
}

fn text(value: &str) -> PdfValue {
    PdfValue::Text(value.to_owned())
}

fn number(value: f64) -> PdfValue {
    PdfValue::Number(value)
}

fn boolean(value: bool) -> PdfValue {
    PdfValue::Bool(value)
}

fn reference(number: u32) -> PdfValue {
    PdfValue::Reference(ObjectRef::new(number, 0))
}

/// A tiny in-memory PDF document used as a [`FormSource`] test double.
#[derive(Debug)]
struct TestDocument {
    objects: BTreeMap<ObjectRef, PdfValue>,
    catalog: PdfValue,
}

impl TestDocument {
    /// Creates a catalog with no `/AcroForm` entry.
    fn new() -> Self {
        Self {
            objects: BTreeMap::new(),
            catalog: dict(&[("Type", name("Catalog"))]),
        }
    }

    /// Stores a direct object and returns its reference.
    fn put(&mut self, number: u32, value: PdfValue) -> ObjectRef {
        let id = ObjectRef::new(number, 0);
        self.objects.insert(id, value);
        id
    }

    /// Adds or replaces a catalog entry.
    fn catalog_entry(mut self, key: &str, value: PdfValue) -> Self {
        if let PdfValue::Dictionary(entries) = &mut self.catalog {
            entries.insert(key.to_owned(), value);
        }
        self
    }

    /// Installs an `/AcroForm` whose `/Fields` is `fields` at object `number`.
    fn with_fields(self, fields: Vec<PdfValue>, number: u32) -> Self {
        let mut document = self;
        document.put(
            number,
            dict(&[("Fields", array(fields)), ("DA", text("/Helv 12 Tf"))]),
        );
        document.catalog_entry("AcroForm", reference(number))
    }
}

impl FormSource for TestDocument {
    fn catalog(&self) -> PdfValue {
        self.catalog.clone()
    }

    fn resolve(&self, value: &PdfValue) -> Option<PdfValue> {
        match value {
            PdfValue::Reference(id) => self.objects.get(id).cloned(),
            other => Some(other.clone()),
        }
    }
}

#[test]
fn loads_text_checkbox_and_choice_fields() {
    let document = TestDocument::new().with_fields(
        vec![
            dict(&[
                ("T", text("name")),
                ("FT", name("Tx")),
                ("V", text("Ada")),
                ("MaxLen", number(20.0)),
            ]),
            dict(&[
                ("T", text("agree")),
                ("FT", name("Btn")),
                ("V", name("Yes")),
            ]),
            dict(&[
                ("T", text("color")),
                ("FT", name("Ch")),
                ("Opt", array(vec![text("Red"), text("Green")])),
                ("V", text("Green")),
            ]),
        ],
        10,
    );

    let AcroFormLoad::Loaded(loaded) = load_acroform(&document).expect("load") else {
        panic!("expected a form");
    };
    let form = &loaded.form;
    assert_eq!(form.len(), 3);
    assert_eq!(
        form.field("name").expect("name").field_type(),
        FieldType::Text
    );
    assert_eq!(form.value("name"), Ok(&FieldValue::Text("Ada".into())));
    assert_eq!(
        form.field("name").expect("name").constraint().max_len(),
        Some(20)
    );
    assert_eq!(
        form.field("agree").expect("agree").field_type(),
        FieldType::CheckBox
    );
    assert_eq!(form.value("agree"), Ok(&FieldValue::Bool(true)));
    assert_eq!(
        form.field("color").expect("color").field_type(),
        FieldType::Choice
    );
    assert_eq!(
        form.field("color").expect("color").choices(),
        &["Red", "Green"]
    );
    assert_eq!(
        form.value("color"),
        Ok(&FieldValue::Choice(vec!["Green".into()]))
    );
    assert_eq!(loaded.fields.len(), 3);
}

#[test]
fn inherits_field_attributes_from_ancestors_and_overrides_them() {
    let mut document = TestDocument::new();
    document.put(
        20,
        dict(&[
            ("FT", name("Tx")),
            ("Ff", number(2.0)),
            ("TU", text("Your name")),
            ("Q", number(2.0)),
            ("DA", text("/Helv 12 Tf")),
            ("MaxLen", number(30.0)),
            ("DV", text("default")),
            ("Kids", array(vec![reference(21), reference(22)])),
        ]),
    );
    document.put(21, dict(&[("T", text("first"))]));
    document.put(
        22,
        dict(&[
            ("T", text("second")),
            ("FT", name("Btn")),
            ("Ff", number((1 << 15) as f64)),
            ("Opt", array(vec![text("S"), text("M")])),
            ("V", text("M")),
        ]),
    );
    document.put(30, dict(&[("Fields", array(vec![reference(20)]))]));
    let document = document.catalog_entry("AcroForm", reference(30));

    let AcroFormLoad::Loaded(loaded) = load_acroform(&document).expect("load") else {
        panic!("expected a form");
    };
    let first = loaded.form.field("first").expect("first");
    assert_eq!(first.field_type(), FieldType::Text);
    assert!(first.is_required());
    assert_eq!(first.constraint().max_len(), Some(30));
    assert_eq!(first.value(), &FieldValue::Text("default".into()));
    assert_eq!(loaded.fields[0].tooltip.as_deref(), Some("Your name"));
    assert_eq!(loaded.fields[0].quadding, Some(2));
    assert_eq!(
        loaded.fields[0].default_appearance.as_deref(),
        Some("/Helv 12 Tf")
    );

    let second = loaded.form.field("second").expect("second");
    assert_eq!(second.field_type(), FieldType::Radio);
    assert_eq!(second.choices(), &["S", "M"]);
    assert_eq!(second.value(), &FieldValue::Choice(vec!["M".to_owned()]));
    assert_eq!(loaded.fields[1].tooltip.as_deref(), Some("Your name"));
}

#[test]
fn nested_kids_build_dotted_qualified_names() {
    let mut document = TestDocument::new();
    document.put(
        40,
        dict(&[("T", text("order")), ("Kids", array(vec![reference(41)]))]),
    );
    document.put(
        41,
        dict(&[("T", text("address")), ("Kids", array(vec![reference(42)]))]),
    );
    document.put(
        42,
        dict(&[
            ("T", text("city")),
            ("FT", name("Tx")),
            ("V", text("Paris")),
        ]),
    );
    document.put(43, dict(&[("Fields", array(vec![reference(40)]))]));
    let document = document.catalog_entry("AcroForm", reference(43));

    let AcroFormLoad::Loaded(loaded) = load_acroform(&document).expect("load") else {
        panic!("expected a form");
    };
    assert_eq!(loaded.form.len(), 1);
    assert_eq!(
        loaded.form.value("order.address.city"),
        Ok(&FieldValue::Text("Paris".into()))
    );
    assert!(loaded.form.field("order").is_none());
    assert!(loaded.form.field("order.address").is_none());
    assert_eq!(loaded.fields[0].partial_name.as_deref(), Some("city"));
}

#[test]
fn preserves_unknown_field_type_as_opaque() {
    let document = TestDocument::new().with_fields(
        vec![
            dict(&[
                ("T", text("meta")),
                ("FT", name("Custom#20Type")),
                ("V", text("payload")),
            ]),
            dict(&[("T", text("untyped")), ("V", boolean(true))]),
        ],
        11,
    );

    let form = load_acroform(&document)
        .expect("load")
        .into_form()
        .expect("form");
    assert_eq!(
        form.field("meta").expect("meta").field_type(),
        FieldType::Unknown
    );
    assert_eq!(form.value("meta"), Ok(&FieldValue::Text("payload".into())));
    assert_eq!(
        form.field("untyped").expect("untyped").field_type(),
        FieldType::Unknown
    );
    assert_eq!(form.value("untyped"), Ok(&FieldValue::Bool(true)));
}

#[test]
fn missing_acroform_returns_no_form() {
    let absent = TestDocument::new();
    assert!(load_acroform(&absent).expect("load").is_no_form());

    let null = TestDocument::new().catalog_entry("AcroForm", PdfValue::Null);
    assert!(load_acroform(&null).expect("load").is_no_form());
}

#[test]
fn cyclic_kids_is_an_error() {
    let mut document = TestDocument::new();
    document.put(
        1,
        dict(&[
            ("T", text("a")),
            ("FT", name("Tx")),
            ("Kids", array(vec![reference(2)])),
        ]),
    );
    document.put(
        2,
        dict(&[
            ("T", text("b")),
            ("FT", name("Tx")),
            ("Kids", array(vec![reference(1)])),
        ]),
    );
    document.put(3, dict(&[("Fields", array(vec![reference(1)]))]));
    let document = document.catalog_entry("AcroForm", reference(3));

    assert!(matches!(
        load_acroform(&document),
        Err(LoadError::Cycle { id, .. }) if id == ObjectRef::new(1, 0)
    ));
}

#[test]
fn duplicate_fully_qualified_names_is_an_error() {
    let document = TestDocument::new().with_fields(
        vec![
            dict(&[("T", text("same")), ("FT", name("Tx"))]),
            dict(&[("T", text("same")), ("FT", name("Tx"))]),
        ],
        12,
    );

    assert_eq!(
        load_acroform(&document).err(),
        Some(LoadError::DuplicateFieldName("same".to_owned()))
    );
}

#[test]
fn loaded_values_flow_into_fill_session() {
    let document = TestDocument::new().with_fields(
        vec![
            dict(&[("T", text("a")), ("FT", name("Tx")), ("V", text("old"))]),
            dict(&[("T", text("b")), ("FT", name("Btn")), ("V", name("Off"))]),
        ],
        13,
    );
    let form = load_acroform(&document)
        .expect("load")
        .into_form()
        .expect("form");
    let mut fill = crate::FormFill::new(form);

    fill.set("a", FieldValue::Text("new".into()))
        .expect("write a");
    assert_eq!(fill.value("a"), Ok(&FieldValue::Text("new".into())));
    assert_eq!(fill.value("b"), Ok(&FieldValue::Bool(false)));
    let report = fill.changes();
    assert_eq!(report.names().collect::<Vec<_>>(), ["a"]);
    assert_eq!(report.entries()[0].old, FieldValue::Text("old".into()));
    assert_eq!(report.entries()[0].new, FieldValue::Text("new".into()));
}

#[test]
fn widget_kids_collapse_into_one_terminal_field() {
    let mut document = TestDocument::new();
    document.put(
        50,
        dict(&[
            ("T", text("size")),
            ("FT", name("Btn")),
            ("Ff", number((1 << 15) as f64)),
            ("V", name("M")),
            ("Opt", array(vec![text("S"), text("M")])),
            ("Kids", array(vec![reference(51), reference(52)])),
        ]),
    );
    document.put(
        51,
        dict(&[
            ("Subtype", name("Widget")),
            ("Rect", array(vec![number(0.0)])),
        ]),
    );
    document.put(
        52,
        dict(&[
            ("Subtype", name("Widget")),
            ("Rect", array(vec![number(0.0)])),
        ]),
    );
    document.put(53, dict(&[("Fields", array(vec![reference(50)]))]));
    let document = document.catalog_entry("AcroForm", reference(53));

    let form = load_acroform(&document)
        .expect("load")
        .into_form()
        .expect("form");
    assert_eq!(form.len(), 1);
    assert_eq!(
        form.field("size").expect("size").field_type(),
        FieldType::Radio
    );
    assert_eq!(form.field("size").expect("size").choices(), &["S", "M"]);
    assert_eq!(
        form.value("size"),
        Ok(&FieldValue::Choice(vec!["M".to_owned()]))
    );
}

#[test]
fn malformed_values_fall_back_to_empty() {
    let document = TestDocument::new().with_fields(
        vec![
            dict(&[
                ("T", text("bad_text")),
                ("FT", name("Tx")),
                ("V", array(vec![text("not a scalar")])),
            ]),
            dict(&[
                ("T", text("bad_check")),
                ("FT", name("Btn")),
                ("V", dict(&[("weird", boolean(true))])),
            ]),
            dict(&[
                ("T", text("bad_choice")),
                ("FT", name("Ch")),
                ("Opt", number(7.0)),
                ("V", boolean(true)),
            ]),
        ],
        14,
    );

    let form = load_acroform(&document)
        .expect("load")
        .into_form()
        .expect("form");
    assert_eq!(form.value("bad_text"), Ok(&FieldValue::Empty));
    assert_eq!(form.value("bad_check"), Ok(&FieldValue::Empty));
    assert_eq!(form.value("bad_choice"), Ok(&FieldValue::Empty));
    assert!(
        form.field("bad_choice")
            .expect("bad_choice")
            .choices()
            .is_empty()
    );
}

#[test]
fn handles_signature_and_pushbutton_fields() {
    let document = TestDocument::new().with_fields(
        vec![
            dict(&[("T", text("sign")), ("FT", name("Sig")), ("V", text("Ada"))]),
            dict(&[
                ("T", text("press")),
                ("FT", name("Btn")),
                ("Ff", number((1 << 16) as f64)),
                ("V", text("ignored")),
            ]),
        ],
        15,
    );

    let form = load_acroform(&document)
        .expect("load")
        .into_form()
        .expect("form");
    assert_eq!(
        form.field("sign").expect("sign").field_type(),
        FieldType::Signature
    );
    assert_eq!(form.value("sign"), Ok(&FieldValue::Text("Ada".into())));
    assert_eq!(
        form.field("press").expect("press").field_type(),
        FieldType::Button
    );
    assert_eq!(form.value("press"), Ok(&FieldValue::Empty));
}

#[test]
fn resolves_reference_chains_and_root_defaults() {
    let mut document = TestDocument::new();
    document.put(
        60,
        dict(&[
            ("Fields", reference(61)),
            ("DA", text("/Helv 10 Tf")),
            ("Q", number(1.0)),
        ]),
    );
    document.put(61, array(vec![reference(62)]));
    document.put(
        62,
        dict(&[("T", text("deep")), ("FT", name("Tx")), ("V", text("ok"))]),
    );
    let document = document.catalog_entry("AcroForm", reference(60));

    let AcroFormLoad::Loaded(loaded) = load_acroform(&document).expect("load") else {
        panic!("expected a form");
    };
    assert_eq!(
        loaded.form.value("deep"),
        Ok(&FieldValue::Text("ok".into()))
    );
    assert_eq!(loaded.fields[0].quadding, Some(1));
    assert_eq!(
        loaded.fields[0].default_appearance.as_deref(),
        Some("/Helv 10 Tf")
    );
}

#[test]
fn non_dictionary_kids_entries_are_ignored() {
    let document = TestDocument::new().with_fields(
        vec![
            number(42.0),
            dict(&[("T", text("real")), ("FT", name("Tx"))]),
        ],
        16,
    );

    let form = load_acroform(&document)
        .expect("load")
        .into_form()
        .expect("form");
    assert_eq!(form.len(), 1);
    assert!(form.contains("real"));
}

#[test]
fn missing_fields_yields_an_empty_form() {
    let document =
        TestDocument::new().catalog_entry("AcroForm", dict(&[("NeedAppearances", boolean(true))]));

    let AcroFormLoad::Loaded(loaded) = load_acroform(&document).expect("load") else {
        panic!("expected a form");
    };
    assert!(loaded.form.is_empty());
    assert!(loaded.fields.is_empty());
}

#[test]
fn unresolved_structural_reference_is_an_error() {
    let document = TestDocument::new().with_fields(vec![reference(999)], 17);

    assert_eq!(
        load_acroform(&document).err(),
        Some(LoadError::UnresolvedReference(ObjectRef::new(999, 0)))
    );
}

#[test]
fn hex_escapes_in_names_decode() {
    let document = TestDocument::new().with_fields(
        vec![
            dict(&[("T", text("who")), ("FT", name("T#78")), ("V", text("Zoe"))]),
            dict(&[("T", text("other")), ("FT", name("F#6Frm"))]),
        ],
        18,
    );

    let AcroFormLoad::Loaded(loaded) = load_acroform(&document).expect("load") else {
        panic!("expected a form");
    };
    assert_eq!(
        loaded.form.field("who").expect("who").field_type(),
        FieldType::Text
    );
    assert_eq!(
        loaded.form.value("who"),
        Ok(&FieldValue::Text("Zoe".into()))
    );
    assert_eq!(
        loaded.form.field("other").expect("other").field_type(),
        FieldType::Unknown
    );
}
