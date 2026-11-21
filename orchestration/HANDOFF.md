# PDFXI handoff - 2026-11-20

> The `Current state` section below is the Phase 1 snapshot and is kept for
> history. The live state is the last checkpoint section (currently
> `T-0255A checkpoint`): 145 feature rows with 33 `verified`, HEAD moving with
> each checkpoint.

## Current state

- Repository: https://github.com/chucky747/pdfxi (public), branch `main`, no branches.
- HEAD: the commit that added this file; check `git log -1 --oneline`. The release
  evidence commit immediately before it is `91c2639`.
- Worktree: clean when this file was committed (docs-writer's T-0206 changes are in
  the same commit).
- Features: 143 rows - 25 `verified`, 18 `library`, 96 `model`, 3 `replaced`,
  1 `in-progress`.
- `cargo xtask verify`: green (fmt, clippy with `-D warnings`, workspace tests, the
  `ui,pdfium` tests, the PDFium render tests, rustdoc, docs check, citation check).
- `cargo xtask goal-verify --all`: intentionally **red**. It stops with
  `115 feature rows are not verified or replaced` and does not print
  `GOAL VERIFIED`, per ADR 0009.
- Installed build: `986c550` (built before this evidence commit). Installer and
  portable hashes, install layout, shortcut targets, smoke and captures are in
  `docs/evidence/release/T-0202-final-install.md`.
  - setup `pdfxi-setup-0.1.0-windows-x86_64.exe` sha256
    `72e4a613fe6b68b73cecb96bb774b7e7c18705ae9b3bb1ce82679c9475daf91a`
  - portable `pdfxi-portable-0.1.0-windows-x86_64.zip` sha256
    `1171f797ebd51cc01fd2e0df0e5903cf87b222705013049ce13a624fe1be0fb0`
  - installed `pdfxi-ui.exe` sha256
    `B8C82C134CF1428F26EC360C68E93933876DF5EC029D594CC6724975D6BB3ABD`, equals the
    `target/integration/release` build, PE subsystem 2.
- Unsigned: `orchestration/blockers/T-0202-code-signing.md`.

## What this campaign did

- **T-0200** - baseline: release viewer build plus `ui,pdfium` and PDFium test
  suites on the campaign base commit (`docs/evidence/hardening/T-0200.md`).
- **T-0201** - window controls: Windows caption buttons on the right or macOS
  traffic lights on the left, title-bar settings, persisted light/dark/system
  theme and `--controls`/`--light`/`--dark` flags
  (`docs/evidence/ui/T-0201.md`).
- **T-0202** - packaging: GUI-subsystem exe with icon/version/DPI manifest,
  pinned PDFium chromium/8165 loaded from the exe directory, portable zip and
  per-user Inno Setup installer, signing recorded as a blocker
  (`docs/evidence/release/T-0202.md`, final install in
  `T-0202-final-install.md`).
- **T-0203** - honest shell: empty Home, native open, real page sizes, real
  thumbnails and PDFium text for find/selection; all demo and plan-only content,
  including Print and split/tile, removed (`docs/evidence/ui/T-0203.md`).
- **T-0204** - gate: `xtask verify` and CI run the `ui,pdfium` tests; `features
  verify` resolves every cited test against a `#[test]` index; real-world Excel
  PDF fixture; JS wall-clock busy-loop test (`docs/evidence/hardening/T-0204.md`).
- **T-0205** - honest matrix: ADR 0009 adds `library` and `model`; the matrix now
  reflects end-user reachability (`docs/evidence/hardening/T-0205.md`).
- **T-0206** - docs: README, mdBook pages, install page, security page,
  ACCEPTANCE, T-0192 banner, UI review note and this handoff match the shipped
  viewer (`docs/evidence/hardening/T-0206.md`).
- **T-0207** - core: incremental save accepts cross-reference-stream sources and
  appends a matching section; the Excel hybrid fixture round-trips with text
  intact (`docs/evidence/hardening/T-0207.md`).

## Open items

1. **Code signing** - no certificate. Blocker:
   `orchestration/blockers/T-0202-code-signing.md`. Never claim signed binaries.
2. **cargo-deny bans** - `cargo deny check` reports `advisories ok, bans FAILED,
   licenses ok, sources ok`; the bans failure is wildcard `path` dependencies.
   Blocker: `orchestration/blockers/cargo-deny-bans.md`. `deny.toml` unchanged.
3. **115 rows not verified** - 18 `library`, 96 `model`, 1 `in-progress`, minus
   3 `replaced`; the campaign's end condition is not met. Follow-ups are listed in
   ADR 0009 (end-to-end redaction, production signature crypto, forms/CLI and
   GUI wiring, PRINT titles, split bundled rows).
4. **PKG-003 is `in-progress`** - the installer is smoke-tested manually but has
   no automated test; add one before promoting the row.
5. **Password-protected PDFs** - unsupported; detection is a lowercase
   `"password"` string check because pdfium-render exposes no typed load error.
6. **Full rewrite paths** - `save()`, `optimize()` and page-plan output still
   reject xref-stream sources; only incremental save was extended (T-0207).
