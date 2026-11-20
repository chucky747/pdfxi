# Acceptance criteria tracker

Source: `docs/GOAL.md`. A criterion is checked only when the listed command
output or file evidence is current at HEAD. This file is the integrator's
single view of the end condition; task evidence lives in `docs/evidence/`.

Last updated: 2026-11-19 (M3 gate satisfied; 104 verified feature rows of 140; 163 verified tasks).

| ID | Criterion | Status | Evidence | Blocker |
|---|---|---|---|---|
| AC-01 | Reproducible bootstrap | partial | `scripts/bootstrap.ps1`, `scripts/env.ps1`, `docs/decisions/0001-toolchain.md`; `bootstrap.ps1 -Check` result=PASS, missing=0, verified_tools=5 with pinned `stable-x86_64-pc-windows-gnu` (`docs/evidence/bootstrap-check.md`) | Needs a clean-machine dry run record; winget elevation path not exercised |
| AC-02 | Build quality | partial | per-crate clippy `-D warnings` green for all first-party crates; `cargo build --workspace --release` succeeded in 26.21 s (2026-11-19, target/integration) | Workspace-wide fmt/clippy gate re-run at a quiescent checkpoint |
| AC-03 | Tests | partial | unit/integration tests across all lanes; doctests host-limited (ADR-0005/`docs/ASSUMPTIONS.md`); five fuzz targets with seed corpus and a smoke example (T-0081); `cargo xtask corpus smoke` passed 9/9 with manifest hashes (2026-11-19) | Full-profile fuzz run and coverage targets pending |
| AC-04 | Feature parity | partial | `docs/features.toml` 140 rows, 104 verified, 33 in-progress, 2 replaced, 1 planned; `cargo xtask features verify` green (2026-11-19) | 36 rows open |
| AC-05 | Performance | not met | raw timings only (`bench_open` in T-0024/T-0030) | Budgets in `docs/TESTING.md` not yet measured |
| AC-06 | Security | partial | AES/RC4 handlers incl. AES-256 R5/R6 (T-0036, T-0061), parameter validation (T-0018), redaction/sanitization plans (T-0052), content-stream redaction removal (T-0076), CMS SignedData build/verify (T-0048), X.509 chain and digital-ID store (T-0070), timestamp validation (T-0077), metadata/XMP scrubbing (T-0078) | PDF ByteRange signing, revocation, key-store/UI pending |
| AC-07 | Accessibility | partial | reading-order model (T-0013), validated accessibility tree + reading-order export (T-0055), tagged PDF structure writer/reader (T-0075), PDF/UA-style checker (T-0088) | Editor UI, accesskit integration, and independent veraPDF-style cross-validation pending |
| AC-08 | Documentation | partial | rustdoc on all crates, mdBook skeleton, ADRs 0001-0008, `cargo xtask docs` green | CLI reference and full mdBook content pending |
| AC-09 | Orchestration evidence | partial | board, claims, 163 verified tasks, per-task evidence files, memory inbox notes | Every verified row needs a completed task; audit at goal end |
| AC-10 | Packaging and legal | partial | `docs/THIRD_PARTY.md`, `deny.toml`, PDFium license retained out-of-tree; `scripts/package.ps1 -Check` reports layout_and_notices=PASS, secret_free=True (`docs/evidence/package-check.md`) | Portable archive not built from a release binary yet |

## Rule

No row moves to `partial` or `met` without pasted command output or an
in-repo artifact referenced from this table. `cargo xtask goal-verify --all`
remains the final authority.
