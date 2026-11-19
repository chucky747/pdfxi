# PDFXI CLI reference

The `pdfxi` binary is the headless entry point. Every scriptable operation is
also a library call, so the CLI stays a thin, deterministic shell over the
crates.

```text
pdfxi <COMMAND> [OPTIONS]
```

## `pdfxi info`

Prints the file path, PDF version, byte length, and page count read from the
document's real page tree.

```text
pdfxi info <FILE>
pdfxi info --json <FILE>
```

`--json` emits a stable object with exactly these fields:

```json
{"file":"...","pdf_version":"1.7","bytes":595,"pages":1}
```

Field names and types are part of the CLI contract; new fields are additive.

## `pdfxi text`

Extracts text from page content streams. Only uncompressed streams are decoded
today; compressed streams return an error that names the filter rather than
guessing.

```text
pdfxi text <FILE>
pdfxi text <FILE> --page 2
```

Supported operators: `Tj`, `TJ`, `'`, and `"`. Extraction is bounds-checked and
never panics on malformed content.

## `pdfxi render`

Rasterizes one page through the renderer and writes an image.

```text
pdfxi render <FILE> --page 1 --scale 1.0 --out page.png
pdfxi render <FILE> --page 1 --scale 2.0 --out page.ppm --format ppm
```

- `--page` is 1-based on the command line.
- `--scale` is a multiplier over 72 DPI (`1.0` = 72 DPI, `2.0` = 144 DPI).
- `--format` is `png` (default) or `ppm` (P6 binary).
- Requires a build with the `pdfium` feature and a PDFium library discoverable
  through `PDFIUM_PATH` or `%LOCALAPPDATA%\pdfxi\pdfium\bin\pdfium.dll`.

## Exit codes

| Code | Meaning |
|---|---|
| 0 | Success |
| 1 | Invalid arguments or usage |
| 2 | I/O or parse failure |
| 3 | Feature unavailable in this build |

## Planned commands

`merge`, `split`, `rotate`, `crop`, `forms`, `security`, `convert`, `preflight`,
`search`, `save`. Each lands with integration tests that invoke the real binary.
