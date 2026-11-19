# Command-line interface — M1

> **Status: in progress.** The CLI commands on this page are the intended M1
> surface. `info` is wired in the core; `text` currently validates the input and
> reports that extraction is pending; `render` is planned. Examples show the
> intended interface and may not all succeed yet.

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

Extract plain text from a PDF. Text extraction is implemented in the
`pdfxi-text` lane during M1.

```powershell
pdfxi text .\fixtures\minimal.pdf
```

Intended behaviour:

- Write extracted plain text to standard output, page by page.
- Preserve reading order where the PDF's text structure permits.
- Honour the same cancellation and progress conventions as the UI.

Current behaviour: the command parses the document and then reports that text
extraction lands in M1. Do not depend on its output until the `pdfxi-text`
lane marks the feature verified.

## `pdfxi render`

Render one or more pages to image files (intended; not yet implemented).

```powershell
# Render page 1 at 144 DPI to a PNG.
pdfxi render .\fixtures\minimal.pdf --page 1 --dpi 144 --format png --out .\out\page-1.png

# Render a page range at a fixed scale.
pdfxi render .\report.pdf --pages 1-3 --scale 2.0 --format png --out .\out\
```

Intended options:

| Option | Meaning | Default |
|---|---|---|
| `--page N` | Render a single 1-based page | first page |
| `--pages A-B` | Render an inclusive page range | all pages |
| `--dpi N` | Target resolution | 72 |
| `--scale F` | Scale factor (alternative to `--dpi`) | 1.0 |
| `--format png\|jpeg` | Output image format | `png` |
| `--out PATH` | Output file or directory | required |

Rendering uses the same PDFium-backed `Renderer` as the viewer, so CLI and UI
output are expected to match. `--dpi` and `--scale` are mutually exclusive.

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
