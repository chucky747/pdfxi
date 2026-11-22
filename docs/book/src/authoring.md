# Authoring

Authoring covers creating PDFs, arranging pages, and adding page furniture. Most
of this milestone's work is library-level: the crates expose deterministic,
copy-on-write APIs, while desktop shell and CLI exposure land per row. Check the
[feature matrix](../../FEATURES.md) before assuming a feature has a user-facing
command.

## Create a PDF

| Source | Row | Status | Library | CLI |
|---|---|---|---|---|
| Plain text and a documented Markdown subset | `CREATE-004` | verified | `pdfxi-convert::create` | `create text` |
| PNG and JPEG images | `CREATE-003` | verified | `pdfxi-convert::create` | `create image` |
| PostScript and EPS (bounded subset) | `CREATE-013` | verified | `pdfxi-convert::postscript_ingest` + `postscript_pdf` | `create postscript` |
| Office intake bundles (Word/Excel/PowerPoint) | `CREATE-006` | verified | `pdfxi-convert::office_contract` + `office_import` | `create office` |
| Scanned PNG pages (simplex/duplex) | `CREATE-007` | verified | `pdfxi-convert::scan_capture` | `create scan` |
| OCR words over a scanned image | `CREATE-008` | verified | `pdfxi-convert::ocr_pipeline` | `create ocr` |
| Merge existing files | `CREATE-009` | verified | `pdfxi-core::pages` | — |
| Local HTML and a CSS subset | `CREATE-005` | verified | `pdfxi-convert::create::html` | — |
| Office, scan, portfolios | `CREATE-006`–`CREATE-010` | planned | — | — |

Text creation renders ATX headings, paragraphs, bullet lists, and fenced code
blocks with Helvetica/Courier metrics and paginates across pages. Inline
emphasis, links, tables, and full CommonMark are emitted literally today. Image
creation supports non-interlaced 8-bit grayscale/RGB PNG (no alpha or palette)
and 8-bit JPEG with one or three components; DPI and page-fit rules are
documented in `crates/pdfxi-convert/src/create.rs`.

### Create from the CLI

`CREATE-003` and `CREATE-004` are now exposed headlessly:

```text
pdfxi create image <INPUT.png|jpg|jpeg> <OUTPUT.pdf> [options]
pdfxi create text  <INPUT.txt|md|markdown> <OUTPUT.pdf> [options]
```

Office conversion is exposed as:

```text
pdfxi create office <BUNDLE.json> --out <OUTPUT.pdf> [--strict] [--json]
```

`BUNDLE.json` is the caller-extracted Office content:
`{"kind":"word","title":"Quarterly report","markup":"<h1>...</h1><p>...</p>"}`.
`kind` accepts `word`/`excel`/`powerpoint` (and the usual extensions). The command
validates the envelope, builds the deterministic `OfficeConversionPlan`,
imports the markup into the shared `OfficeDocument`, lays each model page out as
text in a same-size PDF page, merges pages in order, and reopens the output to
require every converted line to extract. `--strict` rejects a bundle with any
unsupported construct instead of reporting it. No OOXML archive or Office
process is opened; the extractor that produces the markup remains host-side.
Searchable OCR layers are exposed as:

```text
pdfxi create ocr <PAGE.png|jpg> --out <OUTPUT.pdf> --words <WORDS.json>
    [--dpi 300] [--language TAG] [--json]
```

`WORDS.json` is the caller-supplied recognition result for one page:
`{"language":"eng","words":[{"text":"Hello","x":10,"y":20,"width":40,"height":14,"confidence":95}]}`.
Boxes are top-left pixel coordinates. The command feeds them through the
`ocr_pipeline` model (confidence filtering, deterministic reading order,
top-left to bottom-left mapping), embeds the image at `pixels / dpi * 72` points,
and appends an invisible (`3 Tr`) text layer at the planned positions. It
reopens the output and requires every kept word to extract. No OCR engine or
process is bundled; any backend that can emit this JSON can drive it.
Scan assembly is exposed as:

```text
pdfxi create scan --out <OUTPUT.pdf> --scan <PAGE.png> [--scan <PAGE.png> ...]
    [--dpi 300] [--duplex simplex|front-first|back-first] [--json]
```

Each `--scan` is a real PNG page in scanner order. The command reads the IHDR
dimensions, describes every page to `ScanSource`, builds the deterministic
`ScanAssemblyPlan` (which reuses the print-capture plan), embeds each image with
`create_from_image` at the plan's exact page size, merges the pages in plan
order, and reopens the output to verify the page count and geometry. Duplex
modes pair consecutive images as front/back of each sheet and reject incomplete
pairs. The TWAIN/WIA device adapter is still the caller's responsibility; any
scanner that can emit PNG can drive this command.
PostScript import is exposed as:

```text
pdfxi create postscript <INPUT.ps|eps> <OUTPUT.pdf> [--strict] [--json]
```

