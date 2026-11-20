# Product shell UI review

## Acceptance target

The acceptance target is the modern Acrobat-style reference supplied with the
goal (image 2). It has a light and a dark layout with these regions:

- a custom title bar with document tabs and window controls;
- a left icon rail: Home, Files, Recent, Starred, Shared, Pages, Comments, AI;
- a pages panel with page thumbnails;
- a rich document page (business proposal);
- a right tools panel with Edit PDF highlighted, Comment, Fill & Sign, Organize
  Pages, Export PDF, Convert to PDF, Combine Files, Protect PDF, an AI Assistant
  card, and an Export card with a format picker and a primary Export action;
- a bottom page/zoom bar.

The shell is the product surface for the verified library and CLI features. It
is not a feature-matrix row and does not replace
`cargo xtask goal-verify --all`.

## Review process

Every shell change follows this loop:

1. Build the release viewer:
   `cargo build --release -p pdfxi-app --bin pdfxi-ui --features ui,pdfium`.
2. Capture both themes with `scripts/capture-ui.ps1`.
3. Inspect both screenshots against the acceptance target and the checklist
   below using vision, not code inspection alone.
4. Record a findings row with the build, screenshot, issue, and action.
5. Fix and recapture until every checklist item passes in both themes.
6. Sign the result. A later shell change reopens the review.

A change that compiles but clips a primary action, breaks dark-mode paper
contrast, or loses a region is rejected. The screenshots are the visual gate.

## Checklist (v8)

| Area | Criteria | Light | Dark |
|---|---|---|---|
| Structure | Title bar, command bar, rail, pages panel, canvas, tools panel, and status bar are all visible at 1280x800 logical; no region overlaps | pass | pass |
| Title bar | Logo, wordmark, active tab with close, new-tab button, saved indicator, Share, theme toggle, help, notifications, avatar, window controls | pass | pass |
| Command bar | Back/forward, Edit, Comment, Fill & Sign, Organize, Export, print, find, search field, AI Assistant pill | pass | pass |
| Rail | Home, Files, Recent, Starred, Shared, Pages, Comments with badge, AI; selected Pages has an accent tint and left accent bar | pass | pass |
| Pages panel | Three page thumbnails, active page border, page numbers, local-document footer, no clipped label | pass | pass |
| Document | Proposal title, cover, executive summary, comment card, key benefits, stat card, timeline, signature, investment, footer | pass | pass |
| Tools | Edit PDF highlighted; Comment, Fill & Sign, Organize Pages, Export PDF, Convert to PDF, Combine Files, Protect PDF; AI and Export cards fully visible | pass | pass |
| Status bar | Page pill, zoom pill, fit, full screen, on-device indicator, status text | pass | pass |
| Color | Light and dark palettes; paper ink independent of the shell palette; accent reserved for primary actions | pass | pass |
| Contrast | Shell text at least 7:1; muted text at least 4.5:1; paper ink at least 12:1; paper accent at least 3:1; unit tests enforce these | pass | pass |
| Density | Rail 68 pt, pages 218 pt, tools 300 pt; primary actions visible without scrolling at 1280x800 | pass | pass |
| States | Active tab underline, active rail item, active page, hover surfaces, empty panels, saved dot, comment badge | pass | pass |
| Evidence | `light-v8.png` and `dark-v8.png`; capture command is reproducible | pass | pass |

## Findings log

| Round | Finding | Action | Result |
|---|---|---|---|
| R1 (v1) | Default egui shell: gray canvas, no product chrome, no pages or tools panels | Rebuilt as a design system and product shell | Rejected |
| R2 (v4) | Shell present but the Export card was clipped; the native title bar produced double chrome; dark paper reused shell accent backgrounds | Custom window chrome, density pass, and a separate `PaperPalette` | Rejected |
| R3 (v6) | Product chrome and full cards; missing command search, active-tab underline, saved indicator, and comment badge | Added search, tab underline, saved dot, and comment badge | Pass with notes |
| R4 (v8) | Final density and document type pass | Larger proposal type, prepared-for line, 1-reply label, tools subtitle removed | Accepted |

## Evidence

- `docs/evidence/ui/light-v8.png` — light theme, 1920x1200 physical at 125%
  scaling (1536x960 logical).
- `docs/evidence/ui/dark-v8.png` — dark theme, same viewport.
- Capture:
  ```powershell
  powershell -NoProfile -File scripts/capture-ui.ps1 `
      -Out docs/evidence/ui/light-v8.png `
      -Exe target/integration/release/pdfxi-ui.exe
  powershell -NoProfile -File scripts/capture-ui.ps1 `
      -Out docs/evidence/ui/dark-v8.png `
      -Exe target/integration/release/pdfxi-ui.exe -Arguments '--dark'
  ```

The capture script uses `PrintWindow` with DPI awareness, so the image is the
viewer surface rather than the desktop behind it.

## Verification

- `cargo test -p pdfxi-app --features ui,pdfium`: 490 passed.
- `cargo clippy --workspace --all-targets --all-features -- -D warnings`: clean.
- `cargo fmt --all --check`: clean.
- `cargo xtask goal-verify --all`: `GOAL VERIFIED:
  d97ace5e224b84b07d510a829518700989fe55e2`; transcript at
  `docs/evidence/ui/goal-verify.log`.

## Sign-off

| Date | Reviewer | Scope | Result |
|---|---|---|---|
| 2026-11-20 | UI author + visual review | Shell light/dark, pages, tools, document, status | Accepted at v8 |

A future shell change must add a new findings row and recapture both themes
before it is accepted.
