# FEATURES

`docs/features.toml` is the canonical parity matrix. It contains 133 feature
rows across 14 domains. This page is the human summary; the TOML is what
`cargo xtask features verify` and `goal-verify --all` enforce.

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
| View & Navigate | 15 | M1, M2 |
| Create | 14 | M2, M5 |
| Edit | 15 | M2, M3 |
| Export | 9 | M2, M4, M5 |
| Forms | 10 | M3 |
| Security | 11 | M3, M4 |
| Review | 9 | M2, M6 |
| Print Production | 8 | M4, M6 |
| Accessibility | 6 | M2, M6 |
| Automation | 8 | M6 |
| Document Management | 6 | M2, M5 |
| Multimedia and 3D | 5 | M6 |
| Platform | 7 | M0, M5, M7 |
| Standards | 10 | M1, M4 |

## Parity rules

1. A feature is not complete until its TOML row has:
   - `status = "verified"`
   - at least one test identifier in `tests`
   - a documentation path in `docs`
   - an evidence path or command in `evidence`
2. P0, P1, and P2 rows cannot be `deferred`. If a dependency is unavailable,
   implement a local equivalent and mark it `replaced`.
3. Retired Adobe services map to local equivalents:
   - FormsCentral → local form distribution and response store.
   - EchoSign → local PKI signature workflow with audit trail.
   - Acrobat.com / SendNow → local share links and LAN transfer.
   - LiveCycle Rights Management → local policy service.
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
