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
| PDF Portfolios from real files | `CREATE-010` | verified | `pdfxi-convert::portfolio_package` | `create portfolio` |
| Office integration round trip | `PLAT-003` | verified | `pdfxi-convert::office_integration` | `office plan` / `office convert` |
| Sandboxed Acrobat-compatible JavaScript | `AUTO-004` | verified | `pdfxi-automation::js` | `script run` |
| Watched-folder conversion | `CREATE-014` | verified | `pdfxi-automation::watched` + `pdfxi-convert` adapters | `watch` |
| Static form-field recognition | `FORMS-005` | verified | `pdfxi-forms::recognition` | `form recognize` |
| Font size/spacing/colour restyle | `EDIT-002` | verified | `pdfxi-edit::restyle_run_text` | `restyle` |
| Action Wizard create/edit/import/export | `AUTO-001` | verified | `pdfxi-automation::action_document` + real action operations | `action create` / `action run --execute` |
| Alt text and table editor | `ACCESS-002` | verified | tagged structure `/XI_NodeId` entries | `accessibility alt` / `accessibility table` |
| Tag tree and reading order editor | `ACCESS-001` | verified | tagged structure `/K` order | `accessibility order` / `accessibility move` |
| PostScript export (positioned text subset) | `EXPORT-009` | verified | `pdfxi-convert::postscript` + `pdfxi-edit::text_blocks` | `export postscript` |
| Transparency and blend modes | `STD-005` | verified | `pdfxi-core::transparency` | `transparency inspect` |
| Cross-file search | `MANAGE-003` | verified | `pdfxi-core` page text + `pdfxi-text` search | `find-files` |
| PDF Portfolio management | `MANAGE-004` | verified | `pdfxi-convert::portfolio_package` | `portfolio --out` with edit flags |
| Unicode CJK and RTL analysis | `PLAT-007` | verified | `pdfxi-core::unicode` | `unicode analyze` |
| Home recents, favorites, collections | `MANAGE-002` | verified | `pdfxi-app::home` | `home list/add/favorite/collection` |
| Read aloud with system speech | `ACCESS-005` | verified | `pdfxi-app::read_aloud` + Windows `System.Speech` | `read-aloud --voices` / `read-aloud --speak` |
| Reflow, high contrast, keyboard navigation | `ACCESS-004` | verified | `pdfxi-app::nav` | `accessibility nav` / `accessibility layout` |
| Screen reader tree and accesskit roles | `ACCESS-006` | verified | `pdfxi-app::a11y` | `accessibility tree` |
| Geospatial viewports and coordinate mapping | `MEDIA-004` | verified | `pdfxi-render::geospatial` | `geo info` / `geo to-pdf` / `geo to-geo` |
| U3D/PRC 3D annotations | `MEDIA-003` | verified | `pdfxi-render::three_d` | `media3d` / `--extract` |
| Interactive actions and triggers | `MEDIA-005` | verified | `pdfxi-render::actions` | `interactions [--strict]` |
| Audio and video annotations | `MEDIA-001` | verified | `pdfxi-render::media` | `media list [--extract]` |
| HTML5 media replacement for Flash | `MEDIA-002` | verified | `pdfxi-render::html5_media` | `media html5` |
| Output preview and separations | `PRINT-004` | verified | `pdfxi-app::print::SeparationsPlan` + content scan | `separations [--plate]` |
| Ink manager and overprint preview | `PRINT-005` | verified | `pdfxi-app::print` + `/ExtGState` scan | `ink` |
| ICC colour management and output intents | `PRINT-003` | verified | `pdfxi-render::color` | `color icc` / `color intents` / `color convert` |
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

The home state is exposed as:

```text
pdfxi home list --state STATE.txt [--json]
pdfxi home add FILE --state STATE.txt [--json]
pdfxi home favorite FILE [--remove] --state STATE.txt [--json]
pdfxi home collection NAME --state STATE.txt [--add FILE] [--remove FILE]
    [--delete] [--json]
```

`HomeModel` keeps bounded, deduplicated recents (most-recent-first), favorites
and named collections over normalized document keys. The CLI owns the state file
using the model's escaped newline format; it records paths and display titles,
and reports or edits the state without opening documents.
Unicode analysis is exposed as:

```text
pdfxi unicode analyze <FILE.pdf> [--font FAMILY=SCRIPTS ...] [--json]
pdfxi unicode analyze --text TEXT [--font ...] [--json]
```

