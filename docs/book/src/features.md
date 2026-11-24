# Feature matrix

The canonical parity matrix is `docs/features.toml` (146 rows). Its human
summary is [FEATURES.md](../../FEATURES.md). At this release every row is
terminal: 142 are `verified`, 3 are `replaced` under ADR 0008/0009/0014, and
PLAT-002 is `deferred` because the project is Windows-only.

Detailed M1 pages:

- [Viewer slice](../../features/view.md)
- [Command-line interface](../../features/cli.md)

A row is `verified` only with a test identifier, a docs path, and an evidence
path. Lanes submit changes as fragments in `orchestration/feature-updates/`;
the integrator merges them. Verified rows currently include the viewer slice,
CLI, annotations, forms, redaction, signatures, HTML creation, flattening, and
the linearized writer; see `docs/FEATURES.md` for the live count.
