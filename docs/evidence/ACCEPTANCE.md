# Acceptance criteria tracker

Source: `docs/GOAL.md`. A criterion is checked only when the listed command
output or file evidence is current at HEAD. This file is the integrator's
single view of the end condition; task evidence lives in `docs/evidence/`.

Last updated: 2026-11-20 (review and hardening campaign: honest shell, pinned
and bundled PDFium, real installer, honest feature matrix). Earlier v16 and
release-wave claims below are marked superseded.

| ID | Criterion | Status | Evidence | Caveat |
|---|---|---|---|---|
| AC-01 | Reproducible bootstrap | partial | `scripts/bootstrap.ps1 -Check` -> `CHECK result=PASS missing=0 verified_tools=5` with pinned `stable-x86_64-pc-windows-gnu` (`docs/evidence/bootstrap-check.md`); M0 gate row set satisfied | No physical clean-machine dry run; the winget elevation path is unexercised and recorded in `docs/ASSUMPTIONS.md` |
| AC-02 | Build quality | met | `cargo build --workspace --release` and `cargo build --release -p pdfxi-app --bin pdfxi-ui --features ui,pdfium` exit 0 (`pdfxi.exe` 1,888,768 B, `pdfxi-ui.exe` 5,996,032 B); `cargo fmt --all --check`, workspace clippy `-D warnings`, workspace tests (47 suites) and warnings-denied rustdoc all exit 0 (`docs/evidence/release/T-0193.md`) | Binary sizes shown are from T-0193; current packaged sizes and resource evidence are in `docs/evidence/release/T-0202.md` |
| AC-03 | Tests | met | `cargo test --workspace` exit 0 (47 test binaries, 0 failures); `cargo xtask corpus smoke` 9/9; fuzz smoke 5 targets / 275 inputs / 555 accepted / 709 rejected; hostile fixture suite 18/18 (`docs/evidence/qa/T-0183.md`) | Doctests are host-limited (`AD-0005`, `docs/ASSUMPTIONS.md` item 11); no network, GUI, Adobe software or secret is used |
| AC-04 | Feature parity | **not met** | `docs/features.toml` 143 rows: 25 verified, 18 library, 96 model, 3 replaced, 1 in-progress; `cargo xtask goal-verify --all` stops with `115 feature rows are not verified or replaced` and does **not** print `GOAL VERIFIED` | Honest statuses per [ADR 0009](../../docs/decisions/0009-feature-status-honesty.md); 115 rows are explicitly unfinished (T-0205, T-0204 evidence) |
| AC-05 | Performance | met | `cargo xtask bench` -> `metrics=18 breaches=0 regressions=0 status=pass` on 2026-11-19 and again on 2026-11-20 against the committed baseline (`docs/evidence/bench.md`, `docs/evidence/qa/bench-baseline.json`); every budget has two orders of magnitude of headroom | Baseline refreshed on the canonical host; run-to-run variance and host load are documented in the evidence |
| AC-06 | Security | met | Encryption (AES-256/RC4), permissions, signature create/verify with keyed integrity, redaction that removes content from text and image extraction, sanitization/XMP scrubbing, and sandboxed JavaScript tests pass (`cargo test --workspace`; row evidence in `docs/evidence/security/`) | Reviewer findings were fixed in T-0176 and T-0185 (envelope redaction, keyed MAC, byte-range validation, authenticity reporting) |
| AC-07 | Accessibility | met | Tagged structure tree with ActualText/expansion/Artifact and object projection, reading-order export, alt text, PDF/UA-style structure checks, keyboard/touch focus navigation, high-contrast tokens and fix planning (`cargo test --workspace`; `docs/evidence/author/T-0177.md`, `docs/evidence/app/T-0171.md`) | Independent veraPDF-style validation remains a CI oracle, not a runtime dependency |
| AC-08 | Documentation | met | Workspace rustdoc with warnings denied exit 0; `cargo xtask docs` -> `DOCS OK`; mdBook under `docs/book/src/`; CLI reference `docs/CLI.md`; architecture, ADRs 0001-0008, `docs/THIRD_PARTY.md`, `docs/PLATFORMS.md` | none |
| AC-09 | Orchestration evidence | met | Board, claims, 180+ task records, per-task evidence under `docs/evidence/`, blocker records, and memory inbox notes; full gate output committed at `docs/evidence/goal-verify.log` | Memory inbox notes are written per task; curated memory is owned by the memory-librarian |
| AC-10 | Packaging and legal | partial | Portable zip and per-user Inno Setup installer built, installed, launched, and uninstalled silently per user (`docs/evidence/release/T-0202.md`); payload includes pinned `pdfium.dll` and its licences; notices, allowlist, and secret scan pass; installed binary is GUI-subsystem with icon/version/DPI manifest. Final installed-build evidence: [T-0202-final-install.md](release/T-0202-final-install.md) (hashes, real install, captures) | **Unsigned** (no certificate; `orchestration/blockers/T-0202-code-signing.md`), so SmartScreen warns. The old `008...` package hash and v16 payload in T-0192 are superseded by T-0202. No MSI/elevated installer |