The command extracts real page text (or uses `--text`), reports the dominant
script, base direction and logical bidi runs, and, when `--font` candidates are
supplied (`FAMILY=latin,cjk,hebrew,arabic,other`), builds the ordered fallback
chain per run through `pdfxi_core::unicode`. It performs no shaping,
rasterization or font I/O; a shaper consumes the plan.
Portfolio management is exposed as:

```text
pdfxi portfolio <FILE.pdf> --out <OUT.pdf>
    [--add LOCAL=NAME] [--rename OLD=NEW] [--describe NAME=TEXT]
    [--remove NAME] [--json]
```

The command loads every embedded entry and its bytes, applies additions,
renames/moves, description changes and removals in that order, validates the
resulting manifest, rebuilds the collection with `build_portfolio_package`, and
reopens it to compare every embedded stream with the source bytes. Listing and
`--extract` remain the default when no edit flag is given.
Cross-file search is exposed as:

```text
pdfxi find-files <PATH...> --query TEXT [--case-sensitive] [--whole-word]
    [--max-results N] [--max-per-file N] [--json]
```

Directories are scanned recursively for `.pdf` files (bounded to 10000 files and
32 levels); explicit files are searched as given. Each page's text is matched
with the shared `pdfxi-text` engine, grouped by file then page, and files rank
by match count with a path tie-break. Caps report `truncated`. This is a live
search, not a persisted index.
Transparency inspection is exposed as:

```text
pdfxi transparency inspect <FILE.pdf> [--json]
```

The command reads each page's real `/Group` and `/ExtGState` dictionaries,
parses `/BM` (name or first array entry), `/I`, `/K`, `/ca` and `/CA`, and
validates them through `pdfxi-core::transparency` (`BlendMode`, `BlendSpec`,
`TransparencyGroup`). It prints canonical descriptors such as
`Isolated|NonKnockout|Screen` and `Multiply ca=128 CA=256`. Unknown blend modes
are typed errors. Pixel compositing remains a render-backend responsibility.
PostScript export is exposed as:

```text
pdfxi export postscript <FILE.pdf> --out <OUT.ps> [--json]
```

Every non-empty text run from `pdfxi-edit`'s geometry-aware page model becomes a
`PsText` at its real page coordinates and font size, with the page media size as
the PostScript page. The exporter writes deterministic DSC PostScript
(`%!PS-Adobe-3.0`, bounding box, pages, fonts). Vector art, images and
annotations are not represented; `--json` lists them as omitted rather than
pretending to be a complete converter.
Tagged reading order is edited with:

```text
pdfxi accessibility order <FILE.pdf> [--json]
pdfxi accessibility move <FILE.pdf> --out <OUT.pdf> --node N [--before M] [--json]
```

`order` prints the pre-order reading order as `(node id, role)` rows.
`move` removes the node from its parent's `/K` array and reinserts it before the
named sibling, or at the end when `--before` is omitted; the output is reopened
and the new order must place the node before its target.
Tagged alt text and table scopes are edited with:

```text
pdfxi accessibility alt <FILE.pdf> --out <OUT.pdf> --node N
    (--text TEXT | --clear) [--json]
pdfxi accessibility table <FILE.pdf> --out <OUT.pdf> --node N
    [--scope row|column|both] [--json]
```

`--node` is the `/XI_NodeId` written by `pdfxi structure apply`. The command
walks the real `/StructTreeRoot` from the catalog, updates `/Alt` (and its
`/XI_Alt` mirror) or the `/Scope` of a `TH`/`TD` cell, saves, reopens and reads
the value back. Omitting `--scope` clears it.
Actions are created and executed as:

```text
pdfxi action create <FILE.act> --name NAME --step ID:KIND [--step ...]
    [--param ID:KEY=VALUE ...] [--description TEXT] [--force] [--json]

pdfxi action run <FILE.act> NAME --input FILE.pdf [--output-dir DIR]
    [--execute] [--json]
```

`action create` validates the step ids/kinds and parameters, then writes the
tab-delimited action file. `action run` without `--execute` is the staging mode
(no bytes written). With `--execute`, `import` requires a real PDF, `watermark`
stamps every page with the step text, and `export` writes the result beneath
`--output-dir`; security and custom-command steps are refused with a typed
message. `action list`/`action show` inspect the same files.
Text restyling is exposed as:

