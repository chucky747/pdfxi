# Product shell UI review

> **2026-11-20 — historical note.** The v16 mock-up shell described below was
> replaced by the honest shell in T-0203. The viewer now starts on an empty Home
> screen, and the demo documents, proposal page, comment thread, cloud storage,
> avatar, notifications, AI assistant, tools/export cards, left rail,
> print dialog, split/tile panes, and layers/bookmarks/attachments/links panes
> shown in the v16 captures were removed from the product. `light-v16.png`,
> `dark-v16.png`, and `acceptance-b.png` therefore show removed content and are
> not evidence of the shipped UI. Current evidence:
> `docs/evidence/ui/T-0203.md` (five captures and limitations) and
> `docs/evidence/ui/T-0201.md` (window controls and settings). The review
> process and the requirement that every visible control works or is removed
> still apply to the replacement shell.

## Acceptance target

The acceptance target is **image 2**, the light/dark Acrobat-style reference
supplied with the goal. It was recovered from the planning session and is kept
at [`docs/evidence/ui/acceptance-b.png`](../evidence/ui/acceptance-b.png). It is
the visual contract for this review; image 1 is the rejected starting point.

The target has these regions in both themes:

- a light title bar in light mode and a charcoal title bar in dark mode, with
  macOS-style traffic lights, a brand mark, two document tabs, a new-tab button,
  theme control, notifications, help, and an avatar;
- a command bar with undo/redo outside a rounded pill containing Edit, Comment,
  Highlight, Sign, Organize, Export, and an AI Assistant pill;
- a left rail with a brand tile, Home, Files, Recent, Starred, Shared, Pages,
  Comments, and AI;
- a pages panel headed `Pages (12)` with grid/list/add controls, page
  thumbnails, and a cloud-storage footer;
- a document page with a red cover crop, proposal title, executive summary with
  a blue selected-text frame and comment marker, a comment card, key benefits,
  a 38% stat card, a timeline, an E-signed signature block, an investment card,
  and a footer;
- a right tools panel with Edit PDF highlighted, the remaining tool rows, a
  compact AI Assistant card, and an Export card;
- a floating page/zoom pill over the bottom of the canvas.

The shell is the product surface for the verified library and CLI features. It
is not a feature-matrix row and does not replace
`cargo xtask goal-verify --all`.

## Review process

Every shell change follows this loop:

1. Build the release viewer:
   `cargo build --release -p pdfxi-app --bin pdfxi-ui --features ui,pdfium`.
2. Capture both themes with `scripts/capture-ui.ps1`.
3. Inspect both screenshots against the acceptance target and the checklist
   below **using vision**, not code inspection alone.
4. Record a findings row with the build, screenshot, issue, and action.
5. Fix and recapture until every checklist item passes in both themes.
6. Sign the result. A later shell change reopens the review.
7. Run `cargo fmt --all --check`, workspace clippy with `-D warnings`, the app
   tests, and `cargo xtask goal-verify --all` on the clean checkpoint.

A change that compiles but clips a primary action, breaks dark-mode paper
contrast, or loses a region is rejected. The screenshots are the visual gate.

## Checklist (v16)

| Area | Criteria | Light | Dark |
|---|---|---|---|
| Structure | Title bar, command bar, rail, pages panel, canvas, tools panel, and floating page bar are all visible at 1920x1200 physical; no region overlaps | pass | pass |
| Title bar | Traffic lights, brand tile and wordmark, two named tabs, active-tab close, new-tab button, theme toggle, notifications, help, avatar | pass | pass |
| Command bar | Undo/redo, rounded command pill, Edit (active), Comment, Highlight, Sign, Organize, Export menu, AI Assistant pill | pass | pass |
| Rail | Brand tile, Home, Files, Recent, Starred, Shared, Pages, Comments with badge, AI; selected Pages has the pink pill | pass | pass |
| Pages panel | `Pages (12)`, grid/list/add controls, thumbnails with red active border and page numbers, cloud-storage footer and progress | pass | pass |
| Document | Proposal title, cover crop, executive summary with blue selection and comment marker, comment card, red check benefits, 38% stat, timeline, E-signed signature, investment, footer | pass | pass |
| Tools | Edit PDF highlighted; Comment, Fill & Sign, Organize Pages, Export PDF, Convert to PDF, Combine Files, Protect PDF; compact AI and Export cards fully visible | pass | pass |
| Page bar | Floating pill, previous/next, `1 of 12`, zoom menu, zoom out/in, fit | pass | pass |
| Color | Neutral gray light palette; charcoal dark palette; title bar follows the theme; dark paper is genuinely dark; accent reserved for primary actions and selection | pass | pass |
| Contrast | Shell text at least 7:1; muted text at least 4.5:1; paper ink at least 12:1; paper accent at least 3:1; unit tests enforce these | pass | pass |
| Density | Rail 78 pt, pages 206 pt, tools 248 pt; primary actions visible without scrolling at 1920x1200 | pass | pass |
| States | Active tab, active rail item, active page, hover surfaces, comment badge, AI focus, page/zoom state | pass | pass |
| Evidence | `acceptance-b.png`, `light-v16.png`, `dark-v16.png`; capture command is reproducible | pass | pass |

