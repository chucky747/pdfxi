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
```

See [Automation](automation.md) for the action-file format and the staging-only
executor's guarantees, and [Getting started](getting-started.md) for build and
fixture setup.
