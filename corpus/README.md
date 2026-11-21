# Corpus

This directory holds **descriptors** for large or third-party PDF corpora. It
does **not** hold the corpus data itself.

## Policy

- Large corpora are never committed. Each `[[corpus]]` entry in
  `manifest.toml` pins an exact HTTPS URL, a lowercase SHA-256 digest, an exact
  byte size, and an SPDX license note.
- A fetch must fail closed: an entry whose `sha256` is empty, whose `status` is
  not `approved`, or whose size is above the 8 MiB bound is documentation only
  and is refused by the fetch tool. If the digest or size does not match, the
  temporary download is discarded and the run fails. No corpus is ever used
  unverified.
- Files are cached **outside** this repository, so no ignore rule is needed for
  downloaded binaries. Fast tests (`cargo test -p pdfxi-testkit`) never touch the
  network; they only inspect files already present in the cache.
- Committed, small, synthetic fixtures live in `fixtures/` and are documented in
  `fixtures/manifest.toml`; they are a different category from these corpora.

## Layout

```text
corpus/
  README.md       # this policy
  manifest.toml   # corpus descriptors (ID, URL, SHA-256, size, license)
```

## Pinned entries (T-0051)

Both approved entries come from **Apache PDFBox** test resources, licensed
**Apache-2.0**, pinned to the immutable commit for release 3.0.8
(`9286e47d89d6877005c9d2d0f2fd38793a62519a`). They are under 2 KiB in total and
were checked with the real `pdfxi_core::Document`: each opens as one page and
yields the marker text recorded in the manifest.

| id | file | bytes | SHA-256 (prefix) |
| --- | --- | ---: | --- |
| `pdfbox-5002` | `PDFBOX-5002.pdf` | 941 | `401586241a33…` |
| `pdfbox-table-document` | `document.pdf` | 1042 | `a31f3b5fc79e…` |

A third entry, `verapdf-test-suite`, remains `unapproved` with an empty digest
and an `example.invalid` URL. It exercises the fail-closed path and must never
be fetched.

## Fetch, verify, and smoke

```text
# Download missing approved entries into the cache, verify, and report:
cargo run -p pdfxi-testkit --example fetch_corpus

# Verify only what is already cached; absent files are skipped, no network:
cargo run -p pdfxi-testkit --example fetch_corpus -- --offline

# Offline hash/page/text smoke over cached files (no network):
cargo xtask corpus smoke
```

The fetch uses `curl` with a 30-second timeout and `--max-filesize`, enforces
the exact declared size and digest, and renames a verified `.part` file into
place only after both checks pass. A missing or broken download produces a
clear error that suggests `--offline`; it never leaves a corrupt cached file.
`cargo xtask corpus smoke` runs the deterministic and real-producer suites plus
`external_corpus_smoke_checks_cached_files`; uncached entries print `SKIP`.

### Cache location

`PDFXI_CORPUS_CACHE` overrides the cache directory. Otherwise the platform
cache directory is used (`%LOCALAPPDATA%\pdfxi\corpus` on Windows,
`$XDG_CACHE_HOME/pdfxi/corpus` or `~/.cache/pdfxi/corpus` elsewhere), falling
back to the system temp directory. Each entry is stored at
`<cache>/<id>/<file>`. No absolute user path is hard-coded anywhere in the code.

## Adding a corpus

1. Add a `[[corpus]]` entry with a unique `id`, an HTTPS `url` pinned to an
   immutable revision, a single-component `file` name, the exact `sha256`,
   `bytes`, expected `pages`, `markers`, `license`, `source`, and
   `status = "approved"`.
2. Confirm the license permits cached use of the data. Never point at a
   proprietary or private document set.
3. Never commit the downloaded archive or its extracted files; they live in the
   out-of-tree cache.
4. Re-run the fetch, verify, and smoke commands and paste the output as task
   evidence. The `external_corpus` integration tests enforce the hash/size
   contract in fast, offline runs.
