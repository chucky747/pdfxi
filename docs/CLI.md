# PDFXI CLI reference

The `pdfxi` binary is the headless entry point. Every scriptable operation is
also a library call, so the CLI stays a thin, deterministic shell over the
crates. The binary never uses the network, writes telemetry, or prompts for
input.

```text
pdfxi <COMMAND> [OPTIONS]
pdfxi --help
pdfxi --version
pdfxi <COMMAND> --help
```

The reference below describes the commands present in this build. Run
`pdfxi --help` for the authoritative list; the command set grows with each
milestone. All examples use the committed fixture `fixtures/minimal.pdf`, a
one-page, 595-byte PDF whose text is `Hello, PDFXI!`.

## `pdfxi info`

Print the file path, declared PDF version, byte length, and page count. The page
count is resolved from the real page tree, not from a cached header value.

```text
pdfxi info <FILE>
pdfxi info --json <FILE>
```

Text output is one `field: value` line per field, in this order:

```text
file: fixtures/minimal.pdf
pdf_version: 1.7
bytes: 595
pages: 1
```

`--json` emits the same four fields as a pretty-printed object. Field names,
types, and ordering are part of the CLI contract; new fields are additive.

```json
{
  "file": "fixtures/minimal.pdf",
  "pdf_version": "1.7",
  "bytes": 595,
  "pages": 1
}
```

```powershell
pdfxi info .\fixtures\minimal.pdf
pdfxi info --json .\fixtures\minimal.pdf
```

## `pdfxi text`

Extract plain text from the page content streams, one page per line, in page
order. FlateDecode streams are decoded transparently by `pdfxi-core`; any other
filter is an error that names the filter and the page, rather than a guess at
undecoded bytes.

```text
pdfxi text <FILE>
```

Supported text-showing operators are `Tj`, `TJ`, `'`, and `"`. Extraction is
bounds-checked and never panics on malformed content.

```powershell
pdfxi text .\fixtures\minimal.pdf
# Hello, PDFXI!
```

There is no per-page selection flag on this command; use `pdfxi search` to
locate a specific substring, or the `pdfxi-text` library for per-page control.

## `pdfxi search`

Find text across every page. One `SearchIndex` is built from one extracted text
page per PDF page, so offsets are stable and directly comparable to
`pdfxi-text` results.

```text
pdfxi search <FILE> <QUERY> [--case-sensitive] [--whole-word] [--json]
```

| Flag | Effect | Default |
|---|---|---|
| `--case-sensitive` | Fold nothing; match case exactly | off (ASCII case-insensitive) |
| `--whole-word` | Require non-word neighbours or page boundaries around the match | off |
| `--json` | Emit the stable JSON object below | off |

In text mode each match is one line, ordered by page and then offset:

```text
page 1 offset 7 PDFXI
```

`page` is **one-based**; `offset` is a **zero-based character offset** (Unicode
scalar values, not bytes) into the matched page's concatenated text; `text` is
the matched substring with its original casing.

```json
{
  "file": "fixtures/minimal.pdf",
  "query": "PDFXI",
  "matches": [
    { "page": 1, "offset": 7, "text": "PDFXI" }
  ]
}
```

```powershell
pdfxi search .\fixtures\minimal.pdf PDFXI
pdfxi search .\fixtures\minimal.pdf pdfxi --case-sensitive
pdfxi search .\fixtures\minimal.pdf PDFXI --json
```

An empty query, or a query with no matches, is **not** an error: the command
exits `0`, writes nothing to stdout, and writes `no matches` to stderr. This
holds in both text and JSON modes, so an empty result is never mistaken for a
decode failure.

## `pdfxi render`

Rasterize one page and write a PNG or PPM image. Requires a build with the
`pdfium` Cargo feature and a discoverable PDFium library.

```text
pdfxi render <FILE> --page N --scale S --out PATH [--format png|ppm]
```