7. **Find coverage** - progressive: the first 8 pages on open, then two pages per
   frame, so very large documents become searchable gradually.
8. **Native file picker** - implemented with `rfd`, not verified interactively.
9. **`rfd` without `ui`** - `rfd` is a hard dependency, so the non-`ui` build
   warns `unused dependency rfd`; make it optional under the `ui` feature.
10. **mdBook build** - not run; mdbook is not installed. Only `cargo xtask docs`
    (page existence) was checked.
11. **Real install uninstaller** - not exercised in the final round; only the
    isolated `Smoke` AppId install/uninstall in T-0202 R4.2 covers uninstall.

## Resume checklist

Run from the repository root, Windows, PowerShell:

    git status --short                 # must be empty before gating
    cargo fmt --all --check
    cargo clippy --workspace --all-targets --all-features -- -D warnings
    cargo test --workspace
    cargo test -p pdfxi-app --features ui,pdfium
    cargo test -p pdfxi-render --features pdfium
    cargo xtask verify
    cargo xtask goal-verify --all      # expected red: 115 rows, per ADR 0009

Packaging and install (from a clean tree; release build commit first):

    cargo build --release -p pdfxi-app --bin pdfxi-ui --features ui,pdfium
    powershell -NoProfile -File scripts/package.ps1 -Installer -TargetDir target/integration
    powershell -NoProfile -File scripts/package.ps1 -SelfTest
    # then run the produced pdfxi-setup-*.exe /VERYSILENT /SUPPRESSMSGBOXES
    # /CURRENTUSER /NORESTART /TASKS="desktopicon" and verify the Desktop shortcut.

TEMP workaround for restricted agent sessions: Inno Setup (ISCC, the setup exe and
the uninstaller) and `cargo install` refuse to execute helpers from the default
`%TEMP%` with `Access is denied (os error 5)`. Before running them, set:

    $env:TEMP = "$PWD\target\tmp\inno-temp"; $env:TMP = $env:TEMP; New-Item -ItemType Directory -Force $env:TEMP

