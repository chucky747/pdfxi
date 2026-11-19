# Authoring

Authoring covers creating PDFs, arranging pages, and adding page furniture. Most
of this milestone's work is library-level: the crates expose deterministic,
copy-on-write APIs, while desktop shell and CLI exposure land per row. Check the
[feature matrix](../../FEATURES.md) before assuming a feature has a user-facing
command.

## Create a PDF

| Source | Row | Status | Library |
|---|---|---|---|
| Plain text and a documented Markdown subset | `CREATE-004` | in-progress | `pdfxi-convert::create` |
| PNG and JPEG images | `CREATE-003` | in-progress | `pdfxi-convert::create` |
| Merge existing files | `CREATE-009` | verified | `pdfxi-core::pages` |
| HTML, Office, scan, portfolios | `CREATE-005`–`CREATE-010` | planned | — |

Text creation renders ATX headings, paragraphs, bullet lists, and fenced code
blocks with Helvetica/Courier metrics and paginates across pages. Inline
emphasis, links, tables, and full CommonMark are emitted literally today. Image
creation supports non-interlaced 8-bit grayscale/RGB PNG (no alpha or palette)
and 8-bit JPEG with one or three components; DPI and page-fit rules are
documented in `crates/pdfxi-convert/src/create.rs`.

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