| Flag | Meaning | Default |
|---|---|---|
| `--page N` | **Zero-based** page index (matching the `Renderer` contract) | required |
| `--scale S` | Finite multiplier over 72 DPI; `1.0` = 72 DPI, `2.0` = 144 DPI; must be in `(0, 8]` | required |
| `--out PATH` | Destination file | required |
| `--format png\|ppm` | `png` (8-bit RGBA, uncompressed deflate blocks) or `ppm` (binary P6, alpha discarded) | `png` |

The page index is validated against the document's page count before rendering;
`--scale` is validated before the renderer is invoked. Both report actionable
errors. A build without the `pdfium` feature still accepts these arguments and
exits non-zero with `pdfxi was built without pdfium support: rebuild with
--features pdfium to use the 'render' command`.

```powershell
pdfxi render .\fixtures\minimal.pdf --page 0 --scale 1.0 --out .\out\page-0.png
pdfxi render .\fixtures\minimal.pdf --page 0 --scale 2.0 --format ppm --out .\out\page-0.ppm
```

On success the command prints one status line, for example
`wrote out\page-0.png (page 0, scale 1, PNG)`.

## `pdfxi action`

Manage named actions stored in a tab-delimited action file. An action file holds
one or more blocks separated by blank lines, using the stable
`ACTION` / `STEP` / `PARAM` text format of `pdfxi-automation`. A short example:

```text
ACTION	nightly	Stage every night
STEP	load	import
STEP	stamp	watermark
PARAM	text	DRAFT
STEP	save	export
PARAM	target	out.pdf
```

```text
pdfxi action list <FILE>
pdfxi action show <FILE> <NAME>
pdfxi action run <FILE> <NAME> --input <PATH>... [--output-dir DIR] [--json]
```

### `pdfxi action list <FILE>`

Print one tab-separated line per action in file order:
`name<TAB>step-count<TAB>description`.

```powershell
pdfxi action list .\staging.act
# nightly	3	Stage every night
```

### `pdfxi action show <FILE> <NAME>`

Print the action's name, description, step count, and ordered steps. Parameters
are shown on an indented `params:` line beneath their step.

```text
name: nightly
description: Stage every night
steps: 3
1 load import
2 stamp watermark
    params: text=DRAFT
3 save export
    params: target=out.pdf
```

An unknown name is an error that also lists the available names.

### `pdfxi action run <FILE> <NAME> --input <PATH>... [--output-dir DIR] [--json]`

Validate the action file, action name, action structure, and **every** input path
before running any job. Jobs then execute in input order with ids `job-0`,
`job-1`, …; one failing input is isolated to its own outcome and never stops its
siblings. The command exits non-zero if any job failed.

`--output-dir DIR` is the base directory for resolving relative export targets.
Relative names (including nested names such as `sub/out.pdf`) resolve underneath
it; absolute paths and paths containing `..`, a root, or a drive prefix are
rejected. **No file is written** — the resolved path is only recorded.

The current executor is a deterministic, no-mutation adapter: `import` stages
the input bytes in memory (bounded at 512 MiB), `convert`, `watermark`,
`apply_security`, and `run_command` record a message while passing the artifact
along, and `export` records its resolved target. Real PDF mutation is owned by
other crates and is deliberately not coupled to the CLI yet.

Text mode prints one tab-separated line per job:
`id<TAB>input<TAB>state<TAB>summary`.

```powershell
pdfxi action run .\staging.act nightly --input .\fixtures\minimal.pdf
# job-0	fixtures/minimal.pdf	succeeded	succeeded steps=3
pdfxi action run .\staging.act nightly --input .\fixtures\minimal.pdf --output-dir .\out --json
```

`--json` emits this stable shape (pretty-printed):

