# ADR 0004 — One workspace, claims instead of worktrees

## Status

Accepted.

## Context

The orchestra runs many agents asynchronously. Worktrees and branches create
merge overhead, duplicate builds, and checkout races. The agents share one
filesystem.

## Decision

- One canonical workspace.
- No clones, worktrees, branches, checkouts, switches, resets, or stashes.
- Tasks declare disjoint path globs.
- `cargo xtask claim` is atomic and rejects overlap.
- Shared files are integrator-only.
- Per-lane `CARGO_TARGET_DIR` avoids build contention.
- A single integrator writes linear git checkpoints.

## Consequences

- No merge conflicts at the git layer.
- Discipline is enforced by claims and permissions.
- A broken shared interface must be fixed through an RFC task.
