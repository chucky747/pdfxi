# PDFXI handoff — 2026-11-20

## Current state

- Repository: https://github.com/chucky747/pdfxi (public)
- Branch: `main`, tracking `origin/main`
- HEAD: `55635fe` (`docs(rules): require a desktop shortcut for every completed viewer build`)
- Worktree: clean
- Last code-checkpoint gate: `00275de2e0cf753c64675a4f9549b85fdf4f62c7`
  (`GOAL VERIFIED`; transcript `docs/evidence/ui/goal-verify.log`)
- UI review: accepted at v16 (`docs/design/UI_REVIEW.md`)

## Completed

- Replaced the default egui shell with the v16 Acrobat-style product shell:
  theme-following title bar with traffic lights and two named tabs, rounded
  command pill, icon rail, pages panel with thumbnails and cloud storage,
  business-proposal page, right tools panel with AI/Export cards, and a floating
  page/zoom pill.
- Recovered acceptance image 2 and stored it at
  `docs/evidence/ui/acceptance-b.png`; final captures are
  `docs/evidence/ui/light-v16.png` and `dark-v16.png`.
- Added the written review process, checklist, findings R1–R8, and sign-off in
  `docs/design/UI_REVIEW.md`.
- Added the desktop-shortcut requirement to `RULES.md` section 5.
- Packaged and installed the latest viewer; the desktop shortcut was verified
  against the new binary and the app was launched.
- Created the public GitHub repository and pushed `main`.

## Pending

- Branch `bug-fixes-and-touch-ups` has **not** been created. `RULES.md` section 3
  forbids branches in the canonical workspace; creating one needs an explicit
  owner exception. Git branch names cannot contain spaces, so the branch name
  must use hyphens.
- Bug-fix and touch-up work has not started.
- Optional visual polish: more architectural cover-art detail, the rail label
  “AI Assistant” instead of “AI”, command-pill width, and page density. None of
  these block the accepted v16 review.

## Resume checklist

1. Read `RULES.md`, `docs/GOAL.md`, `docs/PLAN.md`, `orchestration/BOARD.md`, and
   `docs/ARCHITECTURE.md`.
2. Build the viewer:
   `cargo build --release -p pdfxi-app --bin pdfxi-ui --features ui,pdfium`.
3. Capture both themes with `scripts/capture-ui.ps1` and inspect them against
   `docs/evidence/ui/acceptance-b.png` using vision.
4. Run `cargo fmt --all --check`, workspace clippy with `-D warnings`, the app
   tests, and `cargo xtask goal-verify --all`.
5. If the owner authorizes the branch exception, create
   `bug-fixes-and-touch-ups` through the integrator and keep `main` as the
   release line.
6. After every completed viewer build, run the package/install step with
   `-DesktopShortcut`, verify the shortcut target, and record it in evidence.

## Key files

- `crates/pdfxi-app/src/ui.rs` — shell, title bar, command pill, rail, pages,
  tools, document page, floating page bar.
- `crates/pdfxi-app/src/design.rs` — palettes, title/paper colors, icons,
  metrics, layout and contrast tests.
- `docs/design/UI_REVIEW.md` — acceptance target, process, checklist, findings,
  evidence, sign-off.
- `docs/evidence/ui/` — acceptance image, v16 screenshots, gate transcript.
- `docs/evidence/ACCEPTANCE.md` — acceptance tracker and product-shell section.
- `scripts/capture-ui.ps1` — DPI-aware PrintWindow capture.
- `scripts/package.ps1` and `scripts/install.ps1` — package/install and desktop
  shortcut creation.

## Commands

```text
cargo test -p pdfxi-app --features ui,pdfium          # 493 passed at v16
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo xtask goal-verify --all
cargo build --release -p pdfxi-app --bin pdfxi-ui --features ui,pdfium
powershell -NoProfile -File scripts/capture-ui.ps1 -Out docs/evidence/ui/light-v16.png -Arguments '--light'
powershell -NoProfile -File scripts/capture-ui.ps1 -Out docs/evidence/ui/dark-v16.png -Arguments '--dark'
```

## Notes

- PDFXI branding intentionally differs from Adobe Acrobat; the shell follows the
  layout and visual language of the acceptance image without copying Adobe
  assets, text, or trademarks.
- Installed viewer: `%LOCALAPPDATA%\Programs\PDFXI\pdfxi-ui.exe`; desktop
  shortcut: `%USERPROFILE%\Desktop\PDFXI.lnk`.
- No secrets, tokens, raw prompts, or private documents are stored in this
  handoff.
