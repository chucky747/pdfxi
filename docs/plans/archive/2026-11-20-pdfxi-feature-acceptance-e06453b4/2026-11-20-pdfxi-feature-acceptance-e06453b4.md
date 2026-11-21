# PDFXI plan — features up to the acceptance image

Date: 2026-11-20. Repository: PDFXI (`main`, no branches; RULES §3).
Predecessor: `docs/campaigns/2026-11-20-review-and-hardening.md` (T-0200..T-0207, done).
Acceptance image: `docs/evidence/ui/acceptance-b.png` (already in the repo).

## 1. Goal

Every feature visible in the acceptance image, plus the owner's list (editing,
commenting, form filling, signing, redaction, export, printing, saving,
bookmarks, password-protected PDFs), works in the installed viewer on real PDFs,
with no dead controls and no placeholder content. The orchestrator signs off
each item of the acceptance matrix (§5) personally.

The image is the acceptance for **features and layout**. It is a third-party
mock-up: PDFXI keeps its own name, mark, icons and wording (RULES §12). Sample
content in the image (proposal text, names, numbers) is never shipped.

## 2. Owner decisions (2026-11-20)

| Topic | Decision |
|---|---|
| AI Assistant | Backend is OpenCode. PDFXI connects to the user's OpenCode and uses whatever providers they already have. Model/provider is switchable as in OpenCode. Test model: `opencode-go/hy4-flash-free`. A Connect screen establishes the app connection via MCP or a plugin. |
| Files / Recent / Starred / Shared / storage / avatar / notifications | Local equivalents: local library folder, locally saved recents and stars, a user-chosen shared folder, storage meter of real folder usage, notifications for real local events, a local display name (no account, no server). |
| E-Sign | Local signing (drawn/typed/image appearances and real certificate signatures) plus "request signature" by email through the user's mail client with a local tracker. No hosted service. |
| Builders | Orchestrator: Fable 5.2, which also reviews. Workers: `opencode-go/deepseek-v4.1-flash` lane agents. |

## 3. Build rules for the orchestrator

1. Read `RULES.md`, `orchestration/HANDOFF.md`, ADR 0009 and this plan first.
   Work on `main`; claims before edits; the integrator is the only git writer.
2. Workers have a per-run step cap. Dispatch **small rounds**: at most five
   concrete items, ending at a compiling, tested state; evidence files last.
3. Review every task yourself before it is checkpointed:
   - read the diff, not the report;
   - rerun the task's acceptance commands in `target/integration`;
   - for anything that writes a PDF, reopen the output with PDFium (an
     independent reader) and check the change is present and nothing else broke;
   - look at every screenshot yourself — a lane's description of a picture is
     not evidence (last campaign a lane reported "thin and crisp" icons that
     were blobs);
   - search the diff for stubs: handlers that only set a status string,
     canned data, `todo`, invented sample content;
   - security-sensitive tasks (signing, redaction, encryption, AI) get a
     line-by-line review and negative tests (RULES §8).
4. A feature-matrix row becomes `verified` only with a GUI or CLI caller and a
   test on a real PDF (ADR 0009). Never weaken a gate to pass it.
5. A control appears in the UI only when its command works. Until a feature's
   task lands, its control is absent (capability registry, T-0220). The
   "no dead controls" test (T-0212) must be green at every checkpoint.
6. Tests: only for behaviour a user depends on. Each feature gets one
   end-to-end test on real PDF bytes through the same command the button uses,
   plus negative tests where input is hostile. No constant or tautology tests.
7. Each phase ends with a gate: `cargo xtask verify`, `cargo deny check`,
   package + install with the desktop shortcut verified, captures from the
   installed build in light/dark × Windows/macOS controls, orchestrator review.
8. Restricted agent sessions cannot execute from the default temp folder: set
   `TEMP`/`TMP` to a folder under `target/tmp` for Inno Setup and
   `cargo install`.
9. No absolute user paths, usernames or drive letters in tracked files.

## 4. Architecture decisions (record as ADRs in Phase 0)

**ADR 0010 — one document session, one write path.**
- New crate `pdfxi-document`: owns the file bytes, the `pdfxi-core` `Document`,
  the password, dirty state and undo/redo. The app talks only to it.
- Write path is `pdfxi-core`. The `pdfxi-edit`, `pdfxi-forms` and
  `pdfxi-security` crates are refactored from caller-supplied models to operate
  on a real `Document` through this session.
- Every mutation is appended as an incremental update, then PDFium is reloaded
  from memory. Undo is truncation to the previous revision length; redo
  re-appends. Signatures stay valid because earlier bytes never change.
