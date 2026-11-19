# Command-line interface — M1

> **Status: in progress.** `info`, `text`, and `render` are implemented and
> covered by integration tests. `render` requires a build with the `pdfium`
> feature and a discoverable PDFium library. `search` and the remaining commands
> are still planned. See the canonical [feature matrix](../FEATURES.md).

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
`pdfxi <COMMAND> --help` for the authoritative flag list once a command is
implemented.

## `pdfxi info`

Print basic information about a PDF: file path, PDF version, byte length, and
page count. This is the first working smoke path.

```powershell
pdfxi info .\fixtures\minimal.pdf
```

Intended output shape:

```text
file: .\fixtures\minimal.pdf
pdf_version: 1.7
bytes: 1456
pages: 1
```

The values come from `Document::version()`, `Document::len()`, and
`Document::page_count()`. A malformed or unsupported file returns a `thiserror`
error from `pdfxi-core`, surfaced as a non-zero exit with an actionable message.

## `pdfxi text`

Extract plain text from a PDF.

```powershell
pdfxi text .\fixtures\minimal.pdf
```

Behaviour:

- Writes extracted text to standard output, page by page.
- Decodes Flate-compressed content streams; unsupported filters return an error
  that names the filter.
- Supported text operators: `Tj`, `TJ`, `'`, and `"`. Extraction is
  bounds-checked and never panics on malformed content.

## `pdfxi render`

Render one page to an image (PNG or PPM).

```powershell
# Render page 1 (zero-based page 0) at 72 DPI to PNG.
pdfxi render .\fixtures\minimal.pdf --page 0 --scale 1.0 --out .\out\page-0.png

# Render at 144 DPI as binary PPM.
pdfxi render .\fixtures\minimal.pdf --page 0 --scale 2.0 --format ppm --out .\out\page-0.ppm
```

Options:

| Option | Meaning | Default |
|---|---|---|
| `--page N` | Zero-based page index, matching the `Renderer` contract | required |
| `--scale F` | Multiplier over 72 DPI (`2.0` = 144 DPI) | `1.0` |
| `--format png\|ppm` | Output format | `png` |
| `--out PATH` | Output file | required |

Rendering uses the same PDFium-backed `Renderer` as the viewer. Without the
`pdfium` feature the command exits non-zero with a clear "built without pdfium
support" message instead of failing at compile time. PNG output is valid but
uncompressed (stored deflate blocks).

## Cross-cutting conventions

- **Exit codes:** `0` on success, non-zero on parse/I/O/usage errors.
- **Errors:** actionable messages; library detail comes from `thiserror`
  enums, never a panic.
- **Untrusted input:** every PDF is hostile. Parsers are bounds-checked and
  fuzzed; see [TESTING.md](../TESTING.md) and RULES.md section 8.
- **No network, no telemetry.** CLI operates entirely on local files.
- **Automation row:** the long-term goal is a CLI for every scriptable
  operation (`AUTO-005`, P1). M1 delivers only `info`, `text`, and `render`.

## Verification

The M1 gate is `cargo xtask goal-verify --milestone M1`. CLI smoke paths are
exercised by the `pdfxi-cli` tests; until they pass and evidence is recorded,
the commands above are an interface contract, not a shipped feature.