```text
pdfxi restyle <FILE.pdf> --out <OUT.pdf> [--page N] [--run N]
    [--size PT] [--scale PCT] [--char-spacing PT] [--word-spacing PT]
    [--color R,G,B] [--json]
```

The run selects its paragraph block. The whole block is re-emitted with the
requested `Tc`, `Tw`, `Tz`, `rg` and `Tf` values and reflowed at the block
width; other content operators are preserved and the font family is unchanged
(the existing font resource or the embedded fallback is reused). The output is
reopened and the target text must still extract. Supported edits are font size,
horizontal scaling, character/word spacing and RGB fill colour.
Static form recognition is exposed as:

```text
pdfxi form recognize <HINTS.json> --out <FORM.pdf> [--json]
```

`HINTS.json` lists caller-classified rectangles in page points:
`{"hints":[{"kind":"label","page":0,"x":40,"y":700,"width":80,"height":14,"text":"Name"},{"kind":"input","page":0,"x":130,"y":698,"width":200,"height":18}]}`.
`kind` is `label`, `input`, `checkbox`, `radio` or `choice`; an optional `name`
overrides the generated field name. `recognize` pairs each control with at most
one nearby label on the same page, produces stable names/confidence in reading
order, and writes a one-page AcroForm PDF that is reloaded through
`pdfxi-forms` and `pdfxi-core` before it is reported. Unmatched labels are
listed as orphans. No OCR or page-content analysis is performed.
Watched-folder conversion is exposed as:

```text
pdfxi watch <FOLDER> --out <DIR> [--ext txt ...] [--suffix pdf]
    [--markers SNAPSHOT.json] [--poll-every N] [--ticks N] [--json]
```

Each eligible file is converted with the in-process adapters: `.txt` and
`.md`/`.markdown` become text/markdown PDFs, `.html`/`.htm` go through the
bounded HTML importer, `.png`/`.jpg`/`.jpeg` are embedded, and `.pdf` is passed
through unchanged. Processed paths persist in the marker snapshot so later
polls skip them; converter errors are per-file failures that stay eligible for
retry. The host advances a deterministic number of ticks; the OS scheduler that
invokes it is outside the program.
The JavaScript sandbox is exposed as:

```text
pdfxi script run <SCRIPT.js> [--field NAME=VALUE ...]
    [--event-key KEY] [--event-value VALUE] [--max-steps N] [--json]
```

`--field` seeds the in-memory `this.getField(...)` store; `--event-key` and
`--event-value` seed `event`. The command parses with `Script::parse_with_limits`
and runs with `run_acrobat_script` under the default budgets (100000 steps, 64
nested calls, bounded loops/strings/arrays/objects), or `--max-steps` for a
tighter CPU budget. It reports the return value and its JavaScript type, the
final field store and the event state. There is no file, network, process,
clock, randomness or ambient global access; the store is in memory only.
The Office integration contract is exposed as:

```text
pdfxi office plan <MANIFEST.json> [--json]
pdfxi office convert <MANIFEST.json> --out <OUTPUT> [--json]
```

For `import-to-pdf`, the manifest is
`{"family":"word","flow":"import-to-pdf","name":"Report","markup":"<h1>...</h1>"}`
and the output is a PDF. For `pdf-to-office`, the manifest supplies recovered
pages with runs and optional tables
(`{"family":"excel","flow":"pdf-to-office","name":"Book","pages":[{"width":595,"height":842,"runs":[{"text":"Region","x":40,"y":800,"font_size":11}]}]}`)
and the output is a DOCX/XLSX/PPTX archive matching the family. `plan` prints
the deterministic adapter steps without writing; `convert` executes them,
checks the output extension, and re-reads the result (PDF parse and text for
import, ZIP signature and required OOXML part for export).
Portfolio assembly is exposed as:

```text
pdfxi create portfolio <MANIFEST.json> --out <OUTPUT.pdf> [--json]
```

`MANIFEST.json` names the collection and its files:
`{"title":"Case file","entries":[{"name":"docs/brief.txt","description":"Case brief","order":0,"file":"brief.txt"}]}`.
`name` is the in-portfolio path (relative, traversal-free, forward slashes);
`file` is the source on disk, resolved against the manifest directory when
relative. Each file is read, the manifest is validated, `build_portfolio_package`
writes `/Collection` and a sorted `/EmbeddedFiles` name tree, and the output is
reopened to confirm the collection flag, item count and byte-for-byte embedded
streams. Empty collections are valid. Use `pdfxi portfolio` to list or extract
the result.
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

