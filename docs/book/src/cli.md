# CLI reference

The `pdfxi` binary is the headless, deterministic shell over the PDFXI crates.
It never prompts, uses the network, or writes telemetry. This page is the
in-book summary; [docs/CLI.md](../../CLI.md) is the authoritative reference with
complete JSON shapes and pasted example output.

```text
pdfxi <COMMAND> [OPTIONS]
pdfxi <COMMAND> --help
```

## Commands

| Command | Purpose | Status |
|---|---|---|
| `info <FILE> [--json]` | Path, PDF version, byte length, page count | verified (`AUTO-009`) |
| `text <FILE>` | Plain text from page content streams | verified (`AUTO-010`) |
| `search <FILE> <QUERY> [--case-sensitive] [--whole-word] [--json]` | Cross-page text search | implemented |
| `replace <FILE> --out PATH --find T --replace T [--case-sensitive] [--whole-word] [--diacritic-insensitive] [--limit N] [--json]` | Literal find-and-replace across a document | verified (`EDIT-003`) |
| `measure <FILE> --points "x,y;x,y[;...]" [--page N] [--unit pt\|in\|mm\|cm\|ft] [--dpi D] [--decimals N] [--json]` | Distance, area and perimeter on a page | verified (`REVIEW-009`) |
| `render <FILE> --page N --scale S --out PATH [--format png\|ppm]` | Rasterize one page | implemented, `pdfium` build |
| `action list <FILE>` | List named actions in an action file | implemented |
| `action show <FILE> <NAME>` | Show an action's steps and parameters | implemented |
| `action run <FILE> <NAME> --input <PATH>... [--output-dir DIR] [--json]` | Run an action over inputs | implemented, staging-only |
| `droplet <ACTION_FILE> <NAME> <INPUT>... [--json]` | Explorer-friendly `action run` | implemented, staging-only |
| `portfolio <FILE> [--json] [--extract DIR]` | List or extract a portfolio collection | verified (`VIEW-014`) |
| `compare <LEFT> <RIGHT> [--json] [--lines] [--fail-on-difference]` | Text compare of two revisions | verified (`VIEW-015`) |
| `reflow <FILE> [--width N] [--page N] [--json]` | Reading-order text column | verified (`VIEW-013`) |
| `read-aloud <FILE> [--wpm N] [--page N] [--seek N] [--json]` | Deterministic utterance queue | verified (`VIEW-013`) |
| `optimize <FILE> --out PATH [--remove-metadata] [--remove-optional] [--no-compress] [--json]` | Compact the object graph and report | verified (`EDIT-014`) |
| `decorate header-footer <FILE> --out PATH [--header TEXT] [--footer TEXT] [--side left\|center\|right] [--json]` | Stamp headers and footers | verified (`EDIT-008`) |
| `decorate watermark <FILE> --out PATH --text TEXT [--opacity F] [--rotation D] [--tile] [--json]` | Stamp a text watermark | verified (`EDIT-009`) |
| `decorate bates <FILE> --out PATH [--prefix TEXT] [--start N] [--digits N] [--increment N] [--corner C] [--json]` | Stamp sequential Bates numbers | verified (`EDIT-010`) |
| `api [--once]` | Local JSON request/response API over stdio | verified (`AUTO-006`) |
| `annotate add <FILE> --out PATH --kind free-text\|callout\|cloud\|pencil\|stamp\|file-attachment [--rect x0,y0,x1,y1] [--text T] [--ink STROKES] [--stamp-name N] [--attachment PATH] [--json]` | Add a drawing annotation | verified (`REVIEW-002`, `REVIEW-003`) |
| `annotate list <FILE> [--page N] [--json]` | List one page's annotations | verified (`REVIEW-002`) |
| `outline list <FILE> [--json]` | List the bookmark tree | verified (`EDIT-011`) |
| `outline add <FILE> --out PATH --title T --page N [--parent PATH] [--index N]` | Append a bookmark | verified (`EDIT-011`) |
| `outline rename <FILE> --out PATH --path PATH --title T` | Rename a bookmark | verified (`EDIT-011`) |
| `outline move <FILE> --out PATH --path PATH [--parent PATH] [--index N]` | Move a bookmark | verified (`EDIT-011`) |
| `outline remove <FILE> --out PATH --path PATH` | Remove a bookmark and its children | verified (`EDIT-011`) |
| `link add <FILE> --out PATH --page N --rect x0,y0,x1,y1 (--uri URL \| --to-page N) [--highlight none\|invert\|outline\|push]` | Add a link annotation | verified (`EDIT-011`) |
| `link remove <FILE> --out PATH --page N --index K` | Remove a link annotation | verified (`EDIT-011`) |
| `attach add <FILE> --out PATH --attachment PATH [--name NAME] [--description D] [--mime TYPE]` | Embed a file in the name tree | verified (`EDIT-011`) |
| `attach remove <FILE> --out PATH --name NAME` | Remove an embedded file | verified (`EDIT-011`) |
| `update <FILE> --out PATH [--title T] [--author A] [--subject S] [--keywords K] [--producer P]` | Append a validated incremental revision | verified (`STD-010`) |
| `linearize <FILE> --out PATH [--version 1.7] [--json]` | Write a fully linearized file | verified (`STD-003`) |
| `pack <FILE> --out PATH [--no-compress] [--max-objects N] [--json]` | Pack objects into object streams and an xref stream | verified (`STD-003`) |
| `export conformance <FILE> --profile PROFILE [--out PATH] [--plan] [--policy report\|reject] [--output-intent SUBTYPE] [--json]` | Normalize for PDF/A, PDF/X or PDF/E | verified (`EXPORT-008`) |
| `form create <OUT> --field KIND:NAME:X,Y,W,H[:OPTIONS] ... [--json]` | Build a one-page form PDF | verified (`FORMS-002`) |
| `form list <FILE> [--json]` | List AcroForm fields | verified (`FORMS-002`) |
| `form validate <FILE> [--set NAME=VALUE] ... [--json]` | Validate field definitions and values | verified (`FORMS-003`) |
| `form calculate <FILE> [--set NAME=VALUE] ... [--sum\|--product\|--average\|--min\|--max TARGET=SOURCES] ... [--decimals N] [--json]` | Evaluate calculations | verified (`FORMS-003`) |
| `form tabs <FILE> [--policy rows\|columns\|structure\|annotations] [--include-read-only] [--no-wrap] [--json]` | Show tab order and navigation | verified (`FORMS-004`) |
| `form actions <FILE> [--json]` | Show accepted and rejected field actions | verified (`FORMS-004`) |
| `form reader-enable <FILE> --out PATH [--rights LIST] [--reason TEXT] [--placeholder-bytes N] [--json]` | Write local `/Perms /UR` reader-enable metadata | verified (`FORMS-008`) |
| `form distribute <FILE> --out BUNDLE [--form-id ID] [--title T] [--version N] [--json]` | Export a blank bundle of a form's fields | verified (`FORMS-009`) |
| `form collect <FILE> --response [RESPONDENT=]PATH ... [--json]` | Merge local FDF/XFDF responses | verified (`FORMS-009`) |
| `accessibility check <FILE> [--title T] [--lang TAG] [--json]` | Check accessibility metadata and PDF/UA-1 structure | verified (`ACCESS-003`) |
| `compare <LEFT> <RIGHT> --hunks [--json] [--fail-on-difference]` | Line-hunk version diff with page and line references | verified (`REVIEW-008`) |
| `versions snapshot <FILE> --store PATH [--label L] [--json]` | Capture page text into a version store | verified (`MANAGE-005`) |
| `versions list --store PATH [--json]` | List stored snapshots | verified (`MANAGE-005`) |
| `versions diff <LEFT> <RIGHT> [--store PATH] [--json]` | Diff two snapshots or a snapshot and a PDF | verified (`MANAGE-005`) |
| `versions remove <ID> --store PATH [--json]` | Remove a stored snapshot | verified (`MANAGE-005`) |
| `font add-text <FILE> --out PATH --text T --rect x0,y0,x1,y1 [--page N] [--font-size PT] [--align left\|center\|right] [--json]` | Embed the CID font and insert text | verified (`STD-004`) |
| `structure apply <FILE> --out PATH --tree PATH [--json]` | Write a tagged-PDF structure tree | verified (`STD-006`) |