- Exceptions that require a full rewrite: redaction, sanitize, optimize,
  removing encryption. These never keep an earlier revision.
- Saves are atomic (temp file + rename) and detect external modification.
- Escape hatch: if a lane cannot get a `pdfxi-core` mutation through the PDFium
  interop check within its rounds, the orchestrator may approve PDFium's own
  editing API for that one feature by ADR. PDFium is already bundled.

**ADR 0011 — AI Assistant through OpenCode.** See Phase 3.

**Known facts the plan relies on** (checked 2026-11-20):
- `crates/pdfxi-app` depends only on `pdfxi-render` and `pdfxi-text`;
  `pdfxi-edit`, `pdfxi-forms`, `pdfxi-standards` have no dependents.
- `pdfxi-core` full `save()`, `optimize()` and page plans still reject
  cross-reference-stream and encrypted sources (`crates/pdfxi-core/src/lib.rs`
  around lines 926, 936, 1588).
- `pdfium-render` 0.9.5 has a typed password error:
  `PdfiumError::PdfiumLibraryInternalError(PdfiumInternalError::PasswordError)`.
  The current string heuristic must be replaced with it.
- Signature code has only a test-only toy RSA (`crates/pdfxi-security/src/cms.rs`).
- `crates/pdfxi-app/src/ui.rs` is one 3,000-line file; `home.rs`,
  `shared_folder.rs`, `cross_file.rs`, `print.rs`, `bookmarks.rs` exist as
  models with no UI caller.
- The build host has Word, Excel and PowerPoint (COM) and the
  "Microsoft Print to PDF" printer, usable for validation tests.
- `egui_kittest` is not yet a dependency (egui is 0.33.3).

## 5. Acceptance matrix

The integrator commits this table as `docs/design/FEATURE_ACCEPTANCE.md` in
T-0210 and the orchestrator ticks items only after its own check.

| ID | Image element / owner item | Passes when | Task |
|---|---|---|---|
| A-01 | Title bar: tabs, new tab, window controls | Already shipped; still works in both control styles | regression |
| A-02 | Bell (notifications) | Lists real local events; empty state when none | T-0221 |
| A-03 | Help | Opens the bundled user guide | T-0221 |
| A-04 | User avatar | Shows initials from a local display name used as comment author and signer name | T-0221 |
| A-05 | Undo / redo | Reverts and reapplies every document mutation; disabled when nothing to undo | T-0222 |
| A-06 | Command bar: Edit, Comment, Highlight, Sign, Organize, Export, AI Assistant | Each activates its working tool | P1–P3 |
| A-07 | Rail: Home | Home with recent files and open action | T-0221 |
| A-08 | Rail: Files | Browses the local library folder; opens PDFs | T-0221 |
| A-09 | Rail: Recent | Real recent list, persisted | T-0221 |
| A-10 | Rail: Starred | Star/unstar persists across restarts | T-0221 |
| A-11 | Rail: Shared | Shows the chosen shared folder; shared review comments sync as files | T-0221, T-0230 |
| A-12 | Storage meter | Real usage of the library folder | T-0221 |
| A-13 | Rail: Pages + panel header "Pages (n)", grid toggle, add page | Real count; grid view; insert page works | T-0231 |
| A-14 | Thumbnails with selection | Already shipped; plus multi-select and drag reorder | T-0231 |
| A-15 | Rail: Comments with count badge | Panel lists real comments; badge is the real count | T-0230 |
| A-16 | Rail + card + pill: AI Assistant | Ask, summarize, key insights on the open document through OpenCode | P3 |
| A-17 | Text edit box with handles on a paragraph | Click a paragraph, edit, reflow; saved file shows and extracts the new text | T-0233 |
| A-18 | Highlight | Highlight from a text selection; visible in other readers | T-0230 |
| A-19 | Comment note with thread popup, author, time, menu | Add, reply, edit, delete, resolve; persists in the PDF | T-0230 |
| A-20 | Signature with "E-signed" badge | Badge only when the cryptographic check passes on an unmodified document | T-0235 |
| A-21 | Bottom bar: prev/next, page number box, zoom dropdown, − / +, full screen | All work; page box accepts typed numbers | T-0220 |
| A-22 | Tools panel with search | Search filters the tool list | T-0220 |
| A-23 | Tool: Edit PDF | Text and image editing (A-17 plus add/move/resize/replace/delete) | T-0233 |
| A-24 | Tool: Comment | Markup set of A-18/A-19 | T-0230 |
| A-25 | Tool: Fill & Sign | Fill form fields; place drawn/typed/image signature | T-0234, T-0235 |
| A-26 | Tool: Organize Pages | Reorder, rotate, delete, insert, extract, split | T-0231 |
| A-27 | Tool: Export PDF + Export card (format dropdown, Export button) | Exports to Word, Excel, PowerPoint, image, text, HTML, RTF; Office opens the result | T-0239 |
| A-28 | Tool: Convert to PDF | Creates PDFs from images, text, Markdown, HTML and Office files | T-0240 |
| A-29 | Tool: Combine Files | Merges several files into one PDF | T-0231 |
| A-30 | Tool: Protect PDF | Set/change/remove password and permissions; redact; sanitize | T-0237, T-0238 |
| A-31 | Caption: E-Sign "send, sign, and track" | Request signature by email; tracker updates on import of the signed copy | T-0236 |
| A-32 | Caption: Comment & Review "collaborate" | Shared-folder review and XFDF import/export | T-0230 |
| A-33 | Light and dark, both control styles | Every new panel and dialog is legible in all four combinations | every phase gate |
| O-01 | Saving | Save, Save As, dirty marker, close prompt, atomic write | T-0222 |
| O-02 | Printing | Prints to a real printer through the Windows dialog | T-0223 |
| O-03 | Bookmarks | Navigate, add, rename, delete, reorder; persists | T-0222, T-0232 |
| O-04 | Password-protected PDFs | Prompt, open, honour permissions, save keeps encryption | T-0222, T-0238 |
| O-05 | Form filling | Fill, tab order, save with appearances, FDF/XFDF, flatten | T-0234 |
| O-06 | Redaction | Content is removed, verified by re-extraction and a byte scan | T-0237 |
| O-07 | No dead controls | Automated test: every enabled control emits a working command | T-0212 |

