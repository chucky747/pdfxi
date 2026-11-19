# Corpus

This directory holds **descriptors** for large or third-party PDF corpora. It
does **not** hold the corpus data itself.

## Policy

- Large corpora (for example veraPDF, public-domain document sets, or fuzz seed
  archives) are never committed. They may contain third-party content and are
  often too big for the repository.
- A corpus is described in `manifest.toml` by a stable ID, a download URL, an
  expected SHA-256 digest, and the byte size. A consumer fetches the archive to
  a cache **outside** this repository, verifies its digest, and only then uses
  it.
- A fetch must fail closed: if the digest or size does not match, the data is
  discarded and the run fails. No corpus is ever used unverified.
- Fast tests (`cargo test -p pdfxi-testkit`) never touch the network. Corpus
  smoke suites are gated behind the `full` profile or a dedicated feature.
- Committed, small, synthetic fixtures live in `fixtures/` and are documented in
  `fixtures/manifest.toml`; they are a different category from these corpora.

## Layout

```text
corpus/
  README.md       # this policy
  manifest.toml   # corpus descriptors (ID, URL, SHA-256, size, license)
```

## Adding a corpus

1. Add a `[[corpus]]` entry to `manifest.toml` with a unique `id`, the `url`, the
   expected `sha256`, the exact `bytes`, and the `license`/`source` note.
2. Confirm the license permits redistribution of the descriptor and cached use
   of the data. Never point at a proprietary or private document set.
3. Do not commit the downloaded archive or its extracted files.
4. Reference the corpus by `id` from a gated corpus/fuzz harness.

The current manifest is an empty skeleton: no corpus has been approved for
fetching in M1, so nothing here triggers a download.
