# PDFXI fuzz targets

Seven small libFuzzer targets exercise hostile input against the real library
code. Every target is a thin wrapper around a plain function, so the wrappers and
the deterministic runner test exactly the same code. Five functions live in
`crates/pdfxi-testkit/src/fuzz.rs`; the `redaction` and `edit` functions live in
the standalone `fuzz/entry` member (task T-0260A owns `fuzz/**` only) and reuse
that module's `FuzzOutcome` and `MAX_FUZZ_INPUT` bound.

| Target | Covers |
|---|---|
| `parser` | `Document::from_bytes` (classic xref, xref streams, `/ObjStm`), object-stream save + reparse, `repair_document`, `parse_linearization`, version analysis |
| `filters` | `decode_stream_with_limit` with hostile Flate predictor parameters, an unsupported-filter chain, and an empty chain |
| `content` | `extract_text` tokenizer and `decode_text_string` |
| `forms` | `parse_xfdf`, `import_xfdf`, `import_fdf`, validation `Pattern`, and `load_acroform` through a bounded `FormSource` |
| `crypto` | MD5/SHA-256/SHA-384, RC4, AES-CBC, DER TLV walk, OID decode, `Certificate::parse`, and CMS `verify_signed_data` |
| `redaction` | `pdfxi_edit::apply_redaction` content removal + full `Document::save` + reparse, `verify_redaction`, and `pdfxi_security::sanitize_document_full` + `verify_sanitized` |
| `edit` | `Document::from_bytes`, `content_edit::text_blocks` / `list_objects`, `document_annotations::add_highlight` / `add_text_note` success paths, and two documented typed rejections (empty identifier, degenerate quad) |

All entry points bound the input to 16 KiB, cap decompression at 64 KiB, and
return a small `FuzzOutcome`. They never panic, index unsafely, or use `unsafe`.

## Local deterministic run (this host)

`cargo fuzz` is **not installed** and `libfuzzer-sys` does not compile on the
canonical host (see `orchestration/blockers/T-0260A-fuzz-toolchain.md`).
No network is used. Run the equivalent smoke runners instead:

```powershell
. .\scripts\env.ps1 -Lane qa
cargo run -p pdfxi-testkit --example fuzz_smoke
cargo test -p pdfxi-testkit --test fuzz_smoke
cargo test --manifest-path fuzz/entry/Cargo.toml -- --nocapture
```

The last command builds no C++ libFuzzer runtime. It reads every committed seed
in `fuzz/corpus/redaction/` and `fuzz/corpus/edit/`, then feeds 48 seeded
pseudo-random inputs plus the empty input to each target, and asserts the real
removal and annotation paths (the redaction seed loses its text run across a
save/reparse; the edit seed gains a persisted annotation). Output is
deterministic. Fast tests must stay under 90 seconds; this suite does.

## libFuzzer run (machine with cargo-fuzz)

On a machine with a working nightly toolchain, `cargo-fuzz`, and a C++/linker
setup that can build `libfuzzer-sys`:

```powershell
rustup toolchain install nightly
cargo install cargo-fuzz
cd fuzz
cargo +nightly fuzz run parser -- -max_len=16384 -timeout=5 -rss_limit_mb=2048
cargo +nightly fuzz run filters -- -max_len=16384 -timeout=5 -rss_limit_mb=2048
cargo +nightly fuzz run content -- -max_len=16384 -timeout=5 -rss_limit_mb=2048
cargo +nightly fuzz run forms -- -max_len=16384 -timeout=5 -rss_limit_mb=2048
cargo +nightly fuzz run crypto -- -max_len=16384 -timeout=5 -rss_limit_mb=2048
cargo +nightly fuzz run redaction -- -max_len=16384 -timeout=5 -rss_limit_mb=2048
cargo +nightly fuzz run edit -- -max_len=16384 -timeout=5 -rss_limit_mb=2048
```

`fuzz/Cargo.toml` is a standalone cargo-fuzz manifest (its own `[workspace]`)
because the canonical workspace root is integrator-owned and does not list
`fuzz` as a member. It declares `[package.metadata] cargo-fuzz = true`, one
`[[bin]]` per target, and an `entry` member with the plain functions.
`libfuzzer-sys` is fetched by that manifest only; the fast checks never build it.

To seed a local libFuzzer run, point it at the committed corpus:

```powershell
cargo +nightly fuzz run parser -- fuzz/corpus/parser
cargo +nightly fuzz run redaction -- fuzz/corpus/redaction
cargo +nightly fuzz run edit -- fuzz/corpus/edit
```

## Release requirement

Per `RULES.md` and `docs/TESTING.md`, a release requires a **clean 24-hour
fuzz run** with no panics on the malformed corpus. Run each target for a full
day on the fuzz host and archive the logs plus the crash/leak counts as
evidence before tagging a release:

```powershell
cargo +nightly fuzz run parser -- -max_total_time=86400 -max_len=16384 -rss_limit_mb=2048
```

Repeat for `filters`, `content`, `forms`, `crypto`, `redaction`, and `edit`. Any
crash, hang, OOM, or sanitizer finding blocks the release and needs a
regression test.

## Corpus policy

Seeds are tiny, generated locally, and contain no secrets or customer data.
Large malformed sets (for example SafeDocs) are fetched by checksum in CI and
are never committed.

`fuzz/corpus/manifest.toml` describes only the seeds produced by
`pdfxi_testkit::seed_corpus::seeds()`, and the testkit fast suite pins that file
byte for byte. The `redaction` and `edit` corpora are copies of committed
fixtures (`fixtures/minimal.pdf` plus one real fixture each), so they are not
listed there, exactly like the pre-existing hand-seeded files under the other
target directories; `verify_corpus` ignores unlisted files.
