# PDFXI plan — review findings, window controls, packaging, hardening (2026-11-20)

Workspace: the repository root, branch `main` (no branches; RULES §3). Base commit `ecd1033`.
Workers: `opencode-go/deepseek-v4.1-flash` lane agents; orchestrator reviews every task and does the final visual review.

## Goal
A viewer that is honestly packaged and honestly labelled: Windows-style window controls with a toggle to the macOS style, persisted light/dark theme, a real installer, PDFium shipped and loaded safely, every visible control either working or disabled, no placeholder fixtures or demo material anywhere in the shipped UI, and a gate that actually runs the UI tests.

## Findings driving the plan
- Title bar is custom with macOS traffic lights hard-coded (`crates/pdfxi-app/src/ui.rs:728`, `3541-3572`).
- Theme is in-memory only, defaults to Light, not persisted, does not follow the system. Light-mode toggle glyph is garbled; dark-mode signature curve invisible.
- Packaging: console-subsystem exe, no icon/version resource, no installer or uninstall entry, `pdfium.dll` not shipped, PDFium fetched from `releases/latest` unpinned, unsigned, installed exe likely older than `00275de`, T-0192 evidence hash stale, `Resolve-TargetDir` defaults to `target\lanes\qa`.
- Security: PDFium loaded from `PDFIUM_PATH`/CWD (DLL planting, `crates/pdfxi-render/src/pdfium_backend.rs:71-126`); whole-file read with no size cap (`:219`).
- Honesty: startup tabs, proposal page, comment thread, cloud storage, avatar, notifications and AI answers are painted or canned; find/selection use demo text even for real PDFs; no native file picker; Undo/Redo/Export/most tool rows only set a status string.
- Gate: `goal-verify` runs default-feature tests, so the UI tests and all PDFium tests are skipped; it does not check that cited tests exist (16 of 1,180 missing).
- Docs: `docs/book/src/gui.md`, README, `security.md`, `getting-started.md` describe things that do not work or contradict `docs/features.toml`.

## Tasks
### T-0200 Baseline (integrator)
Build `pdfxi-ui` with `ui,pdfium`; run `cargo test -p pdfxi-app --features ui,pdfium` and `cargo test -p pdfxi-render --features pdfium`; record results in `docs/evidence/hardening/`.

### T-0201 Window controls and settings (lane-app)
- `WindowControlStyle { Windows, MacOs }`; default Windows on Windows. Windows style: minimize, maximize/restore, close at the right edge, close hover red, restore glyph when maximized. macOS style: the existing lights on the left.
- Toggle in a settings menu in the title bar; `--controls=windows|macos` flag.
- Double-click on the title bar toggles maximize.
- Persist theme and control style to `%APPDATA%\PDFXI\settings.toml`; first run follows the Windows system theme; missing or corrupt file falls back to defaults without crashing.
- Fix the light-mode theme-toggle glyph.
- Tests, only these: settings round-trip including a corrupt file; control style mapped to button order and viewport command; flag parsing. Remove the `toggled()` tautology test.

### T-0202 Packaging (lane-qa)
- `#![windows_subsystem = "windows"]` for release; startup errors go to a log file and a message box.
- Embedded icon, version resource, per-monitor-v2 DPI manifest.
- PDFium pinned to a version and SHA-256, bundled with its licence next to the exe, loaded from the exe directory only (developer override only in debug builds). Stop appending to the user `Path`.
- Per-user installer (Inno Setup) with uninstall entry and shortcuts; keep the portable zip. Fix `Resolve-TargetDir` and the manifest binaries list.
- Smoke: install, launch, open a real PDF, uninstall.
- Code signing: no certificate exists; record as an open blocker, never claim it.

### T-0203 Honest shell (lane-app, after T-0201)
- Native file picker for Open; input file size cap with a clear error.
- Find and selection use real PDFium text for the open document.
- Start on an empty Home state. Remove every piece of demo or placeholder material from the shipped UI: fake tabs, proposal page, comment thread, cloud storage, avatar, notifications, canned AI answers, demo text. No sample document.
- Any control without an implementation is removed or disabled with an explanatory tooltip.

### T-0204 Gate and critical tests (lane-qa + integrator)
- `xtask verify` and CI run the `ui,pdfium` tests; goal-verify fails when a cited test does not exist.
- One redistributable real-world PDF through open → text → save → reopen.
- JS wall-clock limit test with a real busy loop.

### T-0205 Honest feature matrix (docs + integrator)
Downgrade rows that are in-memory models with no GUI or CLI caller to an honest status, with an ADR. End-to-end redaction and signature wiring is recorded as follow-up work.

### T-0206 Docs (docs-writer)
`gui.md` (theme, window controls, settings, open), README, install page including PDFium, `security.md` versus the matrix, `getting-started.md`, refreshed T-0192/ACCEPTANCE evidence, HANDOFF.

## Order
T-0200 first. T-0201 and T-0202 in parallel. T-0203 after T-0201. T-0204 before close. T-0205/T-0206 trail.

## Done when
1. Every task above is reviewed, checkpointed on `main`, and its claim released.
2. `cargo fmt --all --check`, workspace clippy with `-D warnings`, and `cargo test -p pdfxi-app --features ui,pdfium` pass.
3. The installer installs a GUI-subsystem `pdfxi-ui.exe` with icon and bundled PDFium; the desktop shortcut targets the new binary; uninstall works.
4. Screenshots exist for light and dark themes in both window-control styles, taken from the installed build with a real PDF open, and the orchestrator has reviewed them.
5. No demo or placeholder content is reachable in the shipped UI.
6. `cargo xtask goal-verify --all` output is pasted as evidence, whatever it prints; if matrix downgrades stop it printing GOAL VERIFIED, the remaining rows are listed as follow-up.
7. Open blockers (code signing) are recorded in `orchestration/blockers/`.