Captures from the installed build:

    powershell -NoProfile -File scripts/capture-ui.ps1 `
      -Exe "$env:LOCALAPPDATA\Programs\PDFXI\pdfxi-ui.exe" `
      -Out docs/evidence/ui/final-doc-light-windows.png `
      -Arguments '"<repo>\fixtures\realworld\microsoft-excel-2019-sample.pdf" --light --controls=windows'

## Campaign checkpoint (2026-11-20, feature acceptance)

Durable resume point for the feature-acceptance campaign. The live state is
`orchestration/BOARD.md` and `orchestration/claims/`; this section is a summary,
not a replacement.

### Verified in this campaign

Phase 1-3 tasks T-0210..T-0256 as shown on the board, plus T-0269 (docs). Key
commits: `8c1f43b` T-0242 comments UI, `a7f75fa` T-0212 harness coverage,
`e3f665e` T-0237 redaction, `b61322e` T-0254 assistant lockdown, `a5801c3` +
`b1e3ed0` T-0269 security/privacy docs, `390443f` Word COM/signing fallbacks,
`b34abd3` T-0242 status fix. Do not re-derive from chat; use the board.

### In flight right now

- T-0243 forms UI (app lane), T-0265 edit transforms (author lane), T-0249 PFX
  identity (security lane) hold claims. T-0212 form-command coverage is being
  extended by QA.
- T-0242, T-0237, T-0254 and T-0269 claims are released and their work is
  committed.

### Remaining queue

- App: T-0243, T-0244, T-0245, T-0246, T-0231, T-0252, T-0253, T-0255, T-0263,
  T-0264, T-0266. T-0263 needs T-0257; T-0264 needs T-0265.
- Security: T-0257 (depends T-0237).
- Follow-ups: T-0267 (signature trust/store/timestamps/revocation), T-0268
  (redaction form XObject and inline images).
- Final: T-0260 (acceptance scenario/fuzz/perf), T-0261 (docs/notices),
  T-0262 (package/install/captures/handoff/archive).

### Acceptance gates

`cargo xtask verify` and `cargo deny check` are the final gates. Full-workspace
clippy is currently blocked only by the in-flight T-0249
`crates/pdfxi-security/src/pkcs12/decrypt.rs` lint; app and library crates are
clean with `--no-deps`. `cargo xtask goal-verify --all` exits 1 with **113
unfinished rows outside the campaign's acceptance surface**; the covered A-/O-
rows must be verified and the output pasted verbatim, not all 145 rows.

### Open blockers

- T-0202 code signing: no certificate, installer unsigned; never claim signed
  binaries.
- T-0215 Word COM save/export: unavailable on this host; the scenario route and
  any A-27 Word check must state what was actually exercised.
- T-0241 RSA advisory: scoped `deny.toml` ignore plus the `rsa_usage_scope`
  guard; revisit if RSA decryption is added or `rsa` is fixed.
- chrono: resolved; remains transitive through `pdfium-render` only, no ignore.

### Next action

Finish T-0243 rounds A/B, then T-0252/T-0253 assistant work, then the remaining
app tasks. Check each lane's output yourself (tests plus a real-fixture or
installed-build check) before checkpointing; do not trust a summary alone.

### Checkpoint update (2026-11-20, later)

- **T-0249** verified as the narrowed PFX/PKCS#12 slice, commit `c8448d6`:
  `from_pfx` imports Windows PBES1/3DES and PBES2/AES-256 fixtures, signs and
  verifies, typed errors. Trust anchors, Windows certificate store, RFC 3161
  timestamps and CRL/OCSP are T-0267.
- **T-0212** harness form coverage committed `0e426f9`: 94 discovered / 79
  covered / 15 pending / allowlist exactly 2, 23 tests, app gate 481 lib + 23
  harness, exact clippy green for the harness state.
- **Packaging** bundles the user guide, commit `1331a82`: `package.ps1 -Check`
  PASS, installer test 28 assertions PASS (installed `docs\README.md`,
  `security.md`, `privacy.md` hash-equal), staging manifest 46 hashes. Installed
  A-03 (Help opens `exe\docs`) is part of the T-0262 final check.
- **In flight:** T-0243 forms UI (Round A evidence/captures done, Round B
  pending), T-0265 edit transforms (Round A docs done, Round B
  resize/replace), T-0257 sanitize/verification.
- **Counts:** 29 feature rows verified; `cargo xtask goal-verify --all` baseline
  113 unfinished rows outside the campaign acceptance surface.
- **Next:** finish T-0243 Round B, then T-0252/T-0253 assistant work, then the
  remaining app tasks. T-0263 needs T-0257; T-0264 needs T-0265.

### Checkpoint update (2026-11-20, forms, sanitize and markup round)

Durable summary of the round that added forms closure, sanitize/verification and
the first markup batch. `orchestration/BOARD.md` remains the live state.

- **HEAD:** `cafc009` (`fix(orchestration): close the T-0204 fragment notes`).
  Linear history this round: `7fb867c` rules, `0975c34` T-0247 Round A,
  `cafc009` T-0204 fragment repair. Nothing was pushed, rebased or amended.
- **Verified commits this round:**
  - T-0243 forms UI A+B: `2f4da28` (fill text and checkboxes with appearances)
    and `e3a42de` (choice/radio, tab order, reset, flatten, FDF/XFDF import and
    export).
  - T-0257 sanitize and independent verification: `9f5167f` (sanitize hidden
    data, fail-closed residual check, second reader) and `bcf48ab` (scrub XMP on
    every reachable object).
  - T-0268 redaction depth: `baceb98` (form XObject recursion) and `96cf730`
    (inline image pixel redaction, hardened form geometry).
  - T-0247 Round A: `0975c34` (underline, strikeout and ink with indirect `/AP`
    appearances, fail-closed validation, PDFium proof). The task stays
    `in_progress`: Round B owns Line, Square, Circle, Polygon, arrow line
    endings and Stamp.
  - T-0051 external corpora: `d757a52` (pinned and smoke-tested downloads).
  - Earlier in the same campaign: T-0249 `c8448d6` (PFX identity) and T-0265
    `da7834f` (page object transforms).
  - `7fb867c` moved the subagent-model rule from §12 to §2 of `RULES.md`.
- **Feature counts:** 145 rows - 32 `verified`, 30 `library`, 80 `model`,
  3 `replaced`. `cargo xtask verify` reports VERIFY OK and
  `cargo xtask features verify` reports `FEATURES OK: 145 rows, 32 verified`.
- **In flight (claims held, nothing of theirs committed here):**
  - T-0252 assistant Connect screen and model picker (app lane).
  - T-0267 trust anchors, Windows certificate store and revocation (security
    lane).
  - T-0247 Round B shapes and stamps (author lane).
- **Remaining app queue after T-0252:** T-0253 assistant panel, T-0245
  signatures and E-signed badge, T-0231 organize/combine, T-0244 export card,
  T-0255 bookmarks and properties, T-0246 shared review, T-0266 convert,
  T-0263 protect/redact, T-0264 edit UI. Then the final tasks: T-0260
  acceptance scenario/fuzz/perf, T-0261 docs, T-0262 package, install,
  captures and archive.
- **Blockers and documented limits:**
  - T-0202: installer and binaries are unsigned; never claim signed output.
  - T-0215: Word COM save/export is unavailable on this host, so A-27 Word
    coverage is open/read only.
  - T-0241: scoped `deny.toml` RSA advisory ignore with the `rsa_usage_scope`
    guard; revisit if RSA decryption is added.
  - T-0243: flatten removes active fields, but incremental save may retain
    unreferenced bytes; documented in the T-0243 evidence.
  - T-0268: vector paths under a redaction mark are not removed; documented
    limit.
  - T-0257: the independent verifier scans UTF-8 and UTF-16LE text only.
- **Subagent rule:** dispatch subagents only with `opencode-go/hy4-flash-free`
  or `opencode-go/deepseek-v4.1-flash`, never a Claude model
  (`RULES.md` §2).
- **Fragment repair:** `orchestration/feature-updates/T-0204.toml` had an
  unterminated `notes` block that aborted every full `features apply`; it is
  repaired and committed in `cafc009`, and a full apply parses again.

### T-0253A checkpoint (2026-11-20, assistant panel Round A)

- **Commit:** `feat(app): add assistant panel with ask stop and citations
  [T-0253]`, a direct child of `0111386` (T-0252). Linear history, not pushed,
  no amend/rebase. HEAD after = that commit (`git log -1 --oneline`).
- **Task state:** T-0253 stays `in_progress` for Round B. The fragment
  `orchestration/feature-updates/T-0253.toml` carries empty `feature_rows` and
  `promoted_rows`, so no matrix row changed and `docs/features.toml` is not in
  the commit. SEC-013 is not claimed.
- **Integrator gates** (`target/integration`, `PDFIUM_PATH` set, TEMP/TMP under
  `target/tmp`): `cargo fmt -p pdfxi-app -- --check` EXIT=0;
  `cargo clippy -p pdfxi-app --all-targets --features ui,pdfium -- -D warnings`
  EXIT=0; `cargo test -p pdfxi-app --features ui,pdfium` EXIT=0 with
  **517 lib + 24 harness + 0 doctests, 0 failed**;
  `RUSTDOCFLAGS=-D warnings cargo doc -p pdfxi-app --no-deps` EXIT=0.
  `cargo xtask features verify` reports `FEATURES OK: 145 rows, 32 verified`.
- **Captures:** the four `docs/evidence/ui/t0253-assistant-{light,dark}-{windows,macos}.png`
  were retaken from the final release binary
  `target/lanes/app6/release/pdfxi-ui.exe` (22:22:07, 10,177,024 B), all
  1920x1200 and 124-141 KB, reviewed with no user path or invented document
  content. The first set predated that binary and was replaced.
- **Evidence:** `docs/evidence/ui/T-0253.md` (517 in the Gates block);
  `docs/evidence/ui/T-0252.md` records the removed auto-probe; ADR 0011 has the
  credential-lifetime amendment. `orchestration/blockers/T-0253.md` and
  `T-0253-harness-coverage.md` are resolved records kept for the permission gap.
- **What landed:** three placements (command-bar pill, Tools row, rail) dispatch
  `AppCommand::AiAssistant`; connected conversation with ask/Enter, "Thinking..."
  and exactly one Stop; page-citation chips dispatch `AssistantCitation(page)` ->
  `GoToPage(page - 1)` and disable past the page count; one session per
  conversation reset on tab change; six panel commands routed through dispatch so
  the T-0212 gate passes; the harness lists the six variants in `PENDING_COVERAGE`
  with `all_commands()` unchanged.
- **Round B queue:** locked session (`create_locked_session`, `attach_locked_mcp`,
  `BudgetedTools`), document-grounded answers, summarize and key-insights
  presets, streaming, a session-reuse + `set_model` test, explicit session delete
  on stop/disconnect, SEC-013 promotion, then the docs refresh (`shell/help.rs`
  privacy text, `docs/book/src/privacy.md`, `docs/book/src/security.md`) which
  still describe the panel as inert.
- **Carried limits:** no document text is sent yet (the captures use a greeting
  on purpose); citations are text-only, not coordinate-based; the pending
  harness reasons for connect/ask/stop/disconnect are stale and can be promoted
  with a `status`/`active_rail` proof once a headless seam exists.

### T-0253B checkpoint (2026-11-20, assistant document grounding)

- **Commit:** `feat(app): ground the assistant with a fenced document excerpt
  [T-0253]`, a direct child of `35e721c` (T-0253A). Linear history, not pushed,
  no amend/rebase. HEAD after = that commit (`git log -1 --oneline`).
- **Task state:** T-0253 is `verified`; its claim is released. T-0270 stays
  `verified` - its diagnostic probe is deleted from the tree and the captured
  probe output lives in `target/tmp/t0253-visibility.txt`, summarised in
  `orchestration/blockers/T-0253-grounding.md`. T-0245 (signature UI, E-signed
  badge, request-signature tracker) is the next app task.
- **ADR 0013 (accepted):** assistant grounding does not use OpenCode's
  client-MCP tool route. Live on OpenCode 2.3.2 with `opencode-go/hy4-flash-free`
  the attach completes and the service lists all five tools, but the model never
  receives their schemas (`tool_names=[]`, `tool_calls=[]`), with prefixed
  allows, unprefixed allows, attach-before-session and a second prompt. The app
  therefore sends a bounded, fenced prompt excerpt: `DocumentSnapshot` plus
  `prompt_excerpt(24_000)` built on the UI thread at Ask time, an
  untrusted-document instruction, an `<untrusted-document-text>` fence with a
  neutralised terminator, the included page range and a truncation marker. The
  session stays locked deny-all with an exact read-back and no MCP, shell, file,
  network or edit tool wired. `BudgetedTools` and the MCP attach stay library
  capabilities in `pdfxi-assistant` that the app does not call.
- **Matrix:** `cargo xtask features verify` reports `FEATURES OK: 145 rows,
  33 verified`. `cargo xtask features apply --clean --task T-0253` merged 0
  `[[feature]]` rows because the fragment's `feature_rows` list is empty; the
  fragment carries the decision, tests and captures, and the integrator promoted
  **SEC-013** to `verified` directly against the ADR 0013 design (locked session
  plus bounded fenced excerpt - never active pdfxi tools). No other row changed
  (row-by-row diff: only SEC-013).
- **Integrator gates** (`target/integration`, `PDFIUM_PATH` set): `cargo fmt
  --all --check` EXIT=0; `cargo clippy --workspace --all-targets --all-features
  -- -D warnings` EXIT=0; `cargo test -p pdfxi-app --features ui,pdfium` EXIT=0
  with **537 lib + 24 harness, 0 failed**; `RUSTDOCFLAGS=-D warnings cargo doc
  -p pdfxi-app --no-deps` EXIT=0; `cargo build --release -p pdfxi-app --bin
  pdfxi-ui --features ui,pdfium` EXIT=0.
- **`cargo xtask verify` is red at this checkpoint for an environment reason, not
  code:** the workspace suites and the app/PDFium suites pass, then
  `cargo test -p pdfxi-assistant --doc` exits 101 immediately after "all
  doctests ran" - the host blocks executing the merged doctest binary from the
  default `%TEMP%` (the documented `TEMP`/`TMP` restriction in the project
  memory). That crate was not touched by this checkpoint. The T-0253B commit is
  therefore made on the green app gates that the orchestrator also ran
  independently; re-run `cargo xtask verify` with `TEMP`/`TMP` under
  `target/tmp` at quiescence.
- **`cargo deny check`:** not re-run here for this checkpoint (no dependency,
  `Cargo.lock` or `Cargo.toml` change; green at `35e721c`).
- **Captures:** the four `docs/evidence/ui/t0253-assistant-{light,dark}-{windows,macos}.png`
  were retaken with the B2 presets against the local OpenCode 2.3.2 service and
  `opencode-go/hy4-flash-free`, and reviewed by the orchestrator. All four are
  1920x1200 and 161-182 KB; light/macos shows Page 1 and Page 2 citation chips.
  The grounded proof is the page-1 sentence "amber compass lantern", which exists
  only in the fixture.
- **What landed:** `assistant_tools.rs` (snapshot, page text, search, selection,
  outline, fenced 24K excerpt with a neutralised terminator), the locked-session
  turn in `assistant_chat.rs` (version gate, deny-all ruleset, exact read-back,
  session reuse, summarize and key-insights presets), a locked-session stub in
  `assistant_stub.rs`, command and help text updates, and the harness coverage in
  `tests/ui_controls.rs`.
- **Docs:** `docs/decisions/0013-assistant-document-grounding-fallback.md`,
  `docs/evidence/ui/T-0253.md` (Round B section), and the privacy/security pages
  now state that the question plus a bounded excerpt go to the local service and
  nothing else.
- **Limits:** the excerpt is 24K chars, so very large documents are partially
  grounded and the prompt says which pages were included; there is no on-demand
  page fetch. If a later OpenCode build exposes client-MCP tools to the model,
  revisit ADR 0013.

### T-0245A checkpoint (2026-11-20, document session signing, T-0245 round A)

- **HEAD before:** `3b7c595` (`feat(app): ground the assistant with a fenced
  document excerpt [T-0253]`). **HEAD after:** this checkpoint commit
  (`feat(document): add session signing with incremental revisions [T-0245A]`;
  `git log -1 --oneline`). Linear history, not pushed, no amend/rebase.
- **Task state:** T-0245A is `verified`; its claim is released. T-0255 (app
  lane, bookmarks panel and document-properties dialog) is `in_progress` and
  holds its claim. T-0245 (signature UI, E-signed badge, request-signature
  tracker) stays `queued` as round B.
- **What landed:** `DocumentSession::sign(&SigningIdentity, &SignOptions) ->
  Result<SignatureReport, SessionError>` in `crates/pdfxi-document/src/session.rs`.
  It calls `pdfxi_security::sign_pdf` on the session document and keeps the
  original session bytes as the exact prefix plus one incremental revision;
  it never calls `edit_full`/`Document::save`. On success it reparses the signed
  bytes, sets `current_digest` (so `is_dirty()` is true), pushes one
  `Revision { start_len, delta, before, after }` and clears redo. On any error
  the unchanged bytes are reparsed, so document, history and redo are intact.
  Encrypted sessions are refused before any byte is produced with the new typed
  `SessionError::EncryptedSignUnsupported` (a signature over decrypted plaintext
  would not cover the bytes on disk). Prefix mismatch reloads and returns
  `RevisionMismatch`. ADR 0010 point 3 remains the design basis.
- **Test:** `crates/pdfxi-document/tests/signing.rs` ->
  `session_signs_saves_reopens_and_detects_tampering` (`#[ignore]`, because
  `crates/pdfxi-security/tests/fixtures/pfx/leaf-aes256.pfx` holds a private key
  and `.pfx` is gitignored; public password `pdfxi-test-pfx`). It signs
  `fixtures/realworld/microsoft-excel-2019-sample.pdf`, asserts the exact
  prefix, one revision, redo cleared and dirty, saves and reopens with unchanged
  page count and page-1 text, gets `Valid`/`bytes_match`/`covers_entire_file`,
  then flips one byte inside the signed range and asserts `Tampered`. Run it
  explicitly with:
  `cargo test -p pdfxi-document --test signing -- --ignored --nocapture`.