## 6. Phases and tasks

Task ids start at T-0210. `owns` lists are set by the integrator when it opens
each task; module ownership follows the split made in T-0211.

### Phase 0 — Foundations (serial; blocks everything else)

| Task | Lane | Deliverable | Acceptance |
|---|---|---|---|
| T-0210 | integrator | Fix the `cargo deny` bans failure; make `rfd` optional under `ui`; automated installer install/uninstall test so PKG-003 can be verified; commit the acceptance matrix and ADR 0010/0011; close superseded queued tasks T-0098..T-0102 and T-0175 with a pointer here | `cargo deny check` exits 0; `cargo xtask verify` |
| T-0211 | app | Split `ui.rs` into `shell/` modules (title bar, command bar, rail, side panel, canvas, tools panel, bottom bar, dialogs, home) and introduce one `AppCommand` enum + dispatcher that every control calls. No behaviour change | Existing 440 app tests pass unchanged in meaning |
| T-0212 | qa | UI interaction harness (`egui_kittest` matching egui 0.33; if incompatible, AccessKit-tree enumeration over the command dispatcher): click-by-label tests and the **no dead controls** gate test, wired into `cargo xtask verify` | The gate fails when a button with no command is added (prove with a negative test) |
| T-0213 | core | Full-rewrite `save()`, `optimize()` and page plans for cross-reference-stream, object-stream and hybrid sources | Real-world fixture: rewrite → PDFium reopens → same pages and text |
| T-0214 | core + security | `pdfxi-document` session crate per ADR 0010: load (incl. password decrypt for RC4/AES-128/AES-256), mutation append, undo/redo by revision, atomic save / save-as, external-change detection, PDFium reload from memory | Encrypted and plain fixtures: mutate → undo → redo → save → reopen in PDFium |
| T-0215 | qa | Real-world corpus (revives T-0051): files produced locally with Word, Excel, PowerPoint and a browser print, a fillable form, password-protected files (user and owner password), plus checksummed external corpora not committed. Smoke: open, render, text, save, reopen | `cargo xtask corpus smoke` |

Phase 0 gate: verify + deny green, viewer behaves exactly as today.

### Phase 1 — Shell to the acceptance layout, saving, printing, passwords

| Task | Lane | Deliverable | Acceptance |
|---|---|---|---|
| T-0220 | app | Acceptance layout: left rail, command bar, right Tools panel with search, bottom bar (page box, zoom dropdown, −/+, full screen), and the capability registry that shows a control only when its command is available. Launch flags to deep-link view state (`--page`, `--tool`, `--panel`) for captures | A-21, A-22; layout compared with the image in all four theme/control combinations |
| T-0221 | app | Home, Files, Recent, Starred, Shared, storage meter, notifications, help, local display name — wiring `home.rs`, `cross_file.rs`, `shared_folder.rs` to real persisted data under the per-user config directory | A-02..A-04, A-07..A-12; state survives restart |
| T-0222 | app | Save / Save As / dirty marker / close prompt / undo-redo through the session; password prompt using the typed PDFium error; permission flags honoured with an explanation; bookmarks panel (navigate) | A-05, O-01, O-04 (open), O-03 (navigate) |
| T-0223 | app + render | Printing through the Windows print dialog: page range, copies, fit/actual size, orientation, rendered at printer resolution in bands; refuses when print permission is denied | O-02: print a fixture to "Microsoft Print to PDF" and verify page count and text of the result |

