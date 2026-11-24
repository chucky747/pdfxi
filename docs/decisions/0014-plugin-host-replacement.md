# ADR 0014 - Sandboxed plugin contract replaces the WASM plugin host

- Status: accepted (2026-11-24)
- Date: 2026-11-24
- Closes AUTO-008 under the ADR 0008 / ADR 0009 replacement path.
- Related: `docs/decisions/0008-local-replacements.md`,
  `docs/decisions/0009-feature-status-honesty.md`, `docs/features.toml`.

## Context

AUTO-008 was scoped as a WebAssembly plugin host: load untrusted `.wasm`
modules, run them in a sandbox with strict CPU and memory limits, and grant no
filesystem or network access. The port already has a plugin contract in
`crates/pdfxi-cli/src/plugin.rs` and the `pdfxi-automation` plugin output
validator:

- `pdfxi plugin plan MANIFEST --operation NAME [--grant CAP ...]` parses a real
  manifest, applies a deny-by-default `CapabilityGrants` set and an
  `InvocationBudget`, and prints the six gate steps without executing code;
- `pdfxi plugin output --type TYPE [value flags]` validates one typed result
  against the output budget, which is the ABI boundary for whatever produces it;
- the capability set is closed (document text, and the three other documented
  capabilities), and no filesystem, process or network capability exists.

That contract is already the host gate and the typed boundary. Adding a
WebAssembly runtime would introduce a large new dependency tree and a second
execution model for one P3 feature whose user-facing surface is the same plan
and output contract. The plan's locked decision for this close-out allows the
documented replacement path before the lane starts.

## Decision

1. AUTO-008 is closed as **replaced** by the existing sandboxed plugin contract:
   a manifest is admitted only through the deny-by-default plan gate, and every
   value crossing back is validated against a typed `OutputBudget`.
2. No WebAssembly runtime is embedded in 0.1.5 and no `.wasm` file is loaded or
   executed. The CLI never claims to run one.
3. The plugin surface remains `pdfxi plugin plan` and `pdfxi plugin output`, with
   the existing capability set and deterministic reports.
4. A future WebAssembly host is a new feature row, not a retroactive claim on
   AUTO-008.

## Consequences

- The matrix reports AUTO-008 as `replaced` with this ADR and the existing
  plugin tests as evidence, which is an honest terminal state under ADR 0009.
- Scripts keep a stable, deterministic gate with no new dependency, and the
  "no filesystem or network access" claim is true because no plugin code runs.
- Extensibility beyond the closed capability set requires a new ADR and a new
  row, not a change to AUTO-008's status.
