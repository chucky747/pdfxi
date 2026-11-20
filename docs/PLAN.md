# PLAN

This is the executable plan. `docs/GOAL.md` is the end contract;
`docs/features.toml` is the parity matrix; `docs/ARCHITECTURE.md` freezes crate
boundaries; `orchestration/BOARD.md` is live state.

## 1. Product decisions

- Name: PDFXI. Clean-room; no Adobe code, assets, services, or trademarks.
- Windows first, then macOS/Linux.
- Rust 2024, pinned stable toolchain, rustfmt, clippy.
- Core: `pdfxi-core` owns the object model and parser; `pdfxi-render` wraps
  PDFium for fidelity; `pdfxi-edit` owns content and page mutation.
- UI: `egui`/`eframe` + `wgpu` + `accesskit`, dockable panels, tabbed documents,
  custom PDF canvas.
- OCR: Tesseract sidecar behind an `OcrEngine` trait.
- Conversion: LibreOffice/headless-browser adapters behind `Converter` traits.
- Security: CMS/PKCS#7, X.509, RFC 3161, AES/RC4 handlers, QuickJS sandbox.
- Retired Adobe cloud services are replaced by local/self-hosted equivalents.
- License: MIT OR Apache-2.0 for original code; notices for third parties.

## 2. Milestones

### M0 — Bootstrap

Deliver repository rules, global memory, OpenCode configuration, agent roster,
task board, claims, goal controller, workspace skeleton, CI, `xtask`, and
`goal-verify`. Exit: `cargo xtask goal-verify --milestone M0`.

### M1 — Vertical slice

Parse headers, xref tables/streams, object graph, filters; render pages through
PDFium; open/navigate/zoom/search/select/copy text; CLI `info`, `text`, and
`render`; save round-trip; 50 generated fixtures; first benchmarks. Exit: M1
gate plus a 500-page open under 1.5 s and first page under 300 ms.

### M2 — Authoring

Create from images/text/HTML; combine/split/extract/reorder/rotate/crop; page
labels; annotations and comments; bookmarks, links, attachments; metadata;
export images/text; print. Exit: M2 gate.

### M3 — Editing, forms, security

Text/image editing with reflow; find/replace; forms fill/create; field
calculations and validation; FDF/XFDF; flattening; signatures; redaction and
sanitization; JS subset. Exit: M3 gate plus security negative tests.

### M4 — Standards and print production

AES-256 and certificate encryption; signature validation and trust; PDF/A,
PDF/X, PDF/E, PDF/VT; preflight profiles and fixups; ICC color; output preview;
optimize/compress; page boxes and printer marks. Exit: M4 gate plus veraPDF
corpus.

### M5 — Conversion and integration

Word/Excel/PowerPoint/HTML/RTF/text export; Office and LibreOffice import; OCR
with suspect-character review; TWAIN/WIA scanning; Windows print driver; browser
capture; portfolios. Exit: M5 gate.

### M6 — Professional workflows

Action Wizard, batch, droplets, watched folders, JS automation; review
workflows; accessibility; compare; measurement; layers; 3D/multimedia; advanced
print production. Exit: M6 gate plus accessibility audit.

### M7 — Release

Fuzzing, performance, packaging, installers, full parity audit, third-party
licenses, reproducible build, release notes. Exit: `goal-verify --all`.

## 2a. Campaign status

Updated 2026-11-19 by the integrator; the board and `docs/features.toml` remain
the authoritative live state.

| Milestone | Gate row status | Notes |
|---|---|---|
| M0 | satisfied | bootstrap, toolchain, xtask, board, goal controller, memory |
| M1 | satisfied | viewer slice, CLI, save round-trip, fixtures, first benchmarks |
| M2 | satisfied | authoring, page operations, annotations, export images/text, print |
| M3 | satisfied | editing, forms, signatures, redaction, security negative tests |
| M4 | open | standards rows STD-005..STD-010 and EXPORT-008 in progress |
| M5 | open | CREATE-010 package assembly, export rows, PLAT-003/PLAT-004 |
| M6 | open | VIEW-013/VIEW-015 and ACCESS-001 in progress |
| M7 | open | PLAT-002 cross-platform evidence, then `goal-verify --all` |

