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