Phase 1 gate: package, install, captures; the shell matches the image's
structure with only working controls present.

### Phase 2 — Document features (parallel lanes after Phase 1)

Each task follows the same shape: library operates on a real `Document` →
session mutation → tool UI → end-to-end test on real PDFs → user docs → matrix
row. Suggested rounds are in parentheses.

| Task | Lane | Deliverable | Acceptance |
|---|---|---|---|
| T-0230 | author | Comments and markup: note, highlight, underline, strikeout, free text, ink; appearance streams; threads (replies), author, time, resolve, edit, delete; Comments panel and badge; popup on the page; XFDF import/export; shared-folder review sync (model → UI → sync) | A-15, A-18, A-19, A-24, A-32; annotations visible when the saved file is rendered by PDFium |
| T-0231 | author + core | Organize: grid view, multi-select, drag reorder, rotate, delete, insert blank/from file, extract, split; Combine Files (PDFs and images) | A-13, A-14, A-26, A-29 |
| T-0232 | author | Bookmark editing: add from current view, rename, delete, reorder, nest | O-03 |
| T-0233 | author + core | Edit PDF (text-block model on real content streams → add text and images → in-place edit with reflow → UI with handles → find and replace). Reuses the embedded font when it has the glyphs; otherwise embeds a subset fallback font and tells the user | A-17, A-23; edited text is extracted by PDFium from the saved file |
| T-0234 | forms | Form filling: text, multiline, checkbox, radio, combo, list; tab order; values and appearances written; FDF/XFDF import/export; flatten; reset. XFA and JavaScript calculations are detected and reported, not executed | O-05, A-25; filled values render in PDFium |
| T-0235 | security + forms | Signing: (a) drawn/typed/image signature appearances with locally protected saved signatures; (b) certificate signatures with vetted crypto crates — PFX file and Windows certificate store, CMS detached, ByteRange, incremental save; the toy RSA is removed from production paths; (c) validation panel and the "E-signed" badge driven only by a passing digest check on an unmodified document, chain status shown separately; (d) interop check with an independent verifier (`openssl cms -verify` over the ByteRange) | A-20, A-25; tampering one byte after signing turns the badge off (negative test) |
| T-0236 | security + app | Request signature: prepare a copy with a signature field, hand it to the default mail client as an attachment (Simple MAPI; fallback reveals the file), local tracker, import of the returned copy validates and completes the request and raises a notification | A-31 |
| T-0237 | security + author | Redaction: mark by selection, area and search; review list; apply removes text operators, image pixels, overlapping annotations, form values and links; optional sanitize (metadata, attachments, scripts, hidden layers); always a full rewrite to a new file. Built-in verification re-extracts text and scans raw bytes, failing closed | O-06, A-30; negative tests for each content type |
| T-0238 | security + app | Protect PDF: set, change, remove open password and permissions (AES-256); saving an encrypted document keeps it encrypted | O-04, A-30; a Word-encrypted fixture opens, and our encrypted output opens in PDFium with the password and fails without it |
| T-0239 | convert | Export to docx, xlsx, pptx (OOXML writers), PNG/JPEG, text, HTML, RTF; Export card and Export menu; progress and cancel | A-27; outputs open through Office automation and contain the expected text (heavy test profile; skipped with a recorded reason when Office is absent) |
| T-0240 | convert | Convert to PDF: images, text, Markdown, HTML, and Office files through installed Office (control absent when Office is missing); dropping a non-PDF offers conversion | A-28 |

Order within the phase: T-0230, T-0231, T-0234, T-0239 first (independent);
T-0235 after T-0234; T-0236 after T-0235; T-0237 and T-0238 after T-0213;
T-0233 is the longest and runs throughout.

Phase 2 gate: every A- and O- item except the AI rows passes on the installed
build.

### Phase 3 — AI Assistant through OpenCode

