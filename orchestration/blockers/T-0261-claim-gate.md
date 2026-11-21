# T-0261 formal claim refused by dependency gate

- Task: T-0261 (docs) - documentation close-out for the acceptance campaign.
- Recorded: 2026-11-21
- Status: host-level / orchestration ordering; documentation work proceeded on task-named paths only.

## Exact command and error

```text
$ cargo xtask claim T-0261 --lane docs
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.24s
     Running `target\debug\xtask.exe claim T-0261 --lane docs`
Error: task T-0261 depends on T-0260, whose status is queued (must be verified)

error: process didn't exit successfully: `target\debug\xtask.exe claim T-0261 --lane docs` (exit code: 1)
```

## Cause

`xtask claim` checks `depends_on` statuses before writing a claim. T-0260 (qa)
is still `queued`; its two sub-tasks T-0260A and T-0260B are `verified` but the
parent was not flipped, and T-0260 carries host-level blockers
(`T-0260A-fuzz-toolchain.md`, `T-0260B-budget.md`). Only the orchestrator may
change task status, so this lane cannot unblock the dependency.

A secondary quirk: T-0260's claim owns `orchestration/blockers/**`, which also
appears in T-0261's `owns`, so even with the dependency satisfied the atomic
claim scan would report a conflict on that shared directory.

## Attempted fixes

1. Re-ran `cargo xtask claim T-0261 --lane docs` - same dependency error.
2. Confirmed T-0260A/T-0260B are `verified` and T-0260 is still `queued`, with a
   qa claim held in `orchestration/claims/T-0260.json`.

## Next viable alternative / decision

Documentation close-out is independent of the QA end-to-end result: all app
tasks it documents (T-0249, T-0252, T-0253, T-0257, T-0263, T-0264, T-0269)
are `verified`, and T-0269 already captured a snapshot of the shipped build.
Proceed on exclusively task-named paths - `docs/book/**`,
`orchestration/feature-updates/T-0261.toml`,
`docs/evidence/acceptance/T-0261.md`, and this task-named blocker file - and
touch no path belonging to another task's work product. No app source,
`docs/features.toml`, `Cargo.toml`, `BOARD.md`, or `xtask/**` is edited.

The orchestrator should mark T-0260 verified (or relax the docs dependency)
and register the T-0261 claim at the next quiescent checkpoint.
