# FEATURES

`docs/features.toml` is the canonical parity matrix. It contains 140 feature
rows across 14 domains. This page is the human summary; the TOML is what
`cargo xtask features verify` and `goal-verify --all` enforce.

Current status: 13 verified, 25 in-progress, 102 planned.

## Detailed feature pages

Narrative documentation for the M1 slice (all **in progress**, not verified):

- [Viewer slice](features/view.md) â€” open, page display, zoom, rotate,
  thumbnails, bookmarks, search, text selection.
- [Command-line interface](features/cli.md) â€” intended `pdfxi info`, `text`,
  and `render` commands with examples.

An mdBook entry point is available in [docs/book/src/README.md](book/src/README.md).

## Status legend

| Status | Meaning |
|---|---|
| `planned` | Scoped, not started |
| `in-progress` | Claimed and being implemented |
| `verified` | Implemented, tested, documented, evidence recorded |
| `replaced` | Adobe cloud/proprietary behavior replaced by a local equivalent |
| `deferred` | P3 only, with a note and an ADR |

## Domains

| Domain | Rows | Primary milestones |
|---|---:|---|
| View & Navigate | 17 | M1, M2 |
| Create | 14 | M2, M5 |
| Edit | 15 | M2, M3 |
| Export | 9 | M2, M4, M5 |
| Forms | 10 | M3 |
| Security | 11 | M3, M4 |
| Review | 9 | M2, M6 |
| Print Production | 8 | M4, M6 |
| Accessibility | 6 | M2, M6 |
| Automation | 10 | M6 |
| Document Management | 6 | M2, M5 |
| Multimedia and 3D | 5 | M6 |
| Platform | 7 | M0, M5, M7 |
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
