# Orchestration

This directory is the durable state of the multi-agent orchestra.

- `BOARD.md` — generated summary; do not edit by hand.
- `tasks/*.json` — one task per file; the source of truth.
- `schema/task.schema.json` — task schema.
- `claims/` — atomic claim files; gitignored.
- `blockers/` — blocker reports; committed when useful.
- `agents/` — optional heartbeat files; gitignored unless they contain evidence.
- `logs/` — runtime logs; gitignored.

## Commands

```text
cargo xtask board
cargo xtask claim T-0006 --lane core
cargo xtask release T-0006
cargo xtask goal-verify --milestone M0
```

## Task fields

| Field | Meaning |
|---|---|
| `id` | Stable `T-####` identifier |
| `title` | Short imperative title |
| `lane` | Owning lane |
| `status` | `queued`, `claimed`, `in-progress`, `review`, `verified`, `blocked` |
| `priority` | `P0`–`P3` |
| `owns` | Path prefixes/globs the lane may write |
| `depends_on` | Task IDs that must be verified first |
| `acceptance` | Exact command or check |
| `evidence` | Path or command output reference |

## Rules

- Lanes read everything, write only their claim.
- Shared files are integrator-only.
- A claim conflict is a hard stop for that task, not the lane.
- Review every completed task before snapshotting.
- The board is regenerated after task changes.
- No worktrees, checkouts, branches, resets, or stashes.
