# Export

The **Export** card converts the open PDF into eight non-PDF formats
([T-0244](../../../evidence/ui/T-0244.md)).

## Getting there

With a document open, **Tools - Export** or the Export command opens the card and
sets "Export: choose a format, then Export". With no document it explains that
one is needed and opens nothing.

## Targets

The "Convert this PDF to:" picker lists every implemented format:

| Group | Targets |
| --- | --- |
| Text | Text, HTML, RTF |
| Office | Word (`.docx`), Excel (`.xlsx`), PowerPoint (`.pptx`) |
| Images | PNG, JPEG |

JPEG exposes a quality slider (1 to 100); image targets use a DPI setting. An
Export button is disabled without a document, and progress, errors, and the last
export path are shown in the card.

## The export path

Choosing a format opens a native save dialog filtered to that extension and
pre-filled with the document's stem. Export writes one file, or one file per
page part named `<stem>-<part.name>.<ext>`, creates missing parent directories,
and reports "Exported N page(s) to `<path>`". Typed failures are shown as
"Export failed: ...".

## Office caveat

Office targets write files with a real Office container signature, but the app
does not re-open them in Office: Word/Excel/PowerPoint compatibility is recorded
as unsupported findings on the success status. The host's Word COM automation is
blocked, so "Office opens the result" is not re-checked
([T-0215 blocker](../../../../orchestration/blockers/T-0215-word-com.md)). PNG and JPEG are
produced through the app's PDFium feature.

## Limits

- Export is synchronous on the UI thread with no cancel.
- No page-range selection: every supported target exports all pages.
- The output is not opened automatically.

[Evidence: T-0244](../../../evidence/ui/T-0244.md) ·
[Convert to PDF](convert-to-pdf.md) · [Desktop viewer](../gui.md)