- **Integrator gates** (`target/integration`, `PDFIUM_PATH` set, TEMP/TMP under
  `target/tmp`): `cargo fmt -p pdfxi-document -- --check` EXIT=0;
  `cargo clippy -p pdfxi-document --all-targets --all-features -- -D warnings`
  EXIT=0; `cargo test -p pdfxi-document` EXIT=0 with **2 lib + 6 session,
  1 ignored signing, 0 failed**; the ignored signing test EXIT=0
  (**1 passed**); `RUSTDOCFLAGS=-D warnings cargo doc -p pdfxi-document
  --no-deps` EXIT=0. `cargo xtask features verify` reports `FEATURES OK: 145
  rows, 33 verified` - unchanged, because T-0245A is a library slice of T-0245
  and the signing row stays `library` until the app shell verifies it. No
  `orchestration/feature-updates/T-0245A.toml` fragment was written by the lane;
  nothing was merged into `docs/features.toml`.
- **Evidence:** `docs/evidence/core/T-0245A.md`.
- **Scope discipline:** the app workspace (`crates/pdfxi-app/**`) was neither
  staged nor tested here; the T-0255 in-flight work stays out of this commit.
- **Next:** finish T-0255 (bookmarks panel and document-properties dialog), then
  T-0245 round B (Sign panel, E-signed badge, request-signature tracker) on top
  of `DocumentSession::sign`.