| Task | Lane | Deliverable | Acceptance |
|---|---|---|---|
| T-0250 | automation | Spike and ADR 0011 against the running service's `/openapi.json`: service discovery, authentication, pairing (`POST /api/pair`, `/auth/connect/{code}`), `GET /api/model`, `GET /api/provider`, session create / prompt / model switch, event stream, client-MCP attach, plugin RPC. Many of these routes are marked experimental: pin what is used and record the OpenCode version | ADR with a working request transcript (secrets redacted) |
| T-0251 | automation | `pdfxi-assistant` crate: OpenCode HTTP client, event stream, timeouts, cancellation; loopback by default; token in Windows Credential Manager, never in settings or logs | Integration test against a local OpenCode; unit tests against recorded responses |
| T-0252 | app | Connect screen: "Connect to OpenCode on this PC" (discovery) and "Pair with a code"; connection status; the app connection itself — **primary:** PDFXI attaches as a client MCP server exposing read-only document tools (document info, page text, search, selection, outline); **fallback** if a native client cannot attach: a small `opencode-pdfxi` plugin bridging RPC to PDFXI on loopback. Model picker fed by OpenCode's providers and models, default and test model `opencode-go/hy4-flash-free`, changeable per conversation and remembered | Connect, list models, switch model, disconnect |
| T-0253 | app | Assistant UI in the three places the image shows it: ask, streamed answer, summarize, key insights, page citations that navigate, stop, per-document conversation; shows "Connect to OpenCode" when not connected | A-16 on the test model |
| T-0254 | security | Hostile-document hardening: the assistant session can call only PDFXI's read-only tools (no shell, file, edit or web tools); document text travels as untrusted tool results with size caps; nothing is sent before the user asks; the UI states what is sent. Negative test: a PDF containing injected instructions cannot trigger any other tool or write any file (assert from the session log) | Orchestrator line-by-line review; negative test green |

### Phase 4 — Acceptance and release

| Task | Lane | Deliverable | Acceptance |
|---|---|---|---|
| T-0260 | qa | Acceptance scenario on real data: create a multi-page document with Word, then open → edit text → highlight → comment thread → fill → sign → export to Word → assistant summary (live part gated on a connected OpenCode). Fuzz targets for new parsers and writers; performance budgets for open, edit, save | Scenario passes; budgets recorded |
| T-0261 | docs | User guide page per tool, security notes (what the assistant sends, signature trust, redaction guarantees and limits), third-party notices and licences for new crates, matrix rows with evidence | `cargo xtask docs`; book builds if mdbook is installed |
| T-0262 | integrator | Package, install, desktop shortcut verified, final captures, handoff, plan archived | §8 |

## 7. Not included (stated so nobody claims them)

- Hosted cloud storage, accounts, or a hosted e-signature service.
- XFA forms; execution of form JavaScript.
- Editing text inside scanned images (no OCR-based editing).
- Feature-matrix rows outside this plan (3D, portfolios, preflight, prepress
  and similar) stay at their current honest status.
- Code signing stays blocked on a certificate
  (`orchestration/blockers/T-0202-code-signing.md`).

## 8. Done when

1. Every A- and O- item in §5 is ticked by the orchestrator after its own check
   on the installed build, with evidence paths in
   `docs/design/FEATURE_ACCEPTANCE.md`.
2. `cargo xtask verify` (including the no-dead-controls test) and
   `cargo deny check` pass on a clean tree.
3. The installer installs the build; the desktop shortcut targets it; uninstall
   is exercised by the automated installer test.
4. Captures from the installed build with real documents exist for light/dark ×
   Windows/macOS controls, covering each tool, and the orchestrator has
   reviewed them against the acceptance image for layout and function.
5. No demo, placeholder or canned content is reachable.
6. Feature-matrix rows covered here are `verified` with a GUI caller and a
   real-PDF test; `cargo xtask goal-verify --all` output is pasted with the new
   counts, whatever it prints.
7. Open blockers and the §7 limits are recorded in `orchestration/blockers/`
   and the handoff.

## 9. Main risks

| Risk | Mitigation |
|---|---|
| The in-house write path corrupts real PDFs | PDFium reopen check on every mutation test; real-world corpus; ADR 0010 escape hatch |
| In-place text editing with fonts is the hardest item | Staged rounds; fallback-font rule; ships last in Phase 2 without blocking other tools |
| A hostile PDF steers the assistant | T-0254 tool lockdown and negative test before the assistant is enabled |
| Signature or redaction looks right but is not | Independent verification (`openssl`, byte scan), tamper tests, line-by-line review |
| OpenCode routes used are experimental and may change | Spike pins routes and version; client isolated in one crate |
| Workers over-report | Orchestrator reruns gates and views every artifact itself (§3) |
