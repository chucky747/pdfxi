# Acceptance criteria tracker

Source: `docs/GOAL.md`. A criterion is checked only when the listed command
output or file evidence is current at HEAD. This file is the integrator's
single view of the end condition; task evidence lives in `docs/evidence/`.

Last updated: 2026-11-20 (release wave: GUI opens real files, bounded rendering,
portable package and user-scope install; 140 rows = 137 verified + 3 replaced).

| ID | Criterion | Status | Evidence | Caveat |
|---|---|---|---|---|
| AC-01 | Reproducible bootstrap | partial | `scripts/bootstrap.ps1 -Check` -> `CHECK result=PASS missing=0 verified_tools=5` with pinned `stable-x86_64-pc-windows-gnu` (`docs/evidence/bootstrap-check.md`); M0 gate row set satisfied | No physical clean-machine dry run; the winget elevation path is unexercised and recorded in `docs/ASSUMPTIONS.md` |
| AC-02 | Build quality | met | `cargo build --workspace --release` and `cargo build --release -p pdfxi-app --bin pdfxi-ui --features ui,pdfium` exit 0 (`pdfxi.exe` 1,888,768 B, `pdfxi-ui.exe` 5,996,032 B); `cargo fmt --all --check`, workspace clippy `-D warnings`, workspace tests (47 suites) and warnings-denied rustdoc all exit 0 (`docs/evidence/release/T-0193.md`) | none |
| AC-03 | Tests | met | `cargo test --workspace` exit 0 (47 test binaries, 0 failures); `cargo xtask corpus smoke` 9/9; fuzz smoke 5 targets / 275 inputs / 555 accepted / 709 rejected; hostile fixture suite 18/18 (`docs/evidence/qa/T-0183.md`) | Doctests are host-limited (`AD-0005`, `docs/ASSUMPTIONS.md` item 11); no network, GUI, Adobe software or secret is used |
| AC-04 | Feature parity | met | `docs/features.toml` 140 rows: 137 verified + 3 replaced; `cargo xtask features verify` -> `FEATURES OK: 140 rows, 137 verified`; `docs/FEATURES.md` generated from the matrix | Three retired integrations are local equivalents under ADR 0008 (CREATE-011, CREATE-012, MANAGE-006) |
| AC-05 | Performance | met | `cargo xtask bench` -> `metrics=18 breaches=0 regressions=0 status=pass` on 2026-11-19 and again on 2026-11-20 against the committed baseline (`docs/evidence/bench.md`, `docs/evidence/qa/bench-baseline.json`); every budget has two orders of magnitude of headroom | Baseline refreshed on the canonical host; run-to-run variance and host load are documented in the evidence |
| AC-06 | Security | met | Encryption (AES-256/RC4), permissions, signature create/verify with keyed integrity, redaction that removes content from text and image extraction, sanitization/XMP scrubbing, and sandboxed JavaScript tests pass (`cargo test --workspace`; row evidence in `docs/evidence/security/`) | Reviewer findings were fixed in T-0176 and T-0185 (envelope redaction, keyed MAC, byte-range validation, authenticity reporting) |
| AC-07 | Accessibility | met | Tagged structure tree with ActualText/expansion/Artifact and object projection, reading-order export, alt text, PDF/UA-style structure checks, keyboard/touch focus navigation, high-contrast tokens and fix planning (`cargo test --workspace`; `docs/evidence/author/T-0177.md`, `docs/evidence/app/T-0171.md`) | Independent veraPDF-style validation remains a CI oracle, not a runtime dependency |
| AC-08 | Documentation | met | Workspace rustdoc with warnings denied exit 0; `cargo xtask docs` -> `DOCS OK`; mdBook under `docs/book/src/`; CLI reference `docs/CLI.md`; architecture, ADRs 0001-0008, `docs/THIRD_PARTY.md`, `docs/PLATFORMS.md` | none |
| AC-09 | Orchestration evidence | met | Board, claims, 180+ task records, per-task evidence under `docs/evidence/`, blocker records, and memory inbox notes; full gate output committed at `docs/evidence/goal-verify.log` | Memory inbox notes are written per task; curated memory is owned by the memory-librarian |
| AC-10 | Packaging and legal | met | `scripts/package.ps1 -SkipBuild` -> `PACKAGE OK target\package\pdfxi-portable-0.1.0-windows-x86_64.zip` (3,635,778 B, sha256 `da966e20…428ebae0`, 8 hashed files incl. both binaries); `-Check` -> `layout_and_notices=PASS`; `scripts/install.ps1 -DesktopShortcut` installed `%LOCALAPPDATA%\Programs\PDFXI` with Start-Menu `PDFXI`/`PDFXI CLI` and Desktop shortcuts; installed GUI launched with a document argument and CLI verified (`docs/evidence/release/T-0192.md`); notices, allowlist and secret scan included | No MSI/elevated installer and no code signing |

## Release wave (2026-11-20)

| Task | Scope | Evidence |
|---|---|---|
| T-0190 | GUI opens real PDFs: Open… path dialog, drag-and-drop, CLI argument; runtime fixture resolution | `docs/evidence/app/T-0190.md` |
| T-0191 | Render safety: 16 Mpx clamp, 256 MiB byte budget, 2 rasters/frame, document-scoped cancellation | `docs/evidence/app/T-0191.md` |
| T-0192 | Portable zip with both binaries; user-scope install with Start-Menu/Desktop shortcuts | `docs/evidence/release/T-0192.md` |
| T-0193 | Release verification: release builds, full gate, bench, package/install transcript | `docs/evidence/release/T-0193.md` |

Installed payload: `%LOCALAPPDATA%\Programs\PDFXI` (`pdfxi-ui.exe`,
`pdfxi.exe`, manifest, notices). Desktop shortcut `PDFXI.lnk` targets the
installed viewer and survives `cargo clean`.

## Product shell (UI) — 2026-11-20

The default egui shell was replaced with a modern Acrobat-style product shell:
custom title bar with tabs and window controls, compact icon rail, pages panel
with thumbnails, rich document page, right tools panel with AI Assistant and
Export cards, and a bottom page/zoom bar. Both themes are captured and the
review is recorded.

| Evidence | Path |
|---|---|
| Light acceptance screenshot | `docs/evidence/ui/light-v8.png` |
| Dark acceptance screenshot | `docs/evidence/ui/dark-v8.png` |
| Review process, checklist, findings, sign-off | `docs/design/UI_REVIEW.md` |
| App tests | `cargo test -p pdfxi-app --features ui,pdfium` -> 490 passed |
| Lint/format | `cargo clippy ... -D warnings`, `cargo fmt --all --check` -> clean |

The shell is the product surface for the verified library/CLI features; it is
not an additional feature-matrix row.

## End condition

```text
$ cargo xtask goal-verify --all
$ cargo fmt --all --check
$ cargo clippy --workspace --all-targets --all-features -- -D warnings
$ cargo test --workspace
$ cargo doc --workspace --no-deps
FEATURES OK: 140 rows, 137 verified
VERIFY OK
GOAL VERIFIED: 23623c9a0ce05b760ef4fdedf4bb8be83a70ece0 unix=1795151774
```

The full transcript (486 KB) of this clean-worktree run is committed at
`docs/evidence/goal-verify.log`.

## Rule

No row moves to `partial` or `met` without pasted command output or an
in-repo artifact referenced from this table. `cargo xtask goal-verify --all`
remains the final authority.