### T-0255A checkpoint (2026-11-20, real-PDF outline reader and writer)

- **HEAD before:** `ed06a67` (`feat(document): add session signing with
  incremental revisions [T-0245A]`). **HEAD after:** this checkpoint commit
  (`checkpoint(acceptance): real-PDF outline reader and writer [T-0255A]`;
  `git log -1 --oneline`). Linear history, not pushed, no amend/rebase.
- **Task state:** T-0255A is `verified`; its claim is released after this
  checkpoint. T-0255 (app lane, bookmarks panel and document-properties dialog)
  is back to `queued` and now `depends_on` T-0255A. T-0245 (signature UI,
  E-signed badge, request-signature tracker) stays `queued` as round B and keeps
  its app claim, which is deliberately left held and uncommitted here.
- **What landed:** `crates/pdfxi-edit/src/navigation/document_outline.rs` adds a
  document-backed outline bridge to `pdfxi_edit::navigation`:
  `read_outline(&Document) -> Result<OutlineRead, OutlineReadError>` and
  `write_outline(&mut Document, &Outline) -> Result<(), OutlineWriteError>`,
  with `OutlineRead`, `OutlineIssue`, `OutlineIssueReason`, `OutlineReadError`,
  `OutlineWriteError` and the `MAX_OUTLINE_ITEMS`/`MAX_OUTLINE_DEPTH` bounds.
  The reader walks the catalog `/Outlines` tree, resolves `/Dest` and `/A` GoTo
  page references to zero-based indexes (including named destinations) and
  reports unresolvable ones as typed issues; the writer installs the edited
  tree as real catalog `/Outlines` with `/Parent`/`/First`/`/Last`/`/Next`/
  `/Prev` and page references, creating `/Outlines` on first add. Malformed
  trees are typed errors, never panics.