### Read a document aloud

`ACCESS-005` (verified) hosts the deterministic utterance plan from
`pdfxi_app::read_aloud` and the Windows system synthesizer:

```text
pdfxi read-aloud <INPUT.pdf> [--wpm 180] [--page N] [--seek N]
    [--voice NAME] [--speak] [--json]
pdfxi read-aloud --voices [--json]
```

The plan splits real page text into sentences and assigns integer-millisecond
start, duration and pause values from `--wpm`, so the same document always
produces the same times. `--voices` lists the installed `System.Speech` voices;
`--speak` hands the planned sentences to the system synthesizer with the chosen
`--voice` (default: the system voice) and a rate mapped from `--wpm` onto the
synthesizer's -10..10 scale. Voice names are validated before they are embedded
in the synthesizer invocation. No bundled TTS engine is used, and no audio is
produced unless `--speak` is given.

### ICC profiles and output intents

`PRINT-003` (verified) hosts the colour model in `pdfxi_render::color`:

```text
pdfxi color icc <PROFILE.icc> [--json]
pdfxi color intents <INPUT.pdf> [--json]
pdfxi color convert <PROFILE.icc> [--space gray|rgb|cmyk] --values C1,C2,... [--json]
```

`color icc` parses a real ICC file and reports its size, version, CMM, device
class, data colour space, profile connection space, rendering intent, tag count,
ASCII description, media white point and analytic conversion plan. `color
intents` reads a PDF's catalog `/OutputIntents` array, decodes each embedded
`/DestOutputProfile` stream, validates the set (missing, multiple, unsupported
subtype, missing profile) and reports the parsed profile facts for the usable
intent. `color convert` runs the analytic matrix/TRC or gray-gamma plan on
normalised components and returns PCS XYZ or linear luminance. Profiles that
need a full colour-management module are reported as `needs-backend:<reason>` and
are never approximated.

### Accessibility tree, navigation and reflow

`ACCESS-006` (verified) and `ACCESS-004` (verified) host the window-free
accessibility models:

```text
pdfxi accessibility tree <INPUT.pdf> [--page N] [--focus REGION]
    [--high-contrast] [--json]
pdfxi accessibility nav [--theme standard|high-contrast] [--tabs N]
    [--reverse] [--json]
pdfxi accessibility layout <INPUT.pdf> [--page N] [--viewport WxH]
    [--font-size PT] [--line-spacing FACTOR] [--margin PT] [--json]
```

`tree` builds the deterministic accessibility tree a screen reader receives
from the real page count, the current page and the focus region, and prints
every node with its AccessKit role, children, focus and spoken announcement. No
screen-reader API is called; a platform bridge publishes this tree.

`nav` audits the high-contrast palette against WCAG (AAA, 7:1, for the
high-contrast theme; AA, 4.5:1, for the standard theme), prints the Tab ring and
simulates forward or reverse Tab traversal, and lists the accessibility shortcut
map (F6/Shift+F6 pane focus, F3 canvas, F4 find, Ctrl+Shift+H contrast, F5 read
aloud).

`layout` computes deterministic reflow metrics from the page's real media box
and the requested viewport: content box, scale, line height, approximate
characters per line and lines per viewport page.

### Geospatial viewports

`MEDIA-004` (verified) hosts the geospatial model in
`pdfxi_render::geospatial`:

```text
pdfxi geo info <INPUT.pdf> [--page N] [--json]
pdfxi geo to-geo <INPUT.pdf> --x X --y Y [--viewport N] [--json]
pdfxi geo to-pdf <INPUT.pdf> --lon DEG --lat DEG [--viewport N] [--json]
```

`geo info` reads the page's real `/VP` array, each viewport's `/BBox`,
`/Measure` (`/Subtype`, `/GPTS`, `/LPTS`, `/XStep`, `/YStep`, `/Units`) and the
`/GCS` coordinate system (name, type, EPSG), validates them through
`GeospatialDocument`, and reports the viewports plus the stable plan descriptor.
`to-geo` and `to-pdf` map a point affinely for a geographic coordinate system;
points outside the selected viewport (or outside every `/GPTS` bound) are
rejected. Web-Mercator and unknown projections are reported as unsupported
rather than approximated, and no map imagery is decoded or rendered.

### 3D annotations

