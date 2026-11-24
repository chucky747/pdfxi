# FEATURES

`docs/features.toml` is the canonical parity matrix. It contains 146 feature
rows across 14 domains. This page is the human summary; the TOML is what
`cargo xtask features verify` and `goal-verify --all` enforce.

Current status: 142 verified (58 `gui`, 84 `cli`), 0 library, 1 model, 1
deferred, 3 replaced (146 rows; see ADR 0009). A `verified` row runs on real
document bytes and a user can reach it through the GUI or the CLI, and it names
that host in `docs/features.toml`. `library` rows are real byte-level
implementations with tests but no GUI or CLI caller; `model` rows are in-memory
models, plans or host-boundary stubs; `deferred` is a P3 row with a recorded
blocker. The two `replaced` rows (`CREATE-011`, `CREATE-012`) are accepted
terminal states under ADR 0008/0009.

`cargo xtask features verify` enforces the honest matrix: every verified row
needs an existing repository evidence path, a `gui`/`cli` host, and test
citations that resolve and are not `#[ignore]`. CI is not used; the local
Windows gate is the verified one.

This matrix is deliberately honest rather than green. `cargo xtask
goal-verify --all` fails while any row is `library`, `model` or `deferred`;
that is the intended result of ADR 0009, not a regression to suppress. The
2026-11-23 review corrected rows that overstated reachability, including
REVIEW-001, EXPORT-006, VIEW-002, VIEW-006, VIEW-012, CREATE-007, CREATE-008,
STD-007, ACCESS-006, REVIEW-007, AUTO-008, SEC-003 and PLAT-002. `ACCESS-007`
is new: canvas text and choice widgets now accept AccessKit `SetValue`.

## Detailed feature pages

Narrative documentation for the M1 viewer and CLI slice:

- [Viewer slice](features/view.md) - open, page display, zoom, rotate,
  thumbnails, bookmarks, search, text selection.
- [Command-line interface](features/cli.md) - `pdfxi info`, `text`, `render`,
  `search`, `action`, and `droplet` commands with examples.

Task-oriented M2 chapters live in the [mdBook](book/src/README.md):
[authoring](book/src/authoring.md), [forms](book/src/forms.md),
[security](book/src/security.md), [accessibility](book/src/accessibility.md),
[automation](book/src/automation.md), and
[standards/preflight](book/src/standards.md).

## Status legend

| Status | Meaning |
|---|---|
| `planned` | Scoped, not started |
| `in-progress` | Claimed and being implemented |
| `verified` | End-user reachable on real documents through the GUI or CLI; tested, documented, evidence recorded |
| `library` | Real byte-level implementation with tests, but no GUI or CLI caller |
| `model` | In-memory model, plan, contract or host-boundary stub, or reachable only with demo data |
| `replaced` | Adobe cloud/proprietary behavior replaced by a local equivalent |
| `deferred` | P3 only, with a note and an ADR |

## Domains

| Domain | Rows | Primary milestones |
|---|---:|---|
| View & Navigate | 19 | M1, M2 |
| Create | 14 | M2, M5 |
| Edit | 15 | M2, M3 |
| Export | 9 | M2, M4, M5 |
| Forms | 10 | M3 |
| Security | 13 | M3, M4 |
| Review | 9 | M2, M6 |
| Print Production | 8 | M4, M6 |
| Accessibility | 6 | M2, M6 |
| Automation | 10 | M6 |
| Document Management | 6 | M2, M5 |
| Multimedia and 3D | 5 | M6 |
| Platform | 9 | M0, M5, M7 |
| Standards | 12 | M1, M4 |

## Parity rules

1. A feature is not complete until its TOML row has:
   - `status = "verified"`
   - at least one test identifier in `tests`
   - a documentation path in `docs`
   - an evidence path or command in `evidence`
2. P0, P1, and P2 rows cannot be `deferred`. If a dependency is unavailable,
   implement a local equivalent and mark it `replaced`.
3. Retired Adobe services map to local equivalents:
   - FormsCentral â†’ local form distribution and response store.
   - EchoSign â†’ local PKI signature workflow with audit trail.
   - Acrobat.com / SendNow â†’ local share links and LAN transfer.
   - LiveCycle Rights Management â†’ local policy service.
4. Adobe binary plug-ins are not supported. PDFXI exposes a WASM/JSON plugin API.
5. Every feature row links to an automated test and a documentation page before
   the final gate.

## Verification

```text
cargo xtask features verify
cargo xtask goal-verify --all
```

`features verify` validates schema, unique IDs, priority/status values, and
evidence requirements. `goal-verify --all` additionally requires every row to be
verified or replaced.
