# FEATURES

`docs/features.toml` is the canonical parity matrix. It contains 145 feature
rows across 14 domains. This page is the human summary; the TOML is what
`cargo xtask features verify` and `goal-verify --all` enforce.

Current status: 54 verified, 13 library, 76 model, 2 replaced, 0 in-progress
(145 rows; see ADR 0009). A `verified` row runs on real document bytes and a
user can reach it through the GUI or the CLI. `library` and `model` are honest
statuses for code that exists but is not a reachable product feature, so 91
rows are not verified.

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
| Security | 11 | M3, M4 |
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