Commands are integration-tested by invoking the real binary; see
`crates/pdfxi-cli/tests/cli.rs`.

## Conventions worth remembering

- `render --page` is **zero-based**, matching the `Renderer` trait. `search`
  page numbers in output are **one-based**.
- `render --scale` is required, finite, and in `(0, 8]`; `1.0` is 72 DPI.
- `search` matches ASCII case-insensitively by default. Offsets in output are
  zero-based Unicode scalar offsets into a page's concatenated text.
- A `search` with no matches exits `0`, prints nothing on stdout, and writes
  `no matches` to stderr — in both text and JSON modes.
- `--json` is available on `info`, `search`, `action run`, and `droplet`.
  Output is pretty-printed JSON with stable field names; new fields are additive.
- `text` decodes FlateDecode streams transparently. Another filter is reported
  as an error that names the filter and page.
- `compare` aligns pages by their extracted text, not by page number, so a page
  inserted or deleted anywhere is reported as such. Pages whose text differs are
  `changed` pairs; `--lines` prints their differing lines as `left | right`
  columns, capped at 20 rows per page. A completely different single page is one
  changed pair rather than a deletion plus an insertion.
- `portfolio` prints `/UF` folder depth with two spaces per level. `--extract`
  recreates the folders and refuses `..`, absolute, drive, `:` stream, control
  character and over-long path components per file instead of aborting.
