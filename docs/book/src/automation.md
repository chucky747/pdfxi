# Automation

Automation spans action files, batch execution, droplets, and a sandboxed
JavaScript subset. The CLI is the scriptable surface; see the
[CLI reference](cli.md) or the authoritative [docs/CLI.md](../../CLI.md) for
exact flags and JSON shapes.

## Action files

An action file is tab-delimited UTF-8. Blocks are separated by blank lines, and
names are unique within a file. The grammar is stable (`pdfxi-automation`):

```text
ACTION	<name>	<description>
STEP	<step-id>	<kind>
PARAM	<key>	<value>
```

Backslashes, tabs, newlines, and carriage returns in values are escaped. Step
kinds are `import`, `convert`, `watermark`, `security`, `export`, and
`custom-command`. Parameters include `source` (optional import override),
`format` (default `pdf`), `text`, `password`, `permissions`, `target` (required
for `export`), `command` (required for `custom-command`), and `arguments`
(`|`-separated).

There are no bundled named actions; actions are user-defined. `nightly` in the
examples is a sample name, not a shipped action.

## Running actions

```text
pdfxi action list <FILE>
pdfxi action show <FILE> <NAME>
pdfxi action run <FILE> <NAME> --input <PATH>... [--output-dir DIR] [--json]
```

Validation happens before any job runs: the action file must parse, the name must
exist, the action must validate, at least one input must be supplied, and every
input path must exist. Jobs run in input order as `job-0`, `job-1`, …, with
per-file failure isolation; the command exits non-zero if any job failed.
`--output-dir` only resolves relative export targets — it never writes files.
Absolute or traversal targets are rejected.

### The staging-only executor

At this milestone the CLI's `EchoOperations` is deliberately side-effect free:
`import` stages bytes in memory (bounded at 512 MiB), the other steps pass the
artifact along with a deterministic message, and `export` records a resolved
path. Real PDF mutation is owned by other crates and is not coupled to the CLI
yet. Replacing the adapter later requires no change to the commands or their
output shapes. This keeps runs deterministic and safe on hostile inputs.

## Droplets

`pdfxi droplet <ACTION_FILE> <NAME> <INPUT>... [--json]` is exactly `action run`
with text output and no `--output-dir`. Its variadic positional inputs make it a
natural Windows Explorer "drop onto target" verb:

```text
"C:\Program Files\PDFXI\pdfxi.exe" droplet "C:\Program Files\PDFXI\staging.act" nightly "%1"
```

Use `%*` to forward several dropped files. One deterministic line per input is
printed in drop order; a failed input sets a non-zero exit so `.bat` wrappers and
task scheduler jobs can react.

## JavaScript subset

`AUTO-004` (in-progress): `pdfxi-automation::js` provides a capability-free,
whitelisted JavaScript subset — a lexer, parser, AST, interpreter, host boundary,
and strict resource limits. Builtins include `Number`, `String`, `Boolean`,
`parseInt`, `parseFloat`, `isNaN`, `Math.{abs,floor,ceil,round,min,max,sqrt,pow}`,
and selected string/array methods. Dangerous globals are rejected at parse time;
unsupported syntax is a typed error.

Execution is bounded by step, loop-iteration, recursion-depth, call-count, and
value-size limits, observes cancellation before and during a run, and rejects
deeply nested values without stack overflow. Host objects are explicit through
`HostObject` / `HostRegistry` / `NoHostObject` and calls are audited. The default
host exposes no filesystem, network, or process capability. No real PDF mutation
is wired to scripts yet.

## Row status

`AUTO-001` (Action Wizard), `AUTO-002` (batch processing), `AUTO-003`
(droplets), `AUTO-004` (JS subset), and `AUTO-005` (CLI coverage) are
in-progress. `AUTO-009` (`info --json`) and `AUTO-010` (text extraction) are
verified. `AUTO-006`–`AUTO-008` (local JSON API, custom toolsets, WASM plugin
host) are planned.

## Related pages

[Getting started](getting-started.md) · [CLI reference](cli.md) ·
[Feature matrix](../../FEATURES.md)
