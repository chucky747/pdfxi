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
page(s) at `<path>`", and records a PDF converted notification. Failures are
shown as "Convert failed: ..." and keep the source list.

## Office is unavailable

Office input (`.docx`, `.xlsx`, `.pptx`) is **not** offered: the app has no Office
extractor, so those extensions are absent from the picker rather than shown as
disabled ghosts. Loading one by another route reports a typed unsupported error.
The same caveat applies to exporting to Office from
[Export](export.md), where the container is written but compatibility is not
re-verified.

## Retired capture features

The Adobe browser-capture extension and mail-client capture are replaced by
local, file-based adapters (ADR 0008). No remote fetch, browser extension,
network client or active content is involved.

- **HTML and MHTML capture.** `pdfxi_convert::capture` renders caller-supplied
  HTML/MHTML-like markup through the bounded HTML subset. `pdfxi watch`
  converts `.html` and `.htm` files through `capture_html_markup`; the
  `capture_html_file` and `capture_mhtml` entry points remain library APIs.
- **Email to PDF.** `pdfxi_convert::email` parses a saved `.eml` file (folded
  headers, bounded MIME parts, a text/plain or text/html body) and renders the
  body to PDF. Attachment names are listed, never decoded or embedded. This is
  currently a library API; there is no CLI host yet.

Both rows (`CREATE-011`, `CREATE-012`) are final `replaced` states under
ADR 0008 and ADR 0009: a local equivalent is delivered instead of a network
integration, and the narrower scope is recorded here and in the row notes.

## Limits

- Page size is fixed to Letter; margins and font size are not exposed.
- Conversion is synchronous on the UI thread with no progress or cancel.
- The result is not opened automatically after conversion.
- Images with alpha are flattened by the converter's image path.

[Evidence: T-0266](../../../evidence/ui/T-0266.md) ·
[Export](export.md) · [Desktop viewer](../gui.md)