The DSC header (`%%BoundingBox`, `%%Pages`, `%%Page`) and a bounded text, line
and axis-aligned rectangle subset are ingested and written as a real PDF with
standard Type 1 fonts. Constructs outside that subset (arcs, arbitrary paths,
images, transforms, unknown operators) are reported per page instead of being
guessed; `--strict` turns any finding into a typed error. The command reopens
its output, checks the page count and confirms every captured text run extracts.
This is not a PostScript interpreter.
Both accept `--page-size letter|a4`, `--margin <PT>`, `--font-size <PT>` and
`--line-height <FACTOR>`; `create text` also accepts `--markdown` to force the
Markdown parser regardless of extension. Output is deterministic — the same
input and options always produce byte-identical PDFs — and malformed or
unsupported input returns a typed, actionable error. The **image subset** is
non-interlaced 8-bit grayscale/RGB PNG (no alpha or palette) and 8-bit JPEG with
one or three components; interlaced/alpha/palette PNG, CMYK JPEG, and other
raster formats are rejected. The **Markdown subset** is ATX headings,
paragraphs, bullet lists, and fenced code blocks; inline emphasis, links,
tables, ordered lists, blockquotes, and full CommonMark are emitted literally.

### Export accessible text

`EXPORT-007` (verified) exposes tagged/accessible text export through
`pdfxi-convert::export_tagged_text` and the CLI:

```text
pdfxi export tagged <INPUT.pdf> <OUTPUT.txt> [--format structured|plain]
```

A deterministic accessibility tree is derived from extracted page text and
serialized as either a nested, indented `@role` outline (`structured`, the
default) or a flat `[role]` text variant (`plain`). Roles and reading order are
preserved, so the output carries document/page/paragraph structure and the
original text content. This is a **structured text representation**, not a
tagged-PDF file and not a PDF/UA certification; producing tagged-PDF structure
belongs to the accessibility lane.

`Document::apply_page_plan` with `PagePlan::merge(...)` copies every source into
a fresh catalog and page tree with deterministic object numbers and remapped
references. Sources that are encrypted or use xref streams are rejected.

## Page boxes and page operations

Row `EDIT-006` (verified) covers reorder, rotation, crop, and resize. Page plans
materialize inherited `/MediaBox`, `/Resources`, and `/Rotate` onto each page,
rebuild a single flat `/Pages` tree with a correct `/Count` and `/Kids`, and
preserve stream bytes. Rotation normalizes to `{0, 90, 180, 270}` and composes
with inherited `/Rotate`. `EDIT-005` (verified) covers insert, delete, extract,
and replace; `EDIT-007` (verified) covers splitting into independent chunks.

Bleed, crop, and printer-mark boxes for print production are tracked separately
as `PRINT-007` (in-progress). Page layout and view rotation in the viewer are
`VIEW-002` and `VIEW-004` (see the [viewer slice](../../features/view.md)).

## Headers, footers, and page numbering

`EDIT-008` (in-progress) provides a typed model in `pdfxi-edit::header_footer`:
token substitution, five page-number formats with a start offset and
prefix/suffix, odd/even and first-page suppression, six overlay slots, and
rotation/MediaBox-aware transform generation. Overlay streams are deterministic
and composed copy-on-write. There is no UI or CLI exposure yet, which is why the
row is still in progress.

## Watermarks and backgrounds

`EDIT-009` (in-progress) provides `pdfxi-edit::watermark`: text, vector, or image
marks with font/size/colour/opacity style, rotation, centre/tile/diagonal/
absolute placement, scale, page ranges, front/behind placement, and
first-page/odd-even suppression. Transparency uses an ExtGState alpha entry;
resource dictionaries are merged without dropping unknown keys. Again, this is
a library-level delivery with no UI or CLI yet.

## Optimize and compress

`EDIT-014` (verified) exposes `Document::optimize(&OptimizeOptions,
&CancellationToken)`. It compacts the object graph (deduplicate, remove
unreachable, renumber while preserving references), applies beneficial Flate
compression under a strict no-growth policy, passes JPEG/image streams through,
optionally removes metadata and optional entries, and regenerates a classic xref
table and `startxref`. The optimization report is deterministic and cancellation
is honored.

## Print

`VIEW-001` (in-progress) provides `pdfxi-render::print`: a validated
`PrintSettings` job model (all pages, a single page, inclusive ranges, or an
explicit list; copies; fit/actual/custom-percent scale; orientation; duplex;
named paper; margins; target DPI) and `parse_page_ranges` for expressions such
as `1-3,5,7-`. `PrintSpooler::spool` renders through `&dyn Renderer`, checks
cancellation, reports progress, and returns page rasters with a stable summary.
Handing those rasters to a platform printer is a later adapter, so the row stays
in progress. Image export (PNG/JPEG/TIFF) is verified as `EXPORT-006`.

## Related pages

[Getting started](getting-started.md) · [CLI reference](cli.md) ·
[Forms](forms.md) · [Standards and preflight](standards.md)