- **Tests:** `cargo test -p pdfxi-edit` EXIT=0 with **405 passed, 0 failed**
  (359 lib + 5 object_render + 13 annotations + 27 redaction + 1 text_render +
  0 doctests). All 13 `document_outline` tests pass, including
  `f32_overflow_coordinate_is_an_issue_and_other_items_still_read`, the
  PowerPoint read -> edit -> write -> PDFium-reopen round trip, the no-outline
  case and the malformed-input cases. `cargo fmt -p pdfxi-edit -- --check`
  EXIT=0; `cargo clippy -p pdfxi-edit --all-targets --all-features -- -D
  warnings` EXIT=0; `RUSTDOCFLAGS=-D warnings cargo doc -p pdfxi-edit
  --no-deps` EXIT=0. These were rerun in `target/integration` after the final
  fix; the full workspace was intentionally not rerun while the app lane is
  mid-edit.
- **PDFium limit:** this workspace's PDFium binding exposes no bookmark
  traversal, so PDFium cannot independently read outline titles/pages. PDFium
  is the independent file-validity and page-tree check on the saved bytes (3
  pages on the PowerPoint fixture); PDFXI's reader asserts the outline order,
  titles and destination pages. The writer also leaves superseded outline
  objects unreachable in the file.
- **Matrix:** `cargo xtask features verify` reports `FEATURES OK: 145 rows, 33
  verified` - unchanged, because the T-0255A fragment promotes nothing
  (VIEW-008 stays `model`, MANAGE-001 stays `library` until the app lane lands
  the visible surface). Nothing was merged into `docs/features.toml`.
