# Convert to PDF

The **Convert to PDF** window creates a new PDF from text, Markdown, HTML, and
PNG/JPEG images. It needs no open document: the command creates one
([T-0266](../../../evidence/ui/T-0266.md)).

## Getting there

**Tools - Convert to PDF** or the `ConvertToPdf` command opens the window and
sets "Convert to PDF: choose files, then Convert". Where the Tools row is
capability-gated, the command or the `--tool=convert` launch route reaches the
card.

## Sources

**Add files** uses a native multi-select filtered to `png`, `jpg`, `jpeg`, `txt`,
`md`, `html`, and `htm`. Each added file is read and classified immediately:

- UTF-8 `.txt` and `.md`;
- `.html` / `.htm`, using the file's parent directory as the base;
- `.png` and `.jpg` / `.jpeg` bytes.

Because sources are loaded at add time, the list you see is exactly what is
converted. Unknown or missing extensions are typed errors naming the supported
set. An unknown extension names the missing Office extractor.

## The window

- chosen files with a per-row **Remove**;
- **Choose output**, defaulting to the first source stem plus `.pdf`;
- an optional **Title** field;
- a fixed `Page size: Letter` line;
- **Convert**, enabled only with at least one source and an output.

Conversion writes the result, reports "Converted N file(s) to a PDF with M
page(s) at <path>", and records a PDF converted notification. Failures are
shown as "Convert failed: ..." and keep the source list.

## Office is unavailable

Office input (`.docx`, `.xlsx`, `.pptx`) is **not** offered: the app has no Office
extractor, so those extensions are absent from the picker rather than shown as
disabled ghosts. Loading one by another route reports a typed unsupported error.
The same caveat applies to exporting to Office from
[Export](export.md), where the container is written but compatibility is not
re-verified.

## Limits

- Page size is fixed to Letter; margins and font size are not exposed.
- Conversion is synchronous on the UI thread with no progress or cancel.
- The result is not opened automatically after conversion.
- Images with alpha are flattened by the converter's image path.

[Evidence: T-0266](../../../evidence/ui/T-0266.md) ·
[Export](export.md) · [Desktop viewer](../gui.md)
