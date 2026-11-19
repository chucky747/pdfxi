# ADR 0005 — Native goal plus controller

## Status

Accepted.

## Context

OpenCode 2.2.1+ provides `/goal` with a reviewer, a 4,000-character objective
limit, 50 rounds, and a pause on server restart. A full project exceeds one
round budget.

## Decision

Use the native goal as the control plane and `scripts/goal.ps1` as a controller:

- Arm through `POST /api/session/{id}/command` with `name=goal`.
- Read through `POST /api/rpc/opencode.goal/get`.
- Resume after the round cap or a restart.
- Advance campaigns only after `cargo xtask goal-verify`.
- Write a blocker after repeated identical pauses.

## Consequences

- The reviewer and continuation behavior remain native.
- The controller must be robust to service URL/password discovery.
- The board and memory are the durable state across compactions.
