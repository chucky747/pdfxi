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

## Campaign checkpoint (2026-11-20, feature acceptance)

Durable resume point for the feature-acceptance campaign. The live state is
`orchestration/BOARD.md` and `orchestration/claims/`; this section is a summary,
not a replacement.

### Verified in this campaign

Phase 1-3 tasks T-0210..T-0256 as shown on the board, plus T-0269 (docs). Key
commits: `8c1f43b` T-0242 comments UI, `a7f75fa` T-0212 harness coverage,
`e3f665e` T-0237 redaction, `b61322e` T-0254 assistant lockdown, `a5801c3` +
`b1e3ed0` T-0269 security/privacy docs, `390443f` Word COM/signing fallbacks,
`b34abd3` T-0242 status fix. Do not re-derive from chat; use the board.

### In flight right now

- T-0243 forms UI (app lane), T-0265 edit transforms (author lane), T-0249 PFX
  identity (security lane) hold claims. T-0212 form-command coverage is being
  extended by QA.
- T-0242, T-0237, T-0254 and T-0269 claims are released and their work is
  committed.

### Remaining queue

- App: T-0243, T-0244, T-0245, T-0246, T-0231, T-0252, T-0253, T-0255, T-0263,
  T-0264, T-0266. T-0263 needs T-0257; T-0264 needs T-0265.
- Security: T-0257 (depends T-0237).
- Follow-ups: T-0267 (signature trust/store/timestamps/revocation), T-0268
  (redaction form XObject and inline images).
- Final: T-0260 (acceptance scenario/fuzz/perf), T-0261 (docs/notices),
  T-0262 (package/install/captures/handoff/archive).

### Acceptance gates

`cargo xtask verify` and `cargo deny check` are the final gates. Full-workspace
clippy is currently blocked only by the in-flight T-0249
`crates/pdfxi-security/src/pkcs12/decrypt.rs` lint; app and library crates are
clean with `--no-deps`. `cargo xtask goal-verify --all` exits 1 with **113
unfinished rows outside the campaign's acceptance surface**; the covered A-/O-
rows must be verified and the output pasted verbatim, not all 145 rows.

### Open blockers

- T-0202 code signing: no certificate, installer unsigned; never claim signed
  binaries.
- T-0215 Word COM save/export: unavailable on this host; the scenario route and
  any A-27 Word check must state what was actually exercised.
- T-0241 RSA advisory: scoped `deny.toml` ignore plus the `rsa_usage_scope`
  guard; revisit if RSA decryption is added or `rsa` is fixed.
- chrono: resolved; remains transitive through `pdfium-render` only, no ignore.

### Next action

Finish T-0243 rounds A/B, then T-0252/T-0253 assistant work, then the remaining
app tasks. Check each lane's output yourself (tests plus a real-fixture or
installed-build check) before checkpointing; do not trust a summary alone.

