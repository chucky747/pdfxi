# ADR 0009 — Feature statuses must mean end-user reachability

- Status: accepted
- Date: 2026-11-20

## Context

`docs/features.toml` marked 137 of 140 rows `verified` (3 more `replaced`). A sampled
review estimated that only ~15% of those rows were usable by an end user on a real PDF.
T-0205 audited every row against the code and the crate dependency graph. The audit
(`docs/evidence/hardening/T-0205.md`) found:

- **23 rows (class A)** are reachable through the GUI or CLI on real document bytes.
- **18 rows (class B)** are real byte-level implementations (whole-document encryption,
  sanitization, optimization, incremental save, page plans, form serialization, image
  export, conformance writing, metadata) with tests, but no GUI or CLI caller.
- **96 rows (class C)** are in-memory models, plans, or host-boundary stubs. This
  includes every FORMS row except FORMS-002, every REVIEW and MEDIA row, most EDIT and
  SEC rows, the action/batch/API rows whose CLI executor is `EchoOperations`, and the
  GUI navigation rows that still read `demo_*` data in `crates/pdfxi-app/src/ui.rs`.
- **3 rows** were already `replaced` and are unchanged.

The matrix was therefore overstating readiness by roughly a factor of six. The cause is
structural, not a single mistake:

1. `verified` had no definition tied to reachability. A tested library model satisfied
   `cargo xtask features verify` (tests, docs, evidence present) and was promoted.
2. Rows were closed at "library level" while the GUI crate depends only on
   `pdfxi-render`, and `pdfxi-edit`, `pdfxi-forms`, `pdfxi-automation` and
   `pdfxi-standards` have no CLI or GUI dependents.
3. The CLI action/API surface stages inputs through `EchoOperations`; it does not run
   document operations.
4. Sixteen cited tests do not exist, and `tests` arrays contain three further entries
   that are commands rather than tests; neither `features verify` nor `goal-verify`
   checked test existence. T-0205's citation table lists 18 entries (the 16 tests plus
   PLAT-001's two command entries); T-0204 records all three non-test entries
   (PLAT-001 twice, PLAT-002 once).

`AC-04` in `docs/GOAL.md` says every row is `verified` or `replaced`. Keeping the current
statuses to satisfy that sentence is exactly the dishonesty the campaign is correcting.

## Decision

1. **`verified` is reserved for class A**: the row's capability runs on real document
   bytes and an end user can reach it through the GUI or the CLI. A tested library API
   with no caller is not `verified`.
2. **Two statuses are added**:
   - `library` (class B): real byte-level implementation over PDF documents, tested, but
     no GUI/CLI caller.
   - `model` (class C): in-memory model, plan, contract, or host-boundary stub over
     caller-supplied data, or reachable only with demo/canned data.
   `replaced` keeps its current meaning. `planned`, `in-progress` and `deferred` are
   unchanged.
3. **Rows are mapped as follows**: 23 A rows stay `verified`; 18 B rows become
   `library`; 96 C rows become `model`; 3 replaced rows stay `replaced`. The per-row
   mapping is in `orchestration/feature-updates/T-0205.toml`.
4. **`docs/features.toml` is not edited by this task.** The fragment is merged by the
   integrator, and `docs/FEATURES.md` must be regenerated from the merged matrix.
5. **Rows whose evidence does not match their title are not rescued by this ADR.**
   PRINT-002 through PRINT-006 and PRINT-008 are `model`: their titles claim standards
   validation, colour management, output preview, ink management, trapping and droplets,
   while their cited code is print imposition planning. They must be retitled or
   re-implemented.
6. **VIEW-008 through VIEW-015, VIEW-017 and VIEW-018** are `model` today because the GUI
   installs `demo_*` bookmarks, attachments, layers, links, text and navigation models
   (`crates/pdfxi-app/src/ui.rs`, `demo_*` helpers). They are re-evaluated after T-0203
   wires real document data into those panes.

## Required xtask change

Exactly one line in `xtask/src/main.rs`:

```rust
// xtask/src/main.rs:38
const ALLOWED_STATUS: &[&str] = &["planned", "in-progress", "verified", "library", "model", "replaced", "deferred"];
```

No other change is required, and none should be made to make the gate pass:

- `features_verify` (`xtask/src/main.rs:518-550`) applies its tests/docs/evidence checks
  only when `status == "verified"`, so `library` and `model` rows validate without
  pretending to be verified.
- `goal_verify --all` (`xtask/src/main.rs:439-442`) already treats every status except
  `verified`, `replaced` and `deferred` as unfinished. `library` and `model` therefore
  count as unfinished automatically.
- Milestone gates (`xtask/src/main.rs:415`) already require `verified` or `replaced`.
- `features apply` (`xtask/src/main.rs:627`) already merges the `status` field.

The integrator owns `xtask/**` and must add the two literals. Until then,
`cargo xtask features verify` rejects fragments using the new statuses.

## Consequences

- **AC-04 is not met and must not be reworded to accept `library` or `model`.** AC-04
  currently requires every row to be `verified` or `replaced`. Under this ADR, 114 rows
  (18 `library` + 96 `model`) are explicitly not done. Rewording AC-04 to count
  `library`/`model` as done would recreate the same overstatement under new names.
- **`cargo xtask goal-verify --all` will stop printing `GOAL VERIFIED`.** It will fail
  with `114 feature rows are not verified or replaced` (the first 20 are listed by the
  existing error path) until rows are actually wired or replaced. That is the intended,
  honest result. It should be recorded as evidence in that state, not suppressed.
- `goal-verify --milestone` for M1 through M7 will also fail wherever a required row is
  `library` or `model`; the M0 gate is unaffected.
- The matrix can no longer be "greened" by a library-only task. Promotion to `verified`
  now requires a CLI or GUI call site and an end-to-end test on a real PDF.
- The 16 non-existent test citations, plus the 3 non-test `tests` entries, remain a
   separate defect. Correcting them does not
  change a row's class, but `feature verify` should gain a test-existence check (T-0204).

## Follow-up work

1. **End-to-end redaction on real PDFs.** `pdfxi-edit` has no `pdfxi-core` dependency and
   no CLI/GUI caller; EDIT-013 is a model over page-content fragments. Wire it to a real
   `Document`, remove content (not merely cover it), and verify with text/image
   extraction, per RULES §8.
2. **Production signature cryptography.** SEC-005 and FORMS-010 rely on the test-only
   `modpow_u128` toy RSA in `crates/pdfxi-security/src/cms.rs:280`. Add a real RSA/PKCS#1
   provider through the existing `SignatureProvider` boundary before either row can be
   `verified`.
3. **Forms wired to the CLI and GUI.** `pdfxi-forms` has no dependent crate. FORMS-001
   through FORMS-010 remain `model` until fill, FDF/XFDF import/export and flattening are
   reachable from `pdfxi` or `pdfxi-ui` on a real AcroForm.
4. **GUI wiring to edit/forms/security.** `crates/pdfxi-app` depends only on
   `pdfxi-render`. Adding the dependency edges and real panes is a prerequisite for
   VIEW-008..015/017/018 and ACCESS/REVIEW rows.
5. **Fix the PRINT row titles/evidence** (PRINT-002..006, PRINT-008) and split bundled
   rows STD-003 and EXPORT-008 per ADR 0007.
6. **Add a test-existence gate** (T-0204) and correct the 18 bad citations listed in
   `docs/evidence/hardening/T-0205.md`.
