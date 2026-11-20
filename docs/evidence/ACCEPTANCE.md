# Acceptance criteria tracker

Source: `docs/GOAL.md`. A criterion is checked only when the listed command
output or file evidence is current at HEAD. This file is the integrator's
single view of the end condition; task evidence lives in `docs/evidence/`.

Last updated: 2026-11-19 (all campaigns delivered; 140 rows = 137 verified +
3 replaced; `cargo xtask goal-verify --all` exit 0 with `GOAL VERIFIED`).

| ID | Criterion | Status | Evidence | Caveat |
|---|---|---|---|---|
| AC-01 | Reproducible bootstrap | partial | `scripts/bootstrap.ps1 -Check` -> `CHECK result=PASS missing=0 verified_tools=5` with pinned `stable-x86_64-pc-windows-gnu` (`docs/evidence/bootstrap-check.md`); M0 gate row set satisfied | No physical clean-machine dry run; the winget elevation path is unexercised and recorded in `docs/ASSUMPTIONS.md` |
| AC-02 | Build quality | met | `cargo build --workspace --release` OK; `cargo fmt --all --check` exit 0; `cargo clippy --workspace --all-targets --all-features -- -D warnings` exit 0; workspace rustdoc with `RUSTDOCFLAGS=-D warnings` exit 0 (all in `docs/evidence/goal-verify.log`) | none |
| AC-03 | Tests | met | `cargo test --workspace` exit 0 (47 test binaries, 0 failures); `cargo xtask corpus smoke` 9/9; fuzz smoke 5 targets / 275 inputs / 555 accepted / 709 rejected; hostile fixture suite 18/18 (`docs/evidence/qa/T-0183.md`) | Doctests are host-limited (`AD-0005`, `docs/ASSUMPTIONS.md` item 11); no network, GUI, Adobe software or secret is used |
| AC-04 | Feature parity | met | `docs/features.toml` 140 rows: 137 verified + 3 replaced; `cargo xtask features verify` -> `FEATURES OK: 140 rows, 137 verified`; `docs/FEATURES.md` generated from the matrix | Three retired integrations are local equivalents under ADR 0008 (CREATE-011, CREATE-012, MANAGE-006) |
| AC-05 | Performance | met | `cargo xtask bench` -> `metrics=18 breaches=0 regressions=0 status=pass` against the refreshed committed baseline (`docs/evidence/bench.md`, `docs/evidence/qa/bench-baseline.json`); every budget has two orders of magnitude of headroom | Baseline refreshed on the canonical host; run-to-run variance and host load are documented in the evidence |
| AC-06 | Security | met | Encryption (AES-256/RC4), permissions, signature create/verify with keyed integrity, redaction that removes content from text and image extraction, sanitization/XMP scrubbing, and sandboxed JavaScript tests pass (`cargo test --workspace`; row evidence in `docs/evidence/security/`) | Reviewer findings were fixed in T-0176 and T-0185 (envelope redaction, keyed MAC, byte-range validation, authenticity reporting) |
| AC-07 | Accessibility | met | Tagged structure tree with ActualText/expansion/Artifact and object projection, reading-order export, alt text, PDF/UA-style structure checks, keyboard/touch focus navigation, high-contrast tokens and fix planning (`cargo test --workspace`; `docs/evidence/author/T-0177.md`, `docs/evidence/app/T-0171.md`) | Independent veraPDF-style validation remains a CI oracle, not a runtime dependency |
| AC-08 | Documentation | met | Workspace rustdoc with warnings denied exit 0; `cargo xtask docs` -> `DOCS OK`; mdBook under `docs/book/src/`; CLI reference `docs/CLI.md`; architecture, ADRs 0001-0008, `docs/THIRD_PARTY.md`, `docs/PLATFORMS.md` | none |
| AC-09 | Orchestration evidence | met | Board, claims, 180+ task records, per-task evidence under `docs/evidence/`, blocker records, and memory inbox notes; full gate output committed at `docs/evidence/goal-verify.log` | Memory inbox notes are written per task; curated memory is owned by the memory-librarian |
| AC-10 | Packaging and legal | met | `scripts/package.ps1 -Check` -> `layout_and_notices=PASS`, `secret_free=True` (`docs/evidence/package-check.md`); `docs/THIRD_PARTY.md`, `deny.toml`, clean-room notices, no Adobe asset or PDFium binary staged | Portable archive is produced by `scripts/package.ps1` when a release binary is present |

## End condition

```text
$ cargo xtask goal-verify --all
$ cargo fmt --all --check
$ cargo clippy --workspace --all-targets --all-features -- -D warnings
$ cargo test --workspace
$ cargo doc --workspace --no-deps
FEATURES OK: 140 rows, 137 verified
VERIFY OK
GOAL VERIFIED: 0d73706deb1afe58e7a2508c0ae07683eeb91d67 unix=1795144564
```

The full 486 KB transcript is committed at `docs/evidence/goal-verify.log`.

## Rule

No row moves to `partial` or `met` without pasted command output or an
in-repo artifact referenced from this table. `cargo xtask goal-verify --all`
remains the final authority.
