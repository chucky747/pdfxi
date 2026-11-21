# PDFXI acceptance campaign - handoff (2026-11-21)

Campaign plan archived at
`docs/plans/archive/2026-11-20-pdfxi-feature-acceptance-e06453b4/` (plan and
goal together).

## Shipped

Installed viewer `%LOCALAPPDATA%\Programs\PDFXI\pdfxi-ui.exe`, SHA-256
`87ccc5a2d962fcc041a5337ba2517b6514f6bb3da693009ce8f0c761eecda97a`:
command bar (Edit, Comment, Highlight, Sign, Organize, Export, AI Assistant),
full Tools panel, Fill & Sign (XFDF/FDF, flatten), signatures with the E-signed
predicate and request-by-email tracker, bookmarks/properties, organize and
combine, eight export targets, convert to PDF, protect/redact/sanitize,
shared-folder review with notifications, and the OpenCode connection panel.

## Acceptance

`docs/design/FEATURE_ACCEPTANCE.md`: 40/40 A-/O- rows `pass`, each with an
evidence path, ticked after the installed-build check and review of
`docs/evidence/ui/final/` (50 captures: four theme/control combinations x main
and every tool, two AcroForm captures, the Print dialog) against
`docs/evidence/ui/acceptance-b.png`.

## Gates

- `cargo xtask verify`: green, including the no-dead-controls test
  (`ui_controls` 35 passed). Full output in `docs/evidence/release/T-0262.md`.
- `cargo deny check`: `advisories ok, bans ok, licenses ok, sources ok`, exit 0.
- `cargo xtask goal-verify --all`: `FEATURES OK: 145 rows, 54 verified` then
  `Error: 89 feature rows are not verified or replaced` (exit 1). Those 89
  rows are outside this campaign's acceptance surface; nothing is claimed as
  verified beyond the matrix.
- `cargo xtask installer-test`: 28/28 assertions, install and uninstall.

## Blockers and limits

Resolved during close-out:

- **Inno installer** (`orchestration/blockers/T-0262-inno-installer.md`):
  the installer aborted because this host's `%TEMP%` denies executing the helper
  Inno extracts there ("Unable to execute file in the temporary directory.
  Setup aborted. Error 5: Access is denied."). Running the installer with
  `TEMP`/`TMP` pointed at `target\tmp\setup-temp` installs, uninstalls and
  reinstalls successfully (all exit 0); the installed hash matches staging
  (`6787a129...` for the final build), the desktop shortcut targets the
  installed binary and `docs\README.md` is present. `installer-test` is 28/28.
- **Office open (A-27)**: Microsoft Word opened the exported `.docx` and
  displayed the edited marker and page break
  (`docs/evidence/ui/final/word-opens-docx.png`). T-0215 still limits producing
  PDFs through Word COM; it does not affect opening the export.
- **Help (A-03)**: the installed viewer was launched with the new `--open-help`
  deep link (T-0273) and captured showing the guide sections and the
  "Open user guide" button (`docs/evidence/ui/final/light-windows-help.png`,
  `dark-macos-help.png`).
- **Printing (O-02)**: with the Brother MFC-L2750DW as default, the installed
  viewer's Print dialog fed the Windows dialog and the job
  `PDFXI document` (1 page) was spooled and transmitted (`JOBS_REMAINING=0`).
  Inspecting the physical sheet is the only human-only step; it is stated in
  the matrix row.

Still open as recorded product limits (not blockers):

- `orchestration/blockers/T-0215-word-com.md`: Word COM PDF save/export hangs
  (route uses the committed PowerPoint fixture; no Word-produced PDF claimed).
- `orchestration/blockers/T-0260A-fuzz-toolchain.md`: libFuzzer is host-blocked;
  deterministic runner 7/7, no target weakened.
- `orchestration/blockers/T-0260B-budget.md`: two derived save-budget breaches,
  unweakened.
- Signed installer: no certificate (T-0202); no signature claimed.
- RSA advisory scoped ignore with guard (T-0241).
- AI assistant: connection panel verified; no live prompt sent.
- mdbook not installed; `cargo xtask docs` and the link pass are green.
- VIEW-009, VIEW-011 and PLAT-002 remain outside this campaign's surface.