- **Evidence:** `docs/evidence/hardening/T-0255A.md`;
  `orchestration/feature-updates/T-0255A.toml`;
  `orchestration/blockers/T-0255-outline-bridge.md` records the library gap as
  resolved with the app pane still outstanding.
- **Scope discipline:** no `crates/pdfxi-app/**` path was staged. The in-progress
  signature UI work (T-0245) stays uncommitted and its claim held.
- **Next:** T-0245 round B (Sign panel, E-signed badge, request-signature
  tracker), then T-0255 app (bookmarks panel wired to the new bridge, plus the
  document-properties dialog).

### T-0260A checkpoint (2026-11-21, fuzz targets for redaction and edit paths)

- **HEAD before:** `b26a24d` (`checkpoint(acceptance): real-PDF outline reader and
  writer [T-0255A]`). **HEAD after:** this checkpoint commit
  (`checkpoint(acceptance): fuzz targets for redaction and edit paths [T-0260A]`;
  `git log -1 --oneline`). Linear history, not pushed, no amend/rebase.
- **Task state:** T-0260A is `verified` and its claim is released. T-0245
  (signature UI, E-signed badge, request-signature tracker) is `in_progress`
  and its app claim stays held and uncommitted. T-0261A (docs) and T-0255
  (bookmarks/properties) are `queued`. The board was regenerated with
  `cargo xtask board` and shows T-0260A `verified`, T-0245 `in_progress`,
  T-0255 `queued`.
- **What landed:** fuzz targets for `content`, `crypto`, `filters`, `forms`,
  `parser` (harness updates) plus new `edit` and `redaction` targets driving
  `pdfxi-edit` redaction and annotation/content-edit paths on real bytes. New
  `fuzz/entry` harness with a `seed_corpus` test and committed seed corpora
  under `fuzz/corpus/edit` and `fuzz/corpus/redaction`.
- **Verification (cited from the lane, not rerun here):** `cargo test
  --manifest-path fuzz/entry/Cargo.toml` -> 7 passed, 0 failed;
  `cargo check --manifest-path fuzz/Cargo.toml --all-targets` exits 101 in the
  pre-existing `libfuzzer-sys` build with MinGW g++, recorded verbatim in
  `orchestration/blockers/T-0260A-fuzz-toolchain.md`; fuzz-entry `cargo fmt`
  and `cargo clippy` are green per the lane and its evidence. The full
  workspace was intentionally not rerun while the app and docs lanes are
  mid-edit.
- **Matrix:** `cargo xtask features verify` reports `FEATURES OK: 145 rows,
  33 verified` - unchanged; this fuzz slice promotes no matrix row and
  `docs/features.toml` is not in the commit.
- **Evidence:** `docs/evidence/acceptance/T-0260A.md`;
  `orchestration/feature-updates/T-0260A.toml`;
  `orchestration/blockers/T-0260A-fuzz-toolchain.md`.
- **Scope discipline:** no `crates/pdfxi-app/**`, `docs/book/**`, or
  `docs/evidence/ui/**` path was staged. The in-flight T-0245 app work and
  T-0261A docs work stay uncommitted with their claims held.
- **Next:** T-0245 round B and T-0261A docs land, then T-0262 final
  package/install/captures/handoff.

### T-0245B checkpoint (2026-11-21, signing card, verification and E-signed badge)

- **HEAD before:** `6ed8e82` (`checkpoint(acceptance): fuzz targets for redaction
  and edit paths [T-0260A]`). **HEAD after:** this checkpoint commit
  (`checkpoint(acceptance): signing card, verification and E-signed badge
  [T-0245B]`; `git log -1 --oneline`). Linear history on `main`, not pushed, no
  amend/rebase.
- **Task state:** T-0245 stays `in_progress` - this is a partial round-B slice:
  A-20 (signing card, verification readout and E-signed badge) is served, while
  A-25 (drawn/typed/image signature placement) and A-31 (request-signature by
  email plus the returned-copy tracker) remain open. T-0245A (document session
  signing) stays `verified`; T-0267 (trust/store/timestamps/revocation) stays
  `in_progress`; T-0255 stays `queued`. T-0245 is **not released** and no claim
  is released here.
- **What landed:** a signing card reachable from the command bar, the Tools row
  and `--tool=sign` through `AppCommand::Sign` under the new `Capability::Signing`;
  PKCS#12 import via `SigningIdentity::from_pfx` with immediate password clear;
  a typed signature appearance plus incremental signature revision through
  `DocumentSession::sign`; a verification readout (status, signer, SHA-256,
  time, whole-file coverage, append-after check, honest "identity not verified"
  while no trust anchor exists); an E-signed badge gated on
  `Valid && bytes_match && covers_entire_file && !appended_after_signature` and
  scoped to the active tab/byte state; and `verify_signature_on_open` so the
  badge shows on open for a signed file and clears for an unsigned one. New
  `crates/pdfxi-app/src/shell/signing.rs` plus capabilities, commands,
  document_view, session, mod and ui.rs wiring.
