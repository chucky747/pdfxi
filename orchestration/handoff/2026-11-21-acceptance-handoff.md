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
- `cargo xtask goal-verify --all`: `FEATURES OK: 145 rows, 33 verified` then
  `Error: 109 feature rows are not verified or replaced` (exit 1). Those 109
  rows are outside this campaign's acceptance surface; nothing is claimed as
  verified beyond the matrix.
- `cargo xtask installer-test`: 28/28 assertions, install and uninstall.

## Blockers and limits

- `orchestration/blockers/T-0262-inno-installer.md`: real-identity Inno
  installer exits 1 on this host; release installed with the portable
  `install.ps1` (hash-verified, desktop shortcut points at the installed
  binary). Inno install/uninstall proven by the Smoke identity test (28/28).
- `orchestration/blockers/T-0215-word-com.md`: no Word-produced PDF and no Word
  open of an export; T-0260 records the deviation; A-27 carries the limit.
- Installer unsigned (T-0202); no signature claimed.
- RSA advisory scoped ignore with guard (T-0241).
- libFuzzer host-blocked; deterministic runner 7/7, targets unweakened
  (T-0260A).
- Two derived save-budget breaches on record (T-0260B).
- O-02: Windows print dialog reached with the Brother MFC-L2750DW as default;
  no physical sheet observed (no operator on the host), recorded as a limit.
- AI assistant: connection panel verified; no live prompt sent (nothing is sent
  before the user asks); T-0260 records the assistant step as skipped.
- Corpus is 1-3 pages per fixture; encrypted save unsupported by design.
- mdbook not installed; `cargo xtask docs` and the link pass are green.
- VIEW-009, VIEW-011 and PLAT-002 remain outside this campaign's surface.
