# CORPUS

Large PDF corpora are fetched, checksum-verified, and gitignored. The committed
`fixtures/` directory contains only small generated files.

## Layout

```text
corpus/
├── manifest.toml
├── cache/            # gitignored downloads
├── private/          # gitignored, never uploaded
└── README.md
```

## manifest.toml

```toml
[[corpus]]
id = "pdfjs"
url = "https://github.com/mozilla/pdf.js/archive/refs/heads/master.zip"
sha256 = "REPLACE_ME"
license = "Apache-2.0"
kind = "render"
```

`cargo xtask corpus fetch` verifies the checksum before extraction.
`cargo xtask corpus smoke` uses only the committed fixture set.

## Rules

- Never commit a corpus download.
- Never upload a private corpus.
- Record license and provenance for every corpus.
- Malformed samples are fuzz seeds, not runtime fixtures.
- Baseline images are generated, not hand-edited.
