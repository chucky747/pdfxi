# Fill & Sign

**Fill & Sign** fills existing AcroForm fields in the open document. It does not
create new fields, and it does not apply a certificate signature — that is the
separate [Signing](signing.md) card.

The form model and its exchange formats are documented in [Forms](forms.md); this
page is the shell workflow.

## Fillable field types

| Field type | Interaction |
|---|---|
| Text | Type into the field; commit with <kbd>Enter</kbd> or by leaving the field |
| Checkbox | Toggle with <kbd>Space</kbd> or a click |
| Radio | Choose one button in the group |
| Choice | Open the choices popup, pick a value, or type in an editable choice |

Field-level read-only flags are respected. A read-only document, or a document
whose permissions deny form filling, does not accept edits.

## Keyboard

| Key | Action |
|---|---|
| <kbd>Tab</kbd> | Next field |
| <kbd>Shift</kbd>+<kbd>Tab</kbd> | Previous field |
| <kbd>Enter</kbd> | Commit the current text field |
| <kbd>Space</kbd> | Toggle a checkbox or radio button |
| <kbd>Escape</kbd> | Close the choices popup / cancel the current edit |

Tab order follows the field order in the document. It does not reorder fields or
write a new `/Tabs` structure.

## Reset

**Reset** returns the form to its unmodified state for this session. It is not a
save, and it does not revert a file that was already written: after a save, Reset
starts from the values now in the session, not from the file's original revision.

## Flatten (confirmed, irreversible)

**Flatten** bakes the current field appearances into page content:

- each widget's appearance is placed on the page;
- the widget annotations are removed;
- the `/AcroForm` dictionary is removed when at least one widget was flattened;
- original page content and non-widget annotations are preserved.

Flattening is **irreversible** in the viewer: after it, the fields are no longer
editable. The action therefore asks for confirmation before it runs. Save
afterwards to write the flattened document; see
[Open, save, and print](open-save-print.md).

This is the document-level flatten described in [Forms](forms.md). Writing a
regenerated `/AP` back into every concrete `/AcroForm` is still in progress, so
do not assume every reader will display an identical appearance.

## XFDF and FDF exchange

You can export the form values to XFDF or FDF and import them back:

- **Export** writes the current field values to the chosen exchange file.
- **Import** applies values from an XFDF or FDF file to the open form.
- Both directions are capped at **8 MiB**. A larger file is rejected rather than
  partially read.

The accepted XFDF subset has no `<annots>` and no rich-text markup, and import
needs the document's form model for type mapping. Malformed XML, unknown fields,
and missing names are reported as typed errors; the full list is in
[Forms](forms.md).

## Signature fields

A signature field is visible in the form but is **read-only** in Fill & Sign.
Clicking it does not create a signature. Use the certificate [Signing](signing.md)
card instead.

## Limits

- No field creation, deletion, or renaming.
- No JavaScript calculations or validation driven from this panel; the form
  model's calculation support is a library capability, not a Fill & Sign control.
- No flatten preview; the confirmation is the safety gate.
- No XFDF/annotation payloads and no 8 MiB+ exchange files.
- Import does not create fields that do not already exist in the document.

## Related pages

[Forms](forms.md) · [Signing and verification](signing.md) ·
[Open, save, and print](open-save-print.md) · [Security](security.md)