Row verification follows `docs/ASSUMPTIONS.md` item 12: a row is verified when
every library-level capability it names is implemented, tested, documented, and
evidenced, with host-only integration listed verbatim in its notes.

## 3. Lanes

| Lane | Owns | Focus |
|---|---|---|
| core | `pdfxi-core`, `pdfxi-cli` | object model, parser, xref, filters, save |
| render | `pdfxi-render`, `pdfxi-text` | PDFium, tiles, text, search, OCR bridge |
| author | `pdfxi-edit`, `pdfxi-accessibility` | page/content edits, annotations, tags |
| forms | `pdfxi-forms` | AcroForm, FDF/XFDF, calculations, flatten |
| security | `pdfxi-security` | crypto, signatures, redaction, sanitize |
| convert | `pdfxi-convert`, `pdfxi-standards` | import/export, preflight, PDF/A/X |
| app | `pdfxi-app` | UI shell, canvas, panels, toolsets |
| automation | `pdfxi-automation` | actions, batch, JS sandbox, plugins |
| qa | `pdfxi-testkit`, fixtures, corpus, fuzz, benches | verification |
| reviewer | none | read-only correctness/security review |
| integrator | shared manifests, git | snapshots, scoped builds |
| memory-librarian | global memory | curation and pruning |

Orchestrator: DeepSeek V4.1 Flash, 500k context, dispatch and review. Workers:
Hy4 Flash Free, fallback LongCat 2.5 Free and Beam Free.

## 4. No-checkout concurrency

- One canonical workspace. No clones, worktrees, branches, checkouts, switches,
  resets, stashes, or destructive clean operations.
- Tasks declare `owns` globs. `cargo xtask claim` is atomic and rejects overlap.
- Shared files are integrator-only: manifests, lockfile, `.opencode`, `xtask`,
  `orchestration`, GOAL/PLAN/features.toml.
- Lane builds use `CARGO_TARGET_DIR=target/lanes/<lane>`.
- Integrator snapshots verified task paths with `checkpoint(<milestone>)`
  commits. No merges.
- Up to five lanes run asynchronously. The orchestrator dispatches in parallel,
  reviews every completed task, and never polls.

## 5. Goal controller

`scripts/goal.ps1` arms and resumes the native OpenCode goal through
`/api/session/{id}/command` and reads it through `/api/rpc/opencode.goal/get`.
It resumes after the native 50-round cap, advances campaigns on success, re-arms
with failure evidence when `goal-verify` fails, and records external blockers.
Context is capped at 588,000 tokens with a 15% compaction buffer, so the
compaction ceiling is ~499,800 tokens.

## 6. Testing and performance

Fast tests under 90 seconds; full under 15 minutes; nightly corpus/fuzz.
Property tests for round-trips; golden render tests with SSIM; CLI tests;
`egui_kittest` UI tests; veraPDF as an oracle; 24-hour fuzz before release.
Benchmarks gate >10% regressions. See `docs/TESTING.md`.

## 7. Risks and mitigations

| Risk | Mitigation |
|---|---|
| PDFium native dependency | Bundle per-platform, pool instances, trait fallback |
| Office/print driver/3D scope | Adapter traits, local equivalents, milestone gates |
| Hy4 free rate limits | Five-lane cap, LongCat/Beam fallbacks, small tasks |
| Shared-workspace races | Claims, ownership, contract freeze, per-lane targets |
| Native goal round cap | Goal controller resumes and advances campaigns |
| Windows toolchain missing | `scripts/bootstrap.ps1` with portable fallbacks |
| Scope creep | Feature matrix statuses, no silent deferrals |
| Legal | Clean-room policy, license allowlist, notices |
