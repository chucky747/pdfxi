# Blocker: T-0253 harness coverage edit denied (integrator permissions)

Status: **resolved (2026-11-20 21:51)**. The fix landed in the working tree
without this session: `PENDING_COVERAGE` now holds the six assistant variants
and `all_commands()` is unchanged. `cargo test -p pdfxi-app --features ui,pdfium`
is green with 514 lib + 24 harness, 0 failed. The record below is kept because
the permission gap and the two-list contradiction will recur.

The T-0253 app lane added six `AppCommand` variants and the shared harness
`crates/pdfxi-app/tests/ui_controls.rs` must learn them. The integrator cannot
make that edit: `crates/pdfxi-app/**` is not in the integrator edit allowlist,
and the write is rejected by the permission layer.

## Exact command and error

```
cargo test -p pdfxi-app --features ui,pdfium
```

```
test source_variants_are_all_covered ... FAILED
failures:
    source_variants_are_all_covered
test result: FAILED. 23 passed; 1 failed; 0 ignored; 0 measured
error: test failed, to rerun pass `-p pdfxi-app --test ui_controls`
```

Failure text:

```
AppCommand coverage drift (T-0212 source guard): source variants missing from
all_commands()/PENDING_COVERAGE: [AssistantAsk, AssistantConnect,
AssistantDisconnect, AssistantPair, AssistantSelectModel, AssistantStop];
harness names with no source variant: [].
Add each missing variant to all_commands() with a real proof, or record it in
PENDING_COVERAGE with a reason.
```

Lib tests are green in the same run: 514 passed, 0 failed. Harness:
`repository_shell_has_no_dead_controls` passes; only the source guard fails.

## Attempted fix

Editing `crates/pdfxi-app/tests/ui_controls.rs` was rejected:

```
Permission denied: edit
```

`crates/pdfxi-app/tests/ui_controls.rs` and `crates/pdfxi-app/src/settings.rs`
both return the same rejection. `.opencode/agents/integrator.md` denies
`edit *` and allows a fixed list that includes `crates/pdfxi-document/**` but
not `crates/pdfxi-app/**`, so no app path is writable by this session. The edit
was not attempted through the shell, because that would bypass the permission
model rather than satisfy it.

## Contradiction in the requested edit

The requested change was "add the six variants to `all_commands()` and to
`PENDING_COVERAGE`". Those two cannot both hold in this harness:

1. `source_variants_are_all_covered` asserts that every `PENDING_COVERAGE`
   variant is absent from `all_commands()`
   ("`{variant}` is both covered and pending; remove one entry", ui_controls.rs
   lines 807-813).
2. `every_app_command_is_proven_or_allowlisted` asserts that every label in
   `all_commands()` is in `PROVEN_LABELS` or `UNOBSERVABLE_ALLOWLIST`
   (lines 780-784), so adding a command without a real dispatch proof fails it.
3. `command_labels_are_stable_unique_and_non_empty` requires
   `all_commands().len() == expected.len()` and a matching label per entry
   (lines 747-765), so a new `all_commands()` row also needs a snapshot row.

A `PROVEN_LABELS` entry needs a real `assert_dispatch_acts` proof, and none of
the six can have one: `Snapshot` has no assistant field, and dispatch probes a
live loopback OpenCode service. So the only consistent registration is
`PENDING_COVERAGE` alone.

## Resolution

At 21:50:41 the harness was edited to add the variants to **both**
`all_commands()` and `PENDING_COVERAGE`; that state cannot pass (violates the
not-both assertion at lines 807-813 and the proven-or-allowlisted rule at lines
780-784). At 21:51:14 the six `all_commands()` rows were removed and only the
`PENDING_COVERAGE` entries remain, which is the consistent registration:

```
cargo test -p pdfxi-app --features ui,pdfium   (21:51)
  test result: ok. 514 passed; 0 failed   (lib)
  test result: ok.  24 passed; 0 failed   (ui_controls harness)
```

`source_variants_are_all_covered`, `every_app_command_is_proven_or_allowlisted`,
`command_labels_are_stable_unique_and_non_empty` and
`repository_shell_has_no_dead_controls` all pass, and the scanner reports no
finding. The pending reasons in the tree are shorter than the draft patch below
but name the owning task and the missing piece.

## Next viable alternative (still open as policy)

The integrator allowlist is the wrong place to widen; the harness belongs to a
session that owns app paths. Either:

1. An app-lane session appends the six entries below to `PENDING_COVERAGE`
   (before its closing `];`, after the `CancelPrint` entry), leaves
   `all_commands()`, `PROVEN_LABELS` and the label snapshot unchanged, and
   reruns `cargo test -p pdfxi-app --features ui,pdfium`; or
2. the orchestrator adds `crates/pdfxi-app/tests/ui_controls.rs` to the
   integrator edit allowlist, and the integrator applies exactly the same patch.

The patch to apply:

```rust
    (
        "AssistantConnect",
        "T-0253 assistant panel; dispatch probes a live loopback OpenCode service \
         and Snapshot has no assistant field, so the effect needs a headless \
         connection seam; the assistant_connect lib tests cover the state machine",
    ),
    (
        "AssistantAsk",
        "T-0253 assistant panel; requires a connected loopback service and an \
         in-flight turn, and Snapshot has no assistant field; the assistant_chat \
         lib tests cover the state machine",
    ),
    (
        "AssistantStop",
        "T-0253 assistant panel; requires a running assistant turn to cancel, and \
         Snapshot has no assistant field; the assistant_chat lib tests cover the \
         state machine",
    ),
    (
        "AssistantDisconnect",
        "T-0253 assistant panel; needs a connected service first, and Snapshot has \
         no assistant field to observe the returned-to-disconnected state",
    ),
    (
        "AssistantPair",
        "T-0253 assistant panel; the pair probe needs an explicit live endpoint and \
         a pairing code, and Snapshot has no assistant field; the \
         assistant_connect lib tests cover the state machine",
    ),
    (
        "AssistantSelectModel",
        "T-0253 assistant panel; requires a connected service that reported models \
         and carries the provider/model label, and Snapshot has no assistant \
         field; the assistant_connect lib tests cover the state machine",
    ),
```

When T-0253 gains a headless connection seam and a `Snapshot` assistant field,
these entries should move into `all_commands()` with `assert_dispatch_acts`
proofs, and the label snapshot should grow by the six stable labels
`assistant-connect`, `assistant-ask`, `assistant-stop`, `assistant-disconnect`,
`assistant-pair`, `assistant-select-model`.

## Evidence

- `target/tmp/t0253-harness-before.log` (full run, unmodified tree).
- `crates/pdfxi-app/tests/ui_controls.rs` lines 747-765, 780-784, 807-818.
- `.opencode/agents/integrator.md` edit allowlist.
