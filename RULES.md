# RULES.md

These rules are binding for every agent, subagent, reviewer, and integrator in
this repository. They exist so that many agents can work asynchronously in one
canonical workspace without checkouts, merge conflicts, or silent regressions.

## 1. Mission and source of truth

The project is **PDFXI**, a performant, fully documented Rust functional remake
of Adobe Acrobat XI Pro. It is a clean-room implementation. Do not copy Adobe
code, assets, icons, help text, or proprietary formats. Use public specifications
and permissively licensed dependencies.

The source of truth, in order:

1. `docs/GOAL.md` — objective, end condition, acceptance criteria.
2. `docs/PLAN.md` — campaigns, milestones, lane map, risks.
3. `docs/features.toml` and `docs/FEATURES.md` — feature parity matrix.
4. `docs/ARCHITECTURE.md` — crate boundaries and contracts.
5. `orchestration/BOARD.md` and `orchestration/tasks/` — live work state.
6. Your lane file in `.opencode/agents/`.

If these disagree, stop editing and write an ADR or a blocker. Never guess a
public interface into existence.

## 2. Autonomy and communication

- Do not ask the user questions. Make the conservative decision, record it in
  `docs/decisions/` or `docs/ASSUMPTIONS.md`, and continue.
- If a task is blocked, write `orchestration/blockers/<task-id>.md` with the
  exact command, error, attempted fixes, and the next viable alternative.
- Continue other lanes. Only a blocker that prevents every lane stops dispatch.
- Report evidence, not claims. Paste command output, test names, file paths, and
  benchmark numbers.
- Keep status updates short. The board is the durable state, not the chat.

## 3. One workspace, no checkouts

The canonical workspace is the only workspace. These are forbidden for every
agent:

```text
git clone
git checkout
git switch
git worktree
git reset --hard
git clean
git stash
git branch <new-branch>
```

- Do not create branches, worktrees, or second clones.
- Do not run `cargo clean` or delete another lane's target directory.
- Git is write-only for the integrator: `git add`, `git commit`, `git status`,
  `git diff`, `git log`, and read-only inspection.
- The integrator commits only verified task paths at quiescent checkpoints.
- Never rewrite history, force-push, or amend a commit that another agent has
  used as evidence.

## 4. Claims and file ownership

- Every task has an `owns` list of path globs in its task JSON.
- Before editing, run `cargo xtask claim <task-id> --lane <lane>` (or the
  PowerShell fallback during bootstrap). The claim is atomic.
- If an overlapping claim exists, do not edit. Report the conflict to the
  orchestrator and wait for reassignment.
- Read anything; write only your claim. Shared files are integrator-owned:
  - `Cargo.toml`, `Cargo.lock`, `rust-toolchain.toml`
  - `AGENTS.md`, `RULES.md`, `README.md`, `LICENSE`, `NOTICE`
  - `.opencode/**`, `opencode.json`
  - `orchestration/**`
  - `xtask/**`, `.github/**`
  - `docs/GOAL.md`, `docs/PLAN.md`, `docs/features.toml`
- Lanes report feature changes as fragments in
  `orchestration/feature-updates/<task-id>.toml`; the integrator merges them into
  `docs/features.toml`. Do not edit the matrix directly.
- Lane leads may spawn specialists at depth 2, but the specialist inherits the
  same claim and ownership restrictions.
- Use `CARGO_TARGET_DIR=target/lanes/<lane>` for lane builds. The integrator uses
  `target/integration`.

## 5. Build, test, and verification

- Rust edition 2024, pinned stable toolchain, rustfmt, clippy.
- `cargo fmt --all --check` and `cargo clippy --workspace --all-targets
  --all-features -- -D warnings` must pass.
- No `unwrap`, `expect`, `panic!`, `todo!`, or `unimplemented!` in non-test code
  unless the invariant is proven and documented with a comment.
- Libraries use `thiserror`; application boundaries may use `anyhow`.
- Every public item has rustdoc. Libraries deny missing docs.
- Every bug fix includes a regression test.
- Every new behavior includes at least one test that fails without the change.
- Fast tests: deterministic, no network, no GUI, no sleeps, under 90 seconds
  total. Heavy tests are feature-gated or in the `full` profile.
- Use `cargo nextest` when available; `cargo test` is the fallback.
- Fuzz targets: parser, filters, content streams, forms, crypto. A release
  requires a clean 24-hour fuzz run.
- Security-sensitive code gets a reviewer and a negative test.

## 6. Documentation

Documentation ships with the code, not after it:

- Rustdoc for every public module, type, function, trait, and error.
- `docs/features.toml` updated with status, tests, docs, and evidence.
- An ADR in `docs/decisions/` for any irreversible or cross-lane decision.
- mdBook pages updated when user-visible behavior changes.
- `cargo doc --workspace --no-deps` must pass with warnings denied.
- `cargo xtask docs` checks links and generated references.

## 7. Performance

- Performance is a feature. Benchmark before and after a change that touches
  parsing, rendering, text extraction, search, save, or OCR.
- Avoid unbounded memory growth. Stream or memory-map large PDFs.
- Parallelize page-level work with bounded rayon pools. Never spawn unbounded
  threads.
- Cancellation must be honored when a user closes a document or a job.
- A regression over 10% on a tracked benchmark blocks verification.

## 8. Security and untrusted input

- Every PDF is hostile until proven otherwise. Fuzz and bounds-check parsers.
- JavaScript runs in a sandbox with no filesystem, network, or process access,
  and with CPU, memory, and wall-clock limits.
- Validate every path used by attachments, exports, memory, and plugins. Reject
  traversal and symlink escapes.
- Redaction removes content; it must not merely cover it. Verify with text and
  image extraction after redaction.
- Never commit secrets, keys, tokens, customer documents, or private corpora.
- Use `cargo deny` and `cargo audit` in CI.

## 9. Memory protocol (Anthropic model)

- At session start, read the global `MEMORY.md` index and the current project's
  `PROJECT.md`.
- At task end, append a short note to the memory inbox. Do not rewrite curated
  memory unless you are the memory-librarian.
- The memory tool semantics are `view`, `create`, `str_replace`, `insert`,
  `delete`, and `rename`, scoped to `/memories`.
- Never store secrets, credentials, raw prompts, or full transcripts.
- Keep `MEMORY.md` under ~150 lines and individual memories under ~300 lines.

## 10. Definition of done

A task is verified only when all of the following are true:

1. Code compiles with no warnings.
2. New behavior has meaningful tests; the tests pass.
3. Public API is documented; the feature matrix row is updated.
4. A reviewer has checked correctness, security, and scope.
5. Evidence is pasted into the task record.
6. The claim is released and the integrator has snapshotted the verified paths.
7. A memory inbox note exists.

## 11. Git conventions

- Linear history on `main`. No merge commits, no worktrees.
- Integrator commits: `checkpoint(<milestone>): <summary> [<task-ids>]`.
- Release tags only after `cargo xtask goal-verify --all` passes.
- Commit only after fast tests pass; never commit a knowingly broken workspace.
- If a lane breaks the workspace, the integrator reverts only that lane's paths
  or asks the owner to fix; destructive git commands remain forbidden.

## 12. Legal and clean-room

- Do not decompile, disassemble, or copy Adobe binaries.
- Do not copy Adobe UI text, icons, or documentation wholesale.
- Adobe is a trademark of Adobe Inc. PDFXI is not affiliated with Adobe.
- All dependencies must pass the license allowlist in `deny.toml`.
- Bundled PDFium, Tesseract, and other third-party components require notices.
