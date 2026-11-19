# PDFXI fuzz targets

Five small libFuzzer targets exercise hostile input against the real library
code. Every target is a thin wrapper around a plain function in
`crates/pdfxi-testkit/src/fuzz.rs`; the wrappers and the deterministic runner
therefore test exactly the same code.

| Target | Covers |
|---|---|
| `parser` | `Document::from_bytes` (classic xref, xref streams, `/ObjStm`), object-stream save + reparse, `repair_document`, `parse_linearization`, version analysis |
| `filters` | `decode_stream_with_limit` with hostile Flate predictor parameters, an unsupported-filter chain, and an empty chain |
| `content` | `extract_text` tokenizer and `decode_text_string` |
| `forms` | `parse_xfdf`, `import_xfdf`, `import_fdf`, validation `Pattern`, and `load_acroform` through a bounded `FormSource` |
| `crypto` | MD5/SHA-256/SHA-384, RC4, AES-CBC, DER TLV walk, OID decode, `Certificate::parse`, and CMS `verify_signed_data` |

All entry points bound the input to 16 KiB, cap decompression at 64 KiB, and
return a small `FuzzOutcome`. They never panic, index unsafely, or use `unsafe`.

## Local deterministic run (this host)

`cargo fuzz`, nightly Rust, and `libfuzzer-sys` are **not installed** on the
canonical host. No network is used. Run the equivalent smoke runner instead:

```powershell
. .\scripts\env.ps1 -Lane qa
cargo run -p pdfxi-testkit --example fuzz_smoke
cargo test -p pdfxi-testkit --test fuzz_smoke
```

The runner reads the committed seeds in `fuzz/corpus/<target>/`, then feeds 48
seeded pseudo-random inputs plus the empty input to each target. Output is
deterministic. Fast tests must stay under 90 seconds; this suite does.

## libFuzzer run (machine with cargo-fuzz)

On a machine with network access for one dependency fetch:

```powershell
rustup toolchain install nightly
cargo install cargo-fuzz
cd fuzz
cargo +nightly fuzz run parser -- -max_len=16384 -timeout=5 -rss_limit_mb=2048
cargo +nightly fuzz run filters -- -max_len=16384 -timeout=5 -rss_limit_mb=2048
cargo +nightly fuzz run content -- -max_len=16384 -timeout=5 -rss_limit_mb=2048
cargo +nightly fuzz run forms -- -max_len=16384 -timeout=5 -rss_limit_mb=2048
cargo +nightly fuzz run crypto -- -max_len=16384 -timeout=5 -rss_limit_mb=2048
```

`fuzz/Cargo.toml` is a standalone cargo-fuzz manifest (its own `[workspace]`)
because the canonical workspace root is integrator-owned and does not list
`fuzz` as a member. It declares `[package.metadata] cargo-fuzz = true` and one
`[[bin]]` per target. `libfuzzer-sys` is fetched by that manifest only; the
fast checks never build it.

To seed a local libFuzzer run, point it at the committed corpus:

```powershell
cargo +nightly fuzz run parser -- fuzz/corpus/parser
```

## Release requirement

Per `RULES.md` and `docs/TESTING.md`, a release requires a **clean 24-hour
fuzz run** with no panics on the malformed corpus. Run each target for a full
day on the fuzz host and archive the logs plus the crash/leak counts as
evidence before tagging a release:

```powershell
cargo +nightly fuzz run parser -- -max_total_time=86400 -max_len=16384 -rss_limit_mb=2048
```

Repeat for `filters`, `content`, `forms`, and `crypto`. Any crash, hang, OOM,
or sanitizer finding blocks the release and needs a regression test.

## Corpus policy

Seeds are tiny, generated locally, and contain no secrets or customer data.
Large malformed sets (for example SafeDocs) are fetched by checksum in CI and
are never committed.