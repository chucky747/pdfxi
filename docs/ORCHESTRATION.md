# ORCHESTRATION

## Model

- **Orchestrator:** DeepSeek V4.1 Flash with a 500k effective context
  (588,000 hard limit, 15% compaction buffer, ~499,800 ceiling).
- **Lane leads:** multiple concurrent `opencode-go/hy4-flash-free` sessions.
- **Fallbacks:** `opencode/longcat-2.5-free`, `opencode/beam-free`.
- **Reviewer:** read-only Hy4 Flash Free.
- **Integrator:** Hy4 Flash Free with the only git write permission.
- **Memory-librarian:** free fallback model, memory path only.

The orchestrator dispatches up to five lanes in parallel, in one model step,
using the `subagent` tool. It does not poll; OpenCode wakes it when a child
idles. It reviews evidence, asks for fixes, and dispatches the next task.

## One canonical workspace

There are no worktrees, branches, clones, checkouts, switches, resets, or
stashes. All agents share `D:\Documents\opencode-work`.

Concurrency is safe because:

1. Task JSON declares `owns` globs.
2. `cargo xtask claim` creates an atomic claim and rejects overlap.
3. Shared files are integrator-owned.
4. Lane builds use separate target directories.
5. Interfaces are frozen per milestone; changes need an RFC task.
6. One git writer snapshots verified paths linearly.

## Task lifecycle

```text
queued -> claimed -> in-progress -> review -> verified -> snapshotted
                         |            |
                         v            v
                      blocked      changes-requested
```

A task JSON:

```json
{
  "id": "T-0001",
  "title": "Bootstrap repository and toolchain",
  "lane": "integrator",
  "status": "in-progress",
  "priority": "P0",
  "owns": ["AGENTS.md", "RULES.md", "Cargo.toml"],
  "depends_on": [],
  "acceptance": "cargo xtask goal-verify --milestone M0",
  "evidence": ""
}
```

The board is generated; edit task files, then run `cargo xtask board`.

Feature status changes are written as fragments to
`orchestration/feature-updates/<task-id>.toml` and merged by the integrator.
Lanes never edit `docs/features.toml` directly.

## Claims

```text
cargo xtask claim T-0001 --lane integrator
cargo xtask release T-0001
cargo xtask features apply --clean
```

The claim helper normalizes globs and treats prefixes as overlapping. A conflict
is a hard stop for that task, not for the lane: pick another ready task.

## Git

- Integrator only.
- `checkpoint(<milestone>): <summary> [<task-ids>]`.
- One linear history. No merge commits, rebases, amends, or force pushes.
- Release tags only after `goal-verify --all`.
- Snapshots happen when the claimed lanes are between tasks.

## Blockers

Write `orchestration/blockers/<task-id>.md`:

```md
# Blocker T-0042

Command: cargo test -p pdfxi-render
Error: ...
Attempts:
1. ...
Next alternative: ...
Lanes still running: core, author, forms
```

Continue every independent lane. Do not ask the user.

## Native goal and campaigns

`scripts/goal.ps1` uses the native OpenCode `/goal` command:

- `arm` posts `{"name":"goal","text":"..."}` to
  `/api/session/{sessionID}/command`.
- `status` posts to `/api/rpc/opencode.goal/get`.
- `resume` posts `{"name":"goal","text":"resume"}` when paused for the 50-round
  cap or a server restart.
- `advance` verifies the current campaign, arms the next one, and records the
  checkpoint.

The objective is capped at 4,000 characters, so it points to
`docs/GOAL.md`/`docs/PLAN.md` instead of restating the plan.

## Context discipline

- Orchestrator reads `BOARD.md`, not every source file.
- Subagents return summaries, test names, and evidence paths.
- Tool output is capped at 1,200 lines / 48 KB.
- Compaction keeps 30,000 recent tokens and starts at ~500k.
- The board and memory are the durable state across compactions.

## Failure handling

| Failure | Response |
|---|---|
| Hy4 rate limit | Dispatch fallback model; reduce concurrency to 3 |
| Claim conflict | Requeue; do not edit |
| Broken shared interface | Freeze interface, assign RFC task |
| Red test | Owner fixes; reviewer verifies; no snapshot until green |
| Goal round cap | Controller resumes; round budget resets |
| Server restart | Controller resumes paused goal; board survives |
| Missing external tool | Implement adapter or local fallback; record ADR |