- `compare --fail-on-difference` exits `1` after printing the report when the
  documents differ; without it a difference still exits `0`.
- `reflow --page` is **one-based**; `--width` is characters, `20..=200`
  (default `80`), and a word longer than the width is kept whole rather than
  split. Paragraphs break on blank lines; a single newline is a soft break.
- `read-aloud` performs no speech or audio calls: it prints the queue a backend
  would speak. Timings are integer milliseconds from `word_ms = 60000 / wpm`
  (`80..=400`, default `180`), so the same document always yields the same
  timings. `--seek N` starts at utterance `N` and rebases the clock to `0`.
- `optimize` never modifies its input: `--out` is required and self-overwrite is
  refused. Compression is per stream and only applied when it strictly shrinks
  that stream, so a file can still end up marginally larger after renumbering;
  the report prints the real signed size change. The report is byte-identical
  across runs on the same input.
- `decorate` writes its overlay as real page content: the stamped text is
  extractable and searchable, and the overlay's fonts and `/ExtGState` entries
  are merged into a copy of each page's resources under names the page does not
  already use. Placeholders are `{page}`, `{pages}`, `{date}`, `{title}` and
  `{filename}`; an unknown placeholder is an error.
- `decorate bates` numbers pages in ascending order, starting at `--start`
  (at least 1) with `--increment` per page, zero-padded to `--digits`. The stamp
  is drawn through the same display-to-page transform as headers, so a rotated
  page gets an upright number in the requested corner. Quote or escape a prefix
  or suffix that begins with `-` (`--suffix=-A`).
- `api` speaks one JSON request per line and one canonical response per line:
  `{"version":1,"id":"...","command":"...","params":{...}}` in,
  `{"error":...,"id":...,"ok":...,"result":...,"version":1}` out. `--once`
  answers exactly one line. Version must be `1`; lines over 1 MiB, duplicate
  keys, deep nesting and malformed JSON return typed errors without exiting.
  Commands are `version`, `commands`, `list-actions`, `show-action`, `dry-run`,
  `run-batch`, `list-toolsets`, `show-toolset`, `validate-toolset` and
  `run-toolset`; runs are staged, so no file is written by a batch.
- `annotate add` writes real annotations: `/Subtype /FreeText` with `/DA` and a
  wrapped-text appearance, `/IT /FreeTextCallout` with `/CL` for a callout, and
  `/Subtype /Polygon` with `/IT /PolygonCloud` plus a scalloped appearance for a
  cloud. `--page` is one-based, `--rect`/`--anchor` are points, and `--font-size`
  is limited to 4-72. `annotate list` reads back every annotation with its `/NM`
  id, subtype, text, author, rect and date.