```json
{
  "action": "nightly",
  "jobs": [
    {
      "id": "job-0",
      "input": "fixtures/minimal.pdf",
      "state": "succeeded",
      "outcome": {
        "state": "succeeded",
        "completed_steps": ["load", "stamp", "save"],
        "failed_step": null,
        "message": null,
        "summary": "succeeded steps=3",
        "steps": [
          {
            "step_id": "load",
            "kind": "import",
            "message": "staged minimal.pdf (595 bytes)",
            "artifact_kind": "bytes",
            "artifact": "bytes(595)"
          }
        ]
      }
    }
  ]
}
```

`state` values are the executor's job states, for example `succeeded`, `failed`,
or `cancelled`. `artifact_kind` is `bytes`, `path`, or `empty`; document bytes are
never printed, only their length. The `steps` array holds one entry per executed
step in order.

## `pdfxi droplet`

A droplet-style entry point for Windows Explorer "drop onto target" verbs. It is
exactly `action run` with default text output and no `--output-dir`, and it
accepts a variadic input list.

```text
pdfxi droplet <ACTION_FILE> <NAME> <INPUT>... [--json]
```

```powershell
pdfxi droplet .\staging.act nightly .\fixtures\minimal.pdf
pdfxi droplet .\staging.act nightly .\fixtures\minimal.pdf --json
```

An Explorer verb registered under
`HKEY_CLASSES_ROOT\*\shell\PDFXI Stage\command` can forward dropped files
directly:

```text
"C:\Program Files\PDFXI\pdfxi.exe" droplet "C:\Program Files\PDFXI\staging.act" nightly "%1"
```

Use `%*` to forward several dropped files at once. Output and exit behaviour are
identical to `action run`.

## Error and exit-code behaviour

Errors are written to stderr as `Error: <message>`, with `Caused by:` chains for
I/O and parse detail. Library errors originate from `thiserror` enums; the CLI
maps them to actionable messages and never panics. Exit codes:

| Code | Meaning |
|---|---|
| `0` | Success, including a `search` with no matches |
| `1` | Runtime failure: missing/unreadable file, invalid PDF, unsupported filter, invalid render scale or page, unknown action, validation failure, a failed job, or a build without `pdfium` for `render` |
| `2` | Command-line usage error reported by the argument parser (unknown command or flag, missing required argument) |

All commands validate their inputs before doing work. For `action run` and
`droplet`, a bad action file, unknown action name, or missing input path exits
before any job starts.

## JSON output summary

| Command | Top-level fields |
|---|---|
| `info --json` | `file`, `pdf_version`, `bytes`, `pages` |
| `search --json` | `file`, `query`, `matches[]` (`page`, `offset`, `text`) |
| `action run --json` | `action`, `jobs[]` (`id`, `input`, `state`, `outcome`) |
| `droplet --json` | same shape as `action run --json` |

Every JSON object is emitted with `serde_json::to_string_pretty`, on a single
write to stdout. Consumers should treat unknown fields as additive.

## Environment variables

| Variable | Effect |
|---|---|
| `PDFIUM_PATH` | Full path to the PDFium shared library, used by `render` in `pdfium` builds (highest discovery priority). |
| `LOCALAPPDATA` | On Windows, used to locate the standard install at `%LOCALAPPDATA%\pdfxi\pdfium\bin\pdfium.dll` when `PDFIUM_PATH` is unset. |
| `CARGO_TARGET_DIR` | **Build-only.** Selects Cargo's output directory (for example `target/lanes/docs`). The installed `pdfxi` binary does not read it. |

PDFium discovery order is `PDFIUM_PATH`, the standard per-user install,
`pdfium.dll` in the current directory, then the platform's system library search
path. The `render` command accepts its flags in every build; without `pdfium` it
returns the non-zero "built without pdfium support" error described above.

## Related documentation

- [Viewer slice](features/view.md) and [CLI overview](features/cli.md) — the M1
  narrative companion to this reference.
- [Feature matrix](FEATURES.md) — implementation status per command
  (`AUTO-009`, `AUTO-010`, `AUTO-005`).
- [GUI/scenario tests](TESTING.md) — how CLI smoke paths are exercised.
