# AGENTS.md

**STOP. Read `RULES.md` in full before any action.**

This file is intentionally a pointer. The binding rules for every agent in this
repository live in `RULES.md`.

## Read order

1. `RULES.md`
2. `docs/GOAL.md`
3. `docs/PLAN.md`
4. `orchestration/BOARD.md`
5. `docs/ARCHITECTURE.md`
6. Your lane file in `.opencode/agents/`

## Non-negotiables

- Never ask the user a question. Record a blocker in `orchestration/blockers/`
  and keep other lanes moving.
- Never run `git checkout`, `git switch`, `git worktree`, `git reset --hard`,
  `git clean`, `git stash`, `git clone`, or any operation that creates a second
  checkout. The workspace is shared and canonical.
- Only edit paths claimed by your task. Shared files belong to the integrator.
- Every task needs meaningful tests, documentation, a feature-matrix update,
  and pasted evidence before it can be marked verified.
- Follow the global memory skill: read `MEMORY.md` at session start, write a
  short inbox note at task end, and never store secrets.
- Prefer small, verifiable steps. A green `cargo xtask goal-verify --milestone
  <milestone>` is the only definition of done.

## Commands

```text
cargo xtask verify                 # fmt, clippy, tests, docs
cargo xtask goal-verify --milestone M0
cargo xtask goal-verify --all
cargo xtask claim T-0001 --lane core
cargo xtask release T-0001
cargo xtask board
cargo xtask features apply --clean
```

If a command is not implemented yet, implement it rather than bypassing it.