- `outline` edits the catalog `/Outlines` tree. Paths are dotted child indices
  (`0.1` is the second child of the first root); an omitted `--parent` addresses
  the root list and an omitted `--index` appends. `outline list` prints the tree
  with one-based page destinations and reports items whose destination could not
  be represented.
- `measure` validates the supplied page points against the page's media box and
  reports a distance for two points or polygon area and perimeter for three or
  more. Units convert through `MeasureScale` (`pt`, `in`, `mm`, `cm`, `ft`, or a
  scan DPI in inches); area uses the square of the linear factor. Every result
  carries the deterministic appearance summary and its vector-command count.- `replace` is the writing half of `find_replace`: it locates literal matches in
  the run model, rewrites the containing run through `replace_run_text`, saves,
  reopens and re-extracts to report `initial_matches`, `replaced` and
  `remaining`. Matches must lie inside one run; a match spanning two runs is
  left untouched and counted as remaining. Whole-run deletions are skipped for
  the same reason. Every replacement reflows its block, so a paragraph may be
  rewrapped or its lines joined. `--limit` bounds the work (default 1000) and
  `limit_reached` reports when it stopped the run.- `structure apply` reads a JSON tree (`lang`, `nodes` with role/page/bbox/
  text/`alt`/`actual_text`/`expansion`/`artifact`/children, and `roots`),
  validates it, then merges `pdfxi_accessibility::write_structure_tree` into a
  real document: remapped structure objects, `/StructTreeRoot`, `/ParentTree`,
  `/Lang`, `/MarkInfo` and `/ViewerPreferences` in the catalog, `/StructParents`
  and an appended marked-content overlay on every page. It saves, reopens and
  verifies the tree root type, parent tree, every element's `/S`/`/P`/`/Pg`,
  the role count and the unchanged page text. No conformance certification is
  claimed.- `font add-text` embeds the bundled Courier Prime program (SIL OFL 1.1) as a
  `/Type0` font with a `/CIDFontType2` descendant, `/FontFile2`, `/CIDToGIDMap`,
  CID-keyed `/W` widths and a `/ToUnicode` CMap, writes a wrapped text box with
  that font, saves, and reopens the result. It verifies the font graph and that
  every inserted word is extractable through the ToUnicode mapping, so the
  command fails rather than silently writing unsearchable text.- `versions` keeps page-text snapshots in a small JSON store (`{"version":1,
  "next_id":..., "snapshots":[...]}`) with monotonically increasing ids.
  `snapshot` extracts normalised page text from a real PDF, `list` shows the
  stored revisions, `diff` compares two ids or an id and a PDF path with the
  same bounded line-hunk model as `compare --hunks`, and `remove` deletes a
  revision without reusing its id. The store contains text only, is written
  atomically, and refuses an unknown format version.- `compare --hunks` uses the `pdfxi_edit::compare` line-hunk model instead of
  page alignment: pages are matched by index and each `added`, `removed` or
  `changed` line carries its one-based page, old line, new line and both texts.
  Output is deterministic, `--json` exposes a summary (`added`, `removed`,
  `changed`, `pages_examined`, `hunks`, `old_lines`, `new_lines`), and
  `--fail-on-difference` exits non-zero when hunks exist. It conflicts with
  `--lines`.- `accessibility check` runs two deterministic checks and merges them: the
  accessibility checker over a tree derived from recovered page text (document,
  page and paragraph nodes) with the document title and `/Lang`, and the
  PDF/UA-1 preflight profile over the file's real catalog and structure. It
  prints stable `UA-nnnn` and `PDFUA1.*` findings, a prioritized fix plan for
  the accessibility findings, and exits `2` when either check reports an error,
  `0` when neither does. `--title`/`--lang` override the values used by the
  checker, not the file. No conformance certification is claimed.- `form tabs` reads the AcroForm `/Tabs` policy (`R`, `C`, `S` or `A`) and the
  real widget rectangles and pages through the document-form model, orders the
  widgets, and reports each field's next and previous target. `--policy`
  overrides the file, `--include-read-only` keeps read-only widgets, and
  `--no-wrap` stops at the ends instead of wrapping.
