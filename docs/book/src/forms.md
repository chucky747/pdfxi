# Forms

PDFXI models AcroForms in `pdfxi-forms` and keeps field values, appearances, and
exchange formats deterministic and testable. The viewer workflow that fills
existing fields, resets, flattens, and exchanges XFDF/FDF is
[Fill & Sign](fill-and-sign.md); this page documents the library underneath it.
Rows `FORMS-006` and `FORMS-007` are verified; the fill and appearance rows
remain in progress because writing a regenerated `/AP` back into a concrete
`/AcroForm` is not wired yet.

## Form model

`load_acroform` reads terminal fields and widget metadata into a `Form` of named
fields with types (text, checkbox, radio, choice, number, signature, pushbutton)
and values. Document-level `/DA` and `/Q` defaults are inherited. Field values
are type-checked and read-only fields are respected by `Form::set_value`.

- `FORMS-001` — fill existing AcroForm fields (in-progress):
  `build_appearance(field, widget, value)` renders a filled field's current value.
- `FORMS-002` — create field appearances (in-progress): deterministic `/AP`
  state dictionaries for text, checkbox, radio, choice, and pushbutton widgets.
- `FORMS-003` — properties, calculations, validation (in-progress): side-effect
  free `sum`/`product`/`average` evaluation, deterministic rounding, and
  `run_calculations` applying an ordered batch; non-numeric input is rejected
  without JavaScript coercion.
- `FORMS-010` — signature fields (in-progress): signature fields load as
  `FieldType::Signature` with a text signing label; cryptographic creation and
  validation belong to [Security](security.md).

## XFDF and FDF exchange

`FORMS-006` (verified) covers `export_fdf`, `import_fdf`, `export_xfdf`,
`parse_xfdf`, and `import_xfdf`. XFDF import uses a bounded, dependency-free XML
reader with type-aware value mapping against a `Form`.

- Text, checkbox, radio, choice, number, and signature values round-trip.
- XML special characters and numeric character references are escaped and
  decoded; hierarchical field names and both default and prefixed namespaces
  are accepted.
- Malformed XML, unknown entities or namespaces, foreign-prefixed elements,
  unknown fields, and missing names produce typed `XfdfError` variants.
- Scope: the XFDF subset has no `<annots>` or rich-text markup and requires a
  `Form` for type mapping.

Import into a fill session is supported: `xfdf_imported_document_applies_to_a_fill_session`
is one of the passing tests. Full evidence is in
`../../evidence/forms/T-0060.md`.

## Flatten

`FORMS-007` (verified) implements `flatten_page` and `flatten_document` over a
documented adapter model:

- A stored or generated appearance is placed on the page with a `q … cm … Q`
  translation and appended to page content.
- Widget annotations are removed; non-widget annotations and original content
  are preserved byte-for-byte.
- `AppearancePolicy` selects `PreferStored`, `Generate`, or `StoredOnly`, with a
  generated fallback. Stateful appearances resolve `/AS`, then the value-derived
  state.
- The `/AcroForm` dictionary is removed only when at least one widget was
  actually flattened; a document with no widgets is a no-op.
- Arbitrary `/Matrix` and `/Rotation` placement still needs a host adapter (see
  the row notes in the matrix).

`EDIT-012` (flatten annotations and form fields as a user-visible editing
operation) remains planned.

## Related pages

[Getting started](getting-started.md) · [Fill & Sign](fill-and-sign.md) ·
[Authoring](authoring.md) · [Security](security.md) ·
[Feature matrix](../../FEATURES.md)
