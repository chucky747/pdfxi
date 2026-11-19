# Command-line interface

> **Status: in progress.** `info`, `text`, `search`, `action list/show/run`, and
> `droplet` are implemented and covered by integration tests that invoke the
> real binary. `render` requires a build with the `pdfium` feature and a
> discoverable PDFium library. The commands are library-level equivalents, not
> a UI; see the canonical [feature matrix](../FEATURES.md) and the authoritative
> [CLI reference](../CLI.md).

The CLI is the headless surface for automation and CI. It shares `pdfxi-core`
and the feature crates with the desktop application, so every operation that
can be scripted is available to both. Implementation context is in
[ARCHITECTURE.md](../ARCHITECTURE.md); feature scope is in
[PLAN.md](../PLAN.md) and the [feature matrix](../FEATURES.md).

## Invocation

```text
pdfxi <COMMAND> [ARGS]
```

The binary is `pdfxi` (crate `pdfxi-cli`). Run `pdfxi --help` or
`pdfxi <COMMAND> --help` for the authoritative flag list.

## Commands

| Command | Purpose | Matrix row |
|---|---|---|
| `info <FILE> [--json]` | Path, version, byte length, page count | `AUTO-009` (verified) |
| `text <FILE>` | Plain text from page content streams | `AUTO-010` (verified) |
| `search <FILE> <QUERY> [--case-sensitive] [--whole-word] [--json]` | Cross-page text search | implemented |
| `render <FILE> --page N --scale S --out PATH [--format png\|ppm]` | Rasterize one page | implemented, `pdfium` build |
| `action list/show/run` | Named action management and execution | `AUTO-001`, `AUTO-002`, `AUTO-005` |
| `droplet <ACTION_FILE> <NAME> <INPUT>... [--json]` | Explorer-friendly action run | `AUTO-003` |

## `pdfxi info`

Print basic information about a PDF: file path, PDF version, byte length, and
page count resolved from the real page tree.

```powershell
pdfxi info .\fixtures\minimal.pdf
```

```text
file: fixtures/minimal.pdf
pdf_version: 1.7
bytes: 595
pages: 1
```

`--json` emits the same four fields as a pretty-printed object. The values come
from `Document::version()`, `Document::len()`, and `Document::page_count()`. A
malformed or unsupported file returns a `thiserror` error from `pdfxi-core`,
surfaced as a non-zero exit with an actionable message.

## `pdfxi text`

Write extracted text to stdout, one page per line, in page order.

```powershell
pdfxi text .\fixtures\minimal.pdf
# Hello, PDFXI!
```

FlateDecode content streams are decoded transparently by `pdfxi-core`; another
filter is an error that names the filter and page. Supported text operators are
`Tj`, `TJ`, `'`, and `"`. Extraction is bounds-checked and never panics on
malformed content. There is no per-page flag on this command; use
`pdfxi search` to locate a substring.

## `pdfxi search`

Build a `SearchIndex` from one text page per PDF page and print every match of
`QUERY`.

```powershell
pdfxi search .\fixtures\minimal.pdf PDFXI
# page 1 offset 7 PDFXI
pdfxi search .\fixtures\minimal.pdf PDFXI --json
```

`page` is one-based and `offset` is a zero-based Unicode scalar offset into the
matched page's concatenated text. Matching is ASCII case-insensitive by default;
`--case-sensitive` and `--whole-word` narrow it. A query with no matches exits
`0`, writes nothing to stdout, and writes `no matches` to stderr in both text
and JSON modes. `--json` emits `file`, `query`, and `matches[]` with `page`,
`offset`, and `text` per match.

## `pdfxi render`

Render one page to an image (PNG by default, or binary PPM).

```powershell
pdfxi render .\fixtures\minimal.pdf --page 0 --scale 1.0 --out .\out\page-0.png
pdfxi render .\fixtures\minimal.pdf --page 0 --scale 2.0 --format ppm --out .\out\page-0.ppm
```

| Option | Meaning | Default |
|---|---|---|
| `--page N` | **Zero-based** page index, matching the `Renderer` contract | required |
| `--scale F` | Finite multiplier over 72 DPI (`2.0` = 144 DPI), in `(0, 8]` | required |
| `--format png\|ppm` | PNG (8-bit RGBA) or P6 PPM (alpha discarded) | `png` |
| `--out PATH` | Destination file | required |

Without the `pdfium` feature the command exits non-zero with
`pdfxi was built without pdfium support: rebuild with --features pdfium to use
the 'render' command` instead of failing at compile time. PNG output uses
uncompressed deflate blocks. PDFium is discovered through `PDFIUM_PATH`, then
`%LOCALAPPDATA%\pdfxi\pdfium\bin\pdfium.dll`, then the current directory and the
system search path.

## Action-file commands

`action list <FILE>` prints `name<TAB>step-count<TAB>description` per action.
`action show <FILE> <NAME>` prints the steps and parameters of one action.
`action run <FILE> <NAME> --input <PATH>... [--output-dir DIR] [--json]` validates
every input first, then runs jobs in input order with failure isolation and a
non-zero exit if any job failed. `droplet <ACTION_FILE> <NAME> <INPUT>...` is the
same run with text output and no output-directory override.

The current executor is a deterministic, no-mutation adapter: inputs are staged
in memory (512 MiB bound), actions record per-step outcomes, and exports resolve
a relative target without writing a file. See the
[automation book chapter](../book/src/automation.md) for the action-file format
and the [CLI reference](../CLI.md) for exact JSON shapes.

## Cross-cutting conventions

- **Exit codes:** `0` success (including a no-match search), `1` runtime failure
  (I/O, parse, validation, failed job, missing `pdfium`), `2` usage error.
- **Errors:** actionable messages; library detail comes from `thiserror`
  enums, never a panic.
- **JSON:** `info`, `search`, `action run`, and `droplet` emit stable,
  pretty-printed objects; unknown fields are additive.
- **Untrusted input:** every PDF is hostile. Parsers are bounds-checked and
  fuzzed; see [TESTING.md](../TESTING.md) and RULES.md section 8.
- **No network, no telemetry.** The CLI operates entirely on local files.
- **Automation row:** `AUTO-005` tracks full CLI coverage; M1 delivered `info`
  and `text`, and later milestones added `search`, `render`, `action`, and
  `droplet`.

## Verification

CLI smoke paths are exercised by `crates/pdfxi-cli/tests/cli.rs`, which runs the
real binary and inspects exit status, stdout, and stderr. The milestone gate is
`cargo xtask goal-verify --milestone <milestone>`; see
[TESTING.md](../TESTING.md) and the [feature matrix](../FEATURES.md).