- `form actions` walks each field and widget dictionary, parses `/A` and `/AA`
  entries (`K`, `F`, `V`, `C`) into the safe reset/submit whitelist, and lists
  every rejected action with its subtype and reason. Scripts are never parsed or
  executed.- `form validate` loads the fields, applies `--set NAME=VALUE` assignments
  (`true`/`false` for checkboxes, `a|b` for choices/radio, numbers otherwise),
  then validates every definition and value through the property model. It
  prints a stable report and exits `2` when the form is invalid, `0` when it is
  valid, and `1` for a typed input error.
- `form calculate` applies the same assignments in memory and evaluates one or
  more `--sum/--product/--average/--min/--max TARGET=SRC1,SRC2` steps in
  dependency order, printing each target's rounded result. The evaluation is
  read-only: results are not written back to the PDF.- `form distribute` reads the AcroForm fields and writes the canonical
  `PDFXI-BUNDLE-1` text bundle (form id, title, version and one `FIELD` line per
  field). The written text is reparsed and compared before success.
- `form collect` loads responses from local FDF or XFDF files (detected from the
  document), assigns each a respondent id from `RESPONDENT=` or the file stem,
  and merges them with `merge_responses`. The lexicographically smallest
  respondent wins a conflict while every differing value is reported; malformed
  responses are rejected rather than aborting. Merged fields must exist in the
  PDF, required fields with no value are listed, and the report is deterministic
  under response reordering. No network is used.- `form reader-enable` writes the local compatibility shape used by
  reader-enable workflows: a `/Perms /UR` dictionary with the PDFXI filter, an
  `/Ff` rights mask, an optional reason and a run of ASCII `0` placeholder
  bytes. It preserves other `/Perms` entries, saves, reopens and reparses the
  dictionary with `parse_rights_dictionary`, then reports the granted rights and
  the fill/save decisions. It is not a signature and never claims Adobe
  certification (`certified: false`). Rights: `fill`, `save`, `annotate`,
  `assemble`, `copy-extract`, `accessibility` or `all`.- `form create` builds typed AcroForm fields from repeatable
  `KIND:NAME:X,Y,W,H[:OPT1|OPT2]` descriptions (`text`, `checkbox`, `radio`,
  `combo`, `list`, `button`, `signature`) on a single 612x792 page, serializes
  them with `pdfxi-forms::serialize_form_pdf`, and reloads the bytes through
  both the forms model and `pdfxi-core` before writing. Rectangles must be
  positive and inside the page, names must be unique, and options are required
  only for radio/combo/list. `form list` reports the terminal fields of any form
  PDF using the same loader.- `export conformance` accepts canonical labels (`PDF/A-2b`) and compact
  lowercase spellings (`pdfa-2b`, `pdfx-4`, `pdfe-1`). It normalizes the header
  version, adds the XMP and output-intent placeholders the target requires,
  detects forbidden catalog entries, and rewrites the xref deterministically.
  `--plan` prints the byte-free plan without writing; `--policy reject` fails on
  the first forbidden entry instead of recording a warning; the `--no-*` flags
  disable individual normalizations. The report always states
  `certification_claimed: false` and prints the scope note, because the XMP and
  ICC streams are placeholders and every other constraint is reported only.- `linearize` writes a complete linearized file: a `/Linearized` parameter
  dictionary, page-offset and shared-object hint tables, a first-page classic
  xref, and a main xref whose `/Prev` points back at it. The command re-reads
  the output, checks the declared `/L` against the written bytes and the
  declared `/N` against the page count, then verifies every page's text against
  the source. `--version` is the header token.
