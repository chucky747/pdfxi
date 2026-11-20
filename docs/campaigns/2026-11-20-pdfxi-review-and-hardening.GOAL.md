# GOAL — PDFXI review and hardening (2026-11-20)

End condition (from `2026-11-20-review-and-hardening.md`, "Done when"):

1. Every task above is reviewed, checkpointed on `main`, and its claim released.
2. `cargo fmt --all --check`, workspace clippy with `-D warnings`, and `cargo test -p pdfxi-app --features ui,pdfium` pass.
3. The installer installs a GUI-subsystem `pdfxi-ui.exe` with icon and bundled PDFium; the desktop shortcut targets the new binary; uninstall works.
4. Screenshots exist for light and dark themes in both window-control styles, taken from the installed build with a real PDF open, and the orchestrator has reviewed them.
5. No demo or placeholder content is reachable in the shipped UI.
6. `cargo xtask goal-verify --all` output is pasted as evidence, whatever it prints; if matrix downgrades stop it printing GOAL VERIFIED, the remaining rows are listed as follow-up.
7. Open blockers (code signing) are recorded in `orchestration/blockers/`.

Tasks: T-0200, T-0201, T-0202, T-0203, T-0204, T-0205, T-0206 (`orchestration/tasks/`).