- **Verification (cited from the orchestrator's final run in `target/integration`,
  not rerun here):** `cargo fmt --all --check` EXIT=0; `cargo clippy -p pdfxi-app
  --all-targets --features ui,pdfium -- -D warnings` EXIT=0; `cargo test -p
  pdfxi-app --features ui,pdfium` EXIT=0 with **546 lib + 24 harness passed,
  0 failed, 4 ignored**. The four ignored signing tests pass:
  `write_signed_capture_fixture`,
  `signed_badge_and_readout_are_scoped_to_the_signing_tab`,
  `signed_document_verifies_and_tampering_clears_the_badge`, and
  `signature_is_verified_when_the_signed_document_opens`. The six captures were
  reviewed by the orchestrator. (The lane fragment's inline note records an
  earlier 544-lib count; 546 is the authoritative final figure.)
- **Matrix:** `cargo xtask features verify` reports `FEATURES OK: 145 rows,
  33 verified` - unchanged; this partial slice promotes no row. The T-0245
  fragment has empty `feature_rows`/`promoted_rows`, `status = "partial"`; SEC-005
  and FORMS-010 stay as they are until A-25/A-31 land, and SEC-006 is T-0267.
  `docs/features.toml` is not in the commit.
- **Evidence:** `docs/evidence/ui/T-0245.md` and the six
  `docs/evidence/ui/t0245-{sign,badge}-{light,dark}-{windows,macos}.png`
  captures; fragment `orchestration/feature-updates/T-0245.toml`.
- **Scope discipline:** only the approved paths were staged. `docs/book/**`,
  `docs/evidence/acceptance/**` and `orchestration/feature-updates/T-0261A.toml`
  were explicitly excluded - the T-0261A docs lane is mid-edit and its claim
  stays held. The regenerated board shows T-0245 `in_progress` and T-0255
  `queued`.
- **Next:** finish T-0245 (A-25 placement and A-31 request/tracker), then T-0261A
  docs, T-0255, and T-0262 final package/install/captures/handoff.

### T-0261A checkpoint (2026-11-21, shipped-tool guides and security pages)

- **HEAD before:** `fc59072` (`checkpoint(acceptance): signing card, verification
  and E-signed badge [T-0245B]`). **HEAD after:** this checkpoint commit
  (`checkpoint(docs): shipped-tool guides and security pages [T-0261A]`;
  `git log -1 --oneline`). Linear history on `main`, not pushed, no amend/rebase.
- **Task state:** T-0261A is `verified`; its claim is released after this
  checkpoint. T-0245 stays `in_progress` with its app claim held and untouched;
  T-0245 round C has not started. T-0261 stays `queued` as the final close-out
  that will re-check these pages against the installed build. The regenerated
  board shows T-0261A `verified`, T-0245 `in_progress`.
- **What landed:** a task page per viewer surface reachable in the current build
  (open/save/print, library, pages panel, comments, Fill & Sign, signing, search,
  assistant), nested under a rewritten `SUMMARY.md` and `gui.md` overview; a
  rewritten `security.md` plus `security/signatures.md`,
  `security/assistant-data.md` and `security/redaction.md`; `notices.md` for the
  bundled Courier Prime font and the seven assistant crates; and corrections to
  `privacy.md` and `forms.md`. The pages describe the shipped code, not the plan:
  the stale `Ctrl+O` hint, 2 GiB input cap, atomic save, exact encrypted-save
  refusal, 8 MiB XFDF/FDF cap, 10,000 search-match cap, 24,000-character fenced
  assistant excerpt, and the `Valid && bytes_match && covers_entire_file &&
  !appended` E-signed badge condition. Signing proves integrity, not identity;
  vector paths under a redaction mark are a named limit.
- **Integrator gates:** `cargo xtask board` regenerated `orchestration/BOARD.md`
  and confirms T-0261A `verified`. `cargo xtask features verify` reports
  `FEATURES OK: 145 rows, 33 verified` (unchanged; three non-test `tests`
  citations skipped). `cargo xtask docs` was run by the lane in
  `target/integration` and printed `DOCS OK` (exit 0); the lane's recursive
  relative-link pass over `docs/book/src` printed `ALL LOCAL LINKS RESOLVE`.
  Both are cited from the lane and its evidence; the workspace was not rerun.
- **Matrix:** the T-0261A fragment carries empty `feature_rows` and
  `promoted_rows`; no feature-row status changed and `docs/features.toml` is not
  in the commit.
- **Evidence:** `docs/evidence/acceptance/T-0261A.md` (full verification block);
  fragment `orchestration/feature-updates/T-0261A.toml`.
- **Scope discipline:** only the approved paths were staged. No `crates/**`,
  `fuzz/**` or `docs/evidence/ui/**` path was staged; `git status` before staging
  showed nothing staged at all. The T-0245 app work stays uncommitted with its
  claim held.
- **Next:** T-0245 round C (A-25 placement, A-31 request/tracker) on the app
  lane, then T-0261 close-out and T-0262 final package/install/captures/handoff.
