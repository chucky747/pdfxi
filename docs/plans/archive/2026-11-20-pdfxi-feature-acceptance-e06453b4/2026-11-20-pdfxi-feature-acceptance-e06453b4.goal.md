# Goal — PDFXI features up to the acceptance image

Plan: `2026-11-20-pdfxi-feature-acceptance-e06453b4.md` (same folder).

Implement Phases 0–4 of the plan on `main`, orchestrating
`opencode-go/deepseek-v4.1-flash` lane agents in small rounds, with the
orchestrator (Fable 5.2) reviewing every task itself.

Done when all seven conditions in the plan's §8 hold:

1. Every A- and O- item of the acceptance matrix is ticked by the orchestrator
   after its own check on the installed build, with evidence paths.
2. `cargo xtask verify` (including the no-dead-controls test) and
   `cargo deny check` pass on a clean tree.
3. The installer installs the build, the desktop shortcut targets it, and the
   automated installer test exercises uninstall.
4. Installed-build captures with real documents exist for light/dark ×
   Windows/macOS controls covering each tool, reviewed by the orchestrator
   against `docs/evidence/ui/acceptance-b.png`.
5. No demo, placeholder or canned content is reachable.
6. Covered feature-matrix rows are `verified` with a GUI caller and a real-PDF
   test; `cargo xtask goal-verify --all` output is pasted with the new counts.
7. Open blockers and stated limits are recorded.

AI Assistant test model: `opencode-go/hy4-flash-free`.