## Findings log

| Round | Finding | Action | Result |
|---|---|---|---|
| R1 (v1) | Default egui shell: gray canvas, no product chrome, no pages or tools panels | Rebuilt as a design system and product shell | Rejected |
| R2 (v4) | Shell present but the Export card was clipped; the native title bar produced double chrome; dark paper reused shell accent backgrounds | Custom window chrome, density pass, and a separate `PaperPalette` | Rejected |
| R3 (v6) | Product chrome and full cards; missing command search, active-tab underline, saved indicator, and comment badge | Added search, tab underline, saved dot, and comment badge | Pass with notes |
| R4 (v8) | Final density and document type pass | Larger proposal type, prepared-for line, 1-reply label, tools subtitle removed | Accepted |
| R5 (v9) | Tool rows were monochrome and the tab read as a fixture | Real document title and color-coded tool icons | Accepted |
| R6 (v10) | Light title bar blended into the body; Acrobat's dark top bar was missing | Added a dark `TitlePalette` and light title controls in both themes | Accepted at v10 |
| R7 (v10 → v11) | The actual acceptance image was recovered. It showed a neutral gray palette, a light title bar in light mode, traffic lights on the left, two tabs, a floating page bar instead of a full-width status bar, a narrower tools column, a genuinely dark page in dark mode, and a 12-page demo | Rebuilt the title bar, command pill, rail, pages panel, page bar, dark `PaperPalette`, AI card, and Export card against the recovered image | Pass |
| R8 (v12 → v16) | Side-by-side vision review: page was too wide, summary was five lines, selection frame was too tall, checks were green, tab labels did not match the reference, and the demo had only three pages | Set the page ratio to 0.67, trimmed the summary to three lines, tightened the selection frame, made checks red, used `Proposal_Final.pdf` and `Quarterly Report.pdf`, set the demo to 12 pages, and reordered the zoom controls | Accepted at v16 |

## Evidence

- [`acceptance-b.png`](../evidence/ui/acceptance-b.png) — the recovered
  acceptance target (image 2), light and dark.
- [`light-v16.png`](../evidence/ui/light-v16.png) — final light shell.
- [`dark-v16.png`](../evidence/ui/dark-v16.png) — final dark shell.

Capture:

```powershell
cargo build --release -p pdfxi-app --bin pdfxi-ui --features ui,pdfium
powershell -NoProfile -File scripts/capture-ui.ps1 `
    -Out docs/evidence/ui/light-v16.png `
    -Exe target/integration/release/pdfxi-ui.exe -Arguments '--light'
powershell -NoProfile -File scripts/capture-ui.ps1 `
    -Out docs/evidence/ui/dark-v16.png `
    -Exe target/integration/release/pdfxi-ui.exe -Arguments '--dark'
```

The capture script uses `PrintWindow` with DPI awareness, so the image is the
viewer surface rather than the desktop behind it.

## Verification

- `cargo test -p pdfxi-app --features ui,pdfium`: **493 passed**, 0 failed.
- `cargo fmt --all --check`: clean.
- `cargo clippy --workspace --all-targets --all-features -- -D warnings`: clean.
- Historical verification of the v16 tree only: `cargo xtask goal-verify --all`: `GOAL VERIFIED:
  00275de2e0cf753c64675a4f9549b85fdf4f62c7`; transcript at
  `docs/evidence/ui/goal-verify.log`. The current matrix gate is intentionally
  not green under ADR 0009; see `docs/evidence/ACCEPTANCE.md`.

## Sign-off

| Date | Reviewer | Scope | Result |
|---|---|---|---|
| 2026-11-20 | UI author + visual review | Shell light/dark, title bar, command pill, rail, pages, document, tools, page bar | Accepted at v16 |

A future shell change must add a new findings row and recapture both themes
before it is accepted.