- `pack` keeps stream objects standalone and packs the rest into `/ObjStm`
  streams with a `/Type /XRef` cross-reference stream. `--max-objects` is the
  `/ObjStm` member limit (1-65535) and `--no-compress` writes uncompressed
  streams. The output is reopened and page text is compared with the source.- `update` rewrites nothing: it stages the `/Info` change and calls the core
  incremental writer, so every original byte remains an exact prefix and only
  the changed objects plus a new cross-reference section are appended. The
  result is checked with the preservation validator (prefix, `/Prev` chain,
  page count and page text) before it is written. Sources that already carry a
  hybrid `/XRefStm` section cannot be walked by that chain check, so for them
  the command verifies prefix, page count and every page's text directly and
  prints `xref_chain_verified: false` with the reason; it never claims the chain
  check passed.- `link add` writes a real `/Link` annotation with `/Rect`, `/H`, `/Border`,
  `/C` and either `/A << /S /URI ... >>` or a `/Dest [page /Fit]` array that
  references the document's page object. `link remove` counts links in
  annotation order, so `--index 0` removes the first link on the page.
- `attach add` registers an `/EmbeddedFile` stream through a `/Filespec` in the
  catalog `/Names /EmbeddedFiles` name tree, which is the same tree
  `pdfxi attachments` reads; `attach remove` deletes the entry. Existing name
  tree entries are preserved and re-sorted.- `annotate add --kind pencil` takes `--ink "x0,y0 x1,y1; x2,y2"` and
  `--stroke-width`; `--kind stamp` takes `--stamp-name` (for example Approved)
  and `--text`; `--kind file-attachment` takes `--attachment PATH` and an
  optional `--name`. An attachment is embedded as an `/EmbeddedFile` stream
  referenced by the annotation's `/FS`, so `pdfxi attachments` lists it and
  `pdfxi attachments --extract` writes the exact bytes. Pencil does not use
  `--rect`; the library derives it from the stroke bounds.

Exit codes:

| Code | Meaning |
|---|---|
| `0` | Success (including a no-match search) |
| `1` | Runtime failure: I/O, parse, validation, failed job, or missing `pdfium` for `render` |
| `2` | Argument/usage error reported by the parser |

`PDFIUM_PATH` selects the PDFium shared library for `render`; on Windows the
fallback is `%LOCALAPPDATA%\pdfxi\pdfium\bin\pdfium.dll`. `CARGO_TARGET_DIR` is a
build-time Cargo setting, not a runtime variable.

## Examples

```powershell
pdfxi info .\fixtures\minimal.pdf
pdfxi info --json .\fixtures\minimal.pdf
pdfxi text .\fixtures\minimal.pdf
pdfxi search .\fixtures\minimal.pdf PDFXI --json
pdfxi render .\fixtures\minimal.pdf --page 0 --scale 2.0 --out .\page.png
pdfxi action list .\staging.act
pdfxi action show .\staging.act nightly
pdfxi action run .\staging.act nightly --input .\fixtures\minimal.pdf --json
pdfxi droplet .\staging.act nightly .\fixtures\minimal.pdf
pdfxi portfolio .\fixtures\authoring\authoring-portfolio.pdf --extract .\portfolio
pdfxi compare .\fixtures\realworld\powerpoint-slides.pdf .\revised.pdf --lines
pdfxi reflow .\fixtures\realworld\powerpoint-slides.pdf --width 40 --page 1
pdfxi read-aloud .\fixtures\realworld\powerpoint-slides.pdf --wpm 150 --seek 1 --json
pdfxi optimize .\fixtures\realworld\powerpoint-slides.pdf --out .\optimized.pdf
pdfxi decorate header-footer .\fixtures\realworld\powerpoint-slides.pdf --out .\paged.pdf --footer "Page {page} of {pages}"
pdfxi decorate watermark .\fixtures\realworld\powerpoint-slides.pdf --out .\draft.pdf --text DRAFT --opacity 0.2
pdfxi decorate bates .\fixtures\realworld\powerpoint-slides.pdf --out .\exhibits.pdf --prefix "EX-" --digits 6
```

See [Automation](automation.md) for the action-file format and the staging-only
executor's guarantees, and [Getting started](getting-started.md) for build and
fixture setup.
