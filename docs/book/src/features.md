# Feature matrix

The canonical parity matrix is `docs/features.toml` (143 rows). Its human
summary is [FEATURES.md](../../FEATURES.md).

Detailed M1 pages:

- [Viewer slice](../../features/view.md)
- [Command-line interface](../../features/cli.md)

A row is `verified` only with a test identifier, a docs path, and an evidence
path. Lanes submit changes as fragments in `orchestration/feature-updates/`;
the integrator merges them. Verified rows currently include the viewer slice,
CLI, annotations, forms, redaction, signatures, HTML creation, flattening, and
the linearized writer; see `docs/FEATURES.md` for the live count.
