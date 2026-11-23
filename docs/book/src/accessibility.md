# Accessibility

Accessibility support is concentrated in `pdfxi-accessibility`, with tagged-PDF
structure writing in `structure.rs` and structured-text export in
`pdfxi-convert`. The data model and writers are implemented and tested; editor UI
and screen-reader integration are later rows. Verify each row in the
[feature matrix](../../FEATURES.md) before treating it as user-facing.

## Tags and reading order

`ACCESS-001` (in-progress): `pdfxi-accessibility::tree` defines `TagRole`
semantic roles (about thirty variants) and a validated `ReadingOrder` tree.
`reading_order.rs` rejects duplicate ids, unknown children, multiple parents,
and cycles (via Kahn traversal), then walks the tree deterministically in
depth-first order. `builder.rs` infers an ordered document → page → paragraph
tree from text blocks and assigns heading roles by font size.

`STD-006` (tagged PDF and structure tree, in-progress) covers structure-tree
serialization: `write_structure_tree` / `read_structure_tree`,
`compose_page_content`, `role_map`, `PageTagInfo`, and `ViewerPreferences`.
Tests confirm that a round trip reconstructs the original tree, figure alt text
is written, MCIDs are unique and in traversal order, and `lang` and viewer
preferences reach the catalog. Serialization is deterministic and names are
escaped.

## Alt text and tables

`ACCESS-002` (in-progress): alt-text coverage is represented by `AltTextCoverage`
and figure/tag metadata in `pdfxi-accessibility::export`. `to_plain_text` and
`summarize` produce a stable textual view of a tag tree. A full alt-text and
table editor UI is not part of the current library delivery.

## Exporting accessible content

`EXPORT-007` (in-progress): `pdfxi-convert::tagged_text` consumes an `A11yTree`
and emits a stable `@role` structured outline plus a `[role]` plain-text
variant, with headings, paragraphs, lists, tables, figures, and optional alt
text. Producing tagged PDF itself belongs to the accessibility lane
(`STD-006`), not the converter.

## Checking PDF/UA

- `ACCESS-003` — Accessibility and PDF/UA checker: planned.
- `STD-009` — PDF/UA conformance: in-progress at the profile level; see
  [Standards and preflight](standards.md).
- `ACCESS-004`—`ACCESS-006` — reflow/high-contrast/keyboard navigation, read
  aloud, and screen-reader/`accesskit` integration: planned. The desktop shell
  keeps every tool on a keyboard path, but a screen-reader smoke test is not
  claimed yet.
- `ACCESS-007` (verified) — canvas text and choice form widgets advertise the
  AccessKit `SetValue` action and apply the requested value through the same
  typed commands as the keyboard editor. UI Automation can therefore fill a
  text field or select a choice without a mouse. Checkboxes and radios use the
  standard Toggle/Select patterns. The gate still has no real screen-reader
  session, so the broader `ACCESS-006` bridge claim stays `model`.

## Design rules

- Never mutate a caller's tag tree when computing a derived order or export.
- Deterministic ids and traversal order are part of the contract so two runs on
  the same input produce byte-identical output.
- Structure tree content must describe real page content: MCIDs written by the
  tag writer must match the content stream's marked-content sequence.

## Related pages

[Getting started](getting-started.md) · [Authoring](authoring.md) ·
[Standards and preflight](standards.md) · [Feature matrix](../../FEATURES.md)