`MEDIA-003` (verified) hosts the 3D model in `pdfxi_render::three_d`:

```text
pdfxi media3d <INPUT.pdf> [--extract PAYLOAD.u3d|.prc] [--json]
```

The command scans every page's `/Annots` for `/Subtype /3D`, reads the `/3DD`
stream reference and its `/Subtype` (`U3D` or `PRC`), the `/3DV` view
dictionaries with their `/XN` names and `/IN` default flag, and the `/3DA`
activation event (`PV`/`PI`/`PO`/`PC`), state (`L`/`F`/`U`) and `/AN` animation
flag. It then validates them through `ThreeDAnnotation`, prints the active view,
the backend requirements (decoder, animation, view activation) and the stable
plan descriptor, and with `--extract` writes the decoded payload for an external
3D viewer. The geometry itself is never decoded.

### Interactive actions and triggers

`MEDIA-005` (verified) hosts the `/AA` whitelist in `pdfxi_render::actions`:

```text
pdfxi interactions <INPUT.pdf> [--strict] [--json]
```

The command reads the catalog, page and annotation `/AA` dictionaries, resolves
each action's `/S` kind and `/D` destination (array page reference or named
destination) and validates it with the safe-action whitelist: page navigation
(`GoTo`, named `GoTo`, next/previous/first/last page) and full-screen toggling
are accepted; `JavaScript`, `Launch`, `ImportData`, `SubmitForm` and
`ResetForm` are reported as dangerous; `URI`, `GoToR`, rendition, movie and
sound actions are reported as unsupported. `--strict` exits non-zero when any
action is rejected; otherwise the report lists every action with its status.
No action is ever executed.

### Media annotations and the HTML5 envelope

`MEDIA-001` and `MEDIA-002` (verified) host `pdfxi_render::media` and
`pdfxi_render::html5_media`:

```text
pdfxi media list <INPUT.pdf> [--extract DIR] [--json]
pdfxi media html5 --source SRC=MIME [--source ...] [--poster SRC]
    [--controls] [--autoplay] [--loop] [--muted] [--width N] [--height N]
    [--out PATH] [--json]
```

`media list` reads real `/Sound`, `/Movie` and `/Screen` annotations, maps their
primary source (`object:n:g` for an embedded stream, `file:name` for a movie
file), a MIME type (from the movie extension when the PDF does not declare one),
volume, activation and rendition, and prints the playback plan and host
requirements. `--extract DIR` writes the decoded embedded payload (for example
the Sound stream) so an external player can open it. No audio or video is
played.

`media html5` validates local-only sources (an `@` prefix marks an embedded
stream) and writes a deterministic, inert `<audio>`/`<video>` element wrapped in
a complete HTML document with a strict content-security policy. Remote URLs,
absolute paths, traversal segments, control characters and oversized dimensions
are rejected. This is the HTML5 replacement surface for a Flash-era player.

### Output preview and separations

`PRINT-004` (verified) derives the print plates from real page content and hosts
`pdfxi_app::print::SeparationsPlan`:

```text
pdfxi separations <INPUT.pdf> [--plate NAME] [--json]
```

For every page the command decodes the content streams and tracks the graphics
state (`q`/`Q`) across colour operators (`cs`/`CS`, `sc`/`scn`/`SC`/`SCN`,
`g`/`G`, `rg`/`RG`, `k`/`K`). `/ColorSpace` resources are resolved through the
page's (inherited) `/Resources`, including `/Separation` and `/DeviceN` arrays,
so spot inks are named from the document. It counts paint operations per plate,
lists the pages that image each plate, and builds the canonical process-first
`SeparationsPlan`. `--plate NAME` is the output-preview query for one ink
(case-insensitive) and reports its operation count and pages. Device-RGB paint
operations are reported separately because they have no fixed process plate.
Nothing is rasterized, separated to files or proofed.

The ink-manager view is:

```text
pdfxi ink <INPUT.pdf> [--json]
```

`ink` reports each plate with its process/spot class, paint operations and the
number executed under an overprint graphics state, resolves each spot's
alternate process space from the `/Separation` or `/DeviceN` array
(`/DeviceCMYK` here), and lists the document's `/ExtGState` overprint states
(`/op`, `/OP`, `/OPM`). Overprint counts come from `gs` selecting a state before
a paint operator; an overprint preview can therefore tell which plates image
content with overprint enabled. No overprint simulation or ink editing is
performed.

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