## Release wave (2026-11-20)

| Task | Scope | Evidence |
|---|---|---|
| T-0190 | GUI opens real PDFs: Open… path dialog, drag-and-drop, CLI argument; runtime fixture resolution | `docs/evidence/app/T-0190.md` |
| T-0191 | Render safety: 16 Mpx clamp, 256 MiB byte budget, 2 rasters/frame, document-scoped cancellation | `docs/evidence/app/T-0191.md` |
| T-0192 | Portable zip with both binaries; user-scope install with Start-Menu/Desktop shortcuts | `docs/evidence/release/T-0192.md` |
| T-0193 | Release verification: release builds, full gate, bench, package/install transcript | `docs/evidence/release/T-0193.md` |

Installed payload and artefact hashes from this wave are **superseded by
T-0202**; use `docs/evidence/release/T-0202.md` for the current package,
installer, and smoke evidence. The final installer hash is added by the
integrator after signing (or as unsigned when the signing blocker remains).

## Honest shell (UI) — 2026-11-20

The v16 Acrobat-style mock-up shell was replaced in T-0203. The viewer starts
on an empty Home screen; demo documents, startup tabs, the proposal page,
comment thread, cloud storage, avatar, notifications, AI answers, left rail,
tools/export cards, print dialog, split/tile, and the
layers/bookmarks/attachments/links panes were removed because nothing real was
behind them. The remaining controls act on a real PDF: native open,
drag-and-drop, path argument, tabs, real thumbnails, navigation, zoom/fit/actual
size/rotate view, find over extracted text, persisted theme and window-control
style, and a dismissible error banner.

| Evidence | Path |
|---|---|
| Honest-shell evidence, five captures, known limitations | `docs/evidence/ui/T-0203.md` |
| Window controls and settings | `docs/evidence/ui/T-0201.md` |
| Post-v16 screenshots | `docs/evidence/ui/t0203-*.png` |
| Dated supersession note for the v16 review | `docs/design/UI_REVIEW.md` |

The v16 screenshots (`light-v16.png`, `dark-v16.png`, `acceptance-b.png`) are
historical review material only; they show removed demo content and are not
evidence of the shipped shell.

## End condition

`cargo xtask goal-verify --all` is **not green** at this head. `cargo xtask
verify` now runs the `ui,pdfium` tests and rejects a feature row whose cited
test does not exist; it passes. The full-goal gate stops on the 115 rows that
are `library` or `model` (AC-04). The gate output recorded for T-0204/T-0205 is
the authoritative result; this file does not re-run or invent it.

The end condition is not met until those rows are wired or replaced and
`goal-verify --all` prints `GOAL VERIFIED`.

## Rule

No row moves to `partial` or `met` without pasted command output or an
in-repo artifact referenced from this table. `cargo xtask goal-verify --all`
remains the final authority.
