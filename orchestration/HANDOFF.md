# PDFXI handoff - 2026-11-20

## Current state

- Repository: https://github.com/chucky747/pdfxi (public), branch `main`, no branches.
- HEAD: the commit that added this file; check `git log -1 --oneline`. The release
  evidence commit immediately before it is `91c2639`.
- Worktree: clean when this file was committed (docs-writer's T-0206 changes are in
  the same commit).
- Features: 143 rows - 25 `verified`, 18 `library`, 96 `model`, 3 `replaced`,
  1 `in-progress`.
- `cargo xtask verify`: green (fmt, clippy with `-D warnings`, workspace tests, the
  `ui,pdfium` tests, the PDFium render tests, rustdoc, docs check, citation check).
- `cargo xtask goal-verify --all`: intentionally **red**. It stops with
  `115 feature rows are not verified or replaced` and does not print
  `GOAL VERIFIED`, per ADR 0009.
- Installed build: `986c550` (built before this evidence commit). Installer and
  portable hashes, install layout, shortcut targets, smoke and captures are in
  `docs/evidence/release/T-0202-final-install.md`.
  - setup `pdfxi-setup-0.1.0-windows-x86_64.exe` sha256
    `72e4a613fe6b68b73cecb96bb774b7e7c18705ae9b3bb1ce82679c9475daf91a`
  - portable `pdfxi-portable-0.1.0-windows-x86_64.zip` sha256
    `1171f797ebd51cc01fd2e0df0e5903cf87b222705013049ce13a624fe1be0fb0`
  - installed `pdfxi-ui.exe` sha256
    `B8C82C134CF1428F26EC360C68E93933876DF5EC029D594CC6724975D6BB3ABD`, equals the
    `target/integration/release` build, PE subsystem 2.
- Unsigned: `orchestration/blockers/T-0202-code-signing.md`.

## What this campaign did

- **T-0200** - baseline: release viewer build plus `ui,pdfium` and PDFium test
  suites on the campaign base commit (`docs/evidence/hardening/T-0200.md`).
- **T-0201** - window controls: Windows caption buttons on the right or macOS
  traffic lights on the left, title-bar settings, persisted light/dark/system
  theme and `--controls`/`--light`/`--dark` flags
  (`docs/evidence/ui/T-0201.md`).
- **T-0202** - packaging: GUI-subsystem exe with icon/version/DPI manifest,
  pinned PDFium chromium/8165 loaded from the exe directory, portable zip and
  per-user Inno Setup installer, signing recorded as a blocker
  (`docs/evidence/release/T-0202.md`, final install in
  `T-0202-final-install.md`).
- **T-0203** - honest shell: empty Home, native open, real page sizes, real
  thumbnails and PDFium text for find/selection; all demo and plan-only content,
  including Print and split/tile, removed (`docs/evidence/ui/T-0203.md`).
- **T-0204** - gate: `xtask verify` and CI run the `ui,pdfium` tests; `features
  verify` resolves every cited test against a `#[test]` index; real-world Excel
  PDF fixture; JS wall-clock busy-loop test (`docs/evidence/hardening/T-0204.md`).
- **T-0205** - honest matrix: ADR 0009 adds `library` and `model`; the matrix now
  reflects end-user reachability (`docs/evidence/hardening/T-0205.md`).
- **T-0206** - docs: README, mdBook pages, install page, security page,
  ACCEPTANCE, T-0192 banner, UI review note and this handoff match the shipped
  viewer (`docs/evidence/hardening/T-0206.md`).
- **T-0207** - core: incremental save accepts cross-reference-stream sources and
  appends a matching section; the Excel hybrid fixture round-trips with text
  intact (`docs/evidence/hardening/T-0207.md`).

## Open items

1. **Code signing** - no certificate. Blocker:
   `orchestration/blockers/T-0202-code-signing.md`. Never claim signed binaries.
2. **cargo-deny bans** - `cargo deny check` reports `advisories ok, bans FAILED,
   licenses ok, sources ok`; the bans failure is wildcard `path` dependencies.
   Blocker: `orchestration/blockers/cargo-deny-bans.md`. `deny.toml` unchanged.
3. **115 rows not verified** - 18 `library`, 96 `model`, 1 `in-progress`, minus
   3 `replaced`; the campaign's end condition is not met. Follow-ups are listed in
   ADR 0009 (end-to-end redaction, production signature crypto, forms/CLI and
   GUI wiring, PRINT titles, split bundled rows).
4. **PKG-003 is `in-progress`** - the installer is smoke-tested manually but has
   no automated test; add one before promoting the row.
5. **Password-protected PDFs** - unsupported; detection is a lowercase
   `"password"` string check because pdfium-render exposes no typed load error.
6. **Full rewrite paths** - `save()`, `optimize()` and page-plan output still
   reject xref-stream sources; only incremental save was extended (T-0207).
7. **Find coverage** - progressive: the first 8 pages on open, then two pages per
   frame, so very large documents become searchable gradually.
8. **Native file picker** - implemented with `rfd`, not verified interactively.
9. **`rfd` without `ui`** - `rfd` is a hard dependency, so the non-`ui` build
   warns `unused dependency rfd`; make it optional under the `ui` feature.
10. **mdBook build** - not run; mdbook is not installed. Only `cargo xtask docs`
    (page existence) was checked.
11. **Real install uninstaller** - not exercised in the final round; only the
    isolated `Smoke` AppId install/uninstall in T-0202 R4.2 covers uninstall.

## Resume checklist

Run from the repository root, Windows, PowerShell:

    git status --short                 # must be empty before gating
    cargo fmt --all --check
    cargo clippy --workspace --all-targets --all-features -- -D warnings
    cargo test --workspace
    cargo test -p pdfxi-app --features ui,pdfium
    cargo test -p pdfxi-render --features pdfium
    cargo xtask verify
    cargo xtask goal-verify --all      # expected red: 115 rows, per ADR 0009

Packaging and install (from a clean tree; release build commit first):

    cargo build --release -p pdfxi-app --bin pdfxi-ui --features ui,pdfium
    powershell -NoProfile -File scripts/package.ps1 -Installer -TargetDir target/integration
    powershell -NoProfile -File scripts/package.ps1 -SelfTest
    # then run the produced pdfxi-setup-*.exe /VERYSILENT /SUPPRESSMSGBOXES
    # /CURRENTUSER /NORESTART /TASKS="desktopicon" and verify the Desktop shortcut.

TEMP workaround for restricted agent sessions: Inno Setup (ISCC, the setup exe and
the uninstaller) and `cargo install` refuse to execute helpers from the default
`%TEMP%` with `Access is denied (os error 5)`. Before running them, set:

    $env:TEMP = "$PWD\target\tmp\inno-temp"; $env:TMP = $env:TEMP; New-Item -ItemType Directory -Force $env:TEMP

Captures from the installed build:

    powershell -NoProfile -File scripts/capture-ui.ps1 `
      -Exe "$env:LOCALAPPDATA\Programs\PDFXI\pdfxi-ui.exe" `
      -Out docs/evidence/ui/final-doc-light-windows.png `
      -Arguments '"<repo>\fixtures\realworld\microsoft-excel-2019-sample.pdf" --light --controls=windows'
