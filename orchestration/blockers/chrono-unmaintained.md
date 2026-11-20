# Blocker - chrono is unmaintained (RUSTSEC-2026-0543)

Date: 2026-11-20. Owner: integrator / orchestrator. Status: open.

## Symptom

`cargo deny check` on the T-0256 tree now reports:

```text
error[unmaintained]: chrono is unmaintained
   ┌─ chrono 0.4.45 registry+https://github.com/rust-lang/crates.io-index
   ├ ID: RUSTSEC-2026-0543
   ├ Advisory: https://rustsec.org/advisories/RUSTSEC-2026-0543
advisories FAILED, bans ok, licenses ok, sources ok
EXIT=1
```

It appeared during the T-0256 checkpoint, but it is **not** caused by T-0256:
`chrono` enters through the root workspace dependency table
(`Cargo.toml`: `chrono = { version = "0.4", default-features = false, features =
["clock", "std"] }`) and no T-0256 manifest adds it. Earlier runs passed because
the advisory database had not yet picked up RUSTSEC-2026-0543; the gate is
therefore red for the whole workspace from now on, not for one crate.

## Impact

`cargo deny check` is required in CI by RULES.md section 8. Until this is
resolved, no checkpoint can honestly claim a green deny gate, and the T-0256
acceptance sentence ("deny green") is captured before the advisory refresh.

## Options for the owner

1. Migrate the `chrono` users to `jiff` or `time` (the advisory's suggested
   replacements) and drop the workspace dependency.
2. Replace chrono's use with `std::time` where only timestamps/PDF dates are
   needed; `pdfxi-security` already computes signing times as unix seconds.
3. If neither is feasible before release, add a scoped, documented
   `[advisories] ignore = ["RUSTSEC-2026-0543"]` with a migration task — this is
   a security/policy decision, not an integrator edit.

## Next step

Owner decision. Track as a cleanup task; meanwhile T-0256's functional gates
(fmt, clippy, 367 tests, the PDFium independent-reader test) are green.
